use axum::{
    async_trait,
    extract::FromRequestParts,
    http::{header, request::Parts, StatusCode},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::{error::ApiError, SharedState};

/// Mirrors Python's `util.auth.UserInfo`.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: Option<i64>,
    pub role: Option<String>,
    pub token: Option<String>,
}

impl AuthUser {
    pub fn anonymous() -> Self {
        Self { id: None, role: None, token: None }
    }

    pub fn is_logged_in(&self) -> bool {
        self.id.is_some()
    }

    pub fn is_admin(&self) -> bool {
        self.role.as_deref() == Some("admin")
    }

    /// Python `is_org()`: role is `org` or `admin`.
    pub fn is_org(&self) -> bool {
        matches!(self.role.as_deref(), Some("org") | Some("admin"))
    }

    pub fn is_tester(&self) -> bool {
        self.role.as_deref() == Some("tester")
    }
}

#[derive(Debug, sqlx::FromRow)]
struct TokenRow {
    access_token: String,
    user: Option<i64>,
    expire: Option<chrono::NaiveDateTime>,
}

#[derive(Debug, sqlx::FromRow)]
struct UserRow {
    id: i64,
    role: String,
}

async fn lookup_token(pool: &SqlitePool, token: &str) -> Option<AuthUser> {
    let row: Option<TokenRow> = sqlx::query_as(
        "SELECT access_token, user, expire FROM oauth2_tokens WHERE access_token = ?",
    )
    .bind(token)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let row = row?;

    // Expired tokens are treated as anonymous.
    if let Some(expire) = row.expire {
        let expire_utc: DateTime<Utc> =
            DateTime::from_naive_utc_and_offset(expire, Utc);
        if expire_utc < Utc::now() {
            return None;
        }
    }

    let user_id = row.user?;
    let user: Option<UserRow> =
        sqlx::query_as("SELECT id, role FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();

    user.map(|u| AuthUser {
        id: Some(u.id),
        role: Some(u.role),
        token: Some(row.access_token),
    })
}

#[async_trait]
impl FromRequestParts<SharedState> for AuthUser {
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &SharedState,
    ) -> Result<Self, Self::Rejection> {
        let auth = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        // `Bearer <token>` — Python takes the last whitespace-separated part.
        let token = auth.split_whitespace().last().unwrap_or("");
        if token.is_empty() {
            return Ok(AuthUser::anonymous());
        }

        Ok(lookup_token(&state.db, token)
            .await
            .unwrap_or_else(AuthUser::anonymous))
    }
}

/// Request/response shapes for `POST /auth` (OAuth2 password grant).
#[derive(Debug, Deserialize)]
pub struct AuthRequest {
    pub grant_type: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_in: i64,
}
