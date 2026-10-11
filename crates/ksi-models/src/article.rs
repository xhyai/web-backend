use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use crate::naive_to_iso;

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
            time_published: naive_to_iso(&a.time_created),
            picture: a.picture.clone().unwrap_or_else(|| DEFAULT_IMAGE.into()),
            published: a.published,
            author: a.author,
            content: a.resource.clone(),
            year: a.year,
        }
    }
}
