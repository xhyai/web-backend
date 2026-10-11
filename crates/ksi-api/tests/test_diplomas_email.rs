//! Integration tests for the e-mail and diploma endpoints.

mod common;

use axum_test::multipart::{MultipartForm, Part};
use common::{assert_swagger_valid, TestApp};
use serde_json::{json, Value};

/// All diploma tests share one on-disk directory (files are namespaced by
/// user/year, so parallel tests do not collide).
fn use_test_diploma_dir() {
    std::env::set_var("DIPLOMA_DIR", "/tmp/ksi-diplomas-test");
    std::fs::create_dir_all("/tmp/ksi-diplomas-test").unwrap();
}

/// Create an admin user + token in the test DB (the shared seed only has
/// org + participant, and grant requires admin).
async fn make_admin(app: &TestApp) -> String {
    let pw = bcrypt::hash("demo", 4).unwrap();
    let admin: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, first_name, last_name, nick_name, sex, password, short_info, role)
         VALUES ('admin@demo.cz', 'Ad', 'Min', NULL, 'male', ?, '', 'admin')
         RETURNING id",
    )
    .bind(&pw)
    .fetch_one(&app.db)
    .await
    .unwrap();
    let token = format!("test-admin-token-{admin}-{}", uuid::Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO oauth2_tokens (access_token, user, expire, refresh_token, granted)
         VALUES (?, ?, datetime('now', '+1 hour'), ?, datetime('now'))",
    )
    .bind(&token)
    .bind(admin)
    .bind(format!("refresh-{token}"))
    .execute(&app.db)
    .await
    .unwrap();
    token
}

fn auth_post_multipart(
    app: &TestApp,
    path: &str,
    token: &str,
    year: Option<i64>,
    form: MultipartForm,
) -> axum_test::TestRequest {
    let mut req = app
        .server
        .post(path)
        .add_header("Authorization", format!("Bearer {token}"));
    if let Some(y) = year {
        req = req.add_header("year", y.to_string());
    }
    req.multipart(form)
}

const FAKE_PDF: &[u8] = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n1 0 obj<</Type/Catalog>>endobj\ntrailer\n<<>>\n%%EOF";

fn pdf_form() -> MultipartForm {
    MultipartForm::new().add_part(
        "files",
        Part::bytes(FAKE_PDF)
            .file_name("diplom.pdf")
            .mime_type("application/pdf"),
    )
}

// ---------------------------------------------------------------------------
// POST /admin/diploma/{userId}/grant
// ---------------------------------------------------------------------------

