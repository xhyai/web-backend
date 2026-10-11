use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use ksi_auth::AuthUser;
use crate::{
    error::{ApiError, ApiResult},
    SharedState,
};
use ksi_models::{AchievementJson, AchievementRow};

/// Mirrors the Python `app.py` middleware: the `YEAR` request header selects
/// the year, otherwise the newest year (highest id) is used.
async fn current_year_id(db: &SqlitePool, headers: &HeaderMap) -> Option<i64> {
    if let Some(v) = headers
        .get("year")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<i64>().ok())
    {
        return Some(v);
    }
    sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(id) FROM years")
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .flatten()
}

async fn config_value(db: &SqlitePool, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>("SELECT value FROM config WHERE \"key\" = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .flatten()
}

#[derive(Serialize)]
pub struct AchievementsResponse {
    pub achievements: Vec<AchievementJson>,
}

/// `GET /achievements` — list achievements of the current year plus
/// persistent (year-less) ones.
pub async fn list_achievements(
    State(state): State<SharedState>,
    _user: AuthUser,
    headers: HeaderMap,
) -> ApiResult<Json<AchievementsResponse>> {
    let year = current_year_id(&state.db, &headers).await;
    let rows: Vec<AchievementRow> = sqlx::query_as(
        "SELECT id, title, picture, description, year FROM achievements
         WHERE year IS NULL OR year = ? ORDER BY id",
    )
    .bind(year)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(AchievementsResponse {
        achievements: rows.iter().map(AchievementJson::from_row).collect(),
    }))
}

#[derive(Serialize)]
pub struct AchievementResponse {
    pub achievement: AchievementJson,
}

/// `GET /achievements/{id}` — achievement detail.
pub async fn get_achievement(
    State(state): State<SharedState>,
    _user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<AchievementResponse>> {
    match fetch_achievement(&state.db, id).await? {
        None => Err(ApiError::NotFound("Trofej s tímto ID neexistuje.".into())),
        Some(a) => Ok(Json(AchievementResponse {
            achievement: AchievementJson::from_row(&a),
        })),
    }
}

async fn fetch_achievement(db: &SqlitePool, id: i64) -> Result<Option<AchievementRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, title, picture, description, year FROM achievements WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

/// `GET /achievements/special/successful` — the trophy awarded to successful
/// participants (id taken from the `successful_participant_trophy_id` config).
pub async fn successful_achievement(
    State(state): State<SharedState>,
    _user: AuthUser,
) -> ApiResult<Json<AchievementResponse>> {
    let trophy_id: Option<i64> = config_value(&state.db, "successful_participant_trophy_id")
        .await
        .and_then(|v| v.parse().ok());
    match trophy_id {
        None => Err(ApiError::NotFound("Successful trophy was not set".into())),
        Some(id) => match fetch_achievement(&state.db, id).await? {
            None => Err(ApiError::NotFound("Trofej s tímto ID neexistuje.".into())),
            Some(a) => Ok(Json(AchievementResponse {
                achievement: AchievementJson::from_row(&a),
            })),
        },
    }
}

#[derive(Debug, Deserialize)]
pub struct AchievementInput {
    pub title: String,
    pub picture: String,
    pub description: Option<String>,
    pub persistent: bool,
}

#[derive(Debug, Deserialize)]
pub struct AchievementBody {
    pub achievement: AchievementInput,
}

fn year_for_input(persistent: bool, year: Option<i64>) -> Option<i64> {
    if persistent {
        None
    } else {
        year
    }
}

/// `POST /achievements` — create a trophy (org only).
pub async fn create_achievement(
    State(state): State<SharedState>,
    user: AuthUser,
    headers: HeaderMap,
    Json(body): Json<AchievementBody>,
) -> ApiResult<Json<AchievementResponse>> {
    if !user.is_logged_in() || !user.is_org() {
        // Python quirk: HTTP 400 with an "Unauthorized" error object.
        return Err(ApiError::BadRequest(
            "Přidání trofeje může provést pouze organizátor.".into(),
        ));
    }
    let year = year_for_input(
        body.achievement.persistent,
        current_year_id(&state.db, &headers).await,
    );
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO achievements (title, picture, description, year)
         VALUES (?, ?, ?, ?) RETURNING id",
    )
    .bind(&body.achievement.title)
    .bind(&body.achievement.picture)
    .bind(&body.achievement.description)
    .bind(year)
    .fetch_one(&state.db)
    .await?;
    let row = fetch_achievement(&state.db, id)
        .await?
        .expect("just-inserted achievement");
    Ok(Json(AchievementResponse {
        achievement: AchievementJson::from_row(&row),
    }))
}

