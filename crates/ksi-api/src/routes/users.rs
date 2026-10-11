use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use ksi_auth::AuthUser;
use ksi_models::{round1, ProfileRow, UserJson, UserRow};
use crate::{
    error::{ApiError, ApiResult},
    routes::profile::{current_year, profile_picture_url, PROFILE_COLS, USER_COLS},
    SharedState,
};

/// Default of `util/config.py::successful_participant_percentage()`
/// (the Rust workspace has no config-table infra yet).
const SUCCESSFUL_PARTICIPANT_PERCENTAGE: f64 = 60.0;

#[derive(Debug, Deserialize)]
pub struct UsersQuery {
    pub filter: Option<String>,
    pub sort: Option<String>,
}

/// One row of the users+profiles join used by `list_users`.
#[derive(Debug, sqlx::FromRow)]
struct UserWithProfile {
    #[sqlx(flatten)]
    user: UserRow,
    #[sqlx(flatten)]
    profile: ProfileRow,
}

#[derive(Debug, Default, Clone)]
struct ScoreAgg {
    total_score: f64,
    cheat: bool,
}

/// Per-user scoreboard aggregates for one year, computed with a handful of
/// grouped queries instead of one mega-query (mirrors the data `Users.on_get`
/// assembles in `endpoint/user.py`).
struct UsersAgg {
    scores: HashMap<i64, ScoreAgg>,
    tasks_cnt: HashMap<i64, i64>,
    achievements: HashMap<i64, Vec<i64>>,
    seasons: HashMap<i64, Vec<i64>>,
    org_seasons: HashMap<i64, Vec<i64>>,
    tasks: HashMap<i64, Vec<i64>>,
    co_tasks: HashMap<i64, Vec<i64>>,
    max_points: f64,
}

fn user_filter(only_user: Option<i64>, col: &str) -> String {
    only_user
        .map(|u| format!(" AND {col} = {u}"))
        .unwrap_or_default()
}

