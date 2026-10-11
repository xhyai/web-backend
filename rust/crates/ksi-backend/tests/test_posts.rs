mod common;

use common::{assert_swagger_valid, TestApp};
use serde_json::{json, Value};

async fn create_post(app: &TestApp, org: bool, body: &str, parent: Option<i64>) -> Value {
    let mut post = json!({"thread": app.ids.thread, "body": body});
    if let Some(p) = parent {
        post["parent"] = json!(p);
    }
    let resp = app
        .auth_post("/posts", org)
        .json(&json!({"post": post}))
        .await;
    resp.assert_status_ok();
    resp.json()
}

#[tokio::test]
async fn create_post_ok_and_swagger_valid() {
    let app = TestApp::new().await;
    let body = create_post(&app, false, "Nerozumím zadání, poradíte?", None).await;
    assert_swagger_valid("post", "/posts", 200, &body);
    assert_eq!(body["post"]["body"], "Nerozumím zadání, poradíte?");
    assert_eq!(body["post"]["author"], app.ids.participant);
    assert_eq!(body["post"]["resolved_at"], Value::Null);
    assert!(body["post"]["published_at"].is_string());
}

#[tokio::test]
async fn create_post_requires_login() {
    let app = TestApp::new().await;
    let resp = app
        .server
        .post("/posts")
        .json(&json!({"post": {"thread": app.ids.thread, "body": "x"}}))
        .await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_post_body_too_long() {
    let app = TestApp::new().await;
    let long = "x".repeat(8001);
    let resp = app
        .auth_post("/posts", false)
        .json(&json!({"post": {"thread": app.ids.thread, "body": long}}))
        .await;
    resp.assert_status(axum::http::StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn create_post_unknown_thread() {
    let app = TestApp::new().await;
    let resp = app
        .auth_post("/posts", false)
        .json(&json!({"post": {"thread": 999999, "body": "x"}}))
        .await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
}

/// #182: an org's reply auto-resolves the parent post.
#[tokio::test]
async fn org_reply_auto_resolves_parent() {
    let app = TestApp::new().await;
    let parent = create_post(&app, false, "Otázka", None).await;
    let parent_id = parent["post"]["id"].as_i64().unwrap();

    create_post(&app, true, "Odpověď orga", Some(parent_id)).await;

    let got: Value = app.server.get(&format!("/posts/{parent_id}")).await.json();
    assert!(got["post"]["resolved_at"].is_string());
}

/// #182: a participant's reply reopens a resolved post.
#[tokio::test]
async fn participant_reply_reopens_resolved_post() {
    let app = TestApp::new().await;
    let parent = create_post(&app, false, "Otázka", None).await;
    let parent_id = parent["post"]["id"].as_i64().unwrap();

    // org resolves manually
    app.auth_post(&format!("/posts/{parent_id}/resolve"), true)
        .await
        .assert_status_ok();
    let resolved: Value = app.server.get(&format!("/posts/{parent_id}")).await.json();
    assert!(resolved["post"]["resolved_at"].is_string());

    // participant replies -> reopened
    create_post(&app, false, "Ještě dotaz", Some(parent_id)).await;
    let reopened: Value = app.server.get(&format!("/posts/{parent_id}")).await.json();
    assert_eq!(reopened["post"]["resolved_at"], Value::Null);
}

/// #182: org can resolve/unresolve manually; participant gets 403.
#[tokio::test]
async fn manual_resolve_org_only() {
    let app = TestApp::new().await;
    let parent = create_post(&app, false, "Otázka", None).await;
    let parent_id = parent["post"]["id"].as_i64().unwrap();

    // participant -> 403
    app.auth_post(&format!("/posts/{parent_id}/resolve"), false)
        .await
        .assert_status(axum::http::StatusCode::FORBIDDEN);

    // org -> 200, swagger-valid
    let resp = app.auth_post(&format!("/posts/{parent_id}/resolve"), true).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("post", "/posts/{id}/resolve", 200, &body);
    assert!(body["post"]["resolved_at"].is_string());

    // org unresolves
    let resp = app
        .auth_delete(&format!("/posts/{parent_id}/resolve"), true)
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["post"]["resolved_at"], Value::Null);
}

/// #182: resolving a nonexistent post -> 404.
#[tokio::test]
async fn resolve_unknown_post_404() {
    let app = TestApp::new().await;
    app.auth_post("/posts/999999/resolve", true)
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_post_404() {
    let app = TestApp::new().await;
    app.server
        .get("/posts/999999")
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
}