/// `PUT /achievements/{id}` — update a trophy (org only).
pub async fn update_achievement(
    State(state): State<SharedState>,
    user: AuthUser,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<AchievementBody>,
) -> ApiResult<Json<AchievementResponse>> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest(
            "Úpravu trofeje může provést pouze organizátor.".into(),
        ));
    }
    if fetch_achievement(&state.db, id).await?.is_none() {
        return Err(ApiError::NotFound("Trofej s tímto ID neexistuje.".into()));
    }
    let year = year_for_input(
        body.achievement.persistent,
        current_year_id(&state.db, &headers).await,
    );
    sqlx::query(
        "UPDATE achievements SET title = ?, picture = ?, description = ?, year = ?
         WHERE id = ?",
    )
    .bind(&body.achievement.title)
    .bind(&body.achievement.picture)
    .bind(&body.achievement.description)
    .bind(year)
    .bind(id)
    .execute(&state.db)
    .await?;
    let row = fetch_achievement(&state.db, id)
        .await?
        .expect("just-updated achievement");
    Ok(Json(AchievementResponse {
        achievement: AchievementJson::from_row(&row),
    }))
}

/// `DELETE /achievements/{id}` — delete a trophy (admin only).
pub async fn delete_achievement(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    if !user.is_logged_in() || !user.is_admin() {
        return Err(ApiError::BadRequest(
            "Smazání trofeje může provést pouze administrátor.".into(),
        ));
    }
    if fetch_achievement(&state.db, id).await?.is_none() {
        return Err(ApiError::NotFound("Trofej s tímto ID neexsituje.".into()));
    }
    sqlx::query("DELETE FROM achievements WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(Json(serde_json::json!({})))
}

// ---------------------------------------------------------------------------
// POST /admin/achievements/grant
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct GrantBody {
    pub users: Vec<i64>,
    pub task: Option<i64>,
    pub achievement: i64,
}

#[derive(Serialize)]
pub struct GrantErrorItem {
    title: String,
}

#[derive(Serialize)]
pub struct GrantResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<GrantErrorItem>>,
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(|d| d.code())
        .map(|c| c == "2067" || c == "1555") // SQLITE_CONSTRAINT_UNIQUE / PRIMARYKEY
        .unwrap_or(false)
}

/// `POST /admin/achievements/grant` — grant an achievement to users (org only).
///
/// Mirrors `endpoint/admin/achievementGrant.py`: per-user problems are
/// collected into an `errors` list (HTTP 200); an empty list means `{}`.
pub async fn grant_achievements(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(body): Json<GrantBody>,
) -> ApiResult<Json<GrantResponse>> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest("Přístup odepřen.".into()));
    }

    let mut errors: Vec<GrantErrorItem> = Vec::new();

    for u in &body.users {
        // When a task is given, the user must have an evaluation for it.
        if let Some(task_id) = body.task {
            let evl: Option<i64> = sqlx::query_scalar(
                "SELECT e.id FROM evaluations e
                 JOIN modules m ON m.id = e.module
                 WHERE e.user = ? AND m.task = ? LIMIT 1",
            )
            .bind(u)
            .bind(task_id)
            .fetch_optional(&state.db)
            .await?;
            if evl.is_none() {
                errors.push(GrantErrorItem {
                    title: format!("Uživatel {u} neodevzdal vybranou úlohu\n"),
                });
                continue;
            }
        }

        let already: Option<i64> = sqlx::query_scalar(
            "SELECT user_id FROM user_achievement WHERE user_id = ? AND achievement_id = ?",
        )
        .bind(u)
        .bind(body.achievement)
        .fetch_optional(&state.db)
        .await?;
        if already.is_some() {
            errors.push(GrantErrorItem {
                title: format!("Uživateli {u} je již trofej přidělena\n"),
            });
            continue;
        }

        match sqlx::query(
            "INSERT INTO user_achievement (user_id, achievement_id, task_id)
             VALUES (?, ?, ?)",
        )
        .bind(u)
        .bind(body.achievement)
        .bind(body.task)
        .execute(&state.db)
        .await
        {
            Ok(_) => {}
            // Concurrent double-grant: report like the explicit check above.
            Err(e) if is_unique_violation(&e) => errors.push(GrantErrorItem {
                title: format!("Uživateli {u} je již trofej přidělena\n"),
            }),
            Err(e) => return Err(ApiError::from(e)),
        }
    }

    Ok(Json(GrantResponse {
        errors: if errors.is_empty() {
            None
        } else {
            Some(errors)
        },
    }))
}
