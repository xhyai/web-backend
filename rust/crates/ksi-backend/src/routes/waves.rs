use axum::{extract::{Path, State}, Json};
use chrono::Utc;
use serde::Serialize;

use crate::{
    auth::AuthUser,
    error::{ApiError, ApiResult},
    models::{Wave, WaveJson},
    routes::years::wave_points,
    SharedState,
};

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
