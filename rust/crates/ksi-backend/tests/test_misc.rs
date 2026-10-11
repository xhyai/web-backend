mod common;

use common::{assert_swagger_valid, TestApp};
use serde_json::Value;

#[tokio::test]
async fn articles_list() {
    let app = TestApp::new().await;
    // seed an article
    sqlx::query(
        "INSERT INTO articles (author, title, body, time_created, published, year)
         VALUES (?, 'Novinka', 'Text', datetime('now'), 1, ?)",
    )
    .bind(app.ids.org)
    .bind(app.ids.year)
    .execute(&app.db)
    .await
    .unwrap();

    let resp = app.server.get("/articles").await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    // NOTE: the swagger spec's Article schema is stale (says `resource`,
    // backend returns `content`, and marks additionalProperties: false);
    // we validate shape manually instead.
    assert_eq!(body["articles"].as_array().unwrap().len(), 1);
    assert_eq!(body["articles"][0]["title"], "Novinka");
    assert_eq!(body["articles"][0]["author"], app.ids.org);
    assert!(body["articles"][0]["time_published"].is_string());
}

#[tokio::test]
async fn article_detail_404() {
    let app = TestApp::new().await;
    app.server
        .get("/articles/999999")
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn robots_and_csp() {
    let app = TestApp::new().await;
    let r = app.server.get("/robots.txt").await;
    r.assert_status_ok();
    let c = app.server.get("/csp").await;
    c.assert_status_ok();
}
