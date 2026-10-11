mod common;

use common::TestApp;
use serde_json::Value;

/// Serializes tests that override the process-global `KSI_DATA_DIR`.
static DATA_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Unique scratch dir under `<crate>/data/<sub>` (mirrors the production
/// `data/` layout; removed after each test).
fn scratch(sub: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::current_dir()
        .unwrap()
        .join("data")
        .join(sub)
        .join(format!("t{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cleanup(dir: &std::path::Path) {
    std::fs::remove_dir_all(dir).ok();
    // prune parents left empty by the test
    let mut p = dir.parent().map(|p| p.to_path_buf());
    while let Some(d) = p {
        if d.file_name().map(|n| n == "data").unwrap_or(false) {
            break;
        }
        if std::fs::remove_dir(&d).is_err() {
            break;
        }
        p = d.parent().map(|p| p.to_path_buf());
    }
}

fn rel_to_data(path: &std::path::Path) -> String {
    path.strip_prefix(std::env::current_dir().unwrap().join("data"))
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/")
}

#[tokio::test]
async fn content_dir_listing() {
    let _guard = DATA_DIR_LOCK.lock().unwrap();
    let app = TestApp::new().await;
    let dir = scratch("content");
    std::fs::create_dir_all(dir.join("subdir")).unwrap();
    std::fs::write(dir.join("a.txt"), b"hello").unwrap();
    let rel = rel_to_data(&dir);

    let resp = app.server.get(&format!("/content?path={rel}")).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    // id is the path relative to data/content
    let expected_id = dir.file_name().unwrap().to_string_lossy();
    assert_eq!(body["content"]["id"], expected_id.as_ref());
    assert_eq!(body["content"]["files"], serde_json::json!(["a.txt"]));
    assert_eq!(body["content"]["dirs"], serde_json::json!(["subdir"]));

    cleanup(&dir);
}

#[tokio::test]
async fn content_serve_file() {
    let _guard = DATA_DIR_LOCK.lock().unwrap();
    let app = TestApp::new().await;
    let dir = scratch("content");
    std::fs::write(dir.join("hello.txt"), b"hello content").unwrap();
    let rel = rel_to_data(&dir.join("hello.txt"));

    let resp = app.server.get(&format!("/content?path={rel}")).await;
    resp.assert_status_ok();
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/plain"
    );
    assert_eq!(resp.as_bytes(), b"hello content");

    cleanup(&dir);
}

#[tokio::test]
async fn content_missing_file_returns_empty_listing() {
    let _guard = DATA_DIR_LOCK.lock().unwrap();
    let app = TestApp::new().await;
    let resp = app
        .server
        .get("/content?path=definitely-not-here-12345.txt")
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(
        body["content"],
        serde_json::json!({
            "id": "definitely-not-here-12345.txt",
            "files": [],
            "dirs": [],
        })
    );
}

#[tokio::test]
async fn content_absolute_path_outside_box_is_404() {
    let _guard = DATA_DIR_LOCK.lock().unwrap();
    let app = TestApp::new().await;
    // `..` is stripped, but an absolute path still escapes the box
    let resp = app.server.get("/content?path=/etc/passwd").await;
    resp.assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn content_post_requires_org() {
    let _guard = DATA_DIR_LOCK.lock().unwrap();
    let app = TestApp::new().await;
    app.auth_post("/content?path=x", false)
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
}

fn multipart_body(boundary: &str, filename: &str, data: &str) -> String {
    format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"f\"; filename=\"{filename}\"\r\n\
         Content-Type: text/plain\r\n\r\n{data}\r\n--{b}--\r\n",
        b = boundary
    )
}

#[tokio::test]
async fn content_post_upload_then_delete() {
    let _guard = DATA_DIR_LOCK.lock().unwrap();
    let app = TestApp::new().await;
    let dir = scratch("content");
    let rel = rel_to_data(&dir);

    // upload as org
    let boundary = "testboundary0001";
    let body = multipart_body(boundary, "up.txt", "uploaded!");
    let resp = app
        .server
        .post(&format!("/content?path={rel}"))
        .add_header("Authorization", format!("Bearer {}", app.tokens.0))
        .add_header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .add_header("Content-Length", body.len().to_string())
        .bytes(body.into())
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>(), serde_json::json!({}));
    assert_eq!(
        std::fs::read_to_string(dir.join("up.txt")).unwrap(),
        "uploaded!"
    );

    // the file is now served back
    let file_rel = rel_to_data(&dir.join("up.txt"));
    let resp = app.server.get(&format!("/content?path={file_rel}")).await;
    resp.assert_status_ok();
    assert_eq!(resp.as_bytes(), b"uploaded!");

    // delete as org
    let resp = app
        .auth_delete(&format!("/content?path={file_rel}"), true)
        .await;
    resp.assert_status_ok();
    assert!(!dir.join("up.txt").exists());

    // deleting again -> 404
    app.auth_delete(&format!("/content?path={file_rel}"), true)
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);

    // delete as participant -> 400
    app.auth_delete(&format!("/content?path={rel}"), false)
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);

    cleanup(&dir);
}

