mod common;

use common::TestApp;
use serde_json::{json, Value};

async fn seed_achievements(app: &TestApp) -> (i64, i64, i64) {
    // (current-year trophy, persistent trophy, other-year trophy)
    let year_id = app.ids.year;
    let other_year: i64 = sqlx::query_scalar(
        "INSERT INTO years (year, sealed, point_pad) VALUES ('2025/2026', 0, 0) RETURNING id",
    )
    .fetch_one(&app.db)
    .await
    .unwrap();

    let a1: i64 = sqlx::query_scalar(
        "INSERT INTO achievements (title, picture, description, year)
         VALUES ('Trofej vlny', 'pic.svg', 'Popis', ?) RETURNING id",
    )
    .bind(year_id)
    .fetch_one(&app.db)
    .await
    .unwrap();
    let a2: i64 = sqlx::query_scalar(
        "INSERT INTO achievements (title, picture, description, year)
         VALUES ('Věčná trofej', 'pic2.svg', 'Popis 2', NULL) RETURNING id",
    )
    .fetch_one(&app.db)
    .await
    .unwrap();
    let a3: i64 = sqlx::query_scalar(
        "INSERT INTO achievements (title, picture, description, year)
         VALUES ('Stará trofej', 'pic3.svg', 'Popis 3', ?) RETURNING id",
    )
    .bind(other_year)
    .fetch_one(&app.db)
    .await
    .unwrap();
    (a1, a2, a3)
}

#[tokio::test]
async fn achievements_list_filters_year() {
    let app = TestApp::new().await;
    let (a1, a2, a3) = seed_achievements(&app).await;

    // without a YEAR header the newest year (highest id) is used
    let resp = app.server.get("/achievements").await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    let ids: Vec<i64> = body["achievements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_i64().unwrap())
        .collect();
    assert!(ids.contains(&a3)); // other-year trophy (newest year)
    assert!(ids.contains(&a2)); // persistent trophy
    assert_eq!(ids.len(), 2);

    // with an explicit YEAR header the selected year's trophies are listed
    let resp = app
        .server
        .get("/achievements")
        .add_header("YEAR", app.ids.year.to_string())
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    let ids: Vec<i64> = body["achievements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_i64().unwrap())
        .collect();
    // current-year + persistent trophies listed, other-year trophy hidden
    assert!(ids.contains(&a1));
    assert!(ids.contains(&a2));
    assert_eq!(ids.len(), 2);

    let a1j = body["achievements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == a1)
        .unwrap();
    assert_eq!(a1j["title"], "Trofej vlny");
    assert_eq!(a1j["persistent"], false);
    assert_eq!(a1j["year"], app.ids.year);
    assert_eq!(a1j["active"], true);
    let a2j = body["achievements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == a2)
        .unwrap();
    assert_eq!(a2j["persistent"], true);
    assert!(a2j["year"].is_null());
}

#[tokio::test]
async fn achievement_detail() {
    let app = TestApp::new().await;
    let (a1, _, _) = seed_achievements(&app).await;

    let resp = app.server.get(&format!("/achievements/{a1}")).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["achievement"]["id"], a1);
    assert_eq!(body["achievement"]["title"], "Trofej vlny");
    assert_eq!(body["achievement"]["description"], "Popis");
}

#[tokio::test]
async fn achievement_detail_404() {
    let app = TestApp::new().await;
    app.server
        .get("/achievements/999999")
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn achievement_create_update_delete() {
    let app = TestApp::new().await;

    // participant may not create
    let resp = app
        .auth_post("/achievements", false)
        .json(&json!({"achievement": {"title": "X", "picture": "p.svg", "description": "d", "persistent": false}}))
        .await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);

    // org creates a year-bound trophy
    let resp = app
        .auth_post("/achievements", true)
        .json(&json!({"achievement": {"title": "Nová", "picture": "n.svg", "description": "popis", "persistent": false}}))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    let id = body["achievement"]["id"].as_i64().unwrap();
    assert_eq!(body["achievement"]["title"], "Nová");
    assert_eq!(body["achievement"]["persistent"], false);
    assert_eq!(body["achievement"]["year"], app.ids.year);

    // org updates it to persistent
    let resp = app
        .server
        .put(&format!("/achievements/{id}"))
        .add_header("Authorization", format!("Bearer {}", app.tokens.0))
        .json(&json!({"achievement": {"title": "Přejmenovaná", "picture": "n.svg", "description": "popis", "persistent": true}}))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["achievement"]["title"], "Přejmenovaná");
    assert_eq!(body["achievement"]["persistent"], true);
    assert!(body["achievement"]["year"].is_null());

    // update of a missing trophy
    app.server
        .put("/achievements/999999")
        .add_header("Authorization", format!("Bearer {}", app.tokens.0))
        .json(&json!({"achievement": {"title": "X", "picture": "p.svg", "description": "d", "persistent": true}}))
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);

    // org is not admin: delete forbidden (400, mirroring the Python quirk)
    app.auth_delete(&format!("/achievements/{id}"), true)
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);

    // make an admin and delete
    let pw = bcrypt::hash("demo", 4).unwrap();
    let admin: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, first_name, last_name, nick_name, sex, password, short_info, role)
         VALUES ('admin@demo.cz', 'Ad', 'Min', NULL, 'male', ?, '', 'admin') RETURNING id",
    )
    .bind(&pw)
    .fetch_one(&app.db)
    .await
    .unwrap();
    let token = format!("test-token-admin-{admin}");
    sqlx::query(
        "INSERT INTO oauth2_tokens (access_token, user, expire, refresh_token, granted)
         VALUES (?, ?, datetime('now', '+1 hour'), 'r', datetime('now'))",
    )
    .bind(&token)
    .bind(admin)
    .execute(&app.db)
    .await
    .unwrap();

    let resp = app
        .server
        .delete(&format!("/achievements/{id}"))
        .add_header("Authorization", format!("Bearer {token}"))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body, json!({}));

    app.server
        .get(&format!("/achievements/{id}"))
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn successful_achievement() {
    let app = TestApp::new().await;
    let (a1, _, _) = seed_achievements(&app).await;

    // not configured -> 404
    app.server
        .get("/achievements/special/successful")
        .await
        .assert_status(axum::http::StatusCode::NOT_FOUND);

    sqlx::query("INSERT INTO config (\"key\", value, secret) VALUES ('successful_participant_trophy_id', ?, 0)")
        .bind(a1.to_string())
        .execute(&app.db)
        .await
        .unwrap();

    let resp = app.server.get("/achievements/special/successful").await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["achievement"]["id"], a1);
}

