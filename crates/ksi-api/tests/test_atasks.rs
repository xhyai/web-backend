//! Integration tests for the admin task endpoints (`/admin/atasks…`,
//! `/admin/waves/{id}/diff`).
//!
//! Git-touching tests point the deploy plumbing at throwaway directories via
//! `KSI_GIT_SEMINAR_PATH` / `KSI_DATA_DIR` / `KSI_LOCK_DIR` and are serialized
//! with a global mutex because those are process-wide environment variables.

mod common;

use common::TestApp;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static GIT_SERIAL: Mutex<()> = Mutex::new(());

fn auth_put(app: &TestApp, path: &str, token: &str) -> axum_test::TestRequest {
    app.server
        .put(path)
        .add_header("Authorization", format!("Bearer {token}"))
}

async fn make_admin(app: &TestApp) -> String {
    let pw = bcrypt::hash("demo", 4).unwrap();
    let admin: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, first_name, last_name, nick_name, sex, password, short_info, role)
         VALUES ('admin@demo.cz', 'Ad', 'Min', NULL, 'male', ?, '', 'admin') RETURNING id",
    )
    .bind(&pw)
    .fetch_one(&app.db)
    .await
    .unwrap();
    let token = format!("test-admin-token-{admin}");
    sqlx::query(
        "INSERT INTO oauth2_tokens (access_token, user, expire, refresh_token, granted)
         VALUES (?, ?, datetime('now', '+1 hour'), ?, datetime('now'))",
    )
    .bind(&token)
    .bind(admin)
    .bind(format!("refresh-{token}"))
    .execute(&app.db)
    .await
    .unwrap();
    token
}

