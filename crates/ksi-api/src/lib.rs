pub mod auth;
pub mod config;
pub mod error;
pub mod routes;

use std::sync::Arc;

use axum::{
    async_trait,
    extract::FromRequestParts,
    http::{header, request::Parts, StatusCode},
    Router,
};

pub use ksi_auth::AuthUser;

pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub config: config::Config,
}

pub type SharedState = Arc<AppState>;

/// Build the axum router. Used by both `main.rs` and integration tests.
pub async fn create_app(state: SharedState) -> Router {
    let cors_origin = state.config.cors_origin.clone();
    let mut router = Router::new()
        .merge(routes::router())
        .with_state(state);

    if let Some(origin) = cors_origin {
        use axum::http::HeaderValue;
        use tower_http::cors::{Any, CorsLayer};
        let layer = CorsLayer::new()
            .allow_origin(origin.parse::<HeaderValue>().unwrap())
            .allow_headers(Any)
            .allow_methods(Any);
        router = router.layer(layer);
    }

    router
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

        Ok(ksi_auth::lookup_token(&state.db, token)
            .await
            .unwrap_or_else(AuthUser::anonymous))
    }
}
