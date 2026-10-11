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

// ---------------------------------------------------------------------------
// Misc admin endpoints (content group)
// ---------------------------------------------------------------------------

use serde_json::Value;

static DATA_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Insert an admin user and return a bearer token for them.
async fn make_admin(app: &TestApp) -> String {
    let pw = bcrypt::hash("demo", 4).unwrap();
    let admin: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, first_name, last_name, sex, password, short_info, role)
         VALUES ('admin@demo.cz', 'Ad', 'Min', 'male', ?, '', 'admin') RETURNING id",
    )
    .bind(&pw)
    .fetch_one(&app.db)
    .await
    .unwrap();
    let access = format!("test-token-admin-{admin}");
    sqlx::query(
        "INSERT INTO oauth2_tokens (access_token, user, expire, refresh_token, granted)
         VALUES (?, ?, datetime('now', '+1 hour'), ?, datetime('now'))",
    )
    .bind(&access)
    .bind(admin)
    .bind(format!("refresh-{access}"))
    .execute(&app.db)
    .await
    .unwrap();
    access
}

#[tokio::test]
async fn admin_subm_eval_zip() {
    let app = TestApp::new().await;
    let eval_id = seed_evaluation(&app, true, false).await;

    let resp = app
        .auth_get(&format!("/admin/subm/eval/{eval_id}"), true)
        .await;
    resp.assert_status_ok();
    assert_eq!(
        resp.headers().get("content-disposition").unwrap(),
        &format!("inline; filename=\"eval_{eval_id}.zip\"")
    );
    let bytes = resp.as_bytes().to_vec();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let names: Vec<String> = zip.file_names().map(|s| s.to_string()).collect();
    assert_eq!(names.len(), 1);
    assert!(names[0].ends_with(".txt"));

    // participant -> denied
    app.auth_get(&format!("/admin/subm/eval/{eval_id}"), false)
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);

    std::fs::remove_file(format!("/tmp/rust-test-{eval_id}.txt")).ok();
}

#[tokio::test]
async fn admin_eval_code() {
    let _guard = DATA_DIR_LOCK.lock().unwrap();
    let app = TestApp::new().await;
    let eval_id = seed_evaluation(&app, false, true).await;

    // execution dir for the participant/module
    let dir = std::env::current_dir()
        .unwrap()
        .join(format!("data-exec-test-{}", std::process::id()));
    let exec_dir = dir
        .join("exec")
        .join(format!("module_{}", app.ids.module))
        .join(format!("user_{}", app.ids.participant));
    std::fs::create_dir_all(exec_dir.join("box")).unwrap();
    std::fs::write(
        exec_dir.join("source"),
        format!("evaluation\n{eval_id}\nrest"),
    )
    .unwrap();
    std::fs::write(exec_dir.join("box").join("run"), b"merged code").unwrap();
    std::fs::write(exec_dir.join("stdout"), b"program output").unwrap();
    std::env::set_var("KSI_DATA_DIR", &dir);

    let resp = app
        .auth_get(&format!("/admin/evalCodes/{eval_id}"), true)
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["evalCode"]["code"], "print(42)");
    assert_eq!(body["evalCode"]["stdout"], "program output");
    assert_eq!(body["evalCode"]["merged"], "merged code");
    // missing files produce the Python-style message
    assert!(body["evalCode"]["stderr"]
        .as_str()
        .unwrap()
        .contains("neexistuje"));

    // stale source file -> "merged" notice, no stdout key
    std::fs::write(exec_dir.join("source"), "evaluation\n99999\n").unwrap();
    let resp = app
        .auth_get(&format!("/admin/evalCodes/{eval_id}"), true)
        .await;
    let body: Value = resp.json();
    assert!(body["evalCode"]["merged"]
        .as_str()
        .unwrap()
        .contains("nahrazeny"));
    assert!(body["evalCode"].get("stdout").is_none());

    // unknown evaluation -> 200 with the legacy error shape
    let resp = app.auth_get("/admin/evalCodes/999999", true).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["errors"][0]["id"], 5);

    // participant -> denied
    app.auth_get(&format!("/admin/evalCodes/{eval_id}"), false)
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);

    std::env::remove_var("KSI_DATA_DIR");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn admin_evaluation_detail() {
    let app = TestApp::new().await;
    let eval_id = seed_evaluation(&app, true, false).await;

    let resp = app
        .auth_get(&format!("/admin/evaluations/{eval_id}"), true)
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["evaluation"]["eval_id"], eval_id);
    assert_eq!(body["evaluation"]["cheat"], false);
    // general module -> files list
    assert_eq!(
        body["evaluation"]["general"]["files"].as_array().unwrap().len(),
        1
    );

    // unknown -> 404
    app.auth_get("/admin/evaluations/999999", true)
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
    // participant -> denied
    app.auth_get(&format!("/admin/evaluations/{eval_id}"), false)
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);

    std::fs::remove_file(format!("/tmp/rust-test-{eval_id}.txt")).ok();
}