/// Create a fresh git seminar repo + origin and point env overrides at it.
/// Returns the seminar path. Caller must hold `GIT_SERIAL`.
fn setup_git_env(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("ksi-atasks-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&base);
    let seminar = base.join("seminar");
    let origin = base.join("origin.git");
    let data = base.join("data");
    let locks = base.join("locks");
    for d in [&seminar, &data, &locks] {
        std::fs::create_dir_all(d).unwrap();
    }

    let git = |dir: &Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("git failed");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    git(&seminar, &["init", "-b", "master"]);
    git(&seminar, &["config", "user.email", "test@example.com"]);
    git(&seminar, &["config", "user.name", "Test"]);
    std::fs::write(seminar.join("README.md"), "seminar\n").unwrap();
    git(&seminar, &["add", "."]);
    git(&seminar, &["commit", "-m", "init"]);
    git(&base, &["init", "--bare", "origin.git"]);
    git(&seminar, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(&seminar, &["push", "-u", "origin", "master"]);

    std::env::set_var("KSI_GIT_SEMINAR_PATH", &seminar);
    std::env::set_var("KSI_DATA_DIR", &data);
    std::env::set_var("KSI_LOCK_DIR", &locks);
    seminar
}

fn head_sha(seminar: &Path) -> String {
    let out = std::process::Command::new("git")
        .current_dir(seminar)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// ---------------------------------------------------------------------------
// list / get
// ---------------------------------------------------------------------------

#[tokio::test]
async fn atasks_list_requires_org() {
    let app = TestApp::new().await;

    let resp = app.server.get("/admin/atasks").await;
    resp.assert_status_bad_request();

    let resp = app.auth_get("/admin/atasks", false).await;
    resp.assert_status_bad_request();
}

#[tokio::test]
async fn atasks_list_ok() {
    let app = TestApp::new().await;
    let resp = app.auth_get("/admin/atasks", true).await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    let atasks = body["atasks"].as_array().unwrap();
    assert_eq!(atasks.len(), 2);

    let first = &atasks[0];
    assert_eq!(first["title"], json!("Demo uloha"));
    assert_eq!(first["wave"], json!(app.ids.wave_public));
    assert_eq!(first["max_score"], json!(10.0));
    assert_eq!(first["deploy_status"], json!("default"));
    assert_eq!(first["testers"], json!([]));
    assert_eq!(first["additional_testers"], json!([]));
    assert!(first["git_path"].is_null());
    assert!(first["eval_comment"].is_null());
}

#[tokio::test]
async fn atasks_list_wave_filter() {
    let app = TestApp::new().await;
    let resp = app
        .auth_get(&format!("/admin/atasks?wave={}", app.ids.wave_hidden), true)
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    let atasks = body["atasks"].as_array().unwrap();
    assert_eq!(atasks.len(), 1);
    assert_eq!(atasks[0]["title"], json!("Skryta uloha"));
}

#[tokio::test]
async fn atask_get_ok_and_404() {
    let app = TestApp::new().await;
    let resp = app
        .auth_get(&format!("/admin/atasks/{}", app.ids.task), true)
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["atask"]["id"], json!(app.ids.task));

    let resp = app.auth_get("/admin/atasks/999999", true).await;
    resp.assert_status_not_found();

    let resp = app
        .auth_get(&format!("/admin/atasks/{}", app.ids.task), false)
        .await;
    resp.assert_status_bad_request();
}

// ---------------------------------------------------------------------------
// create
// ---------------------------------------------------------------------------

fn create_body(wave: i64) -> serde_json::Value {
    json!({
        "atask": {
            "wave": wave,
            "title": "Nova uloha",
            "author": 1,
            "git_path": "ulohy/nova",
            "git_branch": "uloha-nova",
            "git_commit": null,
            "git_create": false,
        }
    })
}

#[tokio::test]
async fn atask_create_ok() {
    let app = TestApp::new().await;
    // wave_hidden is unpublished and org is its garant.
    let resp = app
        .auth_post("/admin/atasks", true)
        .json(&create_body(app.ids.wave_hidden))
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["atask"]["title"], json!("Nova uloha"));
    assert_eq!(body["atask"]["wave"], json!(app.ids.wave_hidden));
    assert_eq!(body["atask"]["git_branch"], json!("uloha-nova"));
    assert_eq!(body["atask"]["deploy_status"], json!("default"));

    // A discussion thread was created for the task.
    let thread_id = body["atask"]["id"].as_i64().unwrap();
    let thread: Option<i64> = sqlx::query_scalar("SELECT thread FROM tasks WHERE id = ?")
        .bind(thread_id)
        .fetch_optional(&app.db)
        .await
        .unwrap()
        .flatten();
    assert!(thread.is_some());
}

#[tokio::test]
async fn atask_create_published_wave_403() {
    let app = TestApp::new().await;
    let resp = app
        .auth_post("/admin/atasks", true)
        .json(&create_body(app.ids.wave_public))
        .await;
    resp.assert_status_forbidden();
}

#[tokio::test]
async fn atask_create_unauthorized() {
    let app = TestApp::new().await;
    // Participant may not create tasks.
    let resp = app
        .auth_post("/admin/atasks", false)
        .json(&create_body(app.ids.wave_hidden))
        .await;
    resp.assert_status_unauthorized();

    // Unknown wave.
    let resp = app
        .auth_post("/admin/atasks", true)
        .json(&create_body(999999))
        .await;
    resp.assert_status_not_found();
}

// ---------------------------------------------------------------------------
// update
// ---------------------------------------------------------------------------

#[tokio::test]
async fn atask_update_ok() {
    let app = TestApp::new().await;
    // Create a task in the unpublished wave first.
    let resp = app
        .auth_post("/admin/atasks", true)
        .json(&create_body(app.ids.wave_hidden))
        .await;
    resp.assert_status_ok();
    let id = resp.json::<serde_json::Value>()["atask"]["id"]
        .as_i64()
        .unwrap();

    let resp = auth_put(&app, &format!("/admin/atasks/{id}"), &app.tokens.0)
        .json(&json!({
            "atask": {
                "wave": app.ids.wave_hidden,
                "title": "Prejmenovana uloha",
                "git_path": "ulohy/nova2",
                "git_branch": "uloha-nova2",
                "git_commit": "abc123",
                "eval_comment": "",
            }
        }))
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["atask"]["title"], json!("Prejmenovana uloha"));
    assert_eq!(body["atask"]["git_commit"], json!("abc123"));
    // Explicit "" clears eval_comment to NULL.
    assert!(body["atask"]["eval_comment"].is_null());
}