async fn load_agg(
    db: &sqlx::SqlitePool,
    year_id: i64,
    only_user: Option<i64>,
) -> ApiResult<UsersAgg> {
    // (user, total_score over evaluation_public modules, cheat) — mirrors
    // util/user.py::sum_points.
    let rows: Vec<(i64, Option<f64>, Option<i64>)> = sqlx::query_as(&format!(
        "SELECT pm.user, SUM(pm.pts), MAX(pm.cheat)
         FROM (SELECT e.user AS user, MAX(e.points) AS pts, MAX(e.cheat) AS cheat
               FROM evaluations e
               JOIN modules m ON m.id = e.module
               JOIN tasks t ON t.id = m.task
               JOIN waves w ON w.id = t.wave
               WHERE w.year = ?1 AND t.evaluation_public = 1{}
               GROUP BY e.user, e.module) pm
         GROUP BY pm.user",
        user_filter(only_user, "e.user")
    ))
    .bind(year_id)
    .fetch_all(db)
    .await?;
    let mut scores = HashMap::new();
    for (uid, total, cheat) in rows {
        scores.insert(
            uid,
            ScoreAgg {
                total_score: total.unwrap_or(0.0),
                cheat: cheat.unwrap_or(0) != 0,
            },
        );
    }

    // Number of distinct tasks with any submission this year.
    let rows: Vec<(i64, i64)> = sqlx::query_as(&format!(
        "SELECT e.user, COUNT(DISTINCT t.id)
         FROM evaluations e
         JOIN modules m ON m.id = e.module
         JOIN tasks t ON t.id = m.task
         JOIN waves w ON w.id = t.wave
         WHERE w.year = ?1{}
         GROUP BY e.user",
        user_filter(only_user, "e.user")
    ))
    .bind(year_id)
    .fetch_all(db)
    .await?;
    let tasks_cnt: HashMap<i64, i64> = rows.into_iter().collect();

    // Achievements: this year's + global ones (mirrors util/user.py::achievements).
    let rows: Vec<(i64, i64)> = sqlx::query_as(&format!(
        "SELECT ua.user_id, ua.achievement_id
         FROM user_achievement ua
         JOIN achievements a ON a.id = ua.achievement_id
         WHERE (a.year = ?1 OR a.year IS NULL){}",
        user_filter(only_user, "ua.user_id")
    ))
    .bind(year_id)
    .fetch_all(db)
    .await?;
    let mut achievements: HashMap<i64, Vec<i64>> = HashMap::new();
    for (uid, aid) in rows {
        achievements.entry(uid).or_default().push(aid);
    }

    // Active years via submissions.
    let rows: Vec<(i64, i64)> = sqlx::query_as(&format!(
        "SELECT DISTINCT e.user, w.year
         FROM evaluations e
         JOIN modules m ON m.id = e.module
         JOIN tasks t ON t.id = m.task
         JOIN waves w ON w.id = t.wave{}",
        user_filter(only_user, "e.user")
    ))
    .fetch_all(db)
    .await?;
    let mut seasons: HashMap<i64, Vec<i64>> = HashMap::new();
    for (uid, yid) in rows {
        seasons.entry(uid).or_default().push(yid);
    }

    // Active years as an org.
    let rows: Vec<(i64, i64)> = sqlx::query_as(&format!(
        "SELECT org, year FROM active_orgs WHERE 1 = 1{}",
        user_filter(only_user, "org")
    ))
    .fetch_all(db)
    .await?;
    let mut org_seasons: HashMap<i64, Vec<i64>> = HashMap::new();
    for (uid, yid) in rows {
        org_seasons.entry(uid).or_default().push(yid);
    }

    // Tasks authored / co-authored in this year.
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT t.author, t.id FROM tasks t
         JOIN waves w ON w.id = t.wave
         WHERE w.year = ?1 AND t.author IS NOT NULL",
    )
    .bind(year_id)
    .fetch_all(db)
    .await?;
    let mut tasks: HashMap<i64, Vec<i64>> = HashMap::new();
    for (uid, tid) in rows {
        if only_user.map(|u| u == uid).unwrap_or(true) {
            tasks.entry(uid).or_default().push(tid);
        }
    }
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT t.co_author, t.id FROM tasks t
         JOIN waves w ON w.id = t.wave
         WHERE w.year = ?1 AND t.co_author IS NOT NULL",
    )
    .bind(year_id)
    .fetch_all(db)
    .await?;
    let mut co_tasks: HashMap<i64, Vec<i64>> = HashMap::new();
    for (uid, tid) in rows {
        if only_user.map(|u| u == uid).unwrap_or(true) {
            co_tasks.entry(uid).or_default().push(tid);
        }
    }

    // Max attainable points this year (mirrors util/task.py::sum_points
    // with bonus=False, floored by the year's point_pad).
    let sum: Option<f64> = sqlx::query_scalar(
        "SELECT CAST(SUM(m.max_points) AS REAL)
         FROM modules m
         JOIN tasks t ON t.id = m.task
         JOIN waves w ON w.id = t.wave
         WHERE w.year = ?1 AND m.bonus = 0",
    )
    .bind(year_id)
    .fetch_optional(db)
    .await?
    .flatten();
    let point_pad: Option<f64> = sqlx::query_scalar(
        "SELECT CAST(point_pad AS REAL) FROM years WHERE id = ?1",
    )
    .bind(year_id)
    .fetch_optional(db)
    .await?
    .flatten();
    let max_points = sum.unwrap_or(0.0).max(point_pad.unwrap_or(0.0));

    Ok(UsersAgg {
        scores,
        tasks_cnt,
        achievements,
        seasons,
        org_seasons,
        tasks,
        co_tasks,
        max_points,
    })
}

/// Mirror of `util/user.py::to_json` (shape follows the swagger `User` schema).
fn user_to_json(
    row: &UserRow,
    profile: Option<&ProfileRow>,
    agg: &UsersAgg,
    viewer_is_org: bool,
) -> UserJson {
    let mut data = UserJson::base(row, profile_picture_url(row));
    if viewer_is_org {
        data.email = Some(row.email.clone());
        data.discord = row.discord.clone();
    }

    let stats = agg.scores.get(&row.id).cloned().unwrap_or_default();
    data.score = round1(stats.total_score);
    data.tasks_num = agg.tasks_cnt.get(&row.id).copied().unwrap_or(0);
    data.achievements = agg.achievements.get(&row.id).cloned().unwrap_or_default();

    match row.role.as_str() {
        "participant" | "participant_hidden" => {
            data.addr_country = profile.map(|p| p.addr_country.clone());
            data.school_name = profile.map(|p| p.school_name.clone());
            data.seasons = Some(agg.seasons.get(&row.id).cloned().unwrap_or_default());
            data.successful = Some(
                stats.total_score
                    >= (SUCCESSFUL_PARTICIPANT_PERCENTAGE / 100.0) * agg.max_points
                    && !stats.cheat,
            );
            data.cheat = Some(stats.cheat);
        }
        "org" | "admin" => {
            data.tasks = Some(agg.tasks.get(&row.id).cloned().unwrap_or_default());
            data.co_tasks = Some(agg.co_tasks.get(&row.id).cloned().unwrap_or_default());
            data.short_info = Some(row.short_info.clone());
            data.seasons = Some(agg.org_seasons.get(&row.id).cloned().unwrap_or_default());
        }
        "tester" => {
            data.short_info = Some(row.short_info.clone());
        }
        _ => {}
    }
    data
}

