use serde::{Deserialize, Serialize};

/// Row from the `modules` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Module {
    pub id: i64,
    pub task: i64,
    #[sqlx(rename = "type")]
    pub module_type: String,
    pub name: String,
    pub description: Option<String>,
    pub max_points: Option<f64>,
    pub autocorrect: bool,
    pub order: i64,
    pub bonus: bool,
}

/// Module JSON (mirrors `util/module.py::to_json`).
#[derive(Debug, Serialize, Deserialize)]
pub struct ModuleJson {
    pub id: i64,
    pub task: i64,
    #[serde(rename = "type")]
    pub module_type: String,
    pub name: String,
    pub description: Option<String>,
    pub max_points: Option<f64>,
    pub autocorrect: bool,
    pub order: i64,
    pub bonus: bool,
}