#[tokio::test]
async fn admin_execs_list_and_filters() {
    let app = TestApp::new().await;
    let exec_id: i64 = sqlx::query_scalar(
        "INSERT INTO code_executions (module, user, code, result, report)
         VALUES (?, ?, 'print(1)', 'ok', 'rep') RETURNING id",
    )
    .bind(app.ids.module)
    .bind(app.ids.participant)
    .fetch_one(&app.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO code_executions (module, user, code, result, report)
         VALUES (?, ?, 'print(2)', 'error', 'rep2')",
    )
    .bind(app.ids.module)
    .bind(app.ids.org)
    .execute(&app.db)
    .await
    .unwrap();

    let resp = app.auth_get("/admin/execs", true).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["meta"]["total"], 2);
    assert_eq!(body["meta"]["page"], 0);
    assert_eq!(body["execs"].as_array().unwrap().len(), 2);
    // newest first
    assert!(body["execs"][0]["id"].as_i64().unwrap() > body["execs"][1]["id"].as_i64().unwrap());
    assert_eq!(body["execs"][0]["result"], "error");

    // filter by user
    let resp = app
        .auth_get(&format!("/admin/execs?user={}", app.ids.participant), true)
        .await;
    let body: Value = resp.json();
    assert_eq!(body["meta"]["total"], 1);

    // filter by result
    let resp = app.auth_get("/admin/execs?result=ok", true).await;
    let body: Value = resp.json();
    assert_eq!(body["meta"]["total"], 1);

    // pagination
    let resp = app.auth_get("/admin/execs?limit=1&page=1", true).await;
    let body: Value = resp.json();
    assert_eq!(body["execs"].as_array().unwrap().len(), 1);
    assert_eq!(body["meta"]["page"], 1);

    // single exec
    let resp = app
        .auth_get(&format!("/admin/execs/{exec_id}"), true)
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["id"], exec_id);
    assert_eq!(body["code"], "print(1)");

    // unknown exec -> 404
    app.auth_get("/admin/execs/999999", true)
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);

    // participant -> denied
    app.auth_get("/admin/execs", false)
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_instance_config() {
    let app = TestApp::new().await;
    let admin_token = make_admin(&app).await;

    // org (non-admin) cannot read
    app.auth_get("/admin/instanceConfig", true)
        .await
        .assert_status(axum::http::StatusCode::UNAUTHORIZED);
    // anonymous cannot read
    app.server
        .get("/admin/instanceConfig")
        .await
        .assert_status(axum::http::StatusCode::UNAUTHORIZED);

    // admin can read (empty at first)
    let resp = app
        .server
        .get("/admin/instanceConfig")
        .add_header("Authorization", format!("Bearer {admin_token}"))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert!(body["config"].is_array());

    // admin sets a value
    let resp = app
        .server
        .post("/admin/instanceConfig")
        .add_header("Authorization", format!("Bearer {admin_token}"))
        .json(&serde_json::json!({"key": "test_key", "value": "test_val"}))
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>(), serde_json::json!({}));

    let resp = app
        .server
        .get("/admin/instanceConfig")
        .add_header("Authorization", format!("Bearer {admin_token}"))
        .await;
    let body: Value = resp.json();
    let entry = body["config"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["key"] == "test_key")
        .expect("test_key present");
    assert_eq!(entry["value"], "test_val");
    assert_eq!(entry["secret"], false);

    // audit log was written
    let logged: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE scope = 'CONFIG' AND line LIKE '%test_key%'",
    )
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(logged, 1);

    // missing value -> 400
    let resp = app
        .server
        .post("/admin/instanceConfig")
        .add_header("Authorization", format!("Bearer {admin_token}"))
        .json(&serde_json::json!({"key": "k"}))
        .await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);

    // org cannot write
    app.auth_post("/admin/instanceConfig", true)
        .json(&serde_json::json!({"key": "k", "value": "v"}))
        .await
        .assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_monitoring_dashboard() {
    let app = TestApp::new().await;
    sqlx::query("INSERT INTO config (\"key\", value, secret) VALUES ('monitoring_dashboard_url', 'https://grafana.example.com', 0)")
        .execute(&app.db)
        .await
        .unwrap();

    let resp = app.auth_get("/admin/monitoring-dashboard", true).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["url"], "https://grafana.example.com");

    // participant -> 400 with the legacy message
    let resp = app.auth_get("/admin/monitoring-dashboard", false).await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
    let body: Value = resp.json();
    assert_eq!(body, serde_json::json!("Nedostatecna opravneni"));
}

