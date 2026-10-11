use axum::{
    extract::{Path, State},
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use ksi_auth::AuthUser;
use crate::{error::{ApiError, ApiResult}, SharedState};
use ksi_models::{Post, PostJson, Thread, Year};

/// `{"thread": {...}}` body for `POST /threads` (swagger `ThreadCreation`).
#[derive(Debug, Deserialize)]
pub struct ThreadInput {
    pub title: String,
    pub public: Option<bool>,
    pub year: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct ThreadBody {
    pub thread: ThreadInput,
}

#[derive(Serialize)]
pub struct ThreadsResponse {
    threads: Vec<Thread>,
}

pub async fn list_threads(
    State(state): State<SharedState>,
    _user: AuthUser,
) -> ApiResult<Json<ThreadsResponse>> {
    let threads: Vec<Thread> =
        sqlx::query_as("SELECT id, title, public, year FROM threads WHERE public = 1 ORDER BY id")
            .fetch_all(&state.db)
            .await?;
    Ok(Json(ThreadsResponse { threads }))
}

#[derive(Serialize)]
pub struct ThreadResponse {
    thread: Thread,
}

pub async fn get_thread(
    State(state): State<SharedState>,
    _user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<ThreadResponse>> {
    let thread: Option<Thread> =
        sqlx::query_as("SELECT id, title, public, year FROM threads WHERE id = ?")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;

    match thread {
        None => Err(ApiError::NotFound("Vlákno neexistuje.".into())),
        Some(t) => Ok(Json(ThreadResponse { thread: t })),
    }
}

/// `POST /threads` — create a thread. Any logged-in user may create one;
/// a sealed year answers 403 and an over-long title answers 413.
pub async fn create_thread(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(body): Json<ThreadBody>,
) -> ApiResult<Json<ThreadResponse>> {
    if !user.is_logged_in() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }

    let input = body.thread;
    let year_id = input
        .year
        .ok_or_else(|| ApiError::BadRequest("Chybí rok vlákna.".into()))?;
    let year: Option<Year> = sqlx::query_as(
        "SELECT id, year, sealed, CAST(point_pad AS REAL) AS point_pad FROM years WHERE id = ?",
    )
    .bind(year_id)
    .fetch_optional(&state.db)
    .await?;
    let year = match year {
        None => return Err(ApiError::BadRequest("Rok neexistuje.".into())),
        Some(y) => y,
    };
    if year.sealed {
        return Err(ApiError::Forbidden("Ročník zapečetěn.".into()));
    }

    if input.title.chars().count() > 100 {
        return Err(ApiError::PayloadTooLarge(
            "Název vlákna může mít maximálně 100 znaků.".into(),
        ));
    }

    let res = sqlx::query("INSERT INTO threads (title, public, year) VALUES (?, ?, ?)")
        .bind(&input.title)
        .bind(input.public.unwrap_or(true))
        .bind(year_id)
        .execute(&state.db)
        .await?;
    let thread_id = res.last_insert_rowid();

    get_thread(State(state), user, Path(thread_id)).await
}

/// `PUT /threads/{id}` — the frontend calls this as `threadsMarkVisited`
/// (no body, returns `{}`): it records the visit in `threads_visits`,
/// shifting `last_visit` into `last_last_visit`.
///
/// Note: Python's `on_put` is not a thread update at all — it only tracks
/// visits, which is what we port here.
pub async fn mark_thread_visited(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let empty = || Json(serde_json::json!({}));
    let user_id = match user.id {
        Some(uid) => uid,
        // Python returns without a status here (200, no result set).
        None => return Ok(empty()),
    };

    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM threads WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
        // Python intends 400 here but assigns to a local `status` variable
        // instead of `resp.status`, so it actually returns 200. We return
        // the intended 400.
        return Err(ApiError::BadRequest("Vlákno neexistuje.".into()));
    }

    let now = Utc::now().naive_utc();
    let visited: Option<i64> =
        sqlx::query_scalar("SELECT user FROM threads_visits WHERE thread = ? AND user = ?")
            .bind(id)
            .bind(user_id)
            .fetch_optional(&state.db)
            .await?;
    if visited.is_some() {
        sqlx::query(
            "UPDATE threads_visits
             SET last_last_visit = last_visit, last_visit = ?
             WHERE thread = ? AND user = ?",
        )
        .bind(now)
        .bind(id)
        .bind(user_id)
        .execute(&state.db)
        .await?;
    } else {
        sqlx::query("INSERT INTO threads_visits (thread, user, last_visit) VALUES (?, ?, ?)")
            .bind(id)
            .bind(user_id)
            .bind(now)
            .execute(&state.db)
            .await?;
    }

    Ok(empty())
}

/// `DELETE /threads/{id}` — delete a thread (posts and visits cascade via
/// FK). The Python backend has no such endpoint; threads carry no author
/// column, so deletion is org-only (non-orgs get 400, like other
/// authorization failures in the reference).
pub async fn delete_thread(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest(
            "Smazat vlákno může pouze organizátor.".into(),
        ));
    }

    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM threads WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound("Vlákno neexistuje.".into()));
    }

    sqlx::query("DELETE FROM threads WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(Json(serde_json::json!({})))
}

#[derive(Serialize)]
pub struct ThreadDetailsResponse {
    thread: Thread,
    posts: Vec<PostJson>,
}

/// `GET /threadDetails/{id}` — thread with all its posts.
pub async fn get_thread_details(
    State(state): State<SharedState>,
    _user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<ThreadDetailsResponse>> {
    let thread: Option<Thread> =
        sqlx::query_as("SELECT id, title, public, year FROM threads WHERE id = ?")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;

    let thread = match thread {
        None => return Err(ApiError::NotFound("Vlákno neexistuje.".into())),
        Some(t) => t,
    };

    let posts: Vec<Post> = sqlx::query_as(
        "SELECT id, thread, author, body, published_at, parent, resolved_at
         FROM posts WHERE thread = ? ORDER BY published_at, id",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    let mut out = Vec::with_capacity(posts.len());
    for p in &posts {
        let reactions: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM posts WHERE parent = ? ORDER BY id")
                .bind(p.id)
                .fetch_all(&state.db)
                .await?;
        out.push(PostJson::from_row(p, reactions));
    }

    Ok(Json(ThreadDetailsResponse { thread, posts: out }))
}
