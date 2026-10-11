mod common;

use common::{assert_swagger_valid, TestApp};
use serde_json::{json, Value};

fn valid_categories() -> Value {
    json!([
        {"id": "explained", "ftype": "stars", "text": "Vysvětlení?", "answer": 4},
        {"id": "interesting", "ftype": "stars", "text": "Zajímavost?", "answer": 5},
        {"id": "difficult", "ftype": "line", "text": "Těžkost?", "answer": 2},
        {"id": "comment", "ftype": "text_large", "text": "Vzkaz?", "answer": "Moc pěkná úloha, díky za ni! Opravdu jsem si ji užil."},
    ])
}

#[tokio::test]
async fn feedback_get_empty() {
    let app = TestApp::new().await;

    // anonymous -> 400
    app.server
        .get(&format!("/feedbacks/{}", app.ids.task))
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);

    // no feedback yet -> empty shape with default categories
    let resp = app.auth_get(&format!("/feedbacks/{}", app.ids.task), false).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["feedback"]["id"], app.ids.task);
    assert_eq!(body["feedback"]["userId"], app.ids.participant);
    assert_eq!(body["feedback"]["filled"], false);
    assert!(body["feedback"].get("lastUpdated").is_none());
    assert_eq!(body["feedback"]["categories"].as_array().unwrap().len(), 4);

    // unknown task -> 404
    app.auth_get("/feedbacks/999999", false)
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn feedback_create_get_update_delete() {
    let app = TestApp::new().await;
    let url = format!("/feedbacks/{}", app.ids.task);

    // create
    let resp = app
        .auth_post("/feedbacks", false)
        .json(&json!({"feedback": {"taskId": app.ids.task, "categories": valid_categories()}}))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    // NOTE: the swagger spec's POST /feedbacks response schema is stale (it
    // references the *request* schema), so we validate the shape manually.
    assert!(body["feedback"]["filled"].as_bool().unwrap());
    assert_eq!(body["feedback"]["userId"], app.ids.participant);
    assert!(body["feedback"]["lastUpdated"].is_string());
    let cats = body["feedback"]["categories"].as_array().unwrap();
    assert_eq!(cats.len(), 4);
    assert_eq!(cats[0]["answer"], 4);

    // get returns the stored feedback (swagger-valid filled shape)
    let resp = app.auth_get(&url, false).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("get", "/feedbacks/{feedbackId}", 200, &body);
    assert_eq!(body["feedback"]["filled"], true);
    assert_eq!(body["feedback"]["categories"][3]["answer"], "Moc pěkná úloha, díky za ni! Opravdu jsem si ji užil.");

    // update via PUT (answers normalized, deduped)
    let resp = app
        .server
        .put(&url)
        .add_header("Authorization", format!("Bearer {}", app.tokens.1))
        .json(&json!({"feedback": {"categories": [
            {"id": "explained", "ftype": "stars", "text": "V?", "answer": 1},
            {"id": "explained", "ftype": "stars", "text": "V?", "answer": 2},
        ]}}))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("put", "/feedbacks/{feedbackId}", 200, &body);
    let cats = body["feedback"]["categories"].as_array().unwrap();
    assert_eq!(cats.len(), 1); // duplicate id dropped
    assert_eq!(cats[0]["answer"], 1);

    // delete
    let resp = app
        .server
        .delete(&url)
        .add_header("Authorization", format!("Bearer {}", app.tokens.1))
        .await;
    resp.assert_status_ok();
    assert_swagger_valid("delete", "/feedbacks/{feedbackId}", 200, &resp.json());

    // gone again -> empty shape
    let resp = app.auth_get(&url, false).await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>()["feedback"]["filled"], false);

    // deleting twice -> 404
    app.server
        .delete(&url)
        .add_header("Authorization", format!("Bearer {}", app.tokens.1))
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn feedback_create_validations() {
    let app = TestApp::new().await;

    // unknown task -> 404
    let resp = app
        .auth_post("/feedbacks", false)
        .json(&json!({"feedback": {"taskId": 999999, "categories": valid_categories()}}))
        .await;
    resp.assert_status(axum::http::StatusCode::NOT_FOUND);

    // out of range stars -> 400
    let mut cats = valid_categories();
    cats[0]["answer"] = json!(6);
    let resp = app
        .auth_post("/feedbacks", false)
        .json(&json!({"feedback": {"taskId": app.ids.task, "categories": cats}}))
        .await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
    let body: Value = resp.json();
    assert!(body["errors"][0]["detail"].as_str().unwrap().contains("out of range"));

    // wrong answer type -> 400
    let mut cats = valid_categories();
    cats[1]["answer"] = json!("mnoho");
    let resp = app
        .auth_post("/feedbacks", false)
        .json(&json!({"feedback": {"taskId": app.ids.task, "categories": cats}}))
        .await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);

    // missing answer -> 400
    let cats = json!([{"id": "explained", "ftype": "stars", "text": "V?"}]);
    let resp = app
        .auth_post("/feedbacks", false)
        .json(&json!({"feedback": {"taskId": app.ids.task, "categories": cats}}))
        .await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
    let body: Value = resp.json();
    assert!(body["errors"][0]["detail"].as_str().unwrap().contains("Missing answer"));

    // anonymous -> 400
    app.server
        .post("/feedbacks")
        .json(&json!({"feedback": {"taskId": app.ids.task, "categories": valid_categories()}}))
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn feedback_email_endpoint() {
    let app = TestApp::new().await;

    sqlx::query("INSERT INTO feedback_recipients (email) VALUES ('orgs@example.cz')")
        .execute(&app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO mail_eastereggs (id, body) VALUES (1, 'easter egg')")
        .execute(&app.db)
        .await
        .unwrap();

    // empty body -> 200 without sending
    let resp = app
        .server
        .post("/feedback")
        .json(&json!({"body": ""}))
        .await;
    resp.assert_status_ok();

    // normal send -> {"result": "ok"} (e-mail goes to the log sender)
    let resp = app
        .server
        .post("/feedback")
        .json(&json!({"body": "Skvělý seminář!", "email": "fan@example.cz"}))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("post", "/feedback", 200, &body);
    assert_eq!(body, json!({"result": "ok"}));
}
