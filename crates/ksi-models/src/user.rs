use serde::{Deserialize, Serialize};

/// Row from the `users` table.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub first_name: String,
    pub last_name: String,
    pub nick_name: Option<String>,
    pub sex: String,
    pub role: String,
    pub enabled: bool,
}

/// Public user JSON (mirrors `util/user.py`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserJson {
    pub id: i64,
    pub email: String,
    pub first_name: String,
    pub last_name: String,
    pub nick_name: Option<String>,
    pub role: String,
}
