//! JSON shapes for the org correction workflow (`/admin/corrections*`).
//!
//! Mirrors `util/correction.py` and `util/correctionInfo.py` from the Python
//! backend.

use serde::{Deserialize, Serialize};

use crate::AchievementJson;

// ---------------------------------------------------------------------------
// PUT /admin/corrections/{id} request
// ---------------------------------------------------------------------------

/// `{"correction": {...}}` — submit an org's correction of one (task, user).
#[derive(Debug, Deserialize)]
pub struct CorrectionUpdateRequest {
    pub correction: CorrectionUpdate,
}

#[derive(Debug, Deserialize)]
pub struct CorrectionUpdate {
    pub task_id: i64,
    pub user: i64,
    pub comment: Option<i64>,
    #[serde(default)]
    pub achievements: Vec<i64>,
    #[serde(default)]
    pub modules: Vec<CorrectionUpdateModule>,
}

#[derive(Debug, Deserialize)]
pub struct CorrectionUpdateModule {
    pub evaluation: CorrectionEvaluationUpdate,
}

/// One module's evaluation as sent by the correction UI. `corrected_by`
/// and `cheat` are optional in the Python backend (`dict.get` semantics):
/// the org's own id is used when `corrected_by` is absent.
#[derive(Debug, Deserialize)]
pub struct CorrectionEvaluationUpdate {
    pub eval_id: i64,
    pub points: f64,
    pub corrected_by: Option<i64>,
    pub cheat: Option<bool>,
}

// ---------------------------------------------------------------------------
// Correction JSON (GET /admin/corrections, GET/PUT /admin/corrections/{id})
// ---------------------------------------------------------------------------

