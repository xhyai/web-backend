//! Integration tests for the write endpoints ported from the Python backend:
//! `POST`/`PUT`/`DELETE /years`, `/waves`, `/articles`, `/threads` and
//! `PUT`/`DELETE /posts/{id}`.
//!
//! Status codes and authorization rules mirror `endpoint/*.py` in the Python
//! reference (including its quirks, e.g. 400 — not 401 — on authorization
//! failures for years/waves).

mod common;

use axum::http::StatusCode;
use common::{assert_swagger_valid, TestApp};
use serde_json::{json, Value};

async fn admin_token(app: &TestApp) -> String {
    app.add_user("admin").await.1
}

async fn create_post(app: &TestApp, token: &str, thread: i64, body: &str) -> Value {
    let resp = app
        .authed("post", "/posts", token)
        .json(&json!({"post": {"thread": thread, "body": body}}))
        .await;
    resp.assert_status_ok();
    resp.json()
}

// ---------------------------------------------------------------------------
// years
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_year_admin_ok() {
    let app = TestApp::new().await;
    let token = admin_token(&app).await;

    // The admin frontend sends active_orgs as strings.
    let payload = json!({"year": {
        "index": 42,
        "year": "2030/2031",
        "sealed": false,
        "point_pad": 5.0,
        "active_orgs": [app.ids.org.to_string()],
    }});
    let resp = app.authed("post", "/years", &token).json(&payload).await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("post", "/years", 200, &body);

    assert_eq!(body["year"]["id"], 42);
    assert_eq!(body["year"]["index"], 42);
    assert_eq!(body["year"]["year"], "2030/2031");
    assert_eq!(body["year"]["sealed"], false);
    assert_eq!(body["year"]["point_pad"], 5.0);
    assert_eq!(body["year"]["active_orgs"], json!([app.ids.org]));

    // Persisted with the roster.
    let got: Value = app.server.get("/years/42").await.json();
    assert_eq!(got["year"]["active_orgs"], json!([app.ids.org]));
}

#[tokio::test]
async fn create_year_forbidden_for_non_admins() {
    let app = TestApp::new().await;
    let payload = json!({"year": {"index": 43, "year": "x", "sealed": false, "point_pad": 0.0}});

    // Python answers 400 (not 401/403) for non-admins here.
    for token in [&app.tokens.0, &app.tokens.1] {
        let resp = app.authed("post", "/years", token).json(&payload).await;
        resp.assert_status(StatusCode::BAD_REQUEST);
    }
    // Anonymous (no token at all).
    app.server
        .post("/years")
        .json(&payload)
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn update_year_admin_ok() {
    let app = TestApp::new().await;
    let token = admin_token(&app).await;

    // First create a year with the org on the roster…
    app.authed("post", "/years", &token)
        .json(&json!({"year": {
            "index": 44, "year": "2031/2032", "sealed": false,
            "point_pad": 0.0, "active_orgs": [app.ids.org.to_string()],
        }}))
        .await
        .assert_status_ok();

    // …then update it, swapping the roster to empty and sealing it.
    let resp = app
        .authed("put", "/years/44", &token)
        .json(&json!({"year": {
            "index": 44, "year": "2031/2032!", "sealed": true,
            "point_pad": 2.5, "active_orgs": [],
        }}))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["year"]["year"], "2031/2032!");
    assert_eq!(body["year"]["sealed"], true);
    assert_eq!(body["year"]["point_pad"], 2.5);
    assert_eq!(body["year"]["active_orgs"], json!([]));
}

#[tokio::test]
async fn update_year_changes_primary_key() {
    let app = TestApp::new().await;
    let token = admin_token(&app).await;

    // A wave-less year, so the PK change has no FK fallout.
    app.authed("post", "/years", &token)
        .json(&json!({"year": {
            "index": 50, "year": "2032/2033", "sealed": false, "point_pad": 0.0,
        }}))
        .await
        .assert_status_ok();

    let resp = app
        .authed("put", "/years/50", &token)
        .json(&json!({"year": {
            "index": 51, "year": "2032/2033", "sealed": false, "point_pad": 0.0,
        }}))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["year"]["id"], 51);

    app.server.get("/years/50").await.assert_status(StatusCode::NOT_FOUND);
    app.server.get("/years/51").await.assert_status_ok();
}

