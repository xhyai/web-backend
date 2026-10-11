use serde::{Deserialize, Serialize};

/// Row from the `diplomas` table (mirrors `model/diploma.py`).
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Diploma {
    pub user_id: i64,
    pub year_id: i64,
    pub revoked: bool,
}

/// Public diploma JSON (mirrors `endpoint/diploma.py`).
///
/// Note: `url` is a *relative* URL in this port. The Python backend builds an
/// absolute URL from `util.config.backend_url()`; this port has no such
/// config, so the frontend should prefix the host itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiplomaJson {
    pub year: i64,
    pub revoked: bool,
    pub url: String,
}
