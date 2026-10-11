use chrono::NaiveDateTime;

/// Row from the `evaluations` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Evaluation {
    pub id: i64,
    pub user: i64,
    pub module: i64,
    pub evaluator: Option<i64>,
    pub points: Option<f64>,
    pub ok: bool,
    pub cheat: bool,
    pub full_report: String,
    pub time: Option<NaiveDateTime>,
}