#[tokio::test]
async fn update_year_forbidden_and_404() {
    let app = TestApp::new().await;
    let payload = json!({"year": {
        "index": app.ids.year, "year": "x", "sealed": false, "point_pad": 0.0,
    }});

    // Participant may not update.
    let resp = app
        .authed("put", &format!("/years/{}", app.ids.year), &app.tokens.1)
        .json(&payload)
        .await;
    resp.assert_status(StatusCode::BAD_REQUEST);

    // Admin updating a missing year -> 404.
    let token = admin_token(&app).await;
    app.authed("put", "/years/999999", &token)
        .json(&payload)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_year_empty_ok() {
    let app = TestApp::new().await;
    let token = admin_token(&app).await;

    app.authed("post", "/years", &token)
        .json(&json!({"year": {
            "index": 52, "year": "2033/2034", "sealed": false, "point_pad": 0.0,
        }}))
        .await
        .assert_status_ok();

    let resp = app.authed("delete", "/years/52", &token).await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>(), json!({}));
    app.server.get("/years/52").await.assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_year_with_waves_forbidden() {
    let app = TestApp::new().await;
    let token = admin_token(&app).await;

    // The seeded year has waves -> 403 (only empty years may be deleted).
    app.authed("delete", &format!("/years/{}", app.ids.year), &token)
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn delete_year_forbidden_and_404() {
    let app = TestApp::new().await;

    // Participant may not delete (400 like Python).
    app.authed("delete", &format!("/years/{}", app.ids.year), &app.tokens.1)
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    // Admin deleting a missing year -> 404.
    let token = admin_token(&app).await;
    app.authed("delete", "/years/999999", &token)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// waves
// ---------------------------------------------------------------------------

fn wave_payload(year: i64, garant: i64) -> Value {
    json!({"wave": {
        "index": 7,
        "caption": "Sedmá vlna",
        "garant": garant,
        "time_published": "2026-09-01T00:00:00.000000",
        "year": year,
    }})
}

#[tokio::test]
async fn create_wave_admin_ok() {
    let app = TestApp::new().await;
    let token = admin_token(&app).await;

    let resp = app
        .authed("post", "/waves", &token)
        .json(&wave_payload(app.ids.year, app.ids.org))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("post", "/waves", 200, &body);

    assert_eq!(body["wave"]["index"], 7);
    assert_eq!(body["wave"]["caption"], "Sedmá vlna");
    assert_eq!(body["wave"]["garant"], app.ids.org);
    assert_eq!(body["wave"]["year"], app.ids.year);
    assert_eq!(body["wave"]["public"], true);
}

#[tokio::test]
async fn create_wave_admin_only() {
    let app = TestApp::new().await;
    // The Python reference restricts wave creation to admins (400 otherwise),
    // even though looser docs suggest orgs could do it.
    for token in [&app.tokens.0, &app.tokens.1] {
        let resp = app
            .authed("post", "/waves", token)
            .json(&wave_payload(app.ids.year, app.ids.org))
            .await;
        resp.assert_status(StatusCode::BAD_REQUEST);
    }
    app.server
        .post("/waves")
        .json(&wave_payload(app.ids.year, app.ids.org))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_wave_unknown_year_400() {
    let app = TestApp::new().await;
    let token = admin_token(&app).await;
    app.authed("post", "/waves", &token)
        .json(&wave_payload(999999, app.ids.org))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn update_wave_garant_and_admin_ok() {
    let app = TestApp::new().await;

    // The seeded org is the garant of wave_public -> may update.
    let payload = json!({"wave": {
        "index": 0, "caption": "Přejmenovaná vlna",
        "garant": app.ids.org, "time_published": "",
    }});
    let resp = app
        .authed("put", &format!("/waves/{}", app.ids.wave_public), &app.tokens.0)
        .json(&payload)
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["wave"]["caption"], "Přejmenovaná vlna");
    // Empty time_published keeps the old value (Python's truthy rule).
    assert!(body["wave"]["time_published"].is_string());

    // An admin who is not the garant may update as well.
    let token = admin_token(&app).await;
    app.authed("put", &format!("/waves/{}", app.ids.wave_public), &token)
        .json(&payload)
        .await
        .assert_status_ok();
}

#[tokio::test]
async fn update_wave_other_org_400() {
    let app = TestApp::new().await;
    let (_other_org, other_token) = app.add_user("org").await;

    let payload = json!({"wave": {
        "index": 0, "caption": "x", "garant": app.ids.org,
        "time_published": "2026-09-01T00:00:00",
    }});
    // Logged-in org, but neither admin nor the wave's garant -> 400.
    app.authed("put", &format!("/waves/{}", app.ids.wave_public), &other_token)
        .json(&payload)
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    // Participant -> 400 as well.
    app.authed("put", &format!("/waves/{}", app.ids.wave_public), &app.tokens.1)
        .json(&payload)
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn update_wave_404() {
    let app = TestApp::new().await;
    let payload = json!({"wave": {
        "index": 0, "caption": "x", "garant": app.ids.org,
    }});
    app.authed("put", "/waves/999999", &app.tokens.0)
        .json(&payload)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_wave_admin_ok() {
    let app = TestApp::new().await;
    let token = admin_token(&app).await;

    // Create a wave-less wave, then delete it.
    let created: Value = app
        .authed("post", "/waves", &token)
        .json(&wave_payload(app.ids.year, app.ids.org))
        .await
        .json();
    let wave_id = created["wave"]["id"].as_i64().unwrap();

    let resp = app.authed("delete", &format!("/waves/{wave_id}"), &token).await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>(), json!({}));
    app.server
        .get(&format!("/waves/{wave_id}"))
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_wave_with_tasks_403() {
    let app = TestApp::new().await;
    let token = admin_token(&app).await;

    // wave_public has a task -> 403.
    app.authed("delete", &format!("/waves/{}", app.ids.wave_public), &token)
        .await
        .assert_status(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn delete_wave_forbidden() {
    let app = TestApp::new().await;
    // Org (non-admin) and participant -> 400.
    for token in [&app.tokens.0, &app.tokens.1] {
        app.authed("delete", &format!("/waves/{}", app.ids.wave_hidden), token)
            .await
            .assert_status(StatusCode::BAD_REQUEST);
    }
}

// ---------------------------------------------------------------------------
// articles
// ---------------------------------------------------------------------------

fn article_payload(year: i64) -> Value {
    json!({"article": {
        "title": "Nový článek",
        "body": "Obsah článku.",
        "published": true,
        "year": year,
        "time_published": "2026-10-01T12:00:00.000Z",
        "picture": null,
    }})
}

#[tokio::test]
async fn create_article_org_ok() {
    let app = TestApp::new().await;

    let resp = app
        .auth_post("/articles", true)
        .json(&article_payload(app.ids.year))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();

    assert_eq!(body["article"]["title"], "Nový článek");
    assert_eq!(body["article"]["author"], app.ids.org);
    assert_eq!(body["article"]["published"], true);
    // Python sets resource = 'articles/{id}' after insert; the API exposes
    // it as `content`.
    let id = body["article"]["id"].as_i64().unwrap();
    assert_eq!(body["article"]["content"], format!("articles/{id}"));
    // Missing picture falls back to the default image.
    assert_eq!(body["article"]["picture"], "img/box-ksi.svg");
}

#[tokio::test]
async fn create_article_participant_400() {
    let app = TestApp::new().await;
    app.auth_post("/articles", false)
        .json(&article_payload(app.ids.year))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    app.server
        .post("/articles")
        .json(&article_payload(app.ids.year))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn update_article_org_ok() {
    let app = TestApp::new().await;
    let created: Value = app
        .auth_post("/articles", true)
        .json(&article_payload(app.ids.year))
        .await
        .json();
    let id = created["article"]["id"].as_i64().unwrap();

    let resp = app
        .authed("put", &format!("/articles/{id}"), &app.tokens.0)
        .json(&json!({"article": {
            "title": "Upravený článek",
            "body": "Nový obsah.",
            "published": false,
            "time_published": "2026-10-02T12:00:00.000Z",
            "picture": "img/custom.svg",
        }}))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["article"]["title"], "Upravený článek");
    assert_eq!(body["article"]["published"], false);
    assert_eq!(body["article"]["picture"], "img/custom.svg");
    assert!(body["article"]["time_published"]
        .as_str()
        .unwrap()
        .starts_with("2026-10-02"));
}

#[tokio::test]
async fn update_article_forbidden_and_404() {
    let app = TestApp::new().await;
    let payload = json!({"article": {
        "title": "x", "body": "y", "published": true,
        "time_published": "2026-10-02T12:00:00.000Z", "picture": null,
    }});

    // Participant -> 400.
    app.authed("put", "/articles/1", &app.tokens.1)
        .json(&payload)
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    // Org updating a missing article -> 404.
    app.authed("put", "/articles/999999", &app.tokens.0)
        .json(&payload)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_article_org_ok() {
    let app = TestApp::new().await;
    let created: Value = app
        .auth_post("/articles", true)
        .json(&article_payload(app.ids.year))
        .await
        .json();
    let id = created["article"]["id"].as_i64().unwrap();

    // Participant may not delete.
    app.authed("delete", &format!("/articles/{id}"), &app.tokens.1)
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    let resp = app
        .authed("delete", &format!("/articles/{id}"), &app.tokens.0)
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>(), json!({}));
    app.server
        .get(&format!("/articles/{id}"))
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// threads
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_thread_ok() {
    let app = TestApp::new().await;

    // Participant may create a thread; `public` defaults to true.
    let resp = app
        .auth_post("/threads", false)
        .json(&json!({"thread": {"title": "Dotaz k úloze", "year": app.ids.year}}))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_swagger_valid("post", "/threads", 200, &body);
    assert_eq!(body["thread"]["title"], "Dotaz k úloze");
    assert_eq!(body["thread"]["public"], true);
    assert_eq!(body["thread"]["year"], app.ids.year);
}

#[tokio::test]
async fn create_thread_anonymous_400() {
    let app = TestApp::new().await;
    app.server
        .post("/threads")
        .json(&json!({"thread": {"title": "x", "year": app.ids.year}}))
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_thread_title_too_long_413() {
    let app = TestApp::new().await;
    let long = "x".repeat(101);
    app.auth_post("/threads", false)
        .json(&json!({"thread": {"title": long, "year": app.ids.year}}))
        .await
        .assert_status(StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn create_thread_sealed_year_403() {
    let app = TestApp::new().await;
    sqlx::query("UPDATE years SET sealed = 1 WHERE id = ?")
        .bind(app.ids.year)
        .execute(&app.db)
        .await
        .unwrap();

    let resp = app
        .auth_post("/threads", false)
        .json(&json!({"thread": {"title": "x", "year": app.ids.year}}))
        .await;
    resp.assert_status(StatusCode::FORBIDDEN);
    let body: Value = resp.json();
    assert_eq!(body["errors"][0]["detail"], "Ročník zapečetěn.");
}

#[tokio::test]
async fn mark_thread_visited_tracks_visit() {
    let app = TestApp::new().await;

    // First PUT creates the visit row…
    app.authed("put", &format!("/threads/{}", app.ids.thread), &app.tokens.1)
        .await
        .assert_status_ok();

    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT datetime(last_visit), datetime(last_last_visit)
         FROM threads_visits WHERE thread = ? AND user = ?",
    )
    .bind(app.ids.thread)
    .bind(app.ids.participant)
    .fetch_optional(&app.db)
    .await
    .unwrap();
    assert!(row.is_some());
    assert!(row.unwrap().1.is_none());

    // …second PUT shifts last_visit into last_last_visit.
    app.authed("put", &format!("/threads/{}", app.ids.thread), &app.tokens.1)
        .await
        .assert_status_ok();
    let last_last: Option<String> = sqlx::query_scalar(
        "SELECT datetime(last_last_visit) FROM threads_visits WHERE thread = ? AND user = ?",
    )
    .bind(app.ids.thread)
    .bind(app.ids.participant)
    .fetch_optional(&app.db)
    .await
    .unwrap()
    .flatten();
    assert!(last_last.is_some());
}

#[tokio::test]
async fn mark_thread_visited_anonymous_ok_without_row() {
    let app = TestApp::new().await;
    // Not logged in -> 200 {} and no visit row (mirrors Python).
    let resp = app.server.put(&format!("/threads/{}", app.ids.thread)).await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>(), json!({}));

    let cnt: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM threads_visits")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(cnt, 0);
}

#[tokio::test]
async fn mark_thread_visited_unknown_thread_400() {
    let app = TestApp::new().await;
    // Python intends 400 here (it just has a bug assigning the status to a
    // local variable); we return the intended 400.
    app.authed("put", "/threads/999999", &app.tokens.0)
        .await
        .assert_status(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn delete_thread_org_cascades() {
    let app = TestApp::new().await;

    // Org creates a thread with a post in it…
    let created: Value = app
        .auth_post("/threads", true)
        .json(&json!({"thread": {"title": "Ke smazání", "year": app.ids.year}}))
        .await
        .json();
    let thread_id = created["thread"]["id"].as_i64().unwrap();
    let post = create_post(&app, &app.tokens.0, thread_id, "Ahoj").await;
    let post_id = post["post"]["id"].as_i64().unwrap();

    // Participant may not delete (400 like other auth failures).
    app.authed("delete", &format!("/threads/{thread_id}"), &app.tokens.1)
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    // Org deletes: thread gone, post cascaded away.
    let resp = app
        .authed("delete", &format!("/threads/{thread_id}"), &app.tokens.0)
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>(), json!({}));
    app.server
        .get(&format!("/threads/{thread_id}"))
        .await
        .assert_status(StatusCode::NOT_FOUND);
    app.server
        .get(&format!("/posts/{post_id}"))
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// posts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_post_author_ok() {
    let app = TestApp::new().await;
    let post = create_post(&app, &app.tokens.1, app.ids.thread, "Původní text").await;
    let id = post["post"]["id"].as_i64().unwrap();

    let resp = app
        .authed("put", &format!("/posts/{id}"), &app.tokens.1)
        .json(&json!({"post": {"body": "Upravený text"}}))
        .await;
    resp.assert_status_ok();
    let body: Value = resp.json();
    assert_eq!(body["post"]["body"], "Upravený text");
    assert_eq!(body["post"]["author"], app.ids.participant);
}

#[tokio::test]
async fn update_post_by_stranger_behaves_like_get() {
    let app = TestApp::new().await;
    let post = create_post(&app, &app.tokens.1, app.ids.thread, "Původní text").await;
    let id = post["post"]["id"].as_i64().unwrap();

    // Another participant is neither org nor author: Python's quirk makes
    // PUT behave like GET (200, body unchanged) so the frontend keeps working.
    let other = app.add_user("participant").await.1;
    let resp = app
        .authed("put", &format!("/posts/{id}"), &other)
        .json(&json!({"post": {"body": "Pokus o přepsání"}}))
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>()["post"]["body"], "Původní text");
}

#[tokio::test]
async fn update_post_org_ok() {
    let app = TestApp::new().await;
    let post = create_post(&app, &app.tokens.1, app.ids.thread, "Původní text").await;
    let id = post["post"]["id"].as_i64().unwrap();

    // An org may edit anyone's post.
    let resp = app
        .authed("put", &format!("/posts/{id}"), &app.tokens.0)
        .json(&json!({"post": {"body": "Oprava orga"}}))
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>()["post"]["body"], "Oprava orga");
}

#[tokio::test]
async fn update_post_anonymous_behaves_like_get() {
    let app = TestApp::new().await;
    let post = create_post(&app, &app.tokens.1, app.ids.thread, "Původní text").await;
    let id = post["post"]["id"].as_i64().unwrap();

    let resp = app
        .server
        .put(&format!("/posts/{id}"))
        .json(&json!({"post": {"body": "Pokus"}}))
        .await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>()["post"]["body"], "Původní text");
}

#[tokio::test]
async fn update_post_too_long_413() {
    let app = TestApp::new().await;
    let post = create_post(&app, &app.tokens.1, app.ids.thread, "x").await;
    let id = post["post"]["id"].as_i64().unwrap();

    app.authed("put", &format!("/posts/{id}"), &app.tokens.1)
        .json(&json!({"post": {"body": "x".repeat(8001)}}))
        .await
        .assert_status(StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn update_post_404() {
    let app = TestApp::new().await;
    app.authed("put", "/posts/999999", &app.tokens.1)
        .json(&json!({"post": {"body": "x"}}))
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_post_author_ok() {
    let app = TestApp::new().await;
    let post = create_post(&app, &app.tokens.1, app.ids.thread, "Ke smazání").await;
    let id = post["post"]["id"].as_i64().unwrap();

    let resp = app.authed("delete", &format!("/posts/{id}"), &app.tokens.1).await;
    resp.assert_status_ok();
    assert_eq!(resp.json::<Value>(), json!({}));
    app.server
        .get(&format!("/posts/{id}"))
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_post_stranger_403() {
    let app = TestApp::new().await;
    let post = create_post(&app, &app.tokens.1, app.ids.thread, "Cizí příspěvek").await;
    let id = post["post"]["id"].as_i64().unwrap();

    // Another participant: 403.
    let other = app.add_user("participant").await.1;
    app.authed("delete", &format!("/posts/{id}"), &other)
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // But an org may delete it.
    app.authed("delete", &format!("/posts/{id}"), &app.tokens.0)
        .await
        .assert_status_ok();
}

#[tokio::test]
async fn delete_post_anonymous_400_and_404() {
    let app = TestApp::new().await;

    app.server
        .delete("/posts/1")
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    app.authed("delete", "/posts/999999", &app.tokens.1)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}
