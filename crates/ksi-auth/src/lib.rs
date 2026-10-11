//! Authentication primitives: token lookup, password hashing, auth JSON shapes.
//!
//! This crate is intentionally web-framework free — the axum extractor that
//! turns `Authorization: Bearer …` into an [`AuthUser`] lives in `ksi-api`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

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

/// Look up an opaque bearer token in `oauth2_tokens`.
/// Expired or unknown tokens yield `None` (caller treats them as anonymous).
pub async fn lookup_token(pool: &SqlitePool, token: &str) -> Option<AuthUser> {
    let row: TokenRow = sqlx::query_as(
        "SELECT access_token, user, expire FROM oauth2_tokens WHERE access_token = ?",
    )
    .bind(token)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;

    // Expired tokens are treated as anonymous.
    if let Some(expire) = row.expire {
        let expire_utc: DateTime<Utc> = DateTime::from_naive_utc_and_offset(expire, Utc);
        if expire_utc < Utc::now() {
            return None;
        }
    }

    let user_id = row.user?;
    let user: UserRow = sqlx::query_as("SELECT id, role FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()?;

    Some(AuthUser {
        id: Some(user.id),
        role: Some(user.role),
        token: Some(row.access_token),
    })
}

/// Verify a plaintext password against a stored bcrypt hash.
/// Unknown hash formats fail closed (return `false`).
pub fn verify_password(password: &str, hash: &str) -> bool {
    bcrypt::verify(password, hash).unwrap_or(false)
}

/// Create a fresh opaque access token.
pub fn new_token() -> String {
    uuid::Uuid::new_v4().to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles() {
        let org = AuthUser { id: Some(1), role: Some("org".into()), token: None };
        assert!(org.is_org() && !org.is_admin() && org.is_logged_in());
        let admin = AuthUser { id: Some(1), role: Some("admin".into()), token: None };
        assert!(admin.is_org() && admin.is_admin());
        let part = AuthUser { id: Some(2), role: Some("participant".into()), token: None };
        assert!(!part.is_org() && part.is_logged_in());
        assert!(!AuthUser::anonymous().is_logged_in());
    }

    #[test]
    fn new_token_looks_like_uuid() {
        let t = new_token();
        assert_eq!(t.len(), 36);
        assert!(uuid::Uuid::parse_str(&t).is_ok());
    }

    #[test]
    fn bad_hash_fails_closed() {
        assert!(!verify_password("x", "not-a-bcrypt-hash"));
    }
}
