//! Diploma endpoints.
//!
//! * `GET /diplomas/:user_id` — list a user's diplomas (public, like Python).
//! * `GET /diplomas/:user_id/:year_id/show` — download the diploma PDF
//!   (public, like Python). Python streams the stored file with its
//!   libmagic-detected MIME type; here the MIME is derived from the PDF
//!   magic number (`%PDF`), which is the only type the grant endpoint
//!   accepts anyway.
//! * `POST /admin/diploma/:user_id/grant` — upload (grant) a diploma PDF
//!   (admin only). The year comes from the `year` request header, mirroring
//!   the Python year middleware that populated `req.context['year_obj']`.
//!
//! Storage layout mirrors Python: `data/diplomas/year_<year_id>/user_<user_id>.pdf`
//! (overridable with the `DIPLOMA_DIR` env var, e.g. in tests).
//! Returns **204 No Content** on success (per the swagger spec).

use std::path::PathBuf;

use axum::{
    extract::{Multipart, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use ksi_auth::AuthUser;
use ksi_models::DiplomaJson;
use crate::{
    error::{ApiError, ApiResult},
    SharedState,
};

/// Base directory for stored diploma PDFs.
fn diploma_dir() -> PathBuf {
    std::env::var("DIPLOMA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data/diplomas"))
}

/// Mirrors `endpoint/admin/diploma.py::get_diploma_path`.
fn diploma_path(year_id: i64, user_id: i64) -> PathBuf {
    diploma_dir()
        .join(format!("year_{year_id}"))
        .join(format!("user_{user_id}.pdf"))
}

/// A minimal PDF magic check (`%PDF-`), standing in for Python's
/// `magic.Magic(mime=True)`. The grant endpoint only accepts PDFs anyway.
fn is_pdf(data: &[u8]) -> bool {
    data.len() > 5 && &data[..5] == b"%PDF-"
}

/// `GET /diplomas/:user_id`
pub async fn list_diplomas(
    State(state): State<SharedState>,
    Path(user_id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let rows: Vec<(i64, bool)> = sqlx::query_as(
        "SELECT year_id, revoked FROM diplomas WHERE user_id = ? ORDER BY year_id",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await?;

    let diplomas: Vec<DiplomaJson> = rows
        .into_iter()
        .map(|(year_id, revoked)| DiplomaJson {
            year: year_id,
            revoked,
            // Relative URL; the Python backend prefixes backend_url(), which
            // this port does not configure.
            url: format!("/diplomas/{user_id}/{year_id}/show"),
        })
        .collect();

    Ok(Json(json!({ "diplomas": diplomas })))
}

/// `GET /diplomas/:user_id/:year_id/show`
pub async fn show_diploma(
    State(state): State<SharedState>,
    Path((user_id, year_id)): Path<(i64, i64)>,
) -> ApiResult<Response> {
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT user_id FROM diplomas WHERE user_id = ? AND year_id = ?",
    )
    .bind(user_id)
    .bind(year_id)
    .fetch_optional(&state.db)
    .await?;

    if exists.is_none() {
        return Err(ApiError::NotFound(
            "No diploma for this user & year combination found".into(),
        ));
    }

    let path = diploma_path(year_id, user_id);
    let data = std::fs::read(&path).map_err(|_| {
        ApiError::NotFound(format!(
            "Soubor s diplomem nebyl nalezen: {}",
            path.display()
        ))
    })?;

    let mime = if is_pdf(&data) {
        "application/pdf"
    } else {
        "application/octet-stream"
    };

    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (
                header::CONTENT_DISPOSITION,
                &format!("inline; filename=\"diplom_{user_id}_{year_id}.pdf\""),
            ),
        ],
        data,
    )
        .into_response())
}

/// `POST /admin/diploma/:user_id/grant`
///
/// `multipart/form-data` with a single PDF file (field name `files`, per the
/// swagger spec). Returns 204 on success.
pub async fn grant_diploma(
    State(state): State<SharedState>,
    user: AuthUser,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    mut multipart: Multipart,
) -> ApiResult<Response> {
    if !user.is_logged_in() || !user.is_admin() {
        return Err(ApiError::Unauthorized(
            "Nahravat diplomy muze pouze admin.".into(),
        ));
    }

    // Year comes from the `year` header (Python populated `year_obj` via
    // middleware; the swagger spec declares the same header parameter).
    let year_id: i64 = headers
        .get("year")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| ApiError::BadRequest("Chybějící nebo neplatná hlavička 'year'.".into()))?;

    let sealed: Option<bool> =
        sqlx::query_scalar("SELECT sealed FROM years WHERE id = ?")
            .bind(year_id)
            .fetch_optional(&state.db)
            .await?;
    match sealed {
        Some(true) => {
            return Err(ApiError::Forbidden("Ročník zapečetěn.".into()));
        }
        None => {
            return Err(ApiError::BadRequest("Neznámý ročník.".into()));
        }
        Some(false) => {}
    }

    // Take the first uploaded file, like Python does.
    let data = loop {
        match multipart.next_field().await {
            Ok(Some(field)) => match field.bytes().await {
                Ok(bytes) => break bytes.to_vec(),
                Err(e) => {
                    return Err(ApiError::BadRequest(format!(
                        "Chyba při čtení nahrávaného souboru: {e}"
                    )));
                }
            },
            Ok(None) => {
                return Err(ApiError::BadRequest(
                    "No file found in the data".into(),
                ));
            }
            Err(e) => {
                return Err(ApiError::BadRequest(format!(
                    "Chyba při zpracování multipart dat: {e}"
                )));
            }
        }
    };

    if !is_pdf(&data) {
        return Err(ApiError::BadRequest(
            "Allowed file types are ('application/pdf',)".into(),
        ));
    }

    let target = diploma_path(year_id, user_id);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            ApiError::Internal(format!("Nelze vytvořit adresář pro diplomy: {e}"))
        })?;
    }
    std::fs::write(&target, &data).map_err(|e| {
        ApiError::Internal(format!("Nelze uložit diplom: {e}"))
    })?;

    // Python relied on the SQLAlchemy column default for `revoked`; supply it
    // explicitly here.
    sqlx::query("INSERT INTO diplomas (user_id, year_id, revoked) VALUES (?, ?, 0)")
        .bind(user_id)
        .bind(year_id)
        .execute(&state.db)
        .await?;

    Ok(StatusCode::NO_CONTENT.into_response())
}
