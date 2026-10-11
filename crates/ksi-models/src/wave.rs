use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

/// Row from the `waves` table.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Wave {
    pub id: i64,
    pub year: i64,
    pub index: i64,
    pub caption: Option<String>,
    pub garant: i64,
    pub time_published: Option<NaiveDateTime>,
}

/// Wave JSON (mirrors `util/wave.py::to_json`).
#[derive(Debug, Serialize, Deserialize)]
pub struct WaveJson {
    pub id: i64,
    pub index: i64,
    pub year: i64,
    pub caption: Option<String>,
    pub public: bool,
    pub time_published: Option<String>,
    pub sum_points: f64,
    pub tasks_cnt: i64,
    #[serde(rename = "garant")]
    pub garant: i64,
}
