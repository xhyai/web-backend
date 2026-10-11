use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

/// Row from the `years` table.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Year {
    pub id: i64,
    pub year: Option<String>,
    pub sealed: bool,
    pub point_pad: Option<f64>,
}

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

/// Row from the `tasks` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub author: Option<i64>,
    pub co_author: Option<i64>,
    pub wave: i64,
    pub prerequisite: Option<i64>,
    pub intro: String,
    pub body: String,
    pub solution: Option<String>,
    pub thread: i64,
    pub picture_base: Option<String>,
    pub evaluation_public: bool,
}

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

/// Row from the `users` table (subset used by auth).
#[derive(Debug, Clone, sqlx::FromRow)]
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

/// Row from the `threads` table.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Thread {
    pub id: i64,
    pub title: Option<String>,
    pub public: bool,
    pub year: i64,
}

/// Row from the `posts` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Post {
    pub id: i64,
    pub thread: i64,
    pub author: i64,
    pub body: String,
    pub published_at: Option<NaiveDateTime>,
    pub parent: Option<i64>,
    pub resolved_at: Option<NaiveDateTime>,
}

/// Row from the `articles` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ArticleRow {
    pub id: i64,
    pub author: i64,
    pub title: String,
    pub body: Option<String>,
    pub picture: Option<String>,
    pub time_created: Option<NaiveDateTime>,
    pub published: Option<bool>,
    pub year: i64,
    pub resource: Option<String>,
}

/// Article JSON as returned by the API (mirrors `endpoint/article.py::_artice_to_json`).
/// Note: the swagger spec names this field `resource`, but the real backend
/// returns `content` — we match the backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArticleJson {
    pub id: i64,
    pub title: String,
    pub body: Option<String>,
    pub time_published: Option<String>,
    pub picture: String,
    pub published: Option<bool>,
    pub author: i64,
    pub content: Option<String>,
    pub year: i64,
}

const DEFAULT_IMAGE: &str = "img/box-ksi.svg";

impl ArticleJson {
    pub fn from_row(a: &ArticleRow) -> Self {
        Self {
            id: a.id,
            title: a.title.clone(),
            body: a.body.clone(),
            time_published: a.time_created.map(|t| t.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()),
            picture: a.picture.clone().unwrap_or_else(|| DEFAULT_IMAGE.into()),
            published: a.published,
            author: a.author,
            content: a.resource.clone(),
            year: a.year,
        }
    }
}

/// Row from the `achievements` table.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Achievement {
    pub id: i64,
    pub title: String,
    pub description: String,
    pub picture: Option<String>,
}

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

// ---------------------------------------------------------------------------
// JSON serialization shapes (what the API returns).
// These mirror `util/*.py::to_json` from the Python backend.
// ---------------------------------------------------------------------------

fn naive_to_iso(dt: &Option<NaiveDateTime>) -> Option<String> {
    dt.map(|d| d.format("%Y-%m-%dT%H:%M:%S%.6f").to_string())
}

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

#[derive(Debug, Serialize, Deserialize)]
pub struct PostJson {
    pub id: i64,
    pub thread: i64,
    pub author: i64,
    pub body: String,
    pub published_at: Option<String>,
    pub resolved_at: Option<String>,
    pub reaction: Vec<i64>,
    pub is_new: bool,
}

impl PostJson {
    pub fn from_row(p: &Post, reactions: Vec<i64>) -> Self {
        Self {
            id: p.id,
            thread: p.thread,
            author: p.author,
            body: p.body.clone(),
            published_at: naive_to_iso(&p.published_at),
            resolved_at: naive_to_iso(&p.resolved_at),
            reaction: reactions,
            is_new: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_json_carries_resolved_at() {
        let p = Post {
            id: 1,
            thread: 1,
            author: 2,
            body: "hi".into(),
            published_at: None,
            parent: None,
            resolved_at: None,
        };
        let j = PostJson::from_row(&p, vec![]);
        assert_eq!(j.resolved_at, None);
        let v = serde_json::to_value(&j).unwrap();
        assert!(v.get("resolved_at").is_some());
        assert!(v.get("reaction").is_some());
    }

    #[test]
    fn auth_user_roles() {
        use crate::auth::AuthUser;
        let org = AuthUser { id: Some(1), role: Some("org".into()), token: None };
        assert!(org.is_org() && !org.is_admin() && org.is_logged_in());
        let admin = AuthUser { id: Some(1), role: Some("admin".into()), token: None };
        assert!(admin.is_org() && admin.is_admin());
        let part = AuthUser { id: Some(2), role: Some("participant".into()), token: None };
        assert!(!part.is_org() && part.is_logged_in());
        assert!(!AuthUser::anonymous().is_logged_in());
    }
}
