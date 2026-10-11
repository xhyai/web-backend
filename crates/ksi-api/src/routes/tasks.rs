use axum::{extract::{Path, State}, Json};
use chrono::Utc;
use serde::Serialize;

use ksi_auth::AuthUser;
use crate::{error::{ApiError, ApiResult}, SharedState};

#[derive(Debug, Serialize)]
pub struct TaskJson {
    pub id: i64,
    pub title: String,
    pub author: Option<i64>,
    pub co_author: Option<i64>,
    pub details: i64,
    pub intro: String,
    pub max_score: f64,
    pub time_published: Option<String>,
    pub time_deadline: Option<String>,
    pub state: String,
    pub prerequisities: Vec<serde_json::Value>,
    pub picture_base: String,
    pub picture_suffix: String,
    pub wave: i64,
    pub feedbacks: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct TaskRow {
    id: i64,
    title: String,
    author: Option<i64>,
    co_author: Option<i64>,
    wave: i64,
    intro: String,
    time_deadline: Option<chrono::NaiveDateTime>,
    picture_base: Option<String>,
}

async fn wave_published(db: &sqlx::SqlitePool, wave_id: i64) -> Option<chrono::NaiveDateTime> {
    sqlx::query_scalar::<_, Option<chrono::NaiveDateTime>>(
        "SELECT time_published FROM waves WHERE id = ?",
    )
    .bind(wave_id)
    .fetch_one(db)
    .await
    .ok()
    .flatten()
}

async fn max_points(db: &sqlx::SqlitePool, task_id: i64) -> f64 {
    let sum: Option<f64> = sqlx::query_scalar(
        "SELECT CAST(SUM(max_points) AS REAL) FROM modules WHERE task = ?",
    )
    .bind(task_id)
    .fetch_optional(db)
    .await
    .unwrap_or(None)
    .flatten();
    sum.unwrap_or(0.0)
}

async fn task_to_json(
    db: &sqlx::SqlitePool,
    row: &TaskRow,
    wave_published: Option<chrono::NaiveDateTime>,
) -> TaskJson {
    let mp = max_points(db, row.id).await;
    TaskJson {
        id: row.id,
        title: row.title.clone(),
        author: row.author,
        co_author: row.co_author,
        details: row.id,
        intro: row.intro.clone(),
        max_score: (mp * 10.0).round() / 10.0,
        time_published: wave_published
            .map(|t| t.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()),
        time_deadline: row
            .time_deadline
            .map(|t| t.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()),
        state: "locked".into(),
        prerequisities: vec![],
        picture_base: row.picture_base.clone().unwrap_or_else(|| format!("/taskContent/{}/icon/", row.id)),
        picture_suffix: ".svg".into(),
        wave: row.wave,
        feedbacks: row.id,
    }
}

#[derive(Serialize)]
pub struct TasksResponse {
    pub tasks: Vec<TaskJson>,
}

/// `GET /tasks` — tasks visible to the user (public waves, or all for orgs).
pub async fn list_tasks(
    State(state): State<SharedState>,
    user: AuthUser,
) -> ApiResult<Json<TasksResponse>> {
    let now = Utc::now().naive_utc();
    let rows: Vec<TaskRow> = if user.is_org() || user.is_tester() {
        sqlx::query_as(
            "SELECT t.id, t.title, t.author, t.co_author, t.wave, t.intro,
                    t.time_deadline, t.picture_base
             FROM tasks t ORDER BY t.id",
        )
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as(
            "SELECT t.id, t.title, t.author, t.co_author, t.wave, t.intro,
                    t.time_deadline, t.picture_base
             FROM tasks t JOIN waves w ON w.id = t.wave
             WHERE w.time_published <= ? ORDER BY t.id",
        )
        .bind(now)
        .fetch_all(&state.db)
        .await?
    };

    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let wp = wave_published(&state.db, r.wave).await;
        out.push(task_to_json(&state.db, r, wp).await);
    }
    Ok(Json(TasksResponse { tasks: out }))
}

#[derive(Serialize)]
pub struct TaskResponse {
    pub task: TaskJson,
}

/// `GET /tasks/{id}` — single task (403 if locked for non-orgs).
pub async fn get_task(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<TaskResponse>> {
    let row: Option<TaskRow> = sqlx::query_as(
        "SELECT t.id, t.title, t.author, t.co_author, t.wave, t.intro,
                t.time_deadline, t.picture_base
         FROM tasks t WHERE t.id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    let row = match row {
        None => return Err(ApiError::NotFound("Úloha s tímto ID neexistuje.".into())),
        Some(r) => r,
    };

    if !user.is_logged_in() || (!user.is_org() && !user.is_tester()) {
        let published = wave_published(&state.db, row.wave).await;
        let now = Utc::now().naive_utc();
        let is_public = published.map(|t| t <= now).unwrap_or(false);
        if !is_public {
            return Err(ApiError::Forbidden("Úloha s tímto ID je uzamčena.".into()));
        }
    }

    let wp = wave_published(&state.db, row.wave).await;
    Ok(Json(TaskResponse {
        task: task_to_json(&state.db, &row, wp).await,
    }))
}
