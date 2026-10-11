mod common;

use common::{assert_swagger_valid, TestApp};
use serde_json::{json, Value};

async fn seed_profile(app: &TestApp, user_id: i64) {
    sqlx::query(
        "INSERT INTO profiles (user_id, addr_street, addr_city, addr_zip, addr_country,
                               school_name, school_street, school_city, school_zip,
                               school_country, school_finish, tshirt_size)
         VALUES (?, 'Street', 'City', '12345', 'cz', 'School',
                 'SStreet', 'SCity', '54321', 'cz', 2028, 'M')",
    )
    .bind(user_id)
    .execute(&app.db)
    .await
    .unwrap();
}

/// Insert an extra user + bearer token; returns the user id.
async fn add_user(app: &TestApp, email: &str, role: &str) -> (i64, String) {
    let pw = bcrypt::hash("demo", 4).unwrap();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, first_name, last_name, nick_name, sex, password, short_info, role)
         VALUES (?, 'Extra', 'User', NULL, 'female', ?, '', ?)
         RETURNING id",
    )
    .bind(email)
    .bind(&pw)
    .bind(role)
    .fetch_one(&app.db)
    .await
    .unwrap();
    seed_profile(app, id).await;
    let token = format!("test-token-{id}-extra");
    sqlx::query(
        "INSERT INTO oauth2_tokens (access_token, user, expire, refresh_token, granted)
         VALUES (?, ?, datetime('now', '+1 hour'), ?, datetime('now'))",
    )
    .bind(&token)
    .bind(id)
    .bind(format!("refresh-{token}"))
    .execute(&app.db)
    .await
    .unwrap();
    (id, token)
}

#[tokio::test]
async fn users_list_ok() {
    let app = TestApp::new().await;
    seed_profile(&app, app.ids.org).await;
    seed_profile(&app, app.ids.participant).await;

    let resp = app.auth_get("/users", true).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("get", "/users", 200, &body);

    let users = body["users"].as_array().unwrap();
    assert_eq!(users.len(), 2);
    // org sees e-mails
    assert!(users.iter().all(|u| u["email"].is_string()));
    let part = users.iter().find(|u| u["id"] == app.ids.participant).unwrap();
    assert_eq!(part["role"], "participant");
    assert_eq!(part["school_name"], "School");
    assert!(part["seasons"].is_array());
}

#[tokio::test]
async fn users_list_public_for_anonymous() {
    let app = TestApp::new().await;
    seed_profile(&app, app.ids.org).await;
    seed_profile(&app, app.ids.participant).await;

    // the scoreboard is public in the Python backend too
    let resp = app.server.get("/users").await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    // anonymous viewers don't get e-mails
    assert!(body["users"]
        .as_array()
        .unwrap()
        .iter()
        .all(|u| u.get("email").is_none()));
}

#[tokio::test]
async fn users_list_filter_orgs() {
    let app = TestApp::new().await;
    seed_profile(&app, app.ids.org).await;
    seed_profile(&app, app.ids.participant).await;

    let resp = app.auth_get("/users?filter=orgs-all", true).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    let users = body["users"].as_array().unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0]["id"], app.ids.org);
}

#[tokio::test]
async fn users_list_sort_score() {
    let app = TestApp::new().await;
    seed_profile(&app, app.ids.org).await;
    seed_profile(&app, app.ids.participant).await;

    // give the participant some points in the public wave
    sqlx::query(
        "INSERT INTO evaluations (user, module, points, cheat, full_report)
         VALUES (?, ?, 7.5, 0, '')",
    )
    .bind(app.ids.participant)
    .bind(app.ids.module)
    .execute(&app.db)
    .await
    .unwrap();

    let resp = app.auth_get("/users?sort=score", true).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    let users = body["users"].as_array().unwrap();
    assert_eq!(users[0]["id"], app.ids.participant);
    assert_eq!(users[0]["score"], 7.5);
    assert_eq!(users[0]["tasks_num"], 1);
}

#[tokio::test]
async fn users_list_filter_participants() {
    let app = TestApp::new().await;
    seed_profile(&app, app.ids.org).await;
    seed_profile(&app, app.ids.participant).await;

    // no submissions yet -> empty for filter=part
    let resp = app.auth_get("/users?filter=part", true).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert!(body["users"].as_array().unwrap().is_empty());

    sqlx::query(
        "INSERT INTO evaluations (user, module, points, cheat, full_report)
         VALUES (?, ?, 5.0, 0, '')",
    )
    .bind(app.ids.participant)
    .bind(app.ids.module)
    .execute(&app.db)
    .await
    .unwrap();

    let resp = app.auth_get("/users?filter=part", true).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    let users = body["users"].as_array().unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0]["id"], app.ids.participant);
}

#[tokio::test]
async fn user_get_ok() {
    let app = TestApp::new().await;
    seed_profile(&app, app.ids.participant).await;

    let resp = app
        .auth_get(&format!("/users/{}", app.ids.participant), true)
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    let u = &body["user"];
    assert_eq!(u["id"], app.ids.participant);
    assert_eq!(u["first_name"], "Par");
    assert_eq!(u["role"], "participant");
    assert_eq!(u["email"], "part@demo.cz"); // org viewer
    assert!(u["achievements"].is_array());
}

#[tokio::test]
async fn user_get_missing_404() {
    let app = TestApp::new().await;
    let resp = app.auth_get("/users/424242", true).await;
    resp.assert_status(axum::http::StatusCode::NOT_FOUND);
    let body: Value = resp.json();
    assert!(body["errors"].is_array());
}