#[tokio::test]
async fn diploma_grant_roundtrip() {
    use_test_diploma_dir();
    let app = TestApp::new().await;
    let admin = make_admin(&app).await;
    let uid = app.ids.participant;
    let yid = app.ids.year;

    // grant
    let resp = auth_post_multipart(
        &app,
        &format!("/admin/diploma/{uid}/grant"),
        &admin,
        Some(yid),
        pdf_form(),
    )
    .await;
    resp.assert_status(axum::http::StatusCode::NO_CONTENT);

    // file landed on disk
    let path = format!("/tmp/ksi-diplomas-test/year_{yid}/user_{uid}.pdf");
    assert!(std::path::Path::new(&path).exists());
    assert!(std::fs::read(&path).unwrap().starts_with(b"%PDF-"));

    // list
    let resp = app.server.get(&format!("/diplomas/{uid}")).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("get", "/diplomas/{userId}", 200, &body);
    let diplomas = body["diplomas"].as_array().unwrap();
    assert_eq!(diplomas.len(), 1);
    assert_eq!(diplomas[0]["year"], json!(yid));
    assert_eq!(diplomas[0]["revoked"], json!(false));
    assert_eq!(
        diplomas[0]["url"],
        json!(format!("/diplomas/{uid}/{yid}/show"))
    );

    // download
    let resp = app
        .server
        .get(&format!("/diplomas/{uid}/{yid}/show"))
        .await;
    resp.assert_status_ok();
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/pdf"
    );
    assert_eq!(resp.as_bytes(), FAKE_PDF);

    std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn diploma_grant_requires_admin() {
    use_test_diploma_dir();
    let app = TestApp::new().await;
    let uid = app.ids.participant;

    // org (not admin) -> 401
    app.auth_post(&format!("/admin/diploma/{uid}/grant"), true)
        .add_header("year", app.ids.year.to_string())
        .multipart(pdf_form())
        .await
        .assert_status(axum::http::StatusCode::UNAUTHORIZED);
    // anonymous -> 401
    app.server
        .post(&format!("/admin/diploma/{uid}/grant"))
        .add_header("year", app.ids.year.to_string())
        .multipart(pdf_form())
        .await
        .assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn diploma_grant_rejects_non_pdf() {
    use_test_diploma_dir();
    let app = TestApp::new().await;
    let admin = make_admin(&app).await;

    let form = MultipartForm::new().add_part(
        "files",
        Part::bytes(b"definitely not a pdf".as_slice())
            .file_name("x.txt")
            .mime_type("text/plain"),
    );
    let resp = auth_post_multipart(
        &app,
        &format!("/admin/diploma/{}/grant", app.ids.participant),
        &admin,
        Some(app.ids.year),
        form,
    )
    .await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
    let body: Value = resp.json();
    assert!(body["errors"][0]["detail"]
        .as_str()
        .unwrap()
        .contains("application/pdf"));
}

#[tokio::test]
async fn diploma_grant_requires_year_header() {
    use_test_diploma_dir();
    let app = TestApp::new().await;
    let admin = make_admin(&app).await;

    auth_post_multipart(
        &app,
        &format!("/admin/diploma/{}/grant", app.ids.participant),
        &admin,
        None,
        pdf_form(),
    )
    .await
    .assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn diploma_grant_sealed_year_forbidden() {
    use_test_diploma_dir();
    let app = TestApp::new().await;
    let admin = make_admin(&app).await;

    let sealed: i64 = sqlx::query_scalar(
        "INSERT INTO years (year, sealed, point_pad) VALUES ('2025/2026', 1, 0) RETURNING id",
    )
    .fetch_one(&app.db)
    .await
    .unwrap();

    let resp = auth_post_multipart(
        &app,
        &format!("/admin/diploma/{}/grant", app.ids.participant),
        &admin,
        Some(sealed),
        pdf_form(),
    )
    .await;
    resp.assert_status(axum::http::StatusCode::FORBIDDEN);
    let body: Value = resp.json();
    assert!(body["errors"][0]["detail"]
        .as_str()
        .unwrap()
        .contains("zapečetěn"));
}

#[tokio::test]
async fn diploma_show_missing_returns_404() {
    let app = TestApp::new().await;

    app.server
        .get(&format!("/diplomas/{}/99999/show", app.ids.participant))
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);

    // empty list for a user with no diplomas
    let resp = app
        .server
        .get(&format!("/diplomas/{}", app.ids.participant))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body, json!({"diplomas": []}));
    assert_swagger_valid("get", "/diplomas/{userId}", 200, &body);
}

// ---------------------------------------------------------------------------
// POST /admin/e-mail
// ---------------------------------------------------------------------------

/// Seed the participant as an active, subscribed mail recipient.
async fn seed_mail_recipient(app: &TestApp, user_id: i64, sex: &str, notify_ksi: bool) {
    sqlx::query("UPDATE users SET sex = ? WHERE id = ?")
        .bind(sex)
        .bind(user_id)
        .execute(&app.db)
        .await
        .unwrap();
    // active in the year: one evaluation
    sqlx::query(
        "INSERT INTO evaluations (user, module, points, cheat, full_report) VALUES (?, ?, 5, 0, '')",
    )
    .bind(user_id)
    .bind(app.ids.module)
    .execute(&app.db)
    .await
    .unwrap();
    // subscribed
    sqlx::query(
        "INSERT INTO users_notify (user, auth_token, notify_eval, notify_response, notify_ksi, notify_events)
         VALUES (?, 'tok', 1, 1, ?, 1)",
    )
    .bind(user_id)
    .bind(if notify_ksi { 1 } else { 0 })
    .execute(&app.db)
    .await
    .unwrap();
    // profile for Category filtering
    sqlx::query(
        "INSERT INTO profiles (user_id, addr_street, addr_city, addr_zip, addr_country,
                               school_name, school_street, school_city, school_zip, school_country,
                               school_finish, tshirt_size)
         VALUES (?, '', '', '', '', '', '', '', '', '', 2027, 'M')",
    )
    .bind(user_id)
    .execute(&app.db)
    .await
    .unwrap();
}

fn email_payload(to: i64) -> Value {
    json!({
        "e-mail": {
            "Subject": "Ahoj resiteli",
            "Body": "<p>Ahoj!</p>",
            "To": [to],
            "Bcc": [],
            "Type": "ksi",
        }
    })
}

#[tokio::test]
async fn admin_email_sends_to_active_subscribed_participants() {
    let app = TestApp::new().await;
    seed_mail_recipient(&app, app.ids.participant, "male", true).await;

    let resp = app
        .auth_post("/admin/e-mail", true)
        .json(&email_payload(app.ids.year))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body, json!({"count": 1}));
    assert_swagger_valid("post", "/admin/e-mail", 200, &body);
}

