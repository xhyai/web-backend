use std::env;

/// Runtime configuration, read from environment variables.
#[derive(Debug, Clone)]
pub struct Config {
    /// Database URL, e.g. `sqlite://ksi.db` or `sqlite::memory:`
    pub database_url: String,
    /// Listen address, e.g. `127.0.0.1:3030`
    pub listen_addr: String,
    /// Value for the `Access-Control-Allow-Origin` header (optional)
    pub cors_origin: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            database_url: env::var("DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://ksi.db".to_string()),
            listen_addr: env::var("LISTEN_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:3030".to_string()),
            cors_origin: env::var("CORS_ORIGIN").ok(),
        }
    }
}