/// `GET /users` — user list / scoreboard (public; mirrors `endpoint/user.py::Users`).
///
/// Deviation: the Python version caches the result for 30 minutes — the
/// Rust workspace has no cache infra yet, so every request recomputes.
pub async fn list_users(
    State(state): State<SharedState>,
    headers: HeaderMap,
    user: AuthUser,
    Query(q): Query<UsersQuery>,
) -> ApiResult<Json<Value>> {
    let year_id = current_year(&state.db, &headers).await?;
    let year_name: String = sqlx::query_scalar("SELECT year FROM years WHERE id = ?")
        .bind(year_id)
        .fetch_one(&state.db)
        .await?;
    // Mirrors util/year.py::year_end.
    let year_end: i64 = year_name
        .replace(' ', "")
        .split('/')
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .map(|y| y + 1)
        .unwrap_or(0);

    let filter = q.filter.as_deref().unwrap_or("");
    let rows: Vec<UserWithProfile> = match filter {
        "organisators" | "orgs" => {
            sqlx::query_as(&format!(
                "SELECT {USER_COLS}, {PROFILE_COLS} FROM users u
                 JOIN profiles p ON p.user_id = u.id
                 WHERE (u.role = 'org' OR u.role = 'admin')
                   AND EXISTS (SELECT 1 FROM active_orgs ao
                               WHERE ao.org = u.id AND ao.year = ?1)"
            ))
            .bind(year_id)
            .fetch_all(&state.db)
            .await?
        }
        "orgs-all" => {
            sqlx::query_as(&format!(
                "SELECT {USER_COLS}, {PROFILE_COLS} FROM users u
                 JOIN profiles p ON p.user_id = u.id
                 WHERE u.role = 'org' OR u.role = 'admin'"
            ))
            .fetch_all(&state.db)
            .await?
        }
        "part-hs" | "part-other" | "part" | "participants" => {
            sqlx::query_as(&format!(
                "SELECT {USER_COLS}, {PROFILE_COLS} FROM users u
                 JOIN profiles p ON p.user_id = u.id
                 WHERE u.role = 'participant'"
            ))
            .fetch_all(&state.db)
            .await?
        }
        _ => {
            sqlx::query_as(&format!(
                "SELECT {USER_COLS}, {PROFILE_COLS} FROM users u
                 JOIN profiles p ON p.user_id = u.id"
            ))
            .fetch_all(&state.db)
            .await?
        }
    };
    // NOTE: the selected column names are disjoint across the two tables,
    // so the #[sqlx(flatten)] mapping in UserWithProfile is unambiguous.

    let agg = load_agg(&state.db, year_id, None).await?;
    let viewer_is_org = user.is_org();

    let mut out: Vec<(f64, UserJson)> = Vec::with_capacity(rows.len());
    for up in &rows {
        let row = &up.user;
        let profile = &up.profile;
        // A hidden participant is invisible to non-orgs.
        if row.role == "participant_hidden" && !viewer_is_org {
            continue;
        }
        let stats = agg.scores.get(&row.id).cloned().unwrap_or_default();
        let keep = match filter {
            // Participants are only listed in the current year (where
            // tasks_cnt is defined); mirrors the Python filters.
            "part-hs" => stats.total_score > 0.0 && profile.school_finish >= year_end,
            "part-other" => stats.total_score > 0.0 && profile.school_finish < year_end,
            "part" | "participants" => {
                agg.tasks_cnt.get(&row.id).copied().unwrap_or(0) > 0
            }
            _ => true,
        };
        if !keep {
            continue;
        }
        out.push((stats.total_score, user_to_json(row, Some(profile), &agg, viewer_is_org)));
    }

    if q.sort.as_deref() == Some("score") {
        out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    }

    Ok(Json(
        json!({ "users": out.into_iter().map(|(_, u)| u).collect::<Vec<_>>() }),
    ))
}

