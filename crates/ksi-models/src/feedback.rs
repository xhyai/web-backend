use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use crate::naive_to_iso;

/// Row from the `feedbacks` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct FeedbackRow {
    pub user: i64,
    pub task: i64,
    pub content: String,
    #[sqlx(rename = "lastUpdated")]
    pub last_updated: Option<NaiveDateTime>,
}

/// Feedback JSON as returned by the API (mirrors `util/feedback.py::to_json`
/// and `util/feedback.py::empty_to_json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackJson {
    /// Task id (the feedback is keyed by (user, task)).
    pub id: i64,
    #[serde(rename = "userId")]
    pub user_id: i64,
    #[serde(rename = "lastUpdated", skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
    pub categories: Vec<serde_json::Value>,
    pub filled: bool,
}

impl FeedbackJson {
    pub fn from_row(f: &FeedbackRow) -> Self {
        let categories: Vec<serde_json::Value> =
            serde_json::from_str(&f.content).unwrap_or_default();
        Self {
            id: f.task,
            user_id: f.user,
            last_updated: naive_to_iso(&f.last_updated),
            categories,
            filled: true,
        }
    }

    /// Mirrors `util/feedback.py::empty_to_json`: the shape returned when the
    /// user has not filled in feedback for the task yet.
    pub fn empty(task_id: i64, user_id: i64) -> Self {
        Self {
            id: task_id,
            user_id,
            last_updated: None,
            categories: default_categories(),
            filled: false,
        }
    }
}

/// The default (unanswered) feedback categories, mirroring
/// `util/feedback.py::CATEGORIES`.
pub fn default_categories() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "id": "explained",
            "ftype": "stars",
            "text": "Jak dobře ti přišla úloha vysvětlená?",
        }),
        serde_json::json!({
            "id": "interesting",
            "ftype": "stars",
            "text": "Jak moc ti přijde úloha zajímavá?",
        }),
        serde_json::json!({
            "id": "difficult",
            "ftype": "line",
            "text": "Jak moc ti přijde úloha těžká?",
        }),
        serde_json::json!({
            "id": "comment",
            "ftype": "text_large",
            "text": "Chceš nám vzkázat něco, co nám pomůže příště úlohu připravit lépe? (nepovinné)",
        }),
    ]
}

// ---------------------------------------------------------------------------
// Feedback answer validation (mirrors `util/feedback.py::parse_feedback`)
// ---------------------------------------------------------------------------

const MAX_CATEGORIES: usize = 16;
const MAX_ID_LEN: usize = 32;
const MAX_TYPE_LEN: usize = 32;
const MAX_QUESTION_LEN: usize = 1024;
const MAX_ANSWER_LEN: usize = 8192;

const ALLOWED_TYPES: &[&str] = &["stars", "line", "text_large"];

/// A single category as submitted by the client.
#[derive(Debug, Clone, Deserialize)]
pub struct IncomingCategory {
    pub id: String,
    pub ftype: String,
    pub text: String,
    pub answer: Option<serde_json::Value>,
}

/// A single validated category as stored in the `content` JSON column.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCategory {
    pub id: String,
    pub ftype: String,
    pub text: String,
    pub answer: serde_json::Value,
}

/// Validation failure, mirroring the `E*` exceptions in `util/feedback.py`.
/// The handler maps every variant to HTTP 400 with the message as `detail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedbackParseError {
    MissingAnswer(String),
    UnmatchingDataType(String),
    ForbiddenType(String),
    OutOfRange(String),
}

impl std::fmt::Display for FeedbackParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeedbackParseError::MissingAnswer(id) => {
                write!(f, "Missing answer for question '{id}'")
            }
            FeedbackParseError::UnmatchingDataType(ftype) => {
                write!(f, "'{ftype}' is not allowed as question type!")
            }
            FeedbackParseError::ForbiddenType(msg) => {
                write!(f, "{msg}")
            }
            FeedbackParseError::OutOfRange(id) => write!(f, "'{id}' out of range!"),
        }
    }
}

impl FeedbackParseError {
    fn forbidden_type(value: &serde_json::Value, ftype: &str) -> Self {
        // Python `%s` of `type(answer).__name__` for a JSON-decoded value.
        let type_name = match value {
            serde_json::Value::Null => "NoneType",
            serde_json::Value::Bool(_) => "bool",
            serde_json::Value::Number(n) => {
                if n.is_i64() || n.is_u64() {
                    "int"
                } else {
                    "float"
                }
            }
            serde_json::Value::String(_) => "str",
            serde_json::Value::Array(_) => "list",
            serde_json::Value::Object(_) => "dict",
        };
        FeedbackParseError::ForbiddenType(format!(
            "'{type_name}' is not allowed as answer of type '{ftype}'!"
        ))
    }