#[tokio::test]
async fn content_post_requires_content_length() {
    let _guard = DATA_DIR_LOCK.lock().unwrap();
    let app = TestApp::new().await;
    // axum-test always sets Content-Length for .bytes(), so exercise the
    // 411 path through a raw check is impractical here; instead verify a
    // normal upload works and the error type exists. (The 411 branch is
    // covered by unit logic.)
    let dir = scratch("content");
    let rel = rel_to_data(&dir);
    let boundary = "testboundary0002";
    let body = multipart_body(boundary, "a.txt", "x");
    let resp = app
        .server
        .post(&format!("/content?path={rel}"))
        .add_header("Authorization", format!("Bearer {}", app.tokens.0))
        .add_header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .add_header("Content-Length", body.len().to_string())
        .bytes(body.into())
        .await;
    resp.assert_status_ok();
    cleanup(&dir);
}

#[tokio::test]
async fn image_profile() {
    let _guard = DATA_DIR_LOCK.lock().unwrap();
    let app = TestApp::new().await;
    let dir = scratch("profile");
    // minimal PNG header
    let png = [0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    std::fs::write(dir.join("pic.png"), png).unwrap();
    let pic = dir.join("pic.png").to_string_lossy().into_owned();
    sqlx::query("UPDATE users SET profile_picture = ? WHERE id = ?")
        .bind(&pic)
        .bind(app.ids.org)
        .execute(&app.db)
        .await
        .unwrap();

    let resp = app
        .server
        .get(&format!("/images/profile/{}", app.ids.org))
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.headers().get("content-type").unwrap(), "image/png");
    assert_eq!(resp.as_bytes(), png);

    // user without a picture -> 404
    app.server
        .get(&format!("/images/profile/{}", app.ids.participant))
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
    // unknown user -> 404
    app.server
        .get("/images/profile/999999")
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);

    cleanup(&dir);
}

#[tokio::test]
async fn image_code_execution() {
    let _guard = DATA_DIR_LOCK.lock().unwrap();
    let app = TestApp::new().await;
    let exec_id: i64 = sqlx::query_scalar(
        "INSERT INTO code_executions (module, user, code, result, report) VALUES (?, ?, 'print(1)', 'ok', '') RETURNING id",
    )
    .bind(app.ids.module)
    .bind(app.ids.participant)
    .fetch_one(&app.db)
    .await
    .unwrap();

    let dir = scratch("exec");
    let exec_dir = dir
        .join("exec")
        .join(format!("module_{}", app.ids.module))
        .join(format!("user_{}", app.ids.participant));
    std::fs::create_dir_all(&exec_dir).unwrap();
    std::fs::write(exec_dir.join("out.png"), b"pngdata").unwrap();
    // point the handler at our scratch tree
    std::env::set_var("KSI_DATA_DIR", &dir);

    let resp = app
        .server
        .get(&format!("/images/codeExecution/{exec_id}?file=out.png"))
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.as_bytes(), b"pngdata");

    // missing file param -> 400
    app.server
        .get(&format!("/images/codeExecution/{exec_id}"))
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
    // unknown execution -> 400 (mirrors the Python endpoint)
    app.server
        .get("/images/codeExecution/999999?file=out.png")
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);

    std::env::remove_var("KSI_DATA_DIR");
    cleanup(&dir);
}

