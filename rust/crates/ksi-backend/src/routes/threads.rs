use axum::{
    extract::{Path, State},
    Json,
};
use serde::Serialize;

use crate::{
    auth::AuthUser,
    error::{ApiError, ApiResult},
    models::{Post, PostJson, Thread},
    SharedState,
};

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