#[tokio::test]
async fn atask_update_published_wave_403_for_org() {
    let app = TestApp::new().await;
    // wave_public is published; org is not admin.
    let resp = auth_put(
        &app,
        &format!("/admin/atasks/{}", app.ids.task),
        &app.tokens.0,
    )
    .json(&json!({
        "atask": {
            "wave": app.ids.wave_public,
            "title": "X",
            "git_path": "x",
            "git_branch": "x",
            "git_commit": "x",
        }
    }))
    .await;
    resp.assert_status_forbidden();
}

#[tokio::test]
async fn atask_update_admin_can_edit_published() {
    let app = TestApp::new().await;
    let admin_token = make_admin(&app).await;
    let resp = auth_put(
        &app,
        &format!("/admin/atasks/{}", app.ids.task),
        &admin_token,
    )
    .json(&json!({
        "atask": {
            "wave": app.ids.wave_public,
            "title": "Admin edit",
            "git_path": "x",
            "git_branch": "x",
            "git_commit": "x",
        }
    }))
    .await;
    resp.assert_status_ok();
    assert_eq!(
        resp.json::<serde_json::Value>()["atask"]["title"],
        json!("Admin edit")
    );
}

// ---------------------------------------------------------------------------
// delete
// ---------------------------------------------------------------------------

async fn seed_deletable_task(app: &TestApp) -> i64 {
    let thread: i64 = sqlx::query_scalar(
        "INSERT INTO threads (title, public, year) VALUES ('Del vlakno', 1, ?) RETURNING id",
    )
    .bind(app.ids.year)
    .fetch_one(&app.db)
    .await
    .unwrap();
    let task: i64 = sqlx::query_scalar(
        "INSERT INTO tasks (title, author, wave, intro, body, thread, evaluation_public, deploy_status)
         VALUES ('Smazat me', ?, ?, '', '', ?, 0, 'default') RETURNING id",
    )
    .bind(app.ids.org)
    .bind(app.ids.wave_hidden)
    .bind(thread)
    .fetch_one(&app.db)
    .await
    .unwrap();
    let module: i64 = sqlx::query_scalar(
        "INSERT INTO modules (task, type, name, max_points, autocorrect, \"order\", bonus, custom)
         VALUES (?, 'general', 'm', 5, 0, 1, 0, 0) RETURNING id",
    )
    .bind(task)
    .fetch_one(&app.db)
    .await
    .unwrap();
    let eval: i64 = sqlx::query_scalar(
        "INSERT INTO evaluations (user, module, points, cheat, full_report) VALUES (?, ?, 0, 0, '') RETURNING id",
    )
    .bind(app.ids.participant)
    .bind(module)
    .fetch_one(&app.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO code_executions (module, user, code) VALUES (?, ?, 'x')")
        .bind(module)
        .bind(app.ids.participant)
        .execute(&app.db)
        .await
        .unwrap();
    let _ = eval;
    task
}

#[tokio::test]
async fn atask_delete_requires_admin() {
    let app = TestApp::new().await;
    let task = seed_deletable_task(&app).await;

    let resp = app.auth_delete(&format!("/admin/atasks/{task}"), false).await;
    resp.assert_status_bad_request();

    // Org without admin role is not enough either.
    let resp = app.auth_delete(&format!("/admin/atasks/{task}"), true).await;
    resp.assert_status_bad_request();
}

#[tokio::test]
async fn atask_delete_ok_cascades() {
    let app = TestApp::new().await;
    let admin_token = make_admin(&app).await;
    let task = seed_deletable_task(&app).await;
    let thread: i64 = sqlx::query_scalar("SELECT thread FROM tasks WHERE id = ?")
        .bind(task)
        .fetch_one(&app.db)
        .await
        .unwrap();

    let resp = app
        .server
        .delete(&format!("/admin/atasks/{task}"))
        .add_header("Authorization", format!("Bearer {admin_token}"))
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<serde_json::Value>(), json!({}));

    let gone: Option<i64> = sqlx::query_scalar("SELECT id FROM tasks WHERE id = ?")
        .bind(task)
        .fetch_optional(&app.db)
        .await
        .unwrap();
    assert!(gone.is_none());
    let gone: Option<i64> = sqlx::query_scalar("SELECT id FROM threads WHERE id = ?")
        .bind(thread)
        .fetch_optional(&app.db)
        .await
        .unwrap();
    assert!(gone.is_none());
    let modules: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM modules WHERE task = ?")
        .bind(task)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(modules, 0);
}