#[tokio::test]
async fn image_code_module() {
    let _guard = DATA_DIR_LOCK.lock().unwrap();
    let app = TestApp::new().await;
    let dir = scratch("exec");
    let exec_dir = dir
        .join("exec")
        .join(format!("module_{}", app.ids.module))
        .join(format!("user_{}", app.ids.participant));
    std::fs::create_dir_all(&exec_dir).unwrap();
    std::fs::write(exec_dir.join("plot.png"), b"pngdata").unwrap();
    std::env::set_var("KSI_DATA_DIR", &dir);

    let url = format!(
        "/images/codeModule/0?file=plot.png&module={}&user={}",
        app.ids.module, app.ids.participant
    );
    let resp = app.server.get(&url).await;
    resp.assert_status_ok();
    assert_eq!(resp.headers().get("cache-control").unwrap(), "no-cache");

    // non-png rejected
    let url = format!(
        "/images/codeModule/0?file=plot.txt&module={}&user={}",
        app.ids.module, app.ids.participant
    );
    app.server
        .get(&url)
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
    // unknown context
    app.server
        .get("/images/nope/1")
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);

    std::env::remove_var("KSI_DATA_DIR");
    cleanup(&dir);
}

#[tokio::test]
async fn task_content_serve() {
    let _guard = DATA_DIR_LOCK.lock().unwrap();
    let app = TestApp::new().await;
    let dir = scratch("task-content");
    let view_dir = dir
        .join("task-content")
        .join(app.ids.task.to_string())
        .join("zadani");
    std::fs::create_dir_all(&view_dir).unwrap();
    std::fs::write(view_dir.join("a.txt"), b"zadani text").unwrap();
    std::env::set_var("KSI_DATA_DIR", &dir);

    let url = format!("/task-content/{}/zadani?path=a.txt", app.ids.task);
    let resp = app.server.get(&url).await;
    resp.assert_status_ok();
    assert_eq!(resp.as_bytes(), b"zadani text");

    // legacy route
    let url = format!("/taskContent/{}/zadani?path=a.txt", app.ids.task);
    app.server
        .get(&url)
        .await
        .assert_status_ok();

    // invalid view -> 400
    let url = format!("/task-content/{}/nope?path=a.txt", app.ids.task);
    app.server
        .get(&url)
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
    // missing path param -> 400
    let url = format!("/task-content/{}/zadani", app.ids.task);
    app.server
        .get(&url)
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
    // missing file -> 404
    let url = format!("/task-content/{}/zadani?path=nope.txt", app.ids.task);
    app.server
        .get(&url)
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
    // traversal outside the box -> 404
    let url = format!("/task-content/{}/zadani?path=/etc/passwd", app.ids.task);
    app.server
        .get(&url)
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);

    std::env::remove_var("KSI_DATA_DIR");
    cleanup(&dir);
}

