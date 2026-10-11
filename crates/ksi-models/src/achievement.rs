use serde::{Deserialize, Serialize};

/// Row from the `achievements` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AchievementRow {
    pub id: i64,
    pub title: String,
    pub picture: String,
    pub description: Option<String>,
    pub year: Option<i64>,
}

/// Achievement JSON as returned by the API (mirrors `util/achievement.py::to_json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AchievementJson {
    pub id: i64,
    pub title: String,
    pub active: bool,
    pub picture: String,
    pub description: Option<String>,
    pub persistent: bool,
    pub year: Option<i64>,
}

impl AchievementJson {
    pub fn from_row(a: &AchievementRow) -> Self {
        Self {
            id: a.id,
            title: a.title.clone(),
            active: true,
            picture: a.picture.clone(),
            description: a.description.clone(),
            persistent: a.year.is_none(),
            year: a.year,
        }
    }
}

/// Row from the `user_achievement` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserAchievementRow {
    pub user_id: i64,
    pub achievement_id: i64,
    pub task_id: Option<i64>,
}
