use axum::{extract::{Path, State}, Json};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use ksi_auth::AuthUser;
use crate::{error::{ApiError, ApiResult}, routes::years::wave_points, SharedState};
use ksi_models::{parse_datetime, Wave, WaveJson};

/// `{"wave": {...}}` body for `POST /waves` (swagger `WaveCreate`, includes
/// `year`) and `PUT /waves/{id}` (swagger `WaveBase`, no `year`).
#[derive(Debug, Deserialize)]
pub struct WaveInput {
    pub index: i64,
    pub caption: Option<String>,
    pub garant: i64,
    pub time_published: Option<String>,
    pub year: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct WaveBody {
    pub wave: WaveInput,
}

#[derive(Serialize)]
pub struct WavesResponse {
    waves: Vec<WaveJson>,
}

fn wave_to_json(w: &Wave, sum_points: f64, tasks_cnt: i64) -> WaveJson {
    let now = Utc::now().naive_utc();
    let public = w.time_published.map(|t| t <= now).unwrap_or(false);
    WaveJson {
        id: w.id,
        index: w.index,
        year: w.year,
        caption: w.caption.clone(),
        public,
        time_published: w.time_published.map(|t| t.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()),
        sum_points,
        tasks_cnt,
        garant: w.garant,
    }
}

pub async fn list_waves(
    State(state): State<SharedState>,
    _user: AuthUser,
) -> ApiResult<Json<WavesResponse>> {
    let waves: Vec<Wave> = sqlx::query_as(
        "SELECT id, year, \"index\", caption, garant, time_published FROM waves ORDER BY id",
    )
    .fetch_all(&state.db)
    .await?;

    let mut out = Vec::with_capacity(waves.len());
    for w in &waves {
        let (sum_points, tasks_cnt) = wave_points(&state.db, w.id).await?;
        out.push(wave_to_json(w, sum_points, tasks_cnt));
    }
    Ok(Json(WavesResponse { waves: out }))
}

#[derive(Serialize)]
pub struct WaveResponse {
    wave: WaveJson,
}

pub async fn get_wave(
    State(state): State<SharedState>,
    _user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<WaveResponse>> {
    wave_response(&state, id).await
}

async fn wave_response(state: &SharedState, id: i64) -> ApiResult<Json<WaveResponse>> {
    let wave: Option<Wave> = sqlx::query_as(
        "SELECT id, year, \"index\", caption, garant, time_published FROM waves WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;

    match wave {
        None => Err(ApiError::NotFound("Vlna neexistuje.".into())),
        Some(w) => {
            let (sum_points, tasks_cnt) = wave_points(&state.db, w.id).await?;
            Ok(Json(WaveResponse { wave: wave_to_json(&w, sum_points, tasks_cnt) }))
        }
    }
}

/// `POST /waves` — create a wave. Admin only: despite the looser wording in
/// some docs, the Python reference (`endpoint/wave.py`) restricts creation
/// to admins (non-admins get 400).
pub async fn create_wave(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(body): Json<WaveBody>,
) -> ApiResult<Json<WaveResponse>> {
    if !user.is_logged_in() || !user.is_admin() {
        return Err(ApiError::BadRequest(
            "Vytvořit vlnu může pouze administrátor.".into(),
        ));
    }

    let input = body.wave;
    let year_id = input
        .year
        .ok_or_else(|| ApiError::BadRequest("Chybí rok vlny.".into()))?;
    let year_exists: Option<i64> = sqlx::query_scalar("SELECT id FROM years WHERE id = ?")
        .bind(year_id)
        .fetch_optional(&state.db)
        .await?;
    if year_exists.is_none() {
        return Err(ApiError::BadRequest("Rok neexistuje.".into()));
    }

    // Missing time_published falls back to now (the column default), the
    // way the Python model declares `default=datetime.datetime.utcnow`.
    let time_published = match input.time_published {
        Some(s) => parse_datetime(&s).ok_or_else(|| {
            ApiError::BadRequest(format!("Neplatný čas zveřejnění: {s}"))
        })?,
        None => Utc::now().naive_utc(),
    };

    let res = sqlx::query(
        "INSERT INTO waves (year, \"index\", caption, garant, time_published)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(year_id)
    .bind(input.index)
    .bind(input.caption)
    .bind(input.garant)
    .bind(time_published)
    .execute(&state.db)
    .await?;
    let wave_id = res.last_insert_rowid();

    wave_response(&state, wave_id).await
}

/// `PUT /waves/{id}` — update a wave. The caller must be logged in and an
/// org; the wave itself may then only be changed by an admin or by its
/// current garant (both violations answer 400, like Python).
pub async fn update_wave(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<WaveBody>,
) -> ApiResult<Json<WaveResponse>> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest(
            "Upravit vlnu může pouze organizátor.".into(),
        ));
    }

    let wave: Option<Wave> = sqlx::query_as(
        "SELECT id, year, \"index\", caption, garant, time_published FROM waves WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    let wave = match wave {
        None => return Err(ApiError::NotFound("Vlna neexistuje.".into())),
        Some(w) => w,
    };

    if !user.is_admin() && user.id != Some(wave.garant) {
        return Err(ApiError::BadRequest(
            "Vlnu může měnit pouze administrátor nebo její garant.".into(),
        ));
    }

    let input = body.wave;
    // Python only overwrites time_published when the new value is truthy.
    let time_published = match input.time_published.as_deref() {
        Some(s) if !s.trim().is_empty() => Some(parse_datetime(s).ok_or_else(|| {
            ApiError::BadRequest(format!("Neplatný čas zveřejnění: {s}"))
        })?),
        _ => None,
    };

    if let Some(tp) = time_published {
        sqlx::query(
            "UPDATE waves SET \"index\" = ?, caption = ?, garant = ?, time_published = ?
             WHERE id = ?",
        )
        .bind(input.index)
        .bind(input.caption)
        .bind(input.garant)
        .bind(tp)
        .bind(id)
        .execute(&state.db)
        .await?;
    } else {
        sqlx::query("UPDATE waves SET \"index\" = ?, caption = ?, garant = ? WHERE id = ?")
            .bind(input.index)
            .bind(input.caption)
            .bind(input.garant)
            .bind(id)
            .execute(&state.db)
            .await?;
    }

    wave_response(&state, id).await
}

/// `DELETE /waves/{id}` — delete a wave. Only an empty wave (no tasks) may
/// be deleted, otherwise 403. Admin only.
pub async fn delete_wave(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    if !user.is_logged_in() || !user.is_admin() {
        return Err(ApiError::BadRequest(
            "Smazat vlnu může pouze administrátor.".into(),
        ));
    }

    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM waves WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound("Vlna neexistuje.".into()));
    }

    let tasks_cnt: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE wave = ?")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    if tasks_cnt > 0 {
        return Err(ApiError::Forbidden("Smazat lze pouze prázdnou vlnu.".into()));
    }

    sqlx::query("DELETE FROM waves WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    Ok(Json(serde_json::json!({})))
}
