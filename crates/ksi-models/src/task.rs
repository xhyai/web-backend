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

/// Admin task JSON (mirrors `util/admin/task.py::admin_to_json`).
///
/// `testers` / `additional_testers` are only populated when the caller asks
/// for them (`?fetch_testers=true`); the Rust port does not call the GitHub
/// API, so they stay empty.
#[derive(Debug, Serialize, Deserialize)]
pub struct AdminTaskJson {
    pub id: i64,
    pub title: String,
    pub wave: i64,
    pub author: Option<i64>,
    pub co_author: Option<i64>,
    pub testers: Vec<i64>,
    pub git_path: Option<String>,
    pub git_branch: Option<String>,
    pub git_commit: Option<String>,
    pub git_pull_id: Option<i64>,
    pub deploy_date: Option<String>,
    pub deploy_status: String,
    pub max_score: f64,
    pub eval_comment: Option<String>,
    pub additional_testers: Vec<String>,
}