#[tokio::test]
async fn grant_achievement() {
    let app = TestApp::new().await;
    let (a1, _, _) = seed_achievements(&app).await;
    let part = app.ids.participant;

    // anonymous / participant may not grant
    app.server
        .post("/admin/achievements/grant")
        .json(&json!({"users": [part], "task": null, "achievement": a1}))
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);
    app.auth_post("/admin/achievements/grant", false)
        .json(&json!({"users": [part], "task": null, "achievement": a1}))
        .await
        .assert_status(axum::http::StatusCode::BAD_REQUEST);

    // grant without a task -> ok, empty object
    let resp = app
        .auth_post("/admin/achievements/grant", true)
        .json(&json!({"users": [part], "task": null, "achievement": a1}))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body, json!({}));
    let row: Option<(i64, i64)> =
        sqlx::query_as("SELECT user_id, achievement_id FROM user_achievement WHERE user_id = ?")
            .bind(part)
            .fetch_optional(&app.db)
            .await
            .unwrap();
    assert_eq!(row, Some((part, a1)));

    // granting again -> per-user error, still HTTP 200
    let resp = app
        .auth_post("/admin/achievements/grant", true)
        .json(&json!({"users": [part], "task": null, "achievement": a1}))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert!(body["errors"][0]["title"]
        .as_str()
        .unwrap()
        .contains("již trofej přidělena"));

    // grant with a task the user never submitted -> error
    let resp = app
        .auth_post("/admin/achievements/grant", true)
        .json(&json!({"users": [part], "task": app.ids.task, "achievement": a1}))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert!(body["errors"][0]["title"]
        .as_str()
        .unwrap()
        .contains("neodevzdal vybranou úlohu"));

    // add an evaluation for the participant's module, grant with task -> ok
    sqlx::query(
        "INSERT INTO evaluations (user, module, points, ok, cheat, full_report)
         VALUES (?, ?, 10, 1, 0, '')",
    )
    .bind(part)
    .bind(app.ids.module)
    .execute(&app.db)
    .await
    .unwrap();
    // (a_new is a fresh trophy so the grant is clean)
    let a_new: i64 = sqlx::query_scalar(
        "INSERT INTO achievements (title, picture, description, year)
         VALUES ('Grant test', 'g.svg', 'd', NULL) RETURNING id",
    )
    .fetch_one(&app.db)
    .await
    .unwrap();
    let resp = app
        .auth_post("/admin/achievements/grant", true)
        .json(&json!({"users": [part], "task": app.ids.task, "achievement": a_new}))
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>(), json!({}));
    let task_id: Option<i64> = sqlx::query_scalar(
        "SELECT task_id FROM user_achievement WHERE user_id = ? AND achievement_id = ?",
    )
    .bind(part)
    .bind(a_new)
    .fetch_optional(&app.db)
    .await
    .unwrap()
    .flatten();
    assert_eq!(task_id, Some(app.ids.task));
}