#[tokio::test]
async fn hidden_participant_visible_to_org_only() {
    let app = TestApp::new().await;
    let (hidden_id, _) = add_user(&app, "hidden@demo.cz", "participant_hidden").await;

    // participant viewer -> 404
    let resp = app.auth_get(&format!("/users/{hidden_id}"), false).await;
    resp.assert_status(axum::http::StatusCode::NOT_FOUND);

    // org viewer -> 200, role mapped to participant
    let resp = app.auth_get(&format!("/users/{hidden_id}"), true).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["user"]["role"], "participant");

    // hidden from the public list for non-orgs
    seed_profile(&app, app.ids.org).await;
    let resp = app.auth_get("/users", false).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert!(body["users"]
        .as_array()
        .unwrap()
        .iter()
        .all(|u| u["id"] != hidden_id));
}

#[tokio::test]
async fn user_delete_by_admin_ok() {
    let app = TestApp::new().await;
    let (admin_id, admin_token) = add_user(&app, "admin@demo.cz", "admin").await;
    let (victim_id, _) = add_user(&app, "victim@demo.cz", "participant").await;

    let resp = app
        .server
        .delete(format!("/users/{victim_id}").as_str())
        .add_header("Authorization", format!("Bearer {admin_token}"))
        .await;
    resp.assert_status_ok();

    let gone: Option<i64> = sqlx::query_scalar("SELECT id FROM users WHERE id = ?")
        .bind(victim_id)
        .fetch_optional(&app.db)
        .await
        .unwrap();
    assert!(gone.is_none());
    // profile row removed as well
    let gone_p: Option<i64> =
        sqlx::query_scalar("SELECT user_id FROM profiles WHERE user_id = ?")
            .bind(victim_id)
            .fetch_optional(&app.db)
            .await
            .unwrap();
    assert!(gone_p.is_none());
    // admin still there
    let still: Option<i64> = sqlx::query_scalar("SELECT id FROM users WHERE id = ?")
        .bind(admin_id)
        .fetch_optional(&app.db)
        .await
        .unwrap();
    assert!(still.is_some());
}

#[tokio::test]
async fn user_delete_by_participant_400() {
    let app = TestApp::new().await;
    let (victim_id, _) = add_user(&app, "victim@demo.cz", "participant").await;

    let resp = app.auth_delete(&format!("/users/{victim_id}"), false).await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn user_delete_missing_404() {
    let app = TestApp::new().await;
    let (_, admin_token) = add_user(&app, "admin@demo.cz", "admin").await;

    let resp = app
        .server
        .delete("/users/424242")
        .add_header("Authorization", format!("Bearer {admin_token}"))
        .await;
    resp.assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn discord_invite_null_without_discord() {
    let app = TestApp::new().await;
    let resp = app
        .auth_get(&format!("/users/{}/discord", app.ids.participant), false)
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert!(body.is_null());
}

#[tokio::test]
async fn discord_invite_returns_link() {
    let app = TestApp::new().await;
    sqlx::query("UPDATE users SET discord = 'part#1234' WHERE id = ?")
        .bind(app.ids.participant)
        .execute(&app.db)
        .await
        .unwrap();

    let resp = app
        .auth_get(&format!("/users/{}/discord", app.ids.participant), false)
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body, json!("https://discord.gg/test-invite"));
}

#[tokio::test]
async fn discord_invite_anonymous_401() {
    let app = TestApp::new().await;
    let resp = app
        .server
        .get(format!("/users/{}/discord", app.ids.participant).as_str())
        .await;
    resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn change_password_ok() {
    let app = TestApp::new().await;
    let resp = app
        .auth_post("/settings/changePassword", false)
        .json(&json!({
            "old_password": "demo",
            "new_password": "nove-heslo-1",
            "new_password2": "nove-heslo-1",
        }))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body, json!({"result": "ok"}));

    // can log in with the new password
    let login = app
        .server
        .post("/auth")
        .json(&json!({
            "grant_type": "password",
            "username": "part@demo.cz",
            "password": "nove-heslo-1",
        }))
        .await;
    login.assert_status_ok();
}

#[tokio::test]
async fn change_password_wrong_old_401() {
    let app = TestApp::new().await;
    let resp = app
        .auth_post("/settings/changePassword", false)
        .json(&json!({
            "old_password": "spatne",
            "new_password": "nove-heslo-1",
            "new_password2": "nove-heslo-1",
        }))
        .await;
    resp.assert_status(axum::http::StatusCode::UNAUTHORIZED);
    let body: Value = resp.json();
    assert_eq!(body, json!({"result": "error"}));
}

#[tokio::test]
async fn change_password_mismatch_error() {
    let app = TestApp::new().await;
    let resp = app
        .auth_post("/settings/changePassword", false)
        .json(&json!({
            "old_password": "demo",
            "new_password": "nove-heslo-1",
            "new_password2": "jine-heslo-2",
        }))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body, json!({"result": "error"}));
}

#[tokio::test]
async fn change_password_anonymous_400() {
    let app = TestApp::new().await;
    let resp = app
        .server
        .post("/settings/changePassword")
        .json(&json!({
            "old_password": "demo",
            "new_password": "nove-heslo-1",
            "new_password2": "nove-heslo-1",
        }))
        .await;
    resp.assert_status(axum::http::StatusCode::BAD_REQUEST);
}
