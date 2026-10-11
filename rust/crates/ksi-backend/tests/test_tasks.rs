mod common;

use common::{assert_swagger_valid, TestApp};
use serde_json::Value;

#[tokio::test]
async fn tasks_list_participant_sees_only_public() {
    let app = TestApp::new().await;
    let resp = app.auth_get("/tasks", false).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("get", "/tasks", 200, &body);

    let tasks = body["tasks"].as_array().unwrap();
    // only the public wave's task
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["id"], app.ids.task);
    assert_eq!(tasks[0]["max_score"], 10.0);
}

#[tokio::test]
async fn tasks_list_org_sees_all() {
    let app = TestApp::new().await;
    let resp = app.auth_get("/tasks", true).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["tasks"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn task_detail_ok() {
    let app = TestApp::new().await;
    let resp = app.auth_get(&format!("/tasks/{}", app.ids.task), false).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("get", "/tasks/{id}", 200, &body);
    assert_eq!(body["task"]["id"], app.ids.task);
    assert_eq!(body["task"]["details"], app.ids.task);
}

#[tokio::test]
async fn task_detail_locked_for_participant() {
    let app = TestApp::new().await;
    // the hidden task lives in the unreleased wave
    let hidden_task: i64 = sqlx::query_scalar(
        "SELECT t.id FROM tasks t JOIN waves w ON w.id = t.wave WHERE w.id = ?",
    )
    .bind(app.ids.wave_hidden)
    .fetch_one(&app.db)
    .await
    .unwrap();

    let resp = app.auth_get(&format!("/tasks/{hidden_task}"), false).await;
    resp.assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn task_detail_org_can_see_locked() {
    let app = TestApp::new().await;
    let hidden_task: i64 = sqlx::query_scalar(
        "SELECT t.id FROM tasks t JOIN waves w ON w.id = t.wave WHERE w.id = ?",
    )
    .bind(app.ids.wave_hidden)
    .fetch_one(&app.db)
    .await
    .unwrap();

    let resp = app.auth_get(&format!("/tasks/{hidden_task}"), true).await;
    resp.assert_status_ok();
}

#[tokio::test]
async fn task_detail_404() {
    let app = TestApp::new().await;
    app.auth_get("/tasks/999999", false)
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
}
