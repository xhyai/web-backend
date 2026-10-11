mod common;

use axum_test::multipart::{MultipartForm, Part};
use common::{assert_swagger_valid, TestApp};
use serde_json::{json, Value};

/// Seed the `profiles` + `users_notify` rows the profile endpoints expect.
async fn seed_profile(app: &TestApp, user_id: i64) {
    sqlx::query(
        "INSERT INTO profiles (user_id, addr_street, addr_city, addr_zip, addr_country,
                               school_name, school_street, school_city, school_zip,
                               school_country, school_finish, tshirt_size)
         VALUES (?, 'Korunni 2569/70', 'Praha', '12000', 'cz', 'Gymnazium',
                 'Skolni 1', 'Praha', '11000', 'cz', 2028, 'M')",
    )
    .bind(user_id)
    .execute(&app.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO users_notify (user, auth_token, notify_eval, notify_response,
                                   notify_ksi, notify_events)
         VALUES (?, 'testtoken', 1, 0, 1, 1)",
    )
    .bind(user_id)
    .execute(&app.db)
    .await
    .unwrap();
}

fn profile_edit() -> Value {
    json!({
        "email": "part@demo.cz",
        "nick_name": "Particek",
        "first_name": "Par",
        "last_name": "Ticipant",
        "gender": "male",
        "short_info": "Ahoj, jsem resitel.",
        "github": "participant",
        "discord": "participant#1234",
        "addr_street": "Korunni 2569/70",
        "addr_city": "Praha",
        "addr_zip": "12000",
        "addr_country": "CZ",
        "school_name": "Gymnazium",
        "school_street": "Skolni 1",
        "school_city": "Praha",
        "school_zip": "11000",
        "school_country": "CZ",
        "school_finish": 2028,
        "tshirt_size": "M",
        "notify_eval": true,
        "notify_response": false,
        "notify_ksi": true,
        "notify_events": false,
    })
}

#[tokio::test]
async fn profile_get_anonymous_returns_fake() {
    let app = TestApp::new().await;
    let resp = app.server.get("/profile").await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body, json!({"profile": {"id": 0, "signed_in": false}}));
}

#[tokio::test]
async fn profile_get_ok() {
    let app = TestApp::new().await;
    seed_profile(&app, app.ids.participant).await;

    let resp = app.auth_get("/profile", false).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("get", "/profile", 200, &body);

    let p = &body["profile"];
    assert_eq!(p["id"], app.ids.participant);
    assert_eq!(p["first_name"], "Par");
    assert_eq!(p["email"], "part@demo.cz");
    assert_eq!(p["gender"], "male");
    // sensitive fields are included for the owner
    assert_eq!(p["addr_street"], "Korunni 2569/70");
    assert_eq!(p["notify_eval"], true);
    assert_eq!(p["notify_response"], false);
    assert!(body["tasks"].is_array());
    assert!(body["taskScores"].is_array());
}

#[tokio::test]
async fn basic_profile_ok() {
    let app = TestApp::new().await;
    seed_profile(&app, app.ids.participant).await;

    let resp = app.auth_get("/basicProfile", false).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("get", "/basicProfile", 200, &body);

    let p = &body["basicProfile"];
    assert_eq!(p["id"], app.ids.participant);
    assert_eq!(p["signed_in"], true);
    assert_eq!(p["first_name"], "Par");
    assert_eq!(p["role"], "participant");
}

#[tokio::test]
async fn basic_profile_anonymous_returns_fake() {
    let app = TestApp::new().await;
    let resp = app.server.get("/basicProfile").await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body, json!({"profile": {"id": 0, "signed_in": false}}));
}

#[tokio::test]
async fn profile_update_ok() {
    let app = TestApp::new().await;
    seed_profile(&app, app.ids.participant).await;

    let resp = app
        .auth_put("/profile", false)
        .json(&profile_edit())
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("put", "/profile", 200, &body);

    let p = &body["profile"];
    assert_eq!(p["nick_name"], "Particek");
    assert_eq!(p["short_info"], "Ahoj, jsem resitel.");
    assert_eq!(p["github"], "participant");
    assert_eq!(p["discord"], "participant#1234");
    assert_eq!(p["tshirt_size"], "M");
    // countries are lowercased, like the Python endpoint
    assert_eq!(p["addr_country"], "cz");
    assert_eq!(p["notify_events"], false);

    // persisted in the db
    let nick: String = sqlx::query_scalar("SELECT nick_name FROM users WHERE id = ?")
        .bind(app.ids.participant)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(nick, "Particek");
    let street: String =
        sqlx::query_scalar("SELECT addr_street FROM profiles WHERE user_id = ?")
            .bind(app.ids.participant)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(street, "Korunni 2569/70");
}

#[tokio::test]
async fn profile_update_anonymous_400() {
    let app = TestApp::new().await;
    let resp = app.server.put("/profile").json(&profile_edit()).await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn profile_get_other_as_org() {
    let app = TestApp::new().await;
    seed_profile(&app, app.ids.participant).await;

    // org (not admin): no street address
    let resp = app
        .auth_get(&format!("/profile/{}", app.ids.participant), true)
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["profile"]["id"], app.ids.participant);
    assert!(body["profile"].get("addr_street").is_none());
    assert_eq!(body["profile"]["school_name"], "Gymnazium");
}

#[tokio::test]
async fn profile_get_other_as_participant_401() {
    let app = TestApp::new().await;
    let resp = app
        .auth_get(&format!("/profile/{}", app.ids.org), false)
        .await;
    resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn profile_get_other_missing_404() {
    let app = TestApp::new().await;
    let resp = app.auth_get("/profile/424242", true).await;
    resp.assert_status(axum::http::StatusCode::NOT_FOUND);
}

// 1x1 transparent PNG
const PNG_BYTES: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49,
    0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06,
    0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44,
    0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D,
    0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42,
    0x60, 0x82,
];

#[tokio::test]
async fn picture_upload_ok() {
    let app = TestApp::new().await;
    let form = MultipartForm::new()
        .add_part("file", Part::bytes(&PNG_BYTES[..]).file_name("pic.png"));

    let resp = app
        .auth_post("/profile/picture", false)
        .multipart(form)
        .await;
    resp.assert_status_ok();

    let dest = format!("data/images/profile/user_{}.png", app.ids.participant);
    assert!(
        std::path::Path::new(&dest).exists(),
        "uploaded picture should exist at {dest}"
    );
    let stored: String =
        sqlx::query_scalar("SELECT profile_picture FROM users WHERE id = ?")
            .bind(app.ids.participant)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(stored, dest);
    std::fs::remove_file(&dest).unwrap();
}

#[tokio::test]
async fn picture_upload_bad_type_400() {
    let app = TestApp::new().await;
    let form = MultipartForm::new()
        .add_part("file", Part::bytes(b"not an image".as_slice()).file_name("x.txt"));

    let resp = app
        .auth_post("/profile/picture", false)
        .multipart(form)
        .await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn picture_upload_anonymous_400() {
    let app = TestApp::new().await;
    let form = MultipartForm::new()
        .add_part("file", Part::bytes(&PNG_BYTES[..]).file_name("pic.png"));

    let resp = app.server.post("/profile/picture").multipart(form).await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
}
