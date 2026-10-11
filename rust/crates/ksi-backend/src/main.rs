use std::sync::Arc;

use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use ksi_backend::{config::Config, create_app, db, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config = Config::from_env();
    let pool = db::create_pool(&config.database_url).await?;
    let state = Arc::new(AppState { db: pool, config: config.clone() });
    let app = create_app(state).await;

    let listener = TcpListener::bind(&config.listen_addr).await?;
    tracing::info!("listening on {}", config.listen_addr);
    axum::serve(listener, app).await?;
    Ok(())
}
