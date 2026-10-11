mod common;

use common::{assert_swagger_valid, TestApp};
use serde_json::Value;

#[tokio::test]
async fn years_list_returns_all_years() {
    let app = TestApp::new().await;
    let resp = app.server.get("/years").await;
    resp.assert_status_ok();
    let body: Value = resp.json();

    assert_swagger_valid("get", "/years", 200, &body);

    let years = body["years"].as_array().unwrap();
    assert_eq!(years.len(), 1);
    assert_eq!(years[0]["id"], app.ids.year);
    assert_eq!(years[0]["index"], app.ids.year);
}

/// #225 regression: unreleased waves' tasks must NOT leak into the
/// `/years` aggregates.
#[tokio::test]
async fn years_list_excludes_unreleased_tasks() {
    let app = TestApp::new().await;
    let resp = app.server.get("/years").await;
    resp.assert_status_ok();
    let body: Value = resp.json();

    // Only the public wave's module counts: 10 points, 1 task.
    // The hidden wave's module (10 pts) must NOT be included.
    assert_eq!(body["years"][0]["sum_points"], 10.0);
    assert_eq!(body["years"][0]["tasks_cnt"], 1);
}

#[tokio::test]
async fn year_detail_ok() {
    let app = TestApp::new().await;
    let resp = app.server.get(&format!("/years/{}", app.ids.year)).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("get", "/years/{id}", 200, &body);
    assert_eq!(body["year"]["id"], app.ids.year);
    assert_eq!(body["year"]["sum_points"], 10.0);
}

#[tokio::test]
async fn year_detail_404() {
    let app = TestApp::new().await;
    let resp = app.server.get("/years/999999").await;
    resp.assert_status(axum::http::StatusCode::NOT_FOUND);
    let body: Value = resp.json();
    assert!(body["errors"].is_array());
}
