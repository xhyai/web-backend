use axum::{extract::{Path, State}, Json};
use serde::Serialize;

use ksi_auth::AuthUser;
use crate::{error::{ApiError, ApiResult}, SharedState};
use ksi_models::{ArticleJson, ArticleRow};

#[derive(Serialize)]
pub struct ArticlesResponse {
    pub articles: Vec<ArticleJson>,
}

pub async fn list_articles(
    State(state): State<SharedState>,
    _user: AuthUser,
) -> ApiResult<Json<ArticlesResponse>> {
    let rows: Vec<ArticleRow> = sqlx::query_as(
        "SELECT id, author, title, body, picture, time_created, published, year, resource
         FROM articles ORDER BY time_created DESC",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(ArticlesResponse {
        articles: rows.iter().map(ArticleJson::from_row).collect(),
    }))
}

#[derive(Serialize)]
pub struct ArticleResponse {
    pub article: ArticleJson,
}

pub async fn get_article(
    State(state): State<SharedState>,
    _user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<ArticleResponse>> {
    let row: Option<ArticleRow> = sqlx::query_as(
        "SELECT id, author, title, body, picture, time_created, published, year, resource
         FROM articles WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    match row {
        None => Err(ApiError::NotFound("Článek s tímto ID neexistuje.".into())),
        Some(a) => Ok(Json(ArticleResponse { article: ArticleJson::from_row(&a) })),
    }
}
