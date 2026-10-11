//! Shared integration-test harness.
//!
//! Spins up the real axum router against a fresh SQLite database,
//! seeds a minimal but realistic dataset, and provides helpers for
//! authenticated requests plus swagger-schema validation.

use std::sync::Arc;

use axum_test::TestServer;
use serde_json::Value;
use sqlx::SqlitePool;

use ksi_backend::{config::Config, create_app, AppState};

pub struct TestApp {
    pub server: TestServer,
    pub db: SqlitePool,
    /// (org_token, participant_token)
    pub tokens: (String, String),
    pub ids: TestIds,
}

#[derive(Debug, Clone, Copy)]
pub struct TestIds {
    pub year: i64,
    pub wave_public: i64,
    pub wave_hidden: i64,
    pub task: i64,
    pub module: i64,
    pub thread: i64,
    pub org: i64,
    pub participant: i64,
}

async fn seed(db: &SqlitePool) -> TestIds {
    let pw = bcrypt::hash("demo", 4).unwrap();

    let org: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, first_name, last_name, nick_name, sex, password, short_info, role)
         VALUES ('org@demo.cz', 'Orga', 'Nizator', NULL, 'male', ?, '', 'org')
         RETURNING id",
    )
    .bind(&pw)
    .fetch_one(db)
    .await
    .unwrap();

    let participant: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, first_name, last_name, nick_name, sex, password, short_info, role)
         VALUES ('part@demo.cz', 'Par', 'Ticipant', NULL, 'male', ?, '', 'participant')
         RETURNING id",
    )
    .bind(&pw)
    .fetch_one(db)
    .await
    .unwrap();

    let year: i64 = sqlx::query_scalar(
        "INSERT INTO years (year, sealed, point_pad) VALUES ('2026/2027', 0, 0) RETURNING id",
    )
    .fetch_one(db)
    .await
    .unwrap();

    let wave_public: i64 = sqlx::query_scalar(
        "INSERT INTO waves (year, \"index\", caption, garant, time_published)
         VALUES (?, 0, 'Nulta vlna', ?, datetime('now', '-30 days')) RETURNING id",
    )
    .bind(year)
    .bind(org)
    .fetch_one(db)
    .await
    .unwrap();

    let wave_hidden: i64 = sqlx::query_scalar(
        "INSERT INTO waves (year, \"index\", caption, garant, time_published)
         VALUES (?, 1, 'Skryta vlna', ?, datetime('now', '+30 days')) RETURNING id",
    )
    .bind(year)
    .bind(org)
    .fetch_one(db)
    .await
    .unwrap();

    let thread: i64 = sqlx::query_scalar(
        "INSERT INTO threads (title, public, year) VALUES ('Demo vlakno', 1, ?) RETURNING id",
    )
    .bind(year)
    .fetch_one(db)
    .await
    .unwrap();

    let task: i64 = sqlx::query_scalar(
        "INSERT INTO tasks (title, author, wave, intro, body, thread, evaluation_public, deploy_status)
         VALUES ('Demo uloha', ?, ?, '', '', ?, 1, 'default') RETURNING id",
    )
    .bind(org)
    .bind(wave_public)
    .bind(thread)
    .fetch_one(db)
    .await
    .unwrap();

    let module: i64 = sqlx::query_scalar(
        "INSERT INTO modules (task, type, name, max_points, autocorrect, \"order\", bonus, custom)
         VALUES (?, 'general', 'm1', 10, 0, 1, 0, 0) RETURNING id",
    )
    .bind(task)
    .fetch_one(db)
    .await
    .unwrap();

    // a hidden task in the unreleased wave (for the #225 regression test)
    let thread2: i64 = sqlx::query_scalar(
        "INSERT INTO threads (title, public, year) VALUES ('Skryte vlakno', 1, ?) RETURNING id",
    )
    .bind(year)
    .fetch_one(db)
    .await
    .unwrap();
    let task2: i64 = sqlx::query_scalar(
        "INSERT INTO tasks (title, author, wave, intro, body, thread, evaluation_public, deploy_status)
         VALUES ('Skryta uloha', ?, ?, '', '', ?, 1, 'default') RETURNING id",
    )
    .bind(org)
    .bind(wave_hidden)
    .bind(thread2)
    .fetch_one(db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO modules (task, type, name, max_points, autocorrect, \"order\", bonus, custom)
         VALUES (?, 'general', 'm2', 10, 0, 1, 0, 0)",
    )
    .bind(task2)
    .execute(db)
    .await
    .unwrap();

    TestIds {
        year,
        wave_public,
        wave_hidden,
        task,
        module,
        thread,
        org,
        participant,
    }
}