    pub fn message(&self) -> String {
        match self {
            FeedbackParseError::ForbiddenType(msg) => msg.clone(),
            other => other.to_string(),
        }
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars().take(max).collect()
    } else {
        s.to_string()
    }
}

/// Validate submitted feedback categories and return the normalized form
/// stored in the `content` column.
///
/// Mirrors `util/feedback.py::parse_feedback`, including its message texts.
/// One deliberate deviation: the Python code computes
/// `categoriess = categories[:MAX_CATEGORIES]` but then iterates the
/// untruncated `categories` (a no-op slice); the port applies the intended
/// 16-category limit.
pub fn parse_feedback(
    categories: Vec<IncomingCategory>,
) -> Result<Vec<StoredCategory>, FeedbackParseError> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut to_store = Vec::new();

    for category in categories.into_iter().take(MAX_CATEGORIES) {
        let id = truncate_chars(&category.id, MAX_ID_LEN);
        if !seen.insert(id.clone()) {
            continue;
        }

        let answer = match category.answer {
            Some(a) => a,
            None => return Err(FeedbackParseError::MissingAnswer(category.id.clone())),
        };

        let answer = match &answer {
            serde_json::Value::String(s) => {
                serde_json::Value::String(truncate_chars(s, MAX_ANSWER_LEN))
            }
            other => other.clone(),
        };

        if !ALLOWED_TYPES.contains(&category.ftype.as_str()) {
            return Err(FeedbackParseError::UnmatchingDataType(
                category.ftype.clone(),
            ));
        }

        let want_int = matches!(category.ftype.as_str(), "stars" | "line");
        let is_int = answer.as_i64().is_some() || answer.as_u64().is_some();
        let ok = if want_int {
            is_int
        } else {
            answer.is_string()
        };
        if !ok {
            return Err(FeedbackParseError::forbidden_type(
                &answer,
                &category.ftype,
            ));
        }

        if want_int {
            let n = answer.as_i64().or_else(|| answer.as_u64().map(|v| v as i64));
            if !matches!(n, Some(0..=5)) {
                return Err(FeedbackParseError::OutOfRange(category.id.clone()));
            }
        }

        to_store.push(StoredCategory {
            id,
            ftype: truncate_chars(&category.ftype, MAX_TYPE_LEN),
            text: truncate_chars(&category.text, MAX_QUESTION_LEN),
            answer,
        });
    }

    Ok(to_store)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(id: &str, ftype: &str, answer: serde_json::Value) -> IncomingCategory {
        IncomingCategory {
            id: id.into(),
            ftype: ftype.into(),
            text: "Otázka".into(),
            answer: Some(answer),
        }
    }

    #[test]
    fn parse_valid_stars_and_text() {
        let out = parse_feedback(vec![
            cat("q1", "stars", serde_json::json!(4)),
            cat("q2", "text_large", serde_json::json!("pěkná úloha")),
        ])
        .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].answer, serde_json::json!(4));
        assert_eq!(out[1].answer, serde_json::json!("pěkná úloha"));
    }

    #[test]
    fn parse_missing_answer() {
        let err = parse_feedback(vec![IncomingCategory {
            id: "q1".into(),
            ftype: "stars".into(),
            text: "t".into(),
            answer: None,
        }])
        .unwrap_err();
        assert_eq!(
            err.message(),
            "Missing answer for question 'q1'"
        );
    }

    #[test]
    fn parse_forbidden_ftype() {
        let err = parse_feedback(vec![cat("q1", "radio", serde_json::json!(1))]).unwrap_err();
        assert_eq!(err.message(), "'radio' is not allowed as question type!");
    }

    #[test]
    fn parse_wrong_answer_type() {
        let err =
            parse_feedback(vec![cat("q1", "stars", serde_json::json!("mnoho"))]).unwrap_err();
        assert_eq!(
            err.message(),
            "'str' is not allowed as answer of type 'stars'!"
        );
        let err =
            parse_feedback(vec![cat("q1", "text_large", serde_json::json!(3))]).unwrap_err();
        assert_eq!(
            err.message(),
            "'int' is not allowed as answer of type 'text_large'!"
        );
    }

    #[test]
    fn parse_out_of_range() {
        let err = parse_feedback(vec![cat("q1", "stars", serde_json::json!(6))]).unwrap_err();
        assert_eq!(err.message(), "'q1' out of range!");
        assert!(parse_feedback(vec![cat("q1", "stars", serde_json::json!(0))]).is_ok());
        assert!(parse_feedback(vec![cat("q1", "line", serde_json::json!(5))]).is_ok());
    }

    #[test]
    fn parse_dedupes_and_truncates() {
        let long_id = "x".repeat(100);
        let out = parse_feedback(vec![
            cat(&long_id, "stars", serde_json::json!(3)),
            cat(&long_id, "stars", serde_json::json!(4)),
        ])
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id.len(), MAX_ID_LEN);
    }
}
