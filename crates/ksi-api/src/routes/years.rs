use axum::{extract::{Path, State}, Json};
use chrono::Utc;
use serde::Serialize;

use ksi_auth::AuthUser;
use crate::{error::{ApiError, ApiResult}, SharedState};
use ksi_models::{Wave, WaveJson, Year, YearJson};

/// Sum of max_points and task count for a year, considering only waves
/// whose `time_published` is not in the future (mirrors the #225 fix:
/// unreleased tasks must not leak through `GET /years`).
async fn year_points(
    db: &sqlx::SqlitePool,
    year_id: i64,
) -> Result<(f64, i64), sqlx::Error> {
    let now = Utc::now().naive_utc();
    let row: Option<(Option<f64>, Option<i64>)> = sqlx::query_as(
        "SELECT COALESCE(CAST(SUM(m.max_points) AS REAL), 0.0), COUNT(DISTINCT t.id)
         FROM tasks t
         JOIN waves w ON w.id = t.wave
         JOIN modules m ON m.task = t.id
         WHERE w.year = ? AND w.time_published <= ?",
    )
    .bind(year_id)
    .bind(now)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(s, c)| (s.unwrap_or(0.0), c.unwrap_or(0))).unwrap_or((0.0, 0)))
}

pub(crate) async fn wave_points(
    db: &sqlx::SqlitePool,
    wave_id: i64,
) -> Result<(f64, i64), sqlx::Error> {
    let row: Option<(Option<f64>, Option<i64>)> = sqlx::query_as(
        "SELECT COALESCE(CAST(SUM(m.max_points) AS REAL), 0.0), COUNT(DISTINCT t.id)
         FROM tasks t
         JOIN modules m ON m.task = t.id
         WHERE t.wave = ?",
    )
    .bind(wave_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(s, c)| (s.unwrap_or(0.0), c.unwrap_or(0))).unwrap_or((0.0, 0)))
}

#[derive(Serialize)]
pub struct YearsResponse {
    years: Vec<YearJson>,
}

pub async fn list_years(
    State(state): State<SharedState>,
    _user: AuthUser,
) -> ApiResult<Json<YearsResponse>> {
    let years: Vec<Year> =
        sqlx::query_as("SELECT id, year, sealed, CAST(point_pad AS REAL) AS point_pad FROM years ORDER BY id")
            .fetch_all(&state.db)
            .await?;

    let mut out = Vec::with_capacity(years.len());
    for y in &years {
        let (sum_points, tasks_cnt) = year_points(&state.db, y.id).await?;
        let active_orgs: Vec<i64> =
            sqlx::query_scalar("SELECT org FROM active_orgs WHERE year = ?")
                .bind(y.id)
                .fetch_all(&state.db)
                .await?;
        out.push(YearJson {
            id: y.id,
            index: y.id,
            year: y.year.clone(),
            sum_points,
            tasks_cnt,
            sealed: y.sealed,
            point_pad: y.point_pad,
            active_orgs,
        });
    }
    Ok(Json(YearsResponse { years: out }))
}

#[derive(Serialize)]
pub struct YearResponse {
    year: YearJson,
}

pub async fn get_year(
    State(state): State<SharedState>,
    _user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<YearResponse>> {
    let year: Option<Year> =
        sqlx::query_as("SELECT id, year, sealed, CAST(point_pad AS REAL) AS point_pad FROM years WHERE id = ?")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;

    match year {
        None => Err(ApiError::NotFound("Ročník neexistuje.".into())),
        Some(y) => {
            let (sum_points, tasks_cnt) = year_points(&state.db, y.id).await?;
            let active_orgs: Vec<i64> =
                sqlx::query_scalar("SELECT org FROM active_orgs WHERE year = ?")
                    .bind(y.id)
                    .fetch_all(&state.db)
                    .await?;
            Ok(Json(YearResponse {
                year: YearJson {
                    id: y.id,
                    index: y.id,
                    year: y.year,
                    sum_points,
                    tasks_cnt,
                    sealed: y.sealed,
                    point_pad: y.point_pad,
                    active_orgs,
                },
            }))
        }
    }
}
