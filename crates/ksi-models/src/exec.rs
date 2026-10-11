use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

/// Row from the `code_executions` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CodeExecution {
    pub id: i64,
    pub module: i64,
    pub user: i64,
    pub code: Option<String>,
    pub result: Option<String>,
    pub time: Option<NaiveDateTime>,
    pub report: Option<String>,
}

/// Code-execution JSON (mirrors `util/programming.py::exec_to_json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeExecutionJson {
    pub id: i64,
    pub module: i64,
    pub user: i64,
    pub code: Option<String>,
    pub result: Option<String>,
    /// Python serializes `str(time)`; keep the same plain format.
    pub time: Option<String>,
    pub report: Option<String>,
}

impl From<&CodeExecution> for CodeExecutionJson {
    fn from(ex: &CodeExecution) -> Self {
        Self {
            id: ex.id,
            module: ex.module,
            user: ex.user,
            code: ex.code.clone(),
            result: ex.result.clone(),
            time: ex
                .time
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()),
            report: ex.report.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_json_time_format() {
        let ex = CodeExecution {
            id: 1,
            module: 2,
            user: 3,
            code: Some("print(1)".into()),
            result: Some("ok".into()),
            time: Some(
                NaiveDateTime::parse_from_str("2026-10-11 12:00:00", "%Y-%m-%d %H:%M:%S").unwrap(),
            ),
            report: None,
        };
        let j = CodeExecutionJson::from(&ex);
        assert_eq!(j.time.as_deref(), Some("2026-10-11 12:00:00"));
    }
}
