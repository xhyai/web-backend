pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod models;
pub mod routes;

use std::sync::Arc;

use axum::Router;

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
