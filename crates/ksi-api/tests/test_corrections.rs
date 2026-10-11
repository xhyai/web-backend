mod common;

use common::{assert_swagger_valid, TestApp};
use serde_json::json;

async fn seed_eval(app: &TestApp) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO evaluations (\"user\", module, points, cheat, full_report)
         VALUES (?, ?, 0, 0, '') RETURNING id",
    )
    .bind(app.ids.participant)
    .bind(app.ids.module)
    .fetch_one(&app.db)
    .await
    .unwrap()
}

fn correction_id(task: i64, user: i64) -> i64 {
    task * 100000 + user
}

// ---------------------------------------------------------------------------
// GET /admin/corrections
// ---------------------------------------------------------------------------

#[tokio::test]
async fn corrections_list_requires_task_or_participant() {
    let app = TestApp::new().await;
    app.auth_get("/admin/corrections", true)
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn corrections_list_requires_org() {
    let app = TestApp::new().await;
    // participant -> denied
    app.auth_get(&format!("/admin/corrections?task={}", app.ids.task), false)
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
    // anonymous -> denied
    app.server
        .get(&format!("/admin/corrections?task={}", app.ids.task))
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn corrections_list_basic() {
    let app = TestApp::new().await;
    let eval_id = seed_eval(&app).await;

    let resp = app
        .auth_get(&format!("/admin/corrections?task={}", app.ids.task), true)
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();

    // NOTE: the swagger proxy spec declares `last_modified` as a number and
    // `corrected_by` as non-nullable, while the real Python backend returns
    // an ISO-8601 string and nullable values; validation is skipped here.
    let c = &body["corrections"][0];
    assert_eq!(c["user"], app.ids.participant);
    assert_eq!(c["task_id"], app.ids.task);
    assert_eq!(c["id"], correction_id(app.ids.task, app.ids.participant));
    assert_eq!(c["state"], "notcorrected");
    assert!(c["comment"].is_null());
    assert_eq!(c["achievements"], json!([]));
    let m = &c["modules"][0];
    assert_eq!(m["module_id"], app.ids.module);
    assert_eq!(m["evaluations_list"], json!([eval_id]));
    assert_eq!(m["evaluation"]["eval_id"], eval_id);
    assert_eq!(m["evaluation"]["points"], 0.0);
    assert!(m["evaluation"]["corrected_by"].is_null());
    assert!(m["evaluation"]["last_modified"].is_string());
    assert!(m["evaluation"]["general"]["files"].is_array());

    assert_eq!(body["tasks"][0]["title"], "Demo uloha");
    assert_eq!(body["modules"][0]["type"], "general");
    assert_eq!(body["modules"][0]["autocorrect"], false);
    assert!(body["achievements"].is_array());
}

#[tokio::test]
async fn corrections_list_state_filter() {
    let app = TestApp::new().await;
    seed_eval(&app).await;

    // nothing corrected yet -> notcorrected filter keeps it
    let body: serde_json::Value = app
        .auth_get(
            &format!("/admin/corrections?task={}&state=notcorrected", app.ids.task),
            true,
        )
        .await
        .json();
    assert_eq!(body["corrections"].as_array().unwrap().len(), 1);

    // mark as evaluated by an org
    sqlx::query("UPDATE evaluations SET evaluator = ?")
        .bind(app.ids.org)
        .execute(&app.db)
        .await
        .unwrap();

    let body: serde_json::Value = app
        .auth_get(
            &format!("/admin/corrections?task={}&state=corrected", app.ids.task),
            true,
        )
        .await
        .json();
    assert_eq!(body["corrections"].as_array().unwrap().len(), 1);
    assert_eq!(body["corrections"][0]["state"], "corrected");

    let body: serde_json::Value = app
        .auth_get(
            &format!("/admin/corrections?task={}&state=notcorrected", app.ids.task),
            true,
        )
        .await
        .json();
    assert_eq!(body["corrections"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn corrections_list_with_thread_and_post() {
    let app = TestApp::new().await;
    seed_eval(&app).await;

    let thread_id: i64 = sqlx::query_scalar(
        "INSERT INTO threads (title, public, year) VALUES ('Oprava', 1, ?) RETURNING id",
    )
    .bind(app.ids.year)
    .fetch_one(&app.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO solution_comments (thread, \"user\", task) VALUES (?, ?, ?)")
        .bind(thread_id)
        .bind(app.ids.participant)
        .bind(app.ids.task)
        .execute(&app.db)
        .await
        .unwrap();
    let post_id: i64 = sqlx::query_scalar(
        "INSERT INTO posts (thread, author, body, published_at) VALUES (?, ?, 'Hezky!', datetime('now')) RETURNING id",
    )
    .bind(thread_id)
    .bind(app.ids.org)
    .fetch_one(&app.db)
    .await
    .unwrap();

    let body: serde_json::Value = app
        .auth_get(&format!("/admin/corrections?task={}", app.ids.task), true)
        .await
        .json();

    assert_eq!(body["corrections"][0]["comment"], thread_id);
    assert_eq!(body["threads"][0]["id"], thread_id);
    assert_eq!(body["threads"][0]["posts_count"], 1);
    assert_eq!(body["threadDetails"][0]["id"], thread_id);
    assert_eq!(body["threadDetails"][0]["root_posts"], json!([post_id]));
    assert_eq!(body["posts"][0]["id"], post_id);
    assert_eq!(body["posts"][0]["body"], "Hezky!");
    assert!(body["posts"][0]["published_at"].is_string());
    assert_eq!(body["posts"][0]["reaction"], json!([]));
    assert_eq!(body["posts"][0]["is_new"], true);
}

// ---------------------------------------------------------------------------
// GET /admin/corrections/{id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn correction_get_single_and_404() {
    let app = TestApp::new().await;
    let eval_id = seed_eval(&app).await;
    let cid = correction_id(app.ids.task, app.ids.participant);

    let body: serde_json::Value = app
        .auth_get(&format!("/admin/corrections/{cid}"), true)
        .await
        .json();
    assert_eq!(body["correction"]["id"], cid);
    assert_eq!(body["correction"]["modules"][0]["evaluation"]["eval_id"], eval_id);

    // no evaluations for this (task, user) -> 404
    app.auth_get(&format!("/admin/corrections/{}", 99999i64 * 100000 + 1), true)
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn correction_get_single_specific_eval() {
    let app = TestApp::new().await;
    // eval1 has more points, so plain best-eval would pick it;
    // the explicit `moduleX_version=Y` choice must win instead
    let eval1: i64 = sqlx::query_scalar(
        "INSERT INTO evaluations (\"user\", module, points, cheat, full_report)
         VALUES (?, ?, 9, 0, '') RETURNING id",
    )
    .bind(app.ids.participant)
    .bind(app.ids.module)
    .fetch_one(&app.db)
    .await
    .unwrap();
    let eval2 = seed_eval(&app).await; // 0 points
    assert_ne!(eval1, eval2);

    let cid = correction_id(app.ids.task, app.ids.participant);
    // Python: query param `module{module_id}_version={eval_id}`
    let body: serde_json::Value = app
        .auth_get(
            &format!("/admin/corrections/{cid}?module{}_version={}", app.ids.module, eval2),
            true,
        )
        .await
        .json();
    let m = &body["correction"]["modules"][0];
    // both evaluations listed, but the explicitly requested one is shown
    assert_eq!(m["evaluations_list"].as_array().unwrap().len(), 2);
    assert_eq!(m["evaluation"]["eval_id"], eval2);
    assert_eq!(m["evaluation"]["points"], 0.0);
}

// ---------------------------------------------------------------------------
// PUT /admin/corrections/{id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn correction_put_updates_evaluation() {
    let app = TestApp::new().await;
    let eval_id = seed_eval(&app).await;
    let cid = correction_id(app.ids.task, app.ids.participant);

    let payload = json!({
        "correction": {
            "task_id": app.ids.task,
            "user": app.ids.participant,
            "comment": null,
            "achievements": [],
            "modules": [
                {"evaluation": {
                    "eval_id": eval_id,
                    "points": 8.5,
                    "corrected_by": app.ids.org,
                    "cheat": true
                }}
            ]
        }
    });
    let body: serde_json::Value = app
        .authed("put", &format!("/admin/corrections/{cid}"), &app.tokens.0)
        .json(&payload)
        .await
        .json();

    // response carries the updated correction
    assert_eq!(body["correction"]["modules"][0]["evaluation"]["points"], 8.5);
    assert_eq!(body["correction"]["modules"][0]["evaluation"]["cheat"], true);
    assert_eq!(
        body["correction"]["modules"][0]["evaluation"]["corrected_by"],
        app.ids.org
    );

    // and the DB row was updated faithfully
    let row: (f64, Option<i64>, bool, String) = sqlx::query_as(
        "SELECT CAST(points AS REAL), evaluator, cheat, full_report FROM evaluations WHERE id = ?",
    )
    .bind(eval_id)
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(row.0, 8.5);
    assert_eq!(row.1, Some(app.ids.org));
    assert!(row.2);
    assert!(row.3.contains("edited by org"));
    assert!(row.3.contains("8.5 points"));
}

#[tokio::test]
async fn correction_put_defaults_corrected_by_to_org() {
    let app = TestApp::new().await;
    let eval_id = seed_eval(&app).await;
    let cid = correction_id(app.ids.task, app.ids.participant);

    // no corrected_by / cheat in the payload -> org id used, cheat false
    let payload = json!({
        "correction": {
            "task_id": app.ids.task,
            "user": app.ids.participant,
            "comment": null,
            "achievements": [],
            "modules": [
                {"evaluation": {"eval_id": eval_id, "points": 3.0}}
            ]
        }
    });
    app.authed("put", &format!("/admin/corrections/{cid}"), &app.tokens.0)
        .json(&payload)
        .await
        .assert_status_ok();

    let row: (Option<i64>, bool) =
        sqlx::query_as("SELECT evaluator, cheat FROM evaluations WHERE id = ?")
            .bind(eval_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(row.0, Some(app.ids.org));
    assert!(!row.1);
}

#[tokio::test]
async fn correction_put_links_thread_and_achievements() {
    let app = TestApp::new().await;
    let eval_id = seed_eval(&app).await;
    let cid = correction_id(app.ids.task, app.ids.participant);

    let ach_id: i64 = sqlx::query_scalar(
        "INSERT INTO achievements (title, picture, description, year) VALUES ('A', 'a.png', NULL, ?) RETURNING id",
    )
    .bind(app.ids.year)
    .fetch_one(&app.db)
    .await
    .unwrap();
    let thread_id: i64 = sqlx::query_scalar(
        "INSERT INTO threads (title, public, year) VALUES ('Oprava', 1, ?) RETURNING id",
    )
    .bind(app.ids.year)
    .fetch_one(&app.db)
    .await
    .unwrap();

    let payload = json!({
        "correction": {
            "task_id": app.ids.task,
            "user": app.ids.participant,
            "comment": thread_id,
            "achievements": [ach_id],
            "modules": [
                {"evaluation": {"eval_id": eval_id, "points": 5.0}}
            ]
        }
    });
    let body: serde_json::Value = app
        .authed("put", &format!("/admin/corrections/{cid}"), &app.tokens.0)
        .json(&payload)
        .await
        .json();
    assert_eq!(body["correction"]["comment"], thread_id);
    assert_eq!(body["correction"]["achievements"], json!([ach_id]));

    // unlinking works too
    let payload2 = json!({
        "correction": {
            "task_id": app.ids.task,
            "user": app.ids.participant,
            "comment": null,
            "achievements": [],
            "modules": [
                {"evaluation": {"eval_id": eval_id, "points": 5.0}}
            ]
        }
    });
    let body2: serde_json::Value = app
        .authed("put", &format!("/admin/corrections/{cid}"), &app.tokens.0)
        .json(&payload2)
        .await
        .json();
    assert!(body2["correction"]["comment"].is_null());
    assert_eq!(body2["correction"]["achievements"], json!([]));
}

// ---------------------------------------------------------------------------
// GET /admin/correctionInfos
// ---------------------------------------------------------------------------

#[tokio::test]
async fn correction_infos_list() {
    let app = TestApp::new().await;
    seed_eval(&app).await;

    let resp = app.auth_get("/admin/correctionInfos", true).await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();

    // swagger proxy spec diverges from the real backend here (Python also
    // returns `profile_picture`, which the spec omits), so skip validation.
    let info = &body["correctionsInfos"][0];
    assert_eq!(info["title"], "Demo uloha");
    assert_eq!(info["wave"], app.ids.wave_public);
    // the seeded task has evaluation_public = 1
    assert_eq!(info["corr_state"], "published");
    assert_eq!(info["solvers"], json!([app.ids.participant]));

    let users: Vec<i64> = body["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["id"].as_i64().unwrap())
        .collect();
    assert!(users.contains(&app.ids.participant));
    assert!(users.contains(&app.ids.org)); // task author

    assert!(!body["waves"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn correction_infos_state_transitions() {
    let app = TestApp::new().await;
    // fresh task without published corrections
    let thread_id: i64 = sqlx::query_scalar(
        "INSERT INTO threads (title, public, year) VALUES ('t', 1, ?) RETURNING id",
    )
    .bind(app.ids.year)
    .fetch_one(&app.db)
    .await
    .unwrap();
    let task_id: i64 = sqlx::query_scalar(
        "INSERT INTO tasks (title, author, wave, intro, body, thread, evaluation_public, deploy_status)
         VALUES ('Nova', ?, ?, '', '', ?, 0, 'default') RETURNING id",
    )
    .bind(app.ids.org)
    .bind(app.ids.wave_public)
    .bind(thread_id)
    .fetch_one(&app.db)
    .await
    .unwrap();
    let module_id: i64 = sqlx::query_scalar(
        "INSERT INTO modules (task, type, name, max_points, autocorrect, \"order\", bonus, custom)
         VALUES (?, 'quiz', 'q', 5, 0, 1, 0, 0) RETURNING id",
    )
    .bind(task_id)
    .fetch_one(&app.db)
    .await
    .unwrap();

    // no evaluations at all -> everything trivially corrected
    let body: serde_json::Value = app.auth_get("/admin/correctionInfos", true).await.json();
    let state = |b: &serde_json::Value| {
        b["correctionsInfos"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == task_id)
            .unwrap()["corr_state"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(state(&body), "corrected");

    // unevaluated submission -> base, then org evaluates -> corrected
    sqlx::query("INSERT INTO evaluations (\"user\", module, points, cheat, full_report) VALUES (?, ?, 0, 0, '')")
        .bind(app.ids.participant)
        .bind(module_id)
        .execute(&app.db)
        .await
        .unwrap();
    let body: serde_json::Value = app.auth_get("/admin/correctionInfos", true).await.json();
    assert_eq!(state(&body), "base");

    sqlx::query("UPDATE evaluations SET evaluator = ? WHERE module = ?")
        .bind(app.ids.org)
        .bind(module_id)
        .execute(&app.db)
        .await
        .unwrap();
    let body: serde_json::Value = app.auth_get("/admin/correctionInfos", true).await.json();
    assert_eq!(state(&body), "corrected");

    // publish flips to published
    sqlx::query("UPDATE tasks SET evaluation_public = 1 WHERE id = ?")
        .bind(task_id)
        .execute(&app.db)
        .await
        .unwrap();
    let body: serde_json::Value = app.auth_get("/admin/correctionInfos", true).await.json();
    assert_eq!(state(&body), "published");
}

#[tokio::test]
async fn correction_info_single_and_404() {
    let app = TestApp::new().await;

    let resp = app
        .auth_get(&format!("/admin/correctionInfos/{}", app.ids.task), true)
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["correctionsInfo"]["id"], app.ids.task);
    assert_eq!(body["correctionsInfo"]["title"], "Demo uloha");

    app.auth_get("/admin/correctionInfos/99999", true)
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// PUT /admin/correctionsEmail/{id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn corrections_email_sends_stubbed() {
    let app = TestApp::new().await;
    seed_eval(&app).await; // seed task has evaluation_public = 1

    let resp = app
        .authed("put", &format!("/admin/correctionsEmail/{}", app.ids.task), &app.tokens.0)
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["count"], 1);
    assert_swagger_valid("put", "/admin/correctionsEmail/{taskId}", 200, &body);

    // participant -> denied
    app.authed("put", &format!("/admin/correctionsEmail/{}", app.ids.task), &app.tokens.1)
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn corrections_email_not_published_is_400() {
    let app = TestApp::new().await;
    sqlx::query("UPDATE tasks SET evaluation_public = 0 WHERE id = ?")
        .bind(app.ids.task)
        .execute(&app.db)
        .await
        .unwrap();

    app.authed("put", &format!("/admin/correctionsEmail/{}", app.ids.task), &app.tokens.0)
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// GET /admin/corrections/{id}/publish
// ---------------------------------------------------------------------------

#[tokio::test]
async fn corrections_publish_toggles() {
    let app = TestApp::new().await;

    let resp = app
        .auth_get(
            &format!("/admin/corrections/{}/publish?public=0", app.ids.task),
            true,
        )
        .await;
    resp.assert_status_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body, json!({}));
    assert_swagger_valid("get", "/admin/corrections/{correctionId}/publish", 200, &body);

    let pub0: bool = sqlx::query_scalar("SELECT evaluation_public FROM tasks WHERE id = ?")
        .bind(app.ids.task)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(!pub0);

    // no param -> publish (true), like the Python default
    app.auth_get(
        &format!("/admin/corrections/{}/publish", app.ids.task),
        true,
    )
    .await
    .assert_status_ok();
    let pub1: bool = sqlx::query_scalar("SELECT evaluation_public FROM tasks WHERE id = ?")
        .bind(app.ids.task)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(pub1);

    // missing task -> 404
    app.auth_get("/admin/corrections/99999/publish", true)
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);

    // non-org -> 400
    app.auth_get(
        &format!("/admin/corrections/{}/publish?public=0", app.ids.task),
        false,
    )
    .await
    .assert_status(axum::http::StatusCode::BAD_REQUEST);
}
