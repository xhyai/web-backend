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

/// Full module detail returned by `GET /modules/{id}`
/// (mirrors `util/module.py::to_json`).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ModuleDetailJson {
    pub id: i64,
    #[serde(rename = "type")]
    pub module_type: String,
    pub name: String,
    pub description: Option<String>,
    pub autocorrect: bool,
    pub max_score: Option<f64>,
    /// `correct` | `incorrect` | `blank`
    pub state: String,
    /// Module id when the task's evaluation is public, else `None`.
    pub score: Option<f64>,
    // --- programming extras ---
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub default_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub edulint_source_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_origin: Option<String>,
    // --- quiz extras ---
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub questions: Option<Vec<serde_json::Value>>,
    // --- sortable extras ---
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub sortable_list: Option<serde_json::Value>,
    // --- general (file upload) extras ---
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub submitted_files: Option<Vec<SubmittedFileJson>>,
    // --- text extras ---
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub fields: Option<Vec<String>>,
}

/// One uploaded file listed on a general module.
#[derive(Debug, Serialize, Deserialize)]
pub struct SubmittedFileJson {
    pub id: i64,
    pub filename: String,
}

/// Row from the `submitted_files` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SubmittedFile {
    pub id: i64,
    pub evaluation: i64,
    pub mime: Option<String>,
    pub path: String,
}

/// Row from the `modules_custom` table (per-user generated assignments).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ModuleCustom {
    pub module: i64,
    pub user: i64,
    pub description: Option<String>,
    pub description_replace: Option<String>,
    pub data: Option<String>,
    pub error: Option<String>,
}