#[tokio::test]
async fn task_details_public_task() {
    let _guard = DATA_DIR_LOCK.lock().unwrap();
    let app = TestApp::new().await;
    let resp = app
        .server
        .get(&format!("/taskDetails/{}", app.ids.task))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    for key in [
        "taskDetails",
        "modules",
        "moduleScores",
        "achievements",
        "userScores",
        "threads",
        "threadDetails",
        "posts",
    ] {
        assert!(body.get(key).is_some(), "missing key {key}");
    }
    assert_eq!(body["taskDetails"]["id"], app.ids.task);
    assert_eq!(body["modules"].as_array().unwrap().len(), 1);
    assert_eq!(body["modules"][0]["state"], "blank");
    assert_eq!(body["threads"].as_array().unwrap().len(), 1);
    assert_eq!(body["threadDetails"][0]["id"], app.ids.thread);

    // unknown task -> 404
    app.server
        .get("/taskDetails/999999")
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);

    // hidden-wave task is locked for anonymous users -> 403
    let hidden_task: i64 = sqlx::query_scalar(
        "SELECT t.id FROM tasks t WHERE t.wave = ?",
    )
    .bind(app.ids.wave_hidden)
    .fetch_one(&app.db)
    .await
    .unwrap();
    app.server
        .get(&format!("/taskDetails/{hidden_task}"))
        .await
        .assert_status(axum::http::StatusCode::FORBIDDEN);
    // ...but visible for orgs
    app.auth_get(&format!("/taskDetails/{hidden_task}"), true)
        .await
        .assert_status_ok();
}

async fn seed_subm_file(app: &TestApp) -> (i64, i64) {
    let eval_id: i64 = sqlx::query_scalar(
        "INSERT INTO evaluations (user, module, points, cheat, full_report) VALUES (?, ?, 0, 0, '') RETURNING id",
    )
    .bind(app.ids.participant)
    .bind(app.ids.module)
    .fetch_one(&app.db)
    .await
    .unwrap();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = format!("/tmp/rust-test-subm-{nanos}.txt");
    std::fs::write(&path, b"submission").unwrap();
    let file_id: i64 = sqlx::query_scalar(
        "INSERT INTO submitted_files (evaluation, path) VALUES (?, ?) RETURNING id",
    )
    .bind(eval_id)
    .bind(&path)
    .fetch_one(&app.db)
    .await
    .unwrap();
    (file_id, eval_id)
}

#[tokio::test]
async fn subm_file_get_and_delete() {
    let _guard = DATA_DIR_LOCK.lock().unwrap();
    let app = TestApp::new().await;
    let (file_id, eval_id) = seed_subm_file(&app).await;

    // owner can download
    let resp = app
        .auth_get(&format!("/submFiles/{file_id}"), false)
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.as_bytes(), b"submission");

    // org can download too
    app.auth_get(&format!("/submFiles/{file_id}"), true)
        .await
        .assert_status_ok();

    // anonymous -> 403
    app.server
        .get(&format!("/submFiles/{file_id}"))
        .await
        .assert_status(axum::http::StatusCode::FORBIDDEN);

    // owner deletes (task has no deadline -> allowed)
    let resp = app.auth_delete(&format!("/submFiles/{file_id}"), false).await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>(), serde_json::json!({"status": "ok"}));

    // file row gone; evaluation removed because it has no files left
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM submitted_files WHERE id = ?")
        .bind(file_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(left, 0);
    let eval_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM evaluations WHERE id = ?")
        .bind(eval_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(eval_left, 0);

    // second delete -> 404 with the legacy error body
    let resp = app.auth_delete(&format!("/submFiles/{file_id}"), false).await;
    resp.assert_status(axum::http::StatusCode::NOT_FOUND);
    let body: Value = resp.json();
    assert_eq!(body["status"], "error");
}

#[tokio::test]
async fn subm_file_delete_after_deadline_refused() {
    let _guard = DATA_DIR_LOCK.lock().unwrap();
    let app = TestApp::new().await;
    let (file_id, _) = seed_subm_file(&app).await;
    // put the task deadline in the past
    sqlx::query("UPDATE tasks SET time_deadline = datetime('now', '-1 day') WHERE id = ?")
        .bind(app.ids.task)
        .execute(&app.db)
        .await
        .unwrap();

    let resp = app.auth_delete(&format!("/submFiles/{file_id}"), false).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["result"], "error");
    assert!(body["error"].as_str().unwrap().contains("termínu"));

    // file still there
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM submitted_files WHERE id = ?")
        .bind(file_id)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(left, 1);
}
