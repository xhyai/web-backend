use axum::{
    extract::{Path, State},
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use ksi_auth::AuthUser;
use crate::{error::{ApiError, ApiResult}, SharedState};
use ksi_models::{Post, PostJson};

const MAX_POST_LEN: usize = 8000;

async fn reactions_for(db: &sqlx::SqlitePool, post_id: i64) -> Vec<i64> {
    sqlx::query_scalar("SELECT id FROM posts WHERE parent = ? ORDER BY id")
        .bind(post_id)
        .fetch_all(db)
        .await
        .unwrap_or_default()
}

#[derive(Serialize)]
pub struct PostResponse {
    post: PostJson,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePostBody {
    post: UpdatePostInput,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePostInput {
    body: String,
}

/// `PUT /posts/{id}` — update a post's body. Author or org only.
///
/// Python quirks ported faithfully: when the caller is not logged in, or is
/// logged in but is neither an org nor the author, the endpoint behaves
/// like `GET /posts/{id}` ("Toto tady musi byt — jinak nefunguje
/// frontend.").
pub async fn update_post(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<UpdatePostBody>,
) -> ApiResult<Json<PostResponse>> {
    if !user.is_logged_in() {
        return get_post(State(state), user, Path(id)).await;
    }

    if body.post.body.chars().count() > MAX_POST_LEN {
        return Err(ApiError::PayloadTooLarge(
            "Tělo příspěvku může mít maximálně 8000 znaků.".into(),
        ));
    }

    let post: Option<Post> = sqlx::query_as(
        "SELECT id, thread, author, body, published_at, parent, resolved_at
         FROM posts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    let post = match post {
        None => return Err(ApiError::NotFound("Příspěvek neexistuje.".into())),
        Some(p) => p,
    };

    if !user.is_org() && post.author != user.id.unwrap() {
        return get_post(State(state), user, Path(id)).await;
    }

    sqlx::query("UPDATE posts SET body = ? WHERE id = ?")
        .bind(&body.post.body)
        .bind(id)
        .execute(&state.db)
        .await?;

    get_post(State(state), user, Path(id)).await
}

/// `DELETE /posts/{id}` — delete a post. Author or org only.
pub async fn delete_post(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    if !user.is_logged_in() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }

    let post: Option<Post> = sqlx::query_as(
        "SELECT id, thread, author, body, published_at, parent, resolved_at
         FROM posts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    let post = match post {
        None => return Err(ApiError::NotFound("Příspěvek neexistuje.".into())),
        Some(p) => p,
    };

    if !user.is_org() && post.author != user.id.unwrap() {
        return Err(ApiError::Forbidden(
            "Smazat příspěvek může pouze jeho autor nebo organizátor.".into(),
        ));
    }

    sqlx::query("DELETE FROM posts WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(Json(serde_json::json!({})))
}

pub async fn get_post(
    State(state): State<SharedState>,
    _user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<PostResponse>> {
    let post: Option<Post> = sqlx::query_as(
        "SELECT id, thread, author, body, published_at, parent, resolved_at
         FROM posts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    match post {
        None => Err(ApiError::NotFound("Příspěvek neexistuje.".into())),
        Some(p) => {
            let reactions = reactions_for(&state.db, p.id).await;
            Ok(Json(PostResponse { post: PostJson::from_row(&p, reactions) }))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreatePostBody {
    post: NewPost,
}

#[derive(Debug, Deserialize)]
pub struct NewPost {
    thread: i64,
    body: String,
    parent: Option<i64>,
}

/// `POST /posts` — create a post. An org's reply marks the parent as
/// resolved; a participant's reply to a resolved post reopens it.
pub async fn create_post(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(body): Json<CreatePostBody>,
) -> ApiResult<Json<PostResponse>> {
    if !user.is_logged_in() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }
    if body.post.body.chars().count() > MAX_POST_LEN {
        return Err(ApiError::PayloadTooLarge(
            "Tělo příspěvku může mít maximálně 8000 znaků.".into(),
        ));
    }

    let thread_exists: Option<i64> =
        sqlx::query_scalar("SELECT id FROM threads WHERE id = ?")
            .bind(body.post.thread)
            .fetch_optional(&state.db)
            .await?;
    if thread_exists.is_none() {
        return Err(ApiError::BadRequest("Vlákno neexistuje.".into()));
    }

    if let Some(parent_id) = body.post.parent {
        let parent_thread: Option<i64> =
            sqlx::query_scalar("SELECT thread FROM posts WHERE id = ?")
                .bind(parent_id)
                .fetch_optional(&state.db)
                .await?;
        if parent_thread != Some(body.post.thread) {
            return Err(ApiError::BadRequest(
                "Rodičovský příspěvek neexistuje.".into(),
            ));
        }
    }

    let now = Utc::now().naive_utc();
    let user_id = user.id.unwrap();

    let res = sqlx::query(
        "INSERT INTO posts (thread, author, body, published_at, parent)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(body.post.thread)
    .bind(user_id)
    .bind(&body.post.body)
    .bind(now)
    .bind(body.post.parent)
    .execute(&state.db)
    .await?;
    let post_id = res.last_insert_rowid();

    // auto-resolve hook
    if let Some(parent_id) = body.post.parent {
        if user.is_org() {
            sqlx::query("UPDATE posts SET resolved_at = ? WHERE id = ?")
                .bind(now)
                .bind(parent_id)
                .execute(&state.db)
                .await?;
        } else {
            sqlx::query(
                "UPDATE posts SET resolved_at = NULL WHERE id = ? AND resolved_at IS NOT NULL",
            )
            .bind(parent_id)
            .execute(&state.db)
            .await?;
        }
    }

    let post: Post = sqlx::query_as(
        "SELECT id, thread, author, body, published_at, parent, resolved_at
         FROM posts WHERE id = ?",
    )
    .bind(post_id)
    .fetch_one(&state.db)
    .await?;
    let reactions = reactions_for(&state.db, post.id).await;
    Ok(Json(PostResponse { post: PostJson::from_row(&post, reactions) }))
}

/// `POST /posts/{id}/resolve` — manually mark a post resolved (org only).
pub async fn resolve_post(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<PostResponse>> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::Forbidden("Pouze pro organizátory.".into()));
    }
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM posts WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound("Příspěvek neexistuje.".into()));
    }

    let now = Utc::now().naive_utc();
    sqlx::query("UPDATE posts SET resolved_at = ? WHERE id = ?")
        .bind(now)
        .bind(id)
        .execute(&state.db)
        .await?;

    let post: Post = sqlx::query_as(
        "SELECT id, thread, author, body, published_at, parent, resolved_at
         FROM posts WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    let reactions = reactions_for(&state.db, post.id).await;
    Ok(Json(PostResponse { post: PostJson::from_row(&post, reactions) }))
}

/// `DELETE /posts/{id}/resolve` — manually reopen a post (org only).
pub async fn unresolve_post(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<PostResponse>> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::Forbidden("Pouze pro organizátory.".into()));
    }
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM posts WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound("Příspěvek neexistuje.".into()));
    }

    sqlx::query("UPDATE posts SET resolved_at = NULL WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;

    let post: Post = sqlx::query_as(
        "SELECT id, thread, author, body, published_at, parent, resolved_at
         FROM posts WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    let reactions = reactions_for(&state.db, post.id).await;
    Ok(Json(PostResponse { post: PostJson::from_row(&post, reactions) }))
}