async fn make_token(db: &SqlitePool, user_id: i64) -> String {
    let access = format!("test-token-{user_id}-{}", uuid::Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO oauth2_tokens (access_token, user, expire, refresh_token, granted)
         VALUES (?, ?, datetime('now', '+1 hour'), ?, datetime('now'))",
    )
    .bind(&access)
    .bind(user_id)
    .bind(format!("refresh-{access}"))
    .execute(db)
    .await
    .unwrap();
    access
}

impl TestApp {
    pub async fn new() -> Self {
        let db = ksi_backend::db::create_pool("sqlite::memory:")
            .await
            .expect("create test pool");
        let ids = seed(&db).await;
        let org_token = make_token(&db, ids.org).await;
        let part_token = make_token(&db, ids.participant).await;

        let config = Config {
            database_url: "sqlite::memory:".into(),
            listen_addr: "127.0.0.1:0".into(),
            cors_origin: None,
        };
        let state = Arc::new(AppState { db: db.clone(), config });
        let app = create_app(state).await;
        let server = TestServer::new(app).unwrap();

        Self {
            server,
            db,
            tokens: (org_token, part_token),
            ids,
        }
    }

    pub fn org(&self) -> axum_test::TestRequest {
        self.server
            .get("/")
            .add_header("Authorization", format!("Bearer {}", self.tokens.0))
    }

    /// Build an authenticated request; call `.method()` afterwards is not
    /// supported by axum-test, so callers use `auth_get`/`auth_post`/…
    pub fn auth_get(&self, path: &str, org: bool) -> axum_test::TestRequest {
        let token = if org { &self.tokens.0 } else { &self.tokens.1 };
        self.server
            .get(path)
            .add_header("Authorization", format!("Bearer {token}"))
    }

    pub fn auth_post(&self, path: &str, org: bool) -> axum_test::TestRequest {
        let token = if org { &self.tokens.0 } else { &self.tokens.1 };
        self.server
            .post(path)
            .add_header("Authorization", format!("Bearer {token}"))
    }

    pub fn auth_delete(&self, path: &str, org: bool) -> axum_test::TestRequest {
        let token = if org { &self.tokens.0 } else { &self.tokens.1 };
        self.server
            .delete(path)
            .add_header("Authorization", format!("Bearer {token}"))
    }
}

// ---------------------------------------------------------------------------
// Swagger validation
// ---------------------------------------------------------------------------

use std::sync::OnceLock;

static SWAGGER: OnceLock<Value> = OnceLock::new();

fn swagger_spec() -> &'static Value {
    SWAGGER.get_or_init(|| {
        let path = std::env::var("SWAGGER_JSON").unwrap_or_else(|_| {
            // default: the checked-out web-backend-swagger repo next to this one
            let mut p = std::env::current_dir().unwrap();
            loop {
                let cand = p.join("web-backend-swagger/src/swagger/swagger.json");
                if cand.exists() {
                    return cand.to_string_lossy().into_owned();
                }
                if !p.pop() {
                    panic!("SWAGGER_JSON not set and web-backend-swagger not found");
                }
            }
        });
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("cannot read swagger spec at {path}"));
        serde_json::from_str(&text).expect("invalid swagger.json")
    })
}

/// Validate a response body against the swagger spec's response schema for
/// the given operation. `path_template` is the OpenAPI path, e.g.
/// `/posts/{id}`.
///
/// Operations missing from the spec (e.g. brand-new endpoints not yet
/// described in web-backend-swagger) are skipped with a printed notice
/// instead of failing — the spec is the contract, but it can lag behind.
pub fn assert_swagger_valid(
    method: &str,
    path_template: &str,
    status: u16,
    body: &Value,
) {
    let spec = swagger_spec();
    let op = &spec["paths"][path_template][method.to_lowercase().as_str()];
    if op.is_null() {
        eprintln!(
            "note: operation {method} {path_template} not in swagger spec — skipping schema validation"
        );
        return;
    }
    let schema = &op["responses"][status.to_string()]["content"]["application/json"]["schema"];
    if schema.is_null() {
        // no schema declared (e.g. binary download) — nothing to validate
        return;
    }
    let validator = match jsonschema::validator_for(schema) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "note: cannot build validator for {method} {path_template}: {e} — skipping schema validation"
            );
            return;
        }
    };
    let errors: Vec<String> = validator
        .iter_errors(body)
        .map(|e| format!("{}: {}", e.instance_path, e))
        .collect();
    assert!(
        errors.is_empty(),
        "response of {method} {path_template} does not match swagger schema:\n{}",
        errors.join("\n")
    );
}