#[tokio::test]
async fn atask_delete_published_wave_403() {
    let app = TestApp::new().await;
    let admin_token = make_admin(&app).await;
    let resp = app
        .server
        .delete(&format!("/admin/atasks/{}", app.ids.task))
        .add_header("Authorization", format!("Bearer {admin_token}"))
        .await;
    resp.assert_status_forbidden();
}

// ---------------------------------------------------------------------------
// deploy
// ---------------------------------------------------------------------------

#[tokio::test]
async fn deploy_requires_org_and_git_info() {
    let app = TestApp::new().await;

    // Participant cannot deploy.
    let resp = app
        .auth_post(&format!("/admin/atasks/{}/deploy", app.ids.task), false)
        .await;
    resp.assert_status_bad_request();

    // Task without git branch/path.
    let resp = app
        .auth_post(&format!("/admin/atasks/{}/deploy", app.ids.task), true)
        .await;
    resp.assert_status_bad_request();

    // Unknown task.
    let resp = app.auth_post("/admin/atasks/999999/deploy", true).await;
    resp.assert_status_not_found();
}

#[tokio::test]
async fn deploy_status_get() {
    let app = TestApp::new().await;
    let resp = app
        .auth_get(&format!("/admin/atasks/{}/deploy", app.ids.task), true)
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["id"], json!(app.ids.task));
    assert_eq!(body["deploy_status"], json!("default"));
    assert!(body["deploy_date"].is_null());

    let resp = app
        .auth_get(&format!("/admin/atasks/{}/deploy", app.ids.task), false)
        .await;
    resp.assert_status_bad_request();
}

#[tokio::test]
async fn deploy_runs_worker_and_reports_error_without_task_json() {
    let _guard = GIT_SERIAL.lock().unwrap();
    let seminar = setup_git_env("deploy");

    let app = TestApp::new().await;
    // Point the seeded task at the scratch repo; no task.json exists there,
    // so the worker must fail cleanly with deploy_status='error'.
    sqlx::query("UPDATE tasks SET git_branch = 'master', git_path = '' WHERE id = ?")
        .bind(app.ids.task)
        .execute(&app.db)
        .await
        .unwrap();

    let resp = app
        .auth_post(&format!("/admin/atasks/{}/deploy", app.ids.task), true)
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<serde_json::Value>(), json!({}));

    // Wait for the background worker to finish.
    let mut status = String::new();
    for _ in 0..100 {
        let resp = app
            .auth_get(&format!("/admin/atasks/{}/deploy", app.ids.task), true)
            .await;
        let body: serde_json::Value = resp.json();
        status = body["deploy_status"].as_str().unwrap().to_string();
        if status != "deploying" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(status, "error");

    // The log mentions the failed meta-processing step.
    let resp = app
        .auth_get(&format!("/admin/atasks/{}/deploy", app.ids.task), true)
        .await;
    let body: serde_json::Value = resp.json();
    let log = body["log"].as_str().unwrap_or("");
    assert!(
        log.contains("Processing meta"),
        "deploy log should mention meta processing, got: {log}"
    );
}

// ---------------------------------------------------------------------------
// merge
// ---------------------------------------------------------------------------

