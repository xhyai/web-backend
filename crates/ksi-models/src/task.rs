use serde::{Deserialize, Serialize};

/// Row from the `tasks` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub author: Option<i64>,
    pub co_author: Option<i64>,
    pub wave: i64,
    pub prerequisite: Option<i64>,
    pub intro: String,
    pub body: String,
    pub solution: Option<String>,
    pub thread: i64,
    pub picture_base: Option<String>,
    pub evaluation_public: bool,
}

/// Task JSON (mirrors `util/task.py::to_json`).
#[derive(Debug, Serialize, Deserialize)]
pub struct TaskJson {
    pub id: i64,
    pub title: String,
    pub author: Option<i64>,
    pub co_author: Option<i64>,
    pub details: i64,
    pub intro: String,
    pub max_score: f64,
    pub time_published: Option<String>,
    pub time_deadline: Option<String>,
    pub state: String,
    pub prerequisities: Vec<serde_json::Value>,
    pub picture_base: String,
    pub picture_suffix: String,
    pub wave: i64,
    pub feedbacks: i64,
}
