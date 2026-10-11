use axum::{extract::{Path, State}, Json};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use ksi_auth::AuthUser;
use crate::{error::{ApiError, ApiResult}, SharedState};
use ksi_models::{parse_datetime, ArticleJson, ArticleRow};

/// `{"article": {...}}` body for `POST /articles` and `PUT /articles/{id}`.
#[derive(Debug, Deserialize)]
pub struct ArticleInput {
    pub title: String,
    pub body: Option<String>,
    pub published: Option<bool>,
    pub year: Option<i64>,
    pub time_published: Option<String>,
    pub picture: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ArticleBody {
    pub article: ArticleInput,
}

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
    article_response(&state, id).await
}

async fn article_response(state: &SharedState, id: i64) -> ApiResult<Json<ArticleResponse>> {
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

fn require_org(user: &AuthUser, action: &str) -> ApiResult<()> {
    if !user.is_logged_in() || !user.is_org() {
        // Python answers these with HTTP 400 (the errors envelope claims
        // 401, but the status line is 400) — we keep the 400 status code.
        return Err(ApiError::BadRequest(format!("{action} může pouze organizátor.")));
    }
    Ok(())
}

/// `POST /articles` — create an article. Org only.
pub async fn create_article(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(body): Json<ArticleBody>,
) -> ApiResult<Json<ArticleResponse>> {
    require_org(&user, "Přidat článek")?;

    let input = body.article;
    let year_id = input
        .year
        .ok_or_else(|| ApiError::BadRequest("Chybí rok článku.".into()))?;
    let year_exists: Option<i64> = sqlx::query_scalar("SELECT id FROM years WHERE id = ?")
        .bind(year_id)
        .fetch_optional(&state.db)
        .await?;
    if year_exists.is_none() {
        return Err(ApiError::BadRequest("Rok neexistuje.".into()));
    }

    let time_created = match input.time_published {
        Some(s) => parse_datetime(&s).ok_or_else(|| {
            ApiError::BadRequest(format!("Neplatný čas zveřejnění: {s}"))
        })?,
        None => Utc::now().naive_utc(),
    };

    let res = sqlx::query(
        "INSERT INTO articles (author, title, body, published, year, time_created, picture)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(user.id.unwrap())
    .bind(input.title)
    .bind(input.body)
    .bind(input.published)
    .bind(year_id)
    .bind(time_created)
    .bind(input.picture)
    .execute(&state.db)
    .await?;
    let article_id = res.last_insert_rowid();

    // Python sets `resource = 'articles/{id}'` in a second commit.
    sqlx::query("UPDATE articles SET resource = ? WHERE id = ?")
        .bind(format!("articles/{article_id}"))
        .bind(article_id)
        .execute(&state.db)
        .await?;

    article_response(&state, article_id).await
}

/// `PUT /articles/{id}` — update an article. Org only.
pub async fn update_article(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<ArticleBody>,
) -> ApiResult<Json<ArticleResponse>> {
    require_org(&user, "Upravit článek")?;

    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM articles WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound("Článek s tímto ID neexistuje.".into()));
    }

    let input = body.article;
    let time_created = match input.time_published {
        Some(s) => parse_datetime(&s).ok_or_else(|| {
            ApiError::BadRequest(format!("Neplatný čas zveřejnění: {s}"))
        })?,
        None => Utc::now().naive_utc(),
    };

    sqlx::query(
        "UPDATE articles SET title = ?, body = ?, published = ?, time_created = ?, picture = ?
         WHERE id = ?",
    )
    .bind(input.title)
    .bind(input.body)
    .bind(input.published)
    .bind(time_created)
    .bind(input.picture)
    .bind(id)
    .execute(&state.db)
    .await?;

    article_response(&state, id).await
}

/// `DELETE /articles/{id}` — delete an article. Org only.
pub async fn delete_article(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    require_org(&user, "Smazat článek")?;

    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM articles WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound("Článek s tímto ID neexistuje.".into()));
    }

    sqlx::query("DELETE FROM articles WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(Json(serde_json::json!({})))
}