/// Mirrors `util/correction.py::corr_eval_to_json`.
#[derive(Debug, Serialize)]
pub struct CorrectionEvalJson {
    pub eval_id: i64,
    pub points: f64,
    /// `evaluation.time` as ISO-8601, `null` when unknown.
    pub last_modified: Option<String>,
    pub corrected_by: Option<i64>,
    pub full_report: String,
    pub cheat: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub general: Option<CorrectionGeneralJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub programming: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quiz: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sortable: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct CorrectionGeneralJson {
    pub files: Vec<CorrectionFileJson>,
}

#[derive(Debug, Serialize)]
pub struct CorrectionFileJson {
    pub id: i64,
    pub filename: String,
}

/// Mirrors `util/correction.py::_corr_module_to_json`.
#[derive(Debug, Serialize)]
pub struct CorrectionModuleJson {
    pub module_id: i64,
    pub evaluations_list: Vec<i64>,
    pub evaluation: CorrectionEvalJson,
}

/// Mirrors `util/correction.py::to_json`.
#[derive(Debug, Serialize)]
pub struct CorrectionJson {
    /// Artificial id: `task_id * 100000 + user_id`.
    pub id: i64,
    pub task_id: i64,
    pub state: String,
    pub user: i64,
    pub comment: Option<i64>,
    pub achievements: Vec<i64>,
    pub modules: Vec<CorrectionModuleJson>,
}

#[derive(Debug, Serialize)]
pub struct CorrectionResponse {
    pub correction: CorrectionJson,
}

// ---------------------------------------------------------------------------
// GET /admin/corrections list response
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct CorrectionTaskJson {
    pub id: i64,
    pub title: String,
}

#[derive(Debug, Serialize)]
pub struct CorrectionModuleListJson {
    pub id: i64,
    #[serde(rename = "type")]
    pub module_type: String,
    pub name: String,
    pub autocorrect: bool,
    pub max_points: f64,
}

#[derive(Debug, Serialize)]
pub struct CorrectionAchievementJson {
    pub id: i64,
    pub title: String,
    pub active: bool,
    pub picture: String,
    pub description: Option<String>,
    pub persistent: bool,
    pub year: Option<i64>,
}

/// Mirrors `util/thread.py::to_json` (the shape embedded in the corrections
/// list, with `unread`/`posts_count` counters).
#[derive(Debug, Serialize)]
pub struct CorrectionThreadJson {
    pub id: i64,
    pub title: Option<String>,
    pub unread: i64,
    pub posts_count: i64,
    pub details: i64,
    pub year: i64,
}

/// Mirrors `util/thread.py::details_to_json`.
#[derive(Debug, Serialize)]
pub struct CorrectionThreadDetailsJson {
    pub id: i64,
    pub root_posts: Vec<i64>,
}

/// Mirrors `util/post.py::to_json` for posts embedded in the corrections
/// list.
#[derive(Debug, Serialize)]
pub struct CorrectionPostJson {
    pub id: i64,
    pub thread: i64,
    pub author: i64,
    pub body: String,
    pub published_at: Option<String>,
    pub reaction: Vec<i64>,
    pub is_new: bool,
}

#[derive(Debug, Serialize)]
pub struct CorrectionsAllResponse {
    pub corrections: Vec<CorrectionJson>,
    pub tasks: Vec<CorrectionTaskJson>,
    pub modules: Vec<CorrectionModuleListJson>,
    pub achievements: Vec<CorrectionAchievementJson>,
    pub threads: Vec<CorrectionThreadJson>,
    pub posts: Vec<CorrectionPostJson>,
    #[serde(rename = "threadDetails")]
    pub thread_details: Vec<CorrectionThreadDetailsJson>,
}

// ---------------------------------------------------------------------------
// correctionInfos
// ---------------------------------------------------------------------------

/// Mirrors `util/correctionInfo.py::task_to_json`.
#[derive(Debug, Serialize)]
pub struct CorrectionInfoTaskJson {
    pub id: i64,
    pub title: String,
    pub wave: i64,
    pub author: Option<i64>,
    pub corr_state: String,
    pub solvers: Vec<i64>,
}

/// Mirrors `util/correctionInfo.py::user_to_json`.
#[derive(Debug, Serialize)]
pub struct CorrectionInfoUserJson {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
    pub role: String,
    pub profile_picture: Option<String>,
    pub gender: String,
}

#[derive(Debug, Serialize)]
pub struct CorrectionInfoResponse {
    #[serde(rename = "correctionsInfo")]
    pub corrections_info: CorrectionInfoTaskJson,
}

// ---------------------------------------------------------------------------
// PUT /admin/correctionsEmail/{id} response
// ---------------------------------------------------------------------------

/// Mirrors the Python endpoint: `{"count": N}` plus, only when some
/// deliveries failed, an `errors` list.
#[derive(Debug, Serialize)]
pub struct CorrectionsEmailResponse {
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artificial_id_matches_python() {
        // id = task_id * 100000 + user_id (see util/correction.py::to_json)
        let c = CorrectionJson {
            id: 12 * 100000 + 345,
            task_id: 12,
            state: "notcorrected".into(),
            user: 345,
            comment: None,
            achievements: vec![],
            modules: vec![],
        };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["id"], 1_200_345);
        assert!(v["comment"].is_null());
        assert_eq!(v["state"], "notcorrected");
    }

    #[test]
    fn eval_json_type_key_serializes_per_module_type() {
        let e = CorrectionEvalJson {
            eval_id: 1,
            points: 8.0,
            last_modified: Some("2026-10-11T10:00:00.000000".into()),
            corrected_by: None,
            full_report: String::new(),
            cheat: false,
            general: Some(CorrectionGeneralJson {
                files: vec![CorrectionFileJson { id: 7, filename: "a.py".into() }],
            }),
            programming: None,
            quiz: None,
            sortable: None,
            text: None,
        };
        let v = serde_json::to_value(&e).unwrap();
        assert!(v.get("general").is_some());
        assert!(v.get("programming").is_none());
        assert_eq!(v["general"]["files"][0]["filename"], "a.py");
    }
}
