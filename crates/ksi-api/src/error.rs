use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

/// A single JSON:API-style error object, matching the Python backend's
/// `{"errors": [{"status", "title", "detail"}]}` envelope.
#[derive(Debug, Serialize)]
pub struct ErrorObject {
    pub status: String,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    errors: Vec<ErrorObject>,
}

/// Application error type. Every variant maps to an HTTP status code and
/// serializes into the `{"errors": [...]}` envelope the frontend expects.
#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    Unauthorized(String),
    Forbidden(String),
    NotFound(String),
    PayloadTooLarge(String),
    Internal(String),
}

impl ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden(_) => StatusCode::FORBIDDEN,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn title(&self) -> &'static str {
        match self {
            ApiError::BadRequest(_) => "Bad Request",
            ApiError::Unauthorized(_) => "Unauthorized",
            ApiError::Forbidden(_) => "Forbidden",
            ApiError::NotFound(_) => "Not Found",
            ApiError::PayloadTooLarge(_) => "Payload Too Large",
            ApiError::Internal(_) => "Internal Server Error",
        }
    }

    fn detail(&self) -> &str {
        match self {
            ApiError::BadRequest(d)
            | ApiError::Unauthorized(d)
            | ApiError::Forbidden(d)
            | ApiError::NotFound(d)
            | ApiError::PayloadTooLarge(d)
            | ApiError::Internal(d) => d,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = ErrorEnvelope {
            errors: vec![ErrorObject {
                status: status.as_u16().to_string(),
                title: self.title().to_string(),
                detail: self.detail().to_string(),
            }],
        };
        (status, Json(body)).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!("database error: {e}");
        ApiError::Internal("Vnitřní chyba serveru, kontaktujte správce backendu.".into())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError::BadRequest(format!("Neplatný JSON: {e}"))
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