/// `GET /users/{id}` — single user (mirrors `endpoint/user.py::User.on_get`;
/// public, hidden participants visible to orgs only).
pub async fn get_user(
    State(state): State<SharedState>,
    headers: HeaderMap,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let row: Option<UserRow> =
        sqlx::query_as(&format!("SELECT {USER_COLS} FROM users WHERE id = ?"))
            .bind(id)
            .fetch_optional(&state.db)
            .await?;

    let row = match row {
        None => {
            return Err(ApiError::NotFound(
                "Uživatel s tímto ID neexistuje.".into(),
            ))
        }
        Some(r) => r,
    };
    if row.role == "participant_hidden" && !user.is_org() {
        return Err(ApiError::NotFound(
            "Uživatel s tímto ID neexistuje.".into(),
        ));
    }

    let year_id = current_year(&state.db, &headers).await?;
    let profile: Option<ProfileRow> =
        sqlx::query_as(&format!("SELECT {PROFILE_COLS} FROM profiles WHERE user_id = ?"))
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
    let agg = load_agg(&state.db, year_id, Some(id)).await?;
    let data = user_to_json(&row, profile.as_ref(), &agg, user.is_org());
    Ok(Json(json!({ "user": data })))
}

/// `DELETE /users/{id}` — delete a user (admin only).
/// Mirrors Python, which answers HTTP 400 when the caller is not an admin.
pub async fn delete_user(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    if !user.is_logged_in() || !user.is_admin() {
        return Err(ApiError::BadRequest(
            "Mazat uživatele může pouze administrátor.".into(),
        ));
    }
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM users WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound(
            "Uživatel s tímto ID neexistuje.".into(),
        ));
    }

    sqlx::query("DELETE FROM profiles WHERE user_id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({})))
}

/// `GET /users/{id}/discord` — Discord invite link for the *requesting*
/// user (mirrors `endpoint/user.py::DiscordInviteLink`, which ignores the
/// path id and looks at the caller's own Discord username).
pub async fn discord_invite(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(_id): Path<i64>,
) -> ApiResult<Json<Value>> {
    if !user.is_logged_in() {
        return Err(ApiError::Unauthorized("Nejste přihlášeni.".into()));
    }
    let discord: Option<String> = sqlx::query_scalar("SELECT discord FROM users WHERE id = ?")
        .bind(user.id.unwrap())
        .fetch_optional(&state.db)
        .await?
        .flatten();
    match discord.filter(|s| !s.is_empty()) {
        None => Ok(Json(Value::Null)),
        Some(_) => Ok(Json(
            state
                .config
                .discord_invite_link
                .clone()
                .map(Value::from)
                .unwrap_or(Value::Null),
        )),
    }
}

#[derive(Debug, Deserialize)]
pub struct ChangePasswordBody {
    pub old_password: String,
    pub new_password: String,
    pub new_password2: String,
}

/// `POST /settings/changePassword` — change the current user's password.
///
/// Mirrors Python exactly, including the `{'result': 'error'}` bodies
/// (which intentionally do not validate against the swagger
/// `ChangePasswordResult` schema).
pub async fn change_password(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(body): Json<ChangePasswordBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if !user.is_logged_in() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }
    let user_id = user.id.unwrap();
    let hash: Option<String> = sqlx::query_scalar("SELECT password FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?;
    let hash = hash.ok_or_else(|| ApiError::NotFound("Uživatel neexistuje.".into()))?;

    if !ksi_auth::verify_password(&body.old_password, &hash) {
        return Ok((StatusCode::UNAUTHORIZED, Json(json!({"result": "error"}))));
    }
    if body.new_password != body.new_password2 {
        return Ok((StatusCode::OK, Json(json!({"result": "error"}))));
    }

    let new_hash = bcrypt::hash(&body.new_password, bcrypt::DEFAULT_COST)
        .map_err(|_| ApiError::Internal("Nelze zahashovat heslo.".into()))?;
    sqlx::query("UPDATE users SET password = ? WHERE id = ?")
        .bind(&new_hash)
        .bind(user_id)
        .execute(&state.db)
        .await?;

    Ok((StatusCode::OK, Json(json!({"result": "ok"}))))
}
