use axum::{extract::{Path, State}, Json};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use ksi_auth::AuthUser;
use crate::{error::{ApiError, ApiResult}, SharedState};
use ksi_models::{Year, YearJson};

/// Accepts a plain JSON number or a numeric string: the admin frontend
/// sends `active_orgs` as an array of strings (e.g. `["1", "2"]`) while the
/// swagger spec declares numbers.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum NumOrStr {
    Num(i64),
    Str(String),
}

impl NumOrStr {
    fn as_i64(&self) -> ApiResult<i64> {
        match self {
            NumOrStr::Num(n) => Ok(*n),
            NumOrStr::Str(s) => s.trim().parse::<i64>().map_err(|_| {
                ApiError::BadRequest(format!("Neplatné ID: {s}"))
            }),
        }
    }
}

/// `{"year": {...}}` body for `POST /years` and `PUT /years/{id}`.
///
/// Accepts both `index` (what the Python backend reads and the admin
/// frontend sends) and `id` (what the swagger spec calls the field).
#[derive(Debug, Deserialize)]
pub struct YearInput {
    pub index: Option<i64>,
    pub id: Option<i64>,
    pub year: Option<String>,
    pub sealed: Option<bool>,
    pub point_pad: Option<f64>,
    pub active_orgs: Option<Vec<NumOrStr>>,
}

impl YearInput {
    fn resolve_id(&self) -> ApiResult<i64> {
        self.index.or(self.id).ok_or_else(|| {
            ApiError::BadRequest("Chybí index ročníku.".into())
        })
    }

    fn active_org_ids(&self) -> ApiResult<Vec<i64>> {
        self.active_orgs
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(NumOrStr::as_i64)
            .collect()
    }
}

#[derive(Debug, Deserialize)]
pub struct YearBody {
    pub year: YearInput,
}

/// Sum of max_points and task count for a year, considering only waves
/// whose `time_published` is not in the future (unreleased tasks must not
/// leak through `GET /years`).
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
    year_response(&state, id).await
}

async fn year_response(state: &SharedState, id: i64) -> ApiResult<Json<YearResponse>> {
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

/// `POST /years` — create a year. Admin only (like Python, a non-admin gets
/// 400 rather than 401/403).
pub async fn create_year(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(body): Json<YearBody>,
) -> ApiResult<Json<YearResponse>> {
    if !user.is_logged_in() || !user.is_admin() {
        return Err(ApiError::BadRequest(
            "Vytvořit ročník může pouze administrátor.".into(),
        ));
    }

    let input = body.year;
    let id = input.resolve_id()?;
    let sealed = input.sealed.unwrap_or(false);
    let point_pad = input.point_pad.unwrap_or(0.0);
    let org_ids = input.active_org_ids()?;

    sqlx::query("INSERT INTO years (id, year, sealed, point_pad) VALUES (?, ?, ?, ?)")
        .bind(id)
        .bind(input.year)
        .bind(sealed)
        .bind(point_pad)
        .execute(&state.db)
        .await?;

    // Python commits the year first and only then inserts the active orgs.
    for org_id in org_ids {
        sqlx::query("INSERT INTO active_orgs (org, year) VALUES (?, ?)")
            .bind(org_id)
            .bind(id)
            .execute(&state.db)
            .await?;
    }

    year_response(&state, id).await
}

/// `PUT /years/{id}` — update a year, including its primary key (`index`)
/// and the active-org roster. Admin only.
pub async fn update_year(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<YearBody>,
) -> ApiResult<Json<YearResponse>> {
    if !user.is_logged_in() || !user.is_admin() {
        return Err(ApiError::BadRequest(
            "Upravit ročník může pouze administrátor.".into(),
        ));
    }

    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM years WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound("Ročník neexistuje.".into()));
    }

    let input = body.year;
    let new_id = input.resolve_id()?;
    let sealed = input.sealed.unwrap_or(false);
    let point_pad = input.point_pad.unwrap_or(0.0);
    let org_ids = input.active_org_ids()?;

    // One transaction, mirroring Python's single commit at the end of on_put.
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE years SET id = ?, year = ?, sealed = ?, point_pad = ? WHERE id = ?")
        .bind(new_id)
        .bind(input.year)
        .bind(sealed)
        .bind(point_pad)
        .bind(id)
        .execute(&mut *tx)
        .await?;

    // Sync the active-org roster against the (possibly new) year id. Python
    // deletes orgs missing from the new list and inserts the new ones;
    // DELETE + INSERT converges to the same final set.
    sqlx::query("DELETE FROM active_orgs WHERE year = ?")
        .bind(new_id)
        .execute(&mut *tx)
        .await?;
    for org_id in org_ids {
        sqlx::query("INSERT INTO active_orgs (org, year) VALUES (?, ?)")
            .bind(org_id)
            .bind(new_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;

    year_response(&state, new_id).await
}

/// `DELETE /years/{id}` — delete a year. Only an empty year (no waves) may
/// be deleted, otherwise 403. Admin only.
pub async fn delete_year(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    if !user.is_logged_in() || !user.is_admin() {
        return Err(ApiError::BadRequest(
            "Smazat ročník může pouze administrátor.".into(),
        ));
    }

    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM years WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound("Ročník neexistuje.".into()));
    }

    let waves_cnt: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM waves WHERE year = ?")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    if waves_cnt > 0 {
        return Err(ApiError::Forbidden("Smazat lze pouze prázdný ročník.".into()));
    }

    sqlx::query("DELETE FROM years WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(Json(serde_json::json!({})))
}
