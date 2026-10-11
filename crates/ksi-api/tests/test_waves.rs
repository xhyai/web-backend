mod common;

use common::{assert_swagger_valid, TestApp};
use serde_json::Value;

#[tokio::test]
async fn waves_list() {
    let app = TestApp::new().await;
    let resp = app.server.get("/waves").await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("get", "/waves", 200, &body);

    let waves = body["waves"].as_array().unwrap();
    assert_eq!(waves.len(), 2);

    let public: Vec<&Value> = waves.iter().filter(|w| w["public"] == true).collect();
    let hidden: Vec<&Value> = waves.iter().filter(|w| w["public"] == false).collect();
    assert_eq!(public.len(), 1);
    assert_eq!(hidden.len(), 1);
    assert_eq!(public[0]["id"], app.ids.wave_public);
    assert_eq!(public[0]["sum_points"], 10.0);
    assert_eq!(public[0]["tasks_cnt"], 1);
}

#[tokio::test]
async fn wave_detail() {
    let app = TestApp::new().await;
    let resp = app.server.get(&format!("/waves/{}", app.ids.wave_public)).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("get", "/waves/{id}", 200, &body);
    assert_eq!(body["wave"]["id"], app.ids.wave_public);
    assert_eq!(body["wave"]["public"], true);
}

#[tokio::test]
async fn wave_detail_404() {
    let app = TestApp::new().await;
    let resp = app.server.get("/waves/999999").await;
    resp.assert_status(axum::http::StatusCode::NOT_FOUND);
}
