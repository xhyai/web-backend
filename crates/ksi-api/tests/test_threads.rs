mod common;

use common::{assert_swagger_valid, TestApp};
use serde_json::{json, Value};

#[tokio::test]
async fn threads_list() {
    let app = TestApp::new().await;
    let resp = app.server.get("/threads").await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("get", "/threads", 200, &body);
    assert!(body["threads"].as_array().unwrap().len() >= 1);
}

#[tokio::test]
async fn thread_detail() {
    let app = TestApp::new().await;
    let resp = app.server.get(&format!("/threads/{}", app.ids.thread)).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("get", "/threads/{id}", 200, &body);
    assert_eq!(body["thread"]["id"], app.ids.thread);
}

#[tokio::test]
async fn thread_detail_404() {
    let app = TestApp::new().await;
    app.server
        .get("/threads/999999")
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn thread_details_includes_posts_with_resolved_state() {
    let app = TestApp::new().await;

    // participant asks, org answers (auto-resolves)
    let q: Value = app
        .auth_post("/posts", false)
        .json(&json!({"post": {"thread": app.ids.thread, "body": "Otázka?"}}))
        .await
        .json();
    let qid = q["post"]["id"].as_i64().unwrap();
    app.auth_post("/posts", true)
        .json(&json!({"post": {"thread": app.ids.thread, "body": "Odpověď.", "parent": qid}}))
        .await;

    let resp = app.server.get(&format!("/threadDetails/{}", app.ids.thread)).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("get", "/threadDetails/{id}", 200, &body);

    let posts = body["posts"].as_array().unwrap();
    assert_eq!(posts.len(), 2);
    let question = posts.iter().find(|p| p["id"] == qid).unwrap();
    assert!(question["resolved_at"].is_string());
    assert!(question["reaction"].is_array());
}
