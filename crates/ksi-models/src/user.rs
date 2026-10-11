use serde::{Deserialize, Serialize};

/// Row from the `users` table.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub first_name: String,
    pub last_name: String,
    pub nick_name: Option<String>,
    pub sex: String,
    pub role: String,
    pub enabled: bool,
}

/// Full row from the `users` table (everything the profile/user
/// serializers need).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserRow {
    pub id: i64,
    pub email: String,
    pub github: Option<String>,
    pub discord: Option<String>,
    pub first_name: String,
    pub nick_name: Option<String>,
    pub last_name: String,
    pub sex: String,
    pub short_info: String,
    pub profile_picture: Option<String>,
    pub role: String,
    pub enabled: bool,
}

/// Public user JSON (mirrors `util/user.py::to_json`).
///
/// The shape follows the `User` schema from web-backend-swagger exactly
/// (it validates with `additionalProperties: false`): fields that only
/// apply to some roles / viewer privileges are `Option`s skipped when
/// absent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserJson {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
    #[serde(default)]
    pub profile_picture: Option<String>,
    pub gender: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub discord: Option<String>,
    pub role: String,
    pub score: f64,
    pub tasks_num: i64,
    #[serde(default)]
    pub achievements: Vec<i64>,
    pub enabled: bool,
    #[serde(default)]
    pub nick_name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub addr_country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub school_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub seasons: Option<Vec<i64>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub successful: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cheat: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tasks: Option<Vec<i64>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub co_tasks: Option<Vec<i64>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub short_info: Option<String>,
}

impl UserJson {
    /// Base object shared by all roles (mirrors the first half of
    /// `util/user.py::to_json`).
    pub fn base(row: &UserRow, profile_picture: Option<String>) -> Self {
        // A hidden participant looks like a normal one to the frontend.
        let role = if row.role == "participant_hidden" {
            "participant".to_string()
        } else {
            row.role.clone()
        };
        Self {
            id: row.id,
            first_name: row.first_name.clone(),
            last_name: row.last_name.clone(),
            profile_picture,
            gender: row.sex.clone(),
            email: None,
            discord: None,
            role,
            score: 0.0,
            tasks_num: 0,
            achievements: Vec::new(),
            enabled: row.enabled,
            nick_name: row.nick_name.clone().unwrap_or_default(),
            addr_country: None,
            school_name: None,
            seasons: None,
            successful: None,
            cheat: None,
            tasks: None,
            co_tasks: None,
            short_info: None,
        }
    }
}

/// Round like Python's `float(format(x, '.1f'))`.
pub fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}