#[tokio::test]
async fn admin_user_export() {
    let app = TestApp::new().await;
    // profile for the participant (school_finish 2027 -> "Středoškoláci")
    sqlx::query(
        "INSERT INTO profiles (user_id, addr_street, addr_city, addr_zip, addr_country,
                               school_name, school_street, school_city, school_zip, school_country,
                               school_finish, tshirt_size)
         VALUES (?, 'Ulice 1', 'Město', '12345', 'Země', 'Škola', 'Školní 2', 'Město', '54321', 'Země', 2027, 'M')",
    )
    .bind(app.ids.participant)
    .execute(&app.db)
    .await
    .unwrap();
    // evaluation with 8 points on the public task's module
    sqlx::query(
        "INSERT INTO evaluations (user, module, points, cheat, full_report) VALUES (?, ?, 8, 0, '')",
    )
    .bind(app.ids.participant)
    .bind(app.ids.module)
    .execute(&app.db)
    .await
    .unwrap();

    let resp = app
        .server
        .get("/admin/user-export")
        .add_header("Authorization", format!("Bearer {}", app.tokens.0))
        .add_header("year", app.ids.year.to_string())
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.headers().get("content-type").unwrap(), "text/csv");
    assert!(resp
        .headers()
        .get("content-disposition")
        .unwrap()
        .to_str()
        .unwrap()
        .contains(&format!("resitele_{}.csv", app.ids.year)));
    let body = String::from_utf8(resp.as_bytes().to_vec()).unwrap();
    assert!(body.contains("Středoškoláci"));
    assert!(body.contains("Pořadí;Příjmení;Jméno;Body;"));
    assert!(body.contains("Ticipant"));
    assert!(body.contains("Par"));
    // 8 points, threshold is 60% of 10 -> successful ('A'), no cheat
    let row = body.lines().find(|l| l.contains("Ticipant")).unwrap();
    let cols: Vec<&str> = row.split(';').collect();
    assert_eq!(cols[0], "1"); // rank
    assert_eq!(cols[3], "8.0"); // points
    assert_eq!(cols[4], "A"); // successful
    assert_eq!(cols[5], "N"); // cheat

    // participant -> denied
    app.auth_get("/admin/user-export", false)
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
}