#[tokio::test]
async fn admin_email_requires_org() {
    let app = TestApp::new().await;

    // participant -> 400 (mirrors Python)
    app.auth_post("/admin/e-mail", false)
        .json(&email_payload(app.ids.year))
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
    // anonymous -> 400
    app.server
        .post("/admin/e-mail")
        .json(&email_payload(app.ids.year))
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_email_gender_filter() {
    let app = TestApp::new().await;
    // participant is male (seed default), subscribed
    seed_mail_recipient(&app, app.ids.participant, "male", true).await;

    let mut payload = email_payload(app.ids.year);
    payload["e-mail"]["Gender"] = json!("female");
    let resp = app.auth_post("/admin/e-mail", true).json(&payload).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body, json!({"count": 0}));

    payload["e-mail"]["Gender"] = json!("male");
    let resp = app.auth_post("/admin/e-mail", true).json(&payload).await;
    let body: Value = resp.json();
    assert_eq!(body, json!({"count": 1}));
}

#[tokio::test]
async fn admin_email_unsubscribed_filtered_out() {
    let app = TestApp::new().await;
    // notify_ksi = false -> excluded for Type=ksi
    seed_mail_recipient(&app, app.ids.participant, "male", false).await;

    let resp = app
        .auth_post("/admin/e-mail", true)
        .json(&email_payload(app.ids.year))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body, json!({"count": 0}));

    // but Type=events (notify_events = 1) still reaches them
    let mut payload = email_payload(app.ids.year);
    payload["e-mail"]["Type"] = json!("events");
    let resp = app.auth_post("/admin/e-mail", true).json(&payload).await;
    let body: Value = resp.json();
    assert_eq!(body, json!({"count": 1}));
}

#[tokio::test]
async fn admin_email_successful_only() {
    let app = TestApp::new().await;
    seed_mail_recipient(&app, app.ids.participant, "male", true).await;

    let mut payload = email_payload(app.ids.year);
    payload["e-mail"]["Successful"] = json!(true);

    // 5 points out of 20 max with 60% threshold -> not successful
    let resp = app.auth_post("/admin/e-mail", true).json(&payload).await;
    let body: Value = resp.json();
    assert_eq!(body, json!({"count": 0}));

    // bump to full points -> successful
    // (note: the seed year has TWO modules worth 10 pts each — the hidden
    // wave's module counts toward the year max just like in Python's
    // util/task.py::sum_points — so the threshold is 60% of 20 = 12)
    sqlx::query("UPDATE evaluations SET points = 10 WHERE user = ?")
        .bind(app.ids.participant)
        .execute(&app.db)
        .await
        .unwrap();
    let module2: i64 = sqlx::query_scalar(
        "SELECT m.id FROM modules m
         JOIN tasks t ON t.id = m.task
         WHERE t.wave = ?",
    )
    .bind(app.ids.wave_hidden)
    .fetch_one(&app.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO evaluations (user, module, points, cheat, full_report) VALUES (?, ?, 10, 0, '')",
    )
    .bind(app.ids.participant)
    .bind(module2)
    .execute(&app.db)
    .await
    .unwrap();
    let resp = app.auth_post("/admin/e-mail", true).json(&payload).await;
    let body: Value = resp.json();
    assert_eq!(body, json!({"count": 1}));
}

#[tokio::test]
async fn admin_email_category_hs() {
    let app = TestApp::new().await;
    // school_finish = 2027; year '2026/2027' -> year_end = 2027 -> hs matches
    seed_mail_recipient(&app, app.ids.participant, "male", true).await;

    let mut payload = email_payload(app.ids.year);
    payload["e-mail"]["Category"] = json!("hs");
    let resp = app.auth_post("/admin/e-mail", true).json(&payload).await;
    let body: Value = resp.json();
    assert_eq!(body, json!({"count": 1}));

    payload["e-mail"]["Category"] = json!("other");
    let resp = app.auth_post("/admin/e-mail", true).json(&payload).await;
    let body: Value = resp.json();
    assert_eq!(body, json!({"count": 0}));
}

#[tokio::test]
async fn admin_email_easteregg_appends_ps() {
    let app = TestApp::new().await;
    seed_mail_recipient(&app, app.ids.participant, "male", true).await;
    sqlx::query("INSERT INTO mail_eastereggs (body) VALUES ('tajny vtip')")
        .execute(&app.db)
        .await
        .unwrap();

    let mut payload = email_payload(app.ids.year);
    payload["e-mail"]["Easteregg"] = json!(true);
    let resp = app.auth_post("/admin/e-mail", true).json(&payload).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body, json!({"count": 1}));
    // e-mail is logged, not sent — the count is the observable contract
}
