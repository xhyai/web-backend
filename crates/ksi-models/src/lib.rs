pub mod achievement;
pub mod article;
pub mod content;
pub mod correction;
pub mod diploma;
pub mod evaluation;
pub mod exec;
pub mod feedback;
pub mod module;
pub mod post;
pub mod profile;
pub mod task;
pub mod thread;
pub mod user;
pub mod wave;
pub mod year;

pub use achievement::{AchievementJson, AchievementRow, UserAchievementRow};
pub use article::{ArticleJson, ArticleRow};
pub use content::ContentJson;
pub use correction::{
    CorrectionAchievementJson, CorrectionEvalJson, CorrectionEvaluationUpdate, CorrectionFileJson,
    CorrectionGeneralJson, CorrectionInfoResponse, CorrectionInfoTaskJson, CorrectionInfoUserJson,
    CorrectionJson, CorrectionModuleJson, CorrectionModuleListJson, CorrectionPostJson,
    CorrectionResponse, CorrectionTaskJson, CorrectionThreadDetailsJson, CorrectionThreadJson,
    CorrectionUpdate, CorrectionUpdateModule, CorrectionUpdateRequest, CorrectionsAllResponse,
    CorrectionsEmailResponse,
};
pub use diploma::{Diploma, DiplomaJson};
pub use evaluation::Evaluation;
pub use exec::{CodeExecution, CodeExecutionJson};
pub use feedback::{
    default_categories, parse_feedback, FeedbackJson, FeedbackParseError, FeedbackRow,
    IncomingCategory, StoredCategory,
};
pub use module::{Module, ModuleCustom, ModuleDetailJson, ModuleJson, SubmittedFile, SubmittedFileJson};
pub use post::{Post, PostJson};
pub use profile::{BasicProfileJson, NotifyRow, ProfileJson, ProfileRow, TaskScoreJson};
pub use task::{AdminTaskJson, Task, TaskJson};
pub use thread::{Thread, ThreadJson};
pub use user::{round1, User, UserJson, UserRow};
pub use wave::{Wave, WaveJson};
pub use year::{Year, YearJson};

pub(crate) fn naive_to_iso(dt: &Option<chrono::NaiveDateTime>) -> Option<String> {
    dt.map(|d| d.format("%Y-%m-%dT%H:%M:%S%.6f").to_string())
}

/// Parse a datetime string sent by a client into a naive UTC datetime.
///
/// Mirrors the leniency of Python's `dateutil.parser.parse` for the formats
/// the frontends actually send: RFC 3339 (with timezone), ISO-8601 with or
/// without fractional seconds, space-separated datetimes, and plain dates
/// (interpreted as midnight, e.g. from `<input type="date">`).
pub fn parse_datetime(s: &str) -> Option<chrono::NaiveDateTime> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.naive_utc());
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
    ] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return Some(dt);
        }
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return d.and_hms_opt(0, 0, 0);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_datetime_formats() {
        let expected = chrono::NaiveDate::from_ymd_opt(2026, 10, 11)
            .unwrap()
            .and_hms_opt(12, 30, 0)
            .unwrap();
        assert_eq!(parse_datetime("2026-10-11T12:30:00.000Z"), Some(expected));
        assert_eq!(parse_datetime("2026-10-11T12:30:00+00:00"), Some(expected));
        assert_eq!(
            parse_datetime("2026-10-11T12:30:00.000000"),
            Some(expected)
        );
        assert_eq!(parse_datetime("2026-10-11 12:30:00"), Some(expected));
        assert_eq!(
            parse_datetime("2026-10-11"),
            Some(
                chrono::NaiveDate::from_ymd_opt(2026, 10, 11)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
            )
        );
        assert_eq!(parse_datetime("not a date"), None);
        assert_eq!(parse_datetime(""), None);
    }
}
