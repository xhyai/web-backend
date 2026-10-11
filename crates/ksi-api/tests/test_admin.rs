mod common;

use common::TestApp;
use serde_json::json;

async fn seed_evaluation(app: &TestApp, with_file: bool, with_code: bool) -> i64 {
    // evaluation for the participant on the module
    let eval_id: i64 = sqlx::query_scalar(
        "INSERT INTO evaluations (user, module, points, cheat, full_report) VALUES (?, ?, 0, 0, '') RETURNING id",
    )
    .bind(app.ids.participant)
    .bind(app.ids.module)
    .fetch_one(&app.db)
    .await
    .unwrap();

    if with_code {
        sqlx::query("INSERT INTO submitted_codes (evaluation, code) VALUES (?, 'print(42)')")
            .bind(eval_id)
            .execute(&app.db)
            .await
            .unwrap();
    }

    if with_file {
        let path = format!("/tmp/rust-test-{eval_id}.txt");
        std::fs::write(&path, b"file content").unwrap();
        sqlx::query("INSERT INTO submitted_files (evaluation, path) VALUES (?, ?)")
            .bind(eval_id)
            .bind(&path)
            .execute(&app.db)
            .await
            .unwrap();
    }

    eval_id
}

#[tokio::test]
async fn admin_subm_task_zip_with_file_and_code() {
    let app = TestApp::new().await;
    let eval_id = seed_evaluation(&app, true, true).await;

    let resp = app
        .auth_get(&format!("/admin/subm/task/{}", app.ids.task), true)
        .await;
    resp.assert_status_ok();
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/zip"
    );
    let bytes = resp.as_bytes().to_vec();

    // inspect the ZIP
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let names: Vec<String> = zip.file_names().map(|s| s.to_string()).collect();
    assert_eq!(names.len(), 2);
    assert!(names.iter().any(|n| n.ends_with(".txt") && n.contains("module_")));
    assert!(names.iter().any(|n| n == &format!("module_{}/Par_Ticipant/code_{eval_id}.txt", app.ids.module)));

    // cleanup
    std::fs::remove_file(format!("/tmp/rust-test-{eval_id}.txt")).ok();
}

#[tokio::test]
async fn admin_subm_task_empty_returns_404() {
    let app = TestApp::new().await;
    // no evaluations at all -> 404, not an empty ZIP
    let resp = app
        .auth_get(&format!("/admin/subm/task/{}", app.ids.task), true)
        .await;
    resp.assert_status(axum::http::StatusCode::NOT_FOUND);
    let body: serde_json::Value = resp.json();
    assert!(body["errors"][0]["detail"]
        .as_str()
        .unwrap()
        .contains("nebyla nalezena"));
}

#[tokio::test]
async fn admin_subm_task_requires_org() {
    let app = TestApp::new().await;
    // participant -> denied
    app.auth_get(&format!("/admin/subm/task/{}", app.ids.task), false)
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
    // anonymous -> denied
    app.server
        .get(&format!("/admin/subm/task/{}", app.ids.task))
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_subm_task_missing_file_skipped_with_code_kept() {
    let app = TestApp::new().await;
    let eval_id = seed_evaluation(&app, false, true).await;
    // a file row pointing at a nonexistent path
    sqlx::query("INSERT INTO submitted_files (evaluation, path) VALUES (?, '/nonexistent/xyz.txt')")
        .bind(eval_id)
        .execute(&app.db)
        .await
        .unwrap();

    let resp = app
        .auth_get(&format!("/admin/subm/task/{}", app.ids.task), true)
        .await;
    resp.assert_status_ok();
    let bytes = resp.as_bytes().to_vec();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let names: Vec<String> = zip.file_names().map(|s| s.to_string()).collect();
    // only the code entry; the missing file was skipped, not fatal
    assert_eq!(names.len(), 1);
    assert!(names[0].ends_with(&format!("code_{eval_id}.txt")));

    let _ = json!({}); // keep import used
}
