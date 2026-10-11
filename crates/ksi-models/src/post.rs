use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use crate::naive_to_iso;

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

/// Post JSON as returned by the API (includes `resolved_at`).
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
}
