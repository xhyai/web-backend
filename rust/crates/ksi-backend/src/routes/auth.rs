use axum::{extract::State, Json};
use chrono::{Duration, Utc};

use crate::{
    auth::{AuthRequest, AuthResponse, AuthUser},
    error::{ApiError, ApiResult},
    SharedState,
};

fn new_token() -> String {
    uuid::Uuid::new_v4().simple().to_string() + &uuid::Uuid::new_v4().simple().to_string()[..8]
}

/// `POST /auth` — OAuth2 password grant (mirrors `endpoint/oauth2.py`).
pub async fn authorize(
    State(state): State<SharedState>,
    _user: AuthUser,
    Json(req): Json<AuthRequest>,
) -> ApiResult<Json<AuthResponse>> {
    match req.grant_type.as_deref() {
        Some("password") => {
            let username = req.username.unwrap_or_default();
            let password = req.password.unwrap_or_default();

            let row: Option<(i64, String)> =
                sqlx::query_as("SELECT id, password FROM users WHERE email = ?")
                    .bind(&username)
                    .fetch_optional(&state.db)
                    .await?;

            let (user_id, hash) = match row {
                Some(r) => r,
                None => {
                    return Err(ApiError::Unauthorized(
                        "Špatné jméno nebo heslo.".into(),
                    ))
                }
            };

            if !bcrypt::verify(&password, &hash).unwrap_or(false) {
                return Err(ApiError::Unauthorized("Špatné jméno nebo heslo.".into()));
            }

            let access = new_token();
            let refresh = new_token();
            let expire = Utc::now() + Duration::hours(1);

            sqlx::query(
                "INSERT INTO oauth2_tokens (access_token, user, expire, refresh_token, granted)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&access)
            .bind(user_id)
            .bind(expire.naive_utc())
            .bind(&refresh)
            .bind(Utc::now().naive_utc())
            .execute(&state.db)
            .await?;

            // update last login
            sqlx::query("UPDATE users SET last_logged_in = ? WHERE id = ?")
                .bind(Utc::now().naive_utc())
                .bind(user_id)
                .execute(&state.db)
                .await?;

            Ok(Json(AuthResponse {
                access_token: access,
                refresh_token: refresh,
                token_type: "Bearer".into(),
                expires_in: 3599,
            }))
        }
        Some("refresh_token") => Err(ApiError::BadRequest(
            "Obnovení tokenu zatím není podporováno.".into(),
        )),
        _ => Err(ApiError::BadRequest("Neznámý grant_type.".into())),
    }
}

/// `GET /logout` — invalidate the current token.
pub async fn logout(
    State(state): State<SharedState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    if let Some(token) = user.token {
        sqlx::query("DELETE FROM oauth2_tokens WHERE access_token = ?")
            .bind(token)
            .execute(&state.db)
            .await?;
    }
    Ok(Json(serde_json::json!({})))
}
