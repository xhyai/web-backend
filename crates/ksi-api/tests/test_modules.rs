mod common;

use axum_test::multipart::{MultipartForm, Part};
use common::{assert_swagger_valid, TestApp};
use serde_json::{json, Value};

async fn insert_module(app: &TestApp, mtype: &str, data: &str, autocorrect: bool) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO modules (task, type, name, max_points, autocorrect, \"order\", bonus, custom, data)
         VALUES (?, ?, 'm', 10, ?, 1, 0, 0, ?) RETURNING id",
    )
    .bind(app.ids.task)
    .bind(mtype)
    .bind(autocorrect as i64)
    .bind(data)
    .fetch_one(&app.db)
    .await
    .unwrap()
}

fn quiz_data() -> String {
    json!({
        "quiz": [
            {"type": "radio", "question": "Kolik je 1+1?", "text": "",
             "options": ["1", "2", "3"], "correct": [1]},
            {"type": "checkbox", "question": "Vyber sudá.", "text": "",
             "options": ["1", "2", "4"], "correct": [1, 2]},
        ]
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// GET /modules/{id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_module_ok_and_swagger_valid() {
    let app = TestApp::new().await;
    let resp = app.auth_get(&format!("/modules/{}", app.ids.module), false).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("get", "/modules/{modulesId}", 200, &body);
    let m = &body["module"];
    assert_eq!(m["id"], app.ids.module);
    assert_eq!(m["type"], "general");
    assert_eq!(m["state"], "blank");
    assert_eq!(m["score"], Value::Null);
    assert_eq!(m["submitted_files"], json!([]));
    assert!(m["description"].is_string());
}

#[tokio::test]
async fn get_module_requires_login() {
    let app = TestApp::new().await;
    app.server
        .get(&format!("/modules/{}", app.ids.module))
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn get_module_404() {
    let app = TestApp::new().await;
    app.auth_get("/modules/999999", false)
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_module_locked_for_participant() {
    let app = TestApp::new().await;
    // module of the hidden task (wave not published yet)
    let hidden_module: i64 = sqlx::query_scalar(
        "SELECT m.id FROM modules m JOIN tasks t ON t.id = m.task
         WHERE t.id != ? LIMIT 1",
    )
    .bind(app.ids.task)
    .fetch_one(&app.db)
    .await
    .unwrap();

    // participant -> 403
    app.auth_get(&format!("/modules/{hidden_module}"), false)
        .await
        .assert_status(axum::http::StatusCode::FORBIDDEN);

    // org -> 200, swagger-valid
    let resp = app.auth_get(&format!("/modules/{hidden_module}"), true).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("get", "/modules/{modulesId}", 200, &body);
}

#[tokio::test]
async fn get_quiz_module_hides_correct_answers() {
    let app = TestApp::new().await;
    let mid = insert_module(&app, "quiz", &quiz_data(), false).await;
    let resp = app.auth_get(&format!("/modules/{mid}"), false).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("get", "/modules/{modulesId}", 200, &body);
    let m = &body["module"];
    assert_eq!(m["type"], "quiz");
    assert_eq!(m["questions"].as_array().unwrap().len(), 2);
    assert!(m["questions"][0].get("correct").is_none());
    assert_eq!(m["questions"][0]["options"], json!(["1", "2", "3"]));
}

// ---------------------------------------------------------------------------
// POST /modules/{id}/submit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn submit_requires_login() {
    let app = TestApp::new().await;
    app.server
        .post(&format!("/modules/{}/submit", app.ids.module))
        .json(&json!({"content": "x"}))
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn submit_unknown_module_404() {
    let app = TestApp::new().await;
    app.auth_post("/modules/999999/submit", false)
        .json(&json!({"content": "x"}))
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn submit_quiz_ok_and_evaluation_recorded() {
    let app = TestApp::new().await;
    let mid = insert_module(&app, "quiz", &quiz_data(), false).await;

    let resp = app
        .auth_post(&format!("/modules/{mid}/submit"), false)
        .json(&json!({"content": [["1"], ["1", "2"]]}))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("post", "/modules/{modulesId}/submit", 200, &body);
    assert_eq!(body["result"], "ok");
    // report is stripped for non-orgs
    assert!(body.get("report").is_none());

    // wrong answers -> nok, 0 points
    let resp = app
        .auth_post(&format!("/modules/{mid}/submit"), false)
        .json(&json!({"content": [["0"], ["0"]]}))
        .await;
    let body: Value = resp.json();
    assert_eq!(body["result"], "nok");

    let evals: Vec<(bool, f64)> = sqlx::query_as(
        "SELECT ok, CAST(points AS REAL) FROM evaluations WHERE module = ? ORDER BY id",
    )
    .bind(mid)
    .fetch_all(&app.db)
    .await
    .unwrap();
    assert_eq!(evals.len(), 2);
    assert!(evals[0].0);
    assert_eq!(evals[0].1, 10.0);
    assert!(!evals[1].0);
    assert_eq!(evals[1].1, 0.0);

    // org sees the report
    let resp = app
        .auth_post(&format!("/modules/{mid}/submit"), true)
        .json(&json!({"content": [["1"], ["1", "2"]]}))
        .await;
    let body: Value = resp.json();
    assert!(body["report"].as_str().unwrap().contains("Overall result"));
}

#[tokio::test]
async fn submit_sortable_ok() {
    let app = TestApp::new().await;
    let data = json!({
        "sortable": {
            "fixed": [{"content": "a", "offset": 0}],
            "movable": [{"content": "b", "offset": 0}],
            "correct": [["m0", "f0"], ["f0", "m0"]],
        }
    })
    .to_string();
    let mid = insert_module(&app, "sortable", &data, false).await;

    let resp = app
        .auth_post(&format!("/modules/{mid}/submit"), false)
        .json(&json!({"content": ["m0", "f0"]}))
        .await;
    let body: Value = resp.json();
    assert_swagger_valid("post", "/modules/{modulesId}/submit", 200, &body);
    assert_eq!(body["result"], "ok");

    let resp = app
        .auth_post(&format!("/modules/{mid}/submit"), false)
        .json(&json!({"content": ["f0", "x"]}))
        .await;
    let body: Value = resp.json();
    assert_eq!(body["result"], "nok");
}

#[tokio::test]
async fn submit_text_diff() {
    let app = TestApp::new().await;
    let data = json!({
        "text": {"inputs": 2, "diff": ["Praha", "Brno"], "ignore_case": true}
    })
    .to_string();
    let mid = insert_module(&app, "text", &data, false).await;

    let resp = app
        .auth_post(&format!("/modules/{mid}/submit"), false)
        .json(&json!({"content": ["  praha ", "BRNO"]}))
        .await;
    let body: Value = resp.json();
    assert_swagger_valid("post", "/modules/{modulesId}/submit", 200, &body);
    assert_eq!(body["result"], "ok");

    let resp = app
        .auth_post(&format!("/modules/{mid}/submit"), false)
        .json(&json!({"content": ["Praha"]}))
        .await;
    assert_eq!(resp.json::<Value>()["result"], "nok");
}

#[tokio::test]
async fn submit_ratelimit() {
    let app = TestApp::new().await;
    let mid = insert_module(&app, "quiz", &quiz_data(), false).await;
    sqlx::query("UPDATE modules SET submit_ratelimit = 1 WHERE id = ?")
        .bind(mid)
        .execute(&app.db)
        .await
        .unwrap();

    let resp = app
        .auth_post(&format!("/modules/{mid}/submit"), false)
        .json(&json!({"content": [["1"], ["1", "2"]]}))
        .await;
    assert_eq!(resp.json::<Value>()["result"], "ok");

    let resp = app
        .auth_post(&format!("/modules/{mid}/submit"), false)
        .json(&json!({"content": [["1"], ["1", "2"]]}))
        .await;
    let body: Value = resp.json();
    assert_swagger_valid("post", "/modules/{modulesId}/submit", 200, &body);
    assert_eq!(body["result"], "error");
    assert!(body["error"].as_str().unwrap().contains("limit odevzdání"));
}

#[tokio::test]
async fn submit_after_deadline_rejected() {
    let app = TestApp::new().await;
    let mid = insert_module(&app, "quiz", &quiz_data(), false).await;
    sqlx::query("UPDATE tasks SET time_deadline = datetime('now', '-1 day') WHERE id = ?")
        .bind(app.ids.task)
        .execute(&app.db)
        .await
        .unwrap();

    let resp = app
        .auth_post(&format!("/modules/{mid}/submit"), false)
        .json(&json!({"content": [["1"], ["1", "2"]]}))
        .await;
    let body: Value = resp.json();
    assert_eq!(body["result"], "error");
    assert!(body["error"].as_str().unwrap().contains("termínu"));
}

#[tokio::test]
async fn submit_programming_without_isolate_is_503() {
    // No `isolate` binary in the test env → 503, not a crash.
    let app = TestApp::new().await;
    let data = json!({
        "programming": {
            "version": "2.0",
            "default_code": "print(1)",
            "merge_script": "/nonexistent/merge.py",
            "stdin": "/nonexistent/stdin",
            "check_script": "/nonexistent/check.py",
        }
    })
    .to_string();
    let mid = insert_module(&app, "programming", &data, true).await;

    let resp = app
        .auth_post(&format!("/modules/{mid}/submit"), false)
        .json(&json!({"content": "print(1)"}))
        .await;
    resp.assert_status(axum::http::StatusCode::SERVICE_UNAVAILABLE);

    // the code was still recorded
    let codes: Vec<String> =
        sqlx::query_scalar("SELECT code FROM submitted_codes")
            .fetch_all(&app.db)
            .await
            .unwrap();
    assert_eq!(codes, vec!["print(1)".to_string()]);
}

#[tokio::test]
async fn submit_programming_no_autocorrect_just_records() {
    let app = TestApp::new().await;
    let data = json!({
        "programming": {
            "version": "2.0",
            "default_code": "print(1)",
            "merge_script": "/nonexistent/merge.py",
            "stdin": "/nonexistent/stdin",
            "check_script": "/nonexistent/check.py",
        }
    })
    .to_string();
    let mid = insert_module(&app, "programming", &data, false).await;

    let resp = app
        .auth_post(&format!("/modules/{mid}/submit"), false)
        .json(&json!({"content": "print(2)"}))
        .await;
    let body: Value = resp.json();
    assert_swagger_valid("post", "/modules/{modulesId}/submit", 200, &body);
    assert_eq!(body["result"], "ok");

    // second submit reuses the same evaluation row (Python behavior)
    app.auth_post(&format!("/modules/{mid}/submit"), false)
        .json(&json!({"content": "print(3)"}))
        .await;
    let evals: Vec<i64> = sqlx::query_scalar("SELECT id FROM evaluations WHERE module = ?")
        .bind(mid)
        .fetch_all(&app.db)
        .await
        .unwrap();
    assert_eq!(evals.len(), 1);
}

// ---------------------------------------------------------------------------
// POST /modules/{id}/submitFiles
// ---------------------------------------------------------------------------

fn upload_form(files: &[(&str, &[u8])]) -> MultipartForm {
    let mut form = MultipartForm::new();
    for (name, content) in files {
        form = form.add_part(
            "file",
            Part::bytes(content.to_vec())
                .file_name(*name)
                .mime_type("application/octet-stream"),
        );
    }
    form
}

#[tokio::test]
async fn submit_files_ok_and_listed() {
    let app = TestApp::new().await;
    let resp = app
        .auth_post(&format!("/modules/{}/submitFiles", app.ids.module), false)
        .multipart(upload_form(&[("a.txt", b"hello"), ("b.py", b"print(1)")]))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("post", "/modules/{modulesId}/submitFiles", 200, &body);
    assert_eq!(body["result"], "ok");

    // files show up in the module detail
    let resp = app
        .auth_get(&format!("/modules/{}", app.ids.module), false)
        .await;
    let body: Value = resp.json();
    let files = body["module"]["submitted_files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    let names: Vec<&str> = files
        .iter()
        .map(|f| f["filename"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"a.txt"));
    assert!(names.contains(&"b.py"));

    // re-uploading the same file does not duplicate the DB row
    app.auth_post(&format!("/modules/{}/submitFiles", app.ids.module), false)
        .multipart(upload_form(&[("a.txt", b"hello")]))
        .await
        .assert_status_ok();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM submitted_files")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn submit_files_requires_login() {
    let app = TestApp::new().await;
    app.server
        .post(&format!("/modules/{}/submitFiles", app.ids.module))
        .multipart(upload_form(&[("a.txt", b"x")]))
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn submit_files_too_large_rejected() {
    let app = TestApp::new().await;
    let big = vec![b'x'; 26 * 1024 * 1024];
    let resp = app
        .auth_post(&format!("/modules/{}/submitFiles", app.ids.module), false)
        .multipart(upload_form(&[("big.bin", &big)]))
        .await;
    resp.assert_status(axum::http::StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn submit_files_after_deadline_rejected() {
    let app = TestApp::new().await;
    sqlx::query("UPDATE tasks SET time_deadline = datetime('now', '-1 day') WHERE id = ?")
        .bind(app.ids.task)
        .execute(&app.db)
        .await
        .unwrap();
    let resp = app
        .auth_post(&format!("/modules/{}/submitFiles", app.ids.module), false)
        .multipart(upload_form(&[("a.txt", b"x")]))
        .await;
    let body: Value = resp.json();
    assert_eq!(body["result"], "error");
}

// ---------------------------------------------------------------------------
// POST /runCode/{moduleId}/submit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn runcode_requires_login() {
    let app = TestApp::new().await;
    app.server
        .post(&format!("/runCode/{}/submit", app.ids.module))
        .json(&json!({"content": "print(1)"}))
        .await
        .assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn runcode_unknown_module_404() {
    let app = TestApp::new().await;
    app.auth_post("/runCode/999999/submit", false)
        .json(&json!({"content": "print(1)"}))
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn runcode_locked_module_403() {
    let app = TestApp::new().await;
    let hidden_module: i64 = sqlx::query_scalar(
        "SELECT m.id FROM modules m JOIN tasks t ON t.id = m.task
         WHERE t.id != ? LIMIT 1",
    )
    .bind(app.ids.task)
    .fetch_one(&app.db)
    .await
    .unwrap();
    app.auth_post(&format!("/runCode/{hidden_module}/submit"), false)
        .json(&json!({"content": "print(1)"}))
        .await
        .assert_status(axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn runcode_without_isolate_is_503() {
    // No `isolate` binary in the test env → 503, execution row recorded.
    let app = TestApp::new().await;
    let data = json!({
        "programming": {
            "version": "2.0",
            "default_code": "print(1)",
            "merge_script": "/nonexistent/merge.py",
            "stdin": "/nonexistent/stdin",
            "check_script": "/nonexistent/check.py",
        }
    })
    .to_string();
    let mid = insert_module(&app, "programming", &data, true).await;

    let resp = app
        .auth_post(&format!("/runCode/{mid}/submit"), false)
        .json(&json!({"content": "print(1)"}))
        .await;
    resp.assert_status(axum::http::StatusCode::SERVICE_UNAVAILABLE);

    let execs: Vec<(i64, String)> =
        sqlx::query_as("SELECT module, result FROM code_executions")
            .fetch_all(&app.db)
            .await
            .unwrap();
    assert_eq!(execs.len(), 1);
    assert_eq!(execs[0].0, mid);
    assert_eq!(execs[0].1, "error");
}
