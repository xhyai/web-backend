mod common;

use common::{assert_swagger_valid, TestApp};
use serde_json::{json, Value};

#[tokio::test]
async fn auth_password_grant_ok() {
    let app = TestApp::new().await;
    let resp = app
        .server
        .post("/auth")
        .json(&json!({
            "grant_type": "password",
            "username": "org@demo.cz",
            "password": "demo",
        }))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("post", "/auth", 200, &body);

    assert_eq!(body["token_type"], "Bearer");
    assert!(!body["access_token"].as_str().unwrap().is_empty());
    assert!(!body["refresh_token"].as_str().unwrap().is_empty());

    // the new token works
    let token = body["access_token"].as_str().unwrap();
    let me = app
        .server
        .get("/years")
        .add_header("Authorization", format!("Bearer {token}"))
        .await;
    me.assert_status_ok();
}

#[tokio::test]
async fn auth_wrong_password_401() {
    let app = TestApp::new().await;
    let resp = app
        .server
        .post("/auth")
        .json(&json!({
            "grant_type": "password",
            "username": "org@demo.cz",
            "password": "wrong",
        }))
        .await;
    resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
    let body: Value = resp.json();
    assert!(body["errors"].is_array());
}

#[tokio::test]
async fn auth_unknown_user_401() {
    let app = TestApp::new().await;
    let resp = app
        .server
        .post("/auth")
        .json(&json!({
            "grant_type": "password",
            "username": "nobody@example.com",
            "password": "demo",
        }))
        .await;
    resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn auth_bad_grant_400() {
    let app = TestApp::new().await;
    let resp = app
        .server
        .post("/auth")
        .json(&json!({"grant_type": "client_credentials"}))
        .await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn logout_invalidates_token() {
    let app = TestApp::new().await;
    // login to get a fresh token
    let login = app
        .server
        .post("/auth")
        .json(&json!({
            "grant_type": "password",
            "username": "part@demo.cz",
            "password": "demo",
        }))
        .await;
    let token = login.json::<Value>()["access_token"].as_str().unwrap().to_string();

    // create a post with the token (proves it works)
    let created = app
        .server
        .post("/posts")
        .add_header("Authorization", format!("Bearer {token}"))
        .json(&json!({"post": {"thread": app.ids.thread, "body": "ahoj"}}))
        .await;
    created.assert_status_ok();

    // logout
    let out = app
        .server
        .get("/logout")
        .add_header("Authorization", format!("Bearer {token}"))
        .await;
    out.assert_status_ok();

    // token no longer works
    let denied = app
        .server
        .post("/posts")
        .add_header("Authorization", format!("Bearer {token}"))
        .json(&json!({"post": {"thread": app.ids.thread, "body": "ahoj"}}))
        .await;
    denied.assert_status(axum::http::StatusCode::BAD_REQUEST); // not logged in
}
