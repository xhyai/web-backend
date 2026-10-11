mod common;

use common::{assert_swagger_valid, TestApp};
use serde_json::{json, Value};

fn registration_payload(email: &str) -> Value {
    json!({
        "email": email,
        "nick_name": "tester",
        "first_name": "Test",
        "last_name": "Tester",
        "gender": "male",
        "short_info": "kratke info o resiteli",
        "github": null,
        "discord": null,
        "addr_street": "Botanicka 68a",
        "addr_city": "Brno",
        "addr_zip": "60200",
        "addr_country": "CZ",
        "school_name": "Gymnazium",
        "school_street": "Skolska 1",
        "school_city": "Brno",
        "school_zip": "60200",
        "school_country": "cz",
        "school_finish": 2027,
        "tshirt_size": "m",
        "notify_eval": true,
        "notify_response": false,
        "password": "tajne-heslo-123",
        "referral": "{}",
    })
}

#[tokio::test]
async fn registration_ok_creates_user_profile_notify() {
    let app = TestApp::new().await;
    let resp = app
        .server
        .post("/registration")
        .json(&registration_payload("newbie@example.com"))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("post", "/registration", 200, &body);
    assert!(body.get("error").is_none(), "unexpected error: {body}");

    // user row exists, password verifies, role defaults to participant
    let row: (i64, String, String) =
        sqlx::query_as("SELECT id, password, role FROM users WHERE email = 'newbie@example.com'")
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(bcrypt::verify("tajne-heslo-123", &row.1).unwrap());
    assert_eq!(row.2, "participant");

    // profile row: country lowercased, tshirt uppercased, school_finish int
    let profile: (String, String, String, i64, String) = sqlx::query_as(
        "SELECT addr_country, school_country, tshirt_size, school_finish, referral
         FROM profiles WHERE user_id = ?",
    )
    .bind(row.0)
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(profile.0, "cz");
    assert_eq!(profile.1, "cz");
    assert_eq!(profile.2, "M");
    assert_eq!(profile.3, 2027);
    assert_eq!(profile.4, "{}");

    // notify row with the requested flags
    let notify: (String, i64, i64) =
        sqlx::query_as("SELECT auth_token, notify_eval, notify_response FROM users_notify WHERE user = ?")
            .bind(row.0)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(notify.0.len(), 40);
    assert_eq!((notify.1, notify.2), (1, 0));

    // the new user can log in with the chosen password
    let login = app
        .server
        .post("/auth")
        .json(&json!({
            "grant_type": "password",
            "username": "newbie@example.com",
            "password": "tajne-heslo-123",
        }))
        .await;
    login.assert_status_ok();
}

#[tokio::test]
async fn registration_duplicate_email_errors() {
    let app = TestApp::new().await;
    // part@demo.cz is seeded by the harness
    let resp = app
        .server
        .post("/registration")
        .json(&registration_payload("part@demo.cz"))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("post", "/registration", 200, &body);
    assert_eq!(
        body["error"],
        "Na tento e-mail je již někdo zaregistrovaný."
    );

    // registering twice with a fresh address: the second one fails too
    let first = app
        .server
        .post("/registration")
        .json(&registration_payload("twice@example.com"))
        .await;
    first.assert_status_ok();
    assert!(first.json::<Value>().get("error").is_none());
    let second = app
        .server
        .post("/registration")
        .json(&registration_payload("twice@example.com"))
        .await;
    second.assert_status_ok();
    assert_eq!(
        second.json::<Value>()["error"],
        "Na tento e-mail je již někdo zaregistrovaný."
    );
}

#[tokio::test]
async fn registration_invalid_email_errors() {
    let app = TestApp::new().await;
    let payload = registration_payload("not-an-email");
    let resp = app.server.post("/registration").json(&payload).await;
    resp.assert_status_ok();
    assert!(resp.json::<Value>().get("error").is_some());

    // and nothing was created
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE email = 'not-an-email'")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn registration_invalid_gender_errors() {
    let app = TestApp::new().await;
    let mut payload = registration_payload("gender@example.com");
    payload["gender"] = json!("robot");
    let resp = app.server.post("/registration").json(&payload).await;
    resp.assert_status_ok();
    assert!(resp.json::<Value>().get("error").is_some());

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE email = 'gender@example.com'")
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn registration_empty_password_errors() {
    let app = TestApp::new().await;
    let mut payload = registration_payload("nopass@example.com");
    payload["password"] = json!("");
    let resp = app.server.post("/registration").json(&payload).await;
    resp.assert_status_ok();
    assert!(resp.json::<Value>().get("error").is_some());
}

#[tokio::test]
async fn forgotten_password_ok_resets_and_audits() {
    let app = TestApp::new().await;

    let before: String =
        sqlx::query_scalar("SELECT password FROM users WHERE email = 'part@demo.cz'")
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(bcrypt::verify("demo", &before).unwrap());

    let resp = app
        .server
        .post("/forgottenPassword")
        .json(&json!({ "email": "part@demo.cz" }))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("post", "/forgottenPassword", 200, &body);
    assert_eq!(body["result"], "ok");

    // password hash changed and the old password no longer verifies
    let after: String = sqlx::query_scalar("SELECT password FROM users WHERE email = 'part@demo.cz'")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_ne!(before, after);
    assert!(!bcrypt::verify("demo", &after).unwrap());

    // audit log entry was written
    let audit: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE scope = 'AUTH' AND line = 'Password reset'
         AND user_id = (SELECT id FROM users WHERE email = 'part@demo.cz')",
    )
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(audit, 1);
}

#[tokio::test]
async fn forgotten_password_unknown_email_400() {
    let app = TestApp::new().await;
    let resp = app
        .server
        .post("/forgottenPassword")
        .json(&json!({ "email": "nobody@example.com" }))
        .await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
    let body: Value = resp.json();
    assert_eq!(body["result"], "error");
}
