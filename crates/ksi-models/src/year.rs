use serde::{Deserialize, Serialize};

/// Row from the `years` table.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Year {
    pub id: i64,
    pub year: Option<String>,
    pub sealed: bool,
    pub point_pad: Option<f64>,
}

/// Year JSON (mirrors `util/year.py::to_json`).
#[derive(Debug, Serialize, Deserialize)]
pub struct YearJson {
    pub id: i64,
    pub index: i64,
    pub year: Option<String>,
    pub sum_points: f64,
    pub tasks_cnt: i64,
    pub sealed: bool,
    pub point_pad: Option<f64>,
    pub active_orgs: Vec<i64>,
}
