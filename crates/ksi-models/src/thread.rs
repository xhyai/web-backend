use serde::{Deserialize, Serialize};

/// Row from the `threads` table.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Thread {
    pub id: i64,
    pub title: Option<String>,
    pub public: bool,
    pub year: i64,
}

/// Thread JSON (mirrors `util/thread.py`).
#[derive(Debug, Serialize, Deserialize)]
pub struct ThreadJson {
    pub id: i64,
    pub title: Option<String>,
    pub public: bool,
    pub year: i64,
}