#[tokio::test]
async fn merge_validations() {
    let app = TestApp::new().await;

    let resp = app.auth_post("/admin/atasks/999999/merge", true).await;
    resp.assert_status_not_found();

    // No git info.
    let resp = app
        .auth_post(&format!("/admin/atasks/{}/merge", app.ids.task), true)
        .await;
    resp.assert_status_bad_request();

    // Already on master.
    sqlx::query("UPDATE tasks SET git_branch = 'master', git_path = 'x' WHERE id = ?")
        .bind(app.ids.task)
        .execute(&app.db)
        .await
        .unwrap();
    let resp = app
        .auth_post(&format!("/admin/atasks/{}/merge", app.ids.task), true)
        .await;
    resp.assert_status_bad_request();

    // Participant may not merge.
    sqlx::query("UPDATE tasks SET git_branch = 'uloha-x', git_path = 'x' WHERE id = ?")
        .bind(app.ids.task)
        .execute(&app.db)
        .await
        .unwrap();
    let resp = app
        .auth_post(&format!("/admin/atasks/{}/merge", app.ids.task), false)
        .await;
    resp.assert_status_bad_request();
}

#[tokio::test]
async fn merge_deletes_branch() {
    let _guard = GIT_SERIAL.lock().unwrap();
    let seminar = setup_git_env("merge");

    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&seminar)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success());
    };
    git(&["checkout", "-b", "uloha-x"]);
    std::fs::write(seminar.join("x.txt"), "x\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "uloha x"]);
    git(&["checkout", "master"]);

    let app = TestApp::new().await;
    sqlx::query("UPDATE tasks SET git_branch = 'uloha-x', git_path = 'x' WHERE id = ?")
        .bind(app.ids.task)
        .execute(&app.db)
        .await
        .unwrap();

    // Org is the garant of wave_public → may merge.
    let resp = app
        .auth_post(&format!("/admin/atasks/{}/merge", app.ids.task), true)
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<serde_json::Value>(), json!({}));

    let branch: String = sqlx::query_scalar("SELECT git_branch FROM tasks WHERE id = ?")
        .bind(app.ids.task)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(branch, "master");

    let out = std::process::Command::new("git")
        .current_dir(&seminar)
        .args(["branch", "--list", "uloha-x"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
}

// ---------------------------------------------------------------------------
// wave diff
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wave_diff_requires_org() {
    let app = TestApp::new().await;
    let resp = app
        .auth_post(&format!("/admin/waves/{}/diff", app.ids.wave_public), false)
        .await;
    resp.assert_status_bad_request();
}

#[tokio::test]
async fn wave_diff_marks_statuses() {
    let _guard = GIT_SERIAL.lock().unwrap();
    let seminar = setup_git_env("wavediff");
    let sha_before = head_sha(&seminar);

    // Second commit → the recorded sha_before is now "behind".
    std::fs::write(seminar.join("dalsi.txt"), "dalsi\n").unwrap();
    let out = std::process::Command::new("git")
        .current_dir(&seminar)
        .args(["add", "."])
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = std::process::Command::new("git")
        .current_dir(&seminar)
        .args(["commit", "-m", "dalsi"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let out = std::process::Command::new("git")
        .current_dir(&seminar)
        .args(["push", "origin", "master"])
        .output()
        .unwrap();
    assert!(out.status.success());

    let app = TestApp::new().await;
    // Task behind HEAD → 'diff'; task without git info → 'default'.
    sqlx::query("UPDATE tasks SET git_branch = 'master', git_path = '.', git_commit = ? WHERE id = ?")
        .bind(&sha_before)
        .bind(app.ids.task)
        .execute(&app.db)
        .await
        .unwrap();

    let resp = app
        .auth_post(&format!("/admin/waves/{}/diff", app.ids.wave_public), true)
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<serde_json::Value>(), json!({}));

    let status: String = sqlx::query_scalar("SELECT deploy_status FROM tasks WHERE id = ?")
        .bind(app.ids.task)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(status, "diff");
}
