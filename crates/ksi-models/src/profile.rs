use serde::Serialize;

/// Row from the `profiles` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProfileRow {
    pub user_id: i64,
    pub addr_street: String,
    pub addr_city: String,
    pub addr_zip: String,
    pub addr_country: String,
    pub school_name: String,
    pub school_street: String,
    pub school_city: String,
    pub school_zip: String,
    pub school_country: String,
    pub school_finish: i64,
    pub tshirt_size: String,
}

/// Row from the `users_notify` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct NotifyRow {
    pub user: i64,
    pub auth_token: String,
    pub notify_eval: bool,
    pub notify_response: bool,
    pub notify_ksi: bool,
    pub notify_events: bool,
}

/// Profile JSON (mirrors `util/profile.py::to_json`).
///
/// The shape follows the `Profile` schema from web-backend-swagger exactly
/// (it validates with `additionalProperties: false`), so fields the old
/// Python serializer emitted on top of the spec (score, percentile,
/// achievements, …) are intentionally not present here.
///
/// `author` / `resource` are legacy contract fields kept for spec
/// compatibility; the frontend never reads them.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileJson {
    pub id: i64,
    pub author: i64,
    pub resource: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addr_street: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addr_city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addr_zip: Option<String>,
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nick_name: Option<String>,
    pub first_name: String,
    pub last_name: String,
    pub gender: String,
    pub short_info: String,
    pub github: Option<String>,
    pub discord: Option<String>,
    pub addr_country: String,
    pub school_name: String,
    pub school_street: String,
    pub school_city: String,
    pub school_zip: String,
    pub school_country: String,
    pub school_finish: i64,
    pub tshirt_size: String,
    pub notify_eval: bool,
    pub notify_response: bool,
    pub notify_ksi: bool,
    pub notify_events: bool,
}

/// Basic profile JSON (mirrors `util/profile.py::_basic_profile_to_json`,
/// shape follows the swagger `BasicProfile` schema).
#[derive(Debug, Clone, Serialize)]
pub struct BasicProfileJson {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
    pub nick_name: String,
    pub profile_picture: String,
    pub short_info: String,
    pub email: String,
    pub gender: String,
    pub role: String,
    pub signed_in: bool,
}

/// Per-task score entry (mirrors `util/profile.py::task_score_to_json`).
#[derive(Debug, Clone, Serialize)]
pub struct TaskScoreJson {
    pub id: i64,
    pub task: i64,
    pub achievements: Vec<i64>,
    pub score: Option<f64>,
}
