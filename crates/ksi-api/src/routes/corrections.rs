//! Org correction workflow: `/admin/corrections*`, `/admin/correctionInfos*`,
//! `/admin/correctionsEmail/{id}` and `/admin/corrections/{id}/publish`.
//!
//! Faithful port of `endpoint/admin/corrections.py`,
//! `endpoint/admin/correctionsInfo.py`, `endpoint/admin/correctionsEmail.py`,
//! `endpoint/admin/correctionsPublish.py` plus the JSON builders in
//! `util/correction.py` and `util/correctionInfo.py`.
//!
//! Exact routes (from `app.py` of the Python backend):
//! - `GET /admin/corrections?task=&participant=&state=`
//! - `GET /admin/corrections/{id}` (`id = task_id * 100000 + user_id`)
//! - `PUT /admin/corrections/{id}`
//! - `GET /admin/corrections/{id}/publish?public=0|1`
//! - `GET /admin/correctionInfos`
//! - `GET /admin/correctionInfos/{id}`
//! - `PUT /admin/correctionsEmail/{id}`
//!
//! E-mail is NOT really sent: deliveries go through
//! [`crate::mail::LogMailSender`], which logs instead of sending (see the
//! module docs there for how to wire up real SMTP).

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

use axum::{
    extract::{Path, Query, RawQuery, State},
    http::HeaderMap,
    Json,
};
use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

use ksi_auth::AuthUser;
use ksi_models::{
    CorrectionAchievementJson, CorrectionEvalJson, CorrectionFileJson, CorrectionGeneralJson,
    CorrectionInfoTaskJson, CorrectionInfoUserJson, CorrectionJson, CorrectionModuleJson,
    CorrectionModuleListJson, CorrectionPostJson, CorrectionResponse, CorrectionTaskJson,
    CorrectionThreadDetailsJson, CorrectionThreadJson, CorrectionUpdateRequest,
    CorrectionsAllResponse, CorrectionsEmailResponse, Wave, WaveJson,
};

use crate::{
    error::{ApiError, ApiResult},
    mail::{LogMailSender, MailMessage, MailSender},
    routes::years::wave_points,
    SharedState,
};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn require_org(user: &AuthUser) -> ApiResult<()> {
    // The Python backend answers 400 for non-orgs here (no JSON body);
    // the Rust port answers the same status with the standard error envelope.
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }
    Ok(())
}

/// The Python `Year_fill` middleware: `year` HTTP header, falling back to
/// the newest year in the DB.
async fn current_year(db: &sqlx::SqlitePool, headers: &HeaderMap) -> ApiResult<i64> {
    if let Some(v) = headers.get("year") {
        if let Ok(s) = v.to_str() {
            if let Ok(id) = s.trim().parse::<i64>() {
                return Ok(id);
            }
        }
    }
    sqlx::query_scalar::<_, Option<i64>>("SELECT id FROM years ORDER BY id DESC LIMIT 1")
        .fetch_optional(db)
        .await?
        .flatten()
        .ok_or_else(|| ApiError::Internal("V databázi není žádný ročník.".into()))
}

fn iso(dt: &Option<NaiveDateTime>) -> Option<String> {
    dt.map(|d| d.format("%Y-%m-%dT%H:%M:%S%.6f").to_string())
}

/// `id = task_id * 100000 + user_id` (see `util/correction.py::to_json`).
fn correction_id(task_id: i64, user_id: i64) -> i64 {
    task_id * 100000 + user_id
}

// ---------------------------------------------------------------------------
// shared evaluation rows
// ---------------------------------------------------------------------------

/// One evaluation row together with its module and task (the `corrs` query).
#[derive(Debug, Clone, sqlx::FromRow)]
struct CorrEvalRow {
    eval_id: i64,
    eval_user: i64,
    #[allow(dead_code)]
    eval_module: i64,
    evaluator: Option<i64>,
    points: Option<f64>,
    cheat: bool,
    full_report: Option<String>,
    eval_time: Option<NaiveDateTime>,
    module_id: i64,
    module_type: String,
    module_name: String,
    module_autocorrect: bool,
    module_max_points: Option<f64>,
    task_id: i64,
    task_title: String,
}

const CORR_EVAL_SELECT: &str = "SELECT e.id AS eval_id, e.\"user\" AS eval_user, \
    e.module AS eval_module, e.evaluator, CAST(e.points AS REAL) AS points, \
    e.cheat, e.full_report, e.time AS eval_time, \
    m.id AS module_id, m.type AS module_type, m.name AS module_name, \
    m.autocorrect AS module_autocorrect, CAST(m.max_points AS REAL) AS module_max_points, \
    t.id AS task_id, t.title AS task_title \
    FROM evaluations e \
    JOIN modules m ON m.id = e.module \
    JOIN tasks t ON t.id = m.task \
    JOIN waves w ON w.id = t.wave";

/// Submitted files grouped by evaluation id.
async fn files_by_eval(
    db: &sqlx::SqlitePool,
    eval_ids: &[i64],
) -> Result<HashMap<i64, Vec<(i64, String)>>, sqlx::Error> {
    let mut out: HashMap<i64, Vec<(i64, String)>> = HashMap::new();
    if eval_ids.is_empty() {
        return Ok(out);
    }
    let placeholders = eval_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT id, evaluation, path FROM submitted_files WHERE evaluation IN ({placeholders})"
    );
    let mut q = sqlx::query_as::<_, (i64, i64, String)>(&sql);
    for id in eval_ids {
        q = q.bind(id);
    }
    for (id, eval, path) in q.fetch_all(db).await? {
        out.entry(eval).or_default().push((id, path));
    }
    Ok(out)
}

fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// `util/correction.py::corr_eval_to_json`.
fn eval_to_json(row: &CorrEvalRow, files: &HashMap<i64, Vec<(i64, String)>>) -> CorrectionEvalJson {
    let general = if row.module_type == "general" {
        Some(CorrectionGeneralJson {
            files: files
                .get(&row.eval_id)
                .map(|v| {
                    v.iter()
                        .map(|(id, path)| CorrectionFileJson {
                            id: *id,
                            filename: basename(path),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
    } else {
        None
    };
    let empty_obj = |present: bool| present.then(|| serde_json::json!({}));
    CorrectionEvalJson {
        eval_id: row.eval_id,
        points: row.points.unwrap_or(0.0),
        last_modified: iso(&row.eval_time),
        corrected_by: row.evaluator,
        full_report: row.full_report.clone().unwrap_or_default(),
        cheat: row.cheat,
        general,
        programming: empty_obj(row.module_type == "programming"),
        quiz: empty_obj(row.module_type == "quiz"),
        sortable: empty_obj(row.module_type == "sortable"),
        text: empty_obj(row.module_type == "text"),
    }
}

/// Pick the evaluation shown for a module: the explicitly requested one, or
/// the max by `(points, time)` (Python: `sorted(evals, key=(points, time),
/// reverse=True)[0]`).
fn best_eval<'a>(rows: &[&'a CorrEvalRow]) -> &'a CorrEvalRow {
    rows.iter()
        .max_by(|a, b| {
            let pa = a.points.unwrap_or(0.0);
            let pb = b.points.unwrap_or(0.0);
            pa.partial_cmp(&pb)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.eval_time.cmp(&b.eval_time))
                .then_with(|| a.eval_id.cmp(&b.eval_id))
        })
        .expect("module has at least one evaluation")
}

/// `util/correction.py::to_json`.
#[allow(clippy::too_many_arguments)]
fn correction_to_json(
    task_id: i64,
    user_id: i64,
    rows: &[&CorrEvalRow],
    specific: &HashMap<i64, CorrEvalRow>,
    thread_id: Option<i64>,
    achievements: Vec<i64>,
    is_corrected: bool,
    files: &HashMap<i64, Vec<(i64, String)>>,
) -> CorrectionJson {
    // group evaluation rows by module, keeping first-seen module order
    let mut modules: Vec<(i64, Vec<&CorrEvalRow>)> = Vec::new();
    for r in rows {
        match modules.iter_mut().find(|(id, _)| *id == r.module_id) {
            Some((_, v)) => v.push(r),
            None => modules.push((r.module_id, vec![r])),
        }
    }
    let modules_json = modules
        .into_iter()
        .map(|(module_id, evals)| {
            let shown: &CorrEvalRow = match specific.get(&module_id) {
                Some(r) => r,
                None => best_eval(&evals),
            };
            CorrectionModuleJson {
                module_id,
                evaluations_list: evals.iter().map(|r| r.eval_id).collect(),
                evaluation: eval_to_json(shown, files),
            }
        })
        .collect();
    CorrectionJson {
        id: correction_id(task_id, user_id),
        task_id,
        state: if is_corrected { "corrected" } else { "notcorrected" }.into(),
        user: user_id,
        comment: thread_id,
        achievements,
        modules: modules_json,
    }
}

// ---------------------------------------------------------------------------
// GET /admin/corrections
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CorrectionsQuery {
    task: Option<i64>,
    participant: Option<i64>,
    state: Option<String>,
}

/// `GET /admin/corrections` — the corrections queue. Requires at least one
/// of `?task=` / `?participant=`.
pub async fn list_corrections(
    State(state): State<SharedState>,
    user: AuthUser,
    headers: HeaderMap,
    Query(q): Query<CorrectionsQuery>,
) -> ApiResult<Json<CorrectionsAllResponse>> {
    require_org(&user)?;
    let year = current_year(&state.db, &headers).await?;

    if q.task.is_none() && q.participant.is_none() {
        return Err(ApiError::BadRequest(
            "Je nutné vyplnit alespoň jeden z parametrů task nebo participant.".into(),
        ));
    }

    let mut sql = format!("{CORR_EVAL_SELECT} WHERE w.year = ?");
    if q.task.is_some() {
        sql.push_str(" AND t.id = ?");
    }
    if q.participant.is_some() {
        sql.push_str(" AND e.\"user\" = ?");
    }
    sql.push_str(" ORDER BY t.id, e.\"user\", m.id, e.id");
    let mut query = sqlx::query_as::<_, CorrEvalRow>(&sql).bind(year);
    if let Some(t) = q.task {
        query = query.bind(t);
    }
    if let Some(p) = q.participant {
        query = query.bind(p);
    }
    let rows: Vec<CorrEvalRow> = query.fetch_all(&state.db).await?;

    // group by (task, user); compute is_corrected like the Python `corrected`
    // subquery (count of evals with autocorrect or evaluator set > 0)
    let mut groups: BTreeMap<(i64, i64), Vec<&CorrEvalRow>> = BTreeMap::new();
    for r in &rows {
        groups.entry((r.task_id, r.eval_user)).or_default().push(r);
    }

    let keep = |is_corrected: bool| match q.state.as_deref() {
        Some("corrected") => is_corrected,
        Some("notcorrected") => !is_corrected,
        _ => true,
    };

    let mut all_eval_ids: Vec<i64> = Vec::new();
    let mut pairs: Vec<((i64, i64), bool)> = Vec::new();
    for ((task_id, user_id), evals) in &groups {
        let is_corrected = evals
            .iter()
            .any(|r| r.module_autocorrect || r.evaluator.is_some());
        if keep(is_corrected) {
            pairs.push(((*task_id, *user_id), is_corrected));
            all_eval_ids.extend(evals.iter().map(|r| r.eval_id));
        }
    }

    let files = files_by_eval(&state.db, &all_eval_ids).await?;

    // solution comment threads and achievements for the selected pairs
    let threads_rows: Vec<(i64, i64, i64)> =
        sqlx::query_as("SELECT \"user\", task, thread FROM solution_comments")
            .fetch_all(&state.db)
            .await?;
    let mut thread_of: HashMap<(i64, i64), i64> = HashMap::new();
    for (u, t, th) in threads_rows {
        thread_of.insert((t, u), th);
    }
    let ach_rows: Vec<(i64, i64, i64)> =
        sqlx::query_as("SELECT user_id, task_id, achievement_id FROM user_achievement")
            .fetch_all(&state.db)
            .await?;
    let mut ach_of: HashMap<(i64, i64), Vec<i64>> = HashMap::new();
    for (u, t, a) in ach_rows {
        if pairs.iter().any(|((pt, pu), _)| *pt == t && *pu == u) {
            ach_of.entry((t, u)).or_default().push(a);
        }
    }

    let corrections: Vec<CorrectionJson> = pairs
        .iter()
        .map(|((task_id, user_id), is_corrected)| {
            let evals = &groups[&(*task_id, *user_id)];
            correction_to_json(
                *task_id,
                *user_id,
                evals,
                &HashMap::new(),
                thread_of.get(&(*task_id, *user_id)).copied(),
                ach_of.remove(&(*task_id, *user_id)).unwrap_or_default(),
                *is_corrected,
                &files,
            )
        })
        .collect();

    // tasks / modules / achievements lists
    let mut task_ids: Vec<i64> = Vec::new();
    let mut task_titles: HashMap<i64, String> = HashMap::new();
    let mut module_seen: HashMap<i64, &CorrEvalRow> = HashMap::new();
    for ((task_id, user_id), _) in &pairs {
        if !task_ids.contains(task_id) {
            task_ids.push(*task_id);
        }
        for r in &groups[&(*task_id, *user_id)] {
            task_titles.entry(r.task_id).or_insert_with(|| r.task_title.clone());
            module_seen.entry(r.module_id).or_insert(*r);
        }
    }
    let tasks_json: Vec<CorrectionTaskJson> = task_ids
        .iter()
        .map(|id| CorrectionTaskJson {
            id: *id,
            title: task_titles[id].clone(),
        })
        .collect();
    let modules_json: Vec<CorrectionModuleListJson> = {
        let mut ms: Vec<(&i64, &&CorrEvalRow)> = module_seen.iter().collect();
        ms.sort_by_key(|(id, _)| **id);
        ms.iter()
            .map(|(_, r)| CorrectionModuleListJson {
                id: r.module_id,
                module_type: r.module_type.clone(),
                name: r.module_name.clone(),
                autocorrect: r.module_autocorrect,
                max_points: r.module_max_points.unwrap_or(0.0),
            })
            .collect()
    };

    let achievements: Vec<CorrectionAchievementJson> =
        sqlx::query_as::<_, (i64, String, String, Option<String>, Option<i64>)>(
            "SELECT id, title, picture, description, year FROM achievements WHERE year = ? ORDER BY id",
        )
        .bind(year)
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|(id, title, picture, description, ayear)| CorrectionAchievementJson {
            id,
            title,
            active: true,
            picture,
            description,
            persistent: ayear.is_none(),
            year: ayear,
        })
        .collect();

    // threads + posts + threadDetails
    let mut thread_set = std::collections::HashSet::new();
    for ((t, u), _) in &pairs {
        if let Some(th) = thread_of.get(&(*t, *u)) {
            thread_set.insert(*th);
        }
    }
    let mut thread_ids: Vec<i64> = thread_set.into_iter().collect();
    thread_ids.sort_unstable();

    let mut threads_json = Vec::new();
    let mut thread_details_json = Vec::new();
    let mut posts_json = Vec::new();
    if !thread_ids.is_empty() {
        let placeholders = thread_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let thread_rows: Vec<(i64, Option<String>, i64)> = {
            let sql = format!("SELECT id, title, year FROM threads WHERE id IN ({placeholders})");
            let mut qq = sqlx::query_as::<_, (i64, Option<String>, i64)>(&sql);
            for id in &thread_ids {
                qq = qq.bind(id);
            }
            qq.fetch_all(&state.db).await?
        };

        for (th_id, title, th_year) in &thread_rows {
            let posts_count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM posts WHERE thread = ?")
                    .bind(th_id)
                    .fetch_one(&state.db)
                    .await?;
            let unread = count_unread(&state.db, user.id, *th_id).await?;
            threads_json.push(CorrectionThreadJson {
                id: *th_id,
                title: title.clone(),
                unread,
                posts_count,
                details: *th_id,
                year: *th_year,
            });

            let root_posts: Vec<i64> = sqlx::query_scalar(
                "SELECT id FROM posts WHERE thread = ? AND parent IS NULL ORDER BY id",
            )
            .bind(th_id)
            .fetch_all(&state.db)
            .await?;
            thread_details_json.push(CorrectionThreadDetailsJson {
                id: *th_id,
                root_posts,
            });
        }

        // visits of the requesting user, for is_new
        let visits: Vec<(i64, Option<NaiveDateTime>, Option<NaiveDateTime>)> = {
            let sql = format!(
                "SELECT tv.thread, tv.last_visit, tv.last_last_visit FROM threads_visits tv \
                 JOIN threads th ON th.id = tv.thread \
                 WHERE tv.\"user\" = ? AND th.year = ? AND tv.thread IN ({placeholders})"
            );
            let mut qq = sqlx::query_as::<_, (i64, Option<NaiveDateTime>, Option<NaiveDateTime>)>(&sql)
                .bind(user.id)
                .bind(year);
            for id in &thread_ids {
                qq = qq.bind(id);
            }
            qq.fetch_all(&state.db).await?
        };
        let visit_of: HashMap<i64, (Option<NaiveDateTime>, Option<NaiveDateTime>)> = visits
            .into_iter()
            .map(|(th, lv, llv)| (th, (lv, llv)))
            .collect();

        let post_rows: Vec<PostRow> = {
            let sql = format!(
                "SELECT id, thread, author, body, published_at, parent FROM posts \
                 WHERE thread IN ({placeholders}) ORDER BY id"
            );
            let mut qq = sqlx::query_as::<_, PostRow>(&sql);
            for id in &thread_ids {
                qq = qq.bind(id);
            }
            qq.fetch_all(&state.db).await?
        };
        for p in &post_rows {
            let reactions: Vec<i64> =
                sqlx::query_scalar("SELECT id FROM posts WHERE parent = ? ORDER BY id")
                    .bind(p.id)
                    .fetch_all(&state.db)
                    .await?;
            let is_new = match visit_of.get(&p.thread) {
                None => true,
                Some((_, llv)) => match (llv, p.published_at) {
                    (None, _) => true,
                    (Some(llv), Some(pa)) => *llv < pa,
                    (Some(_), None) => false,
                },
            };
            posts_json.push(CorrectionPostJson {
                id: p.id,
                thread: p.thread,
                author: p.author,
                body: p.body.clone(),
                published_at: iso(&p.published_at),
                reaction: reactions,
                is_new,
            });
        }
    }

    Ok(Json(CorrectionsAllResponse {
        corrections,
        tasks: tasks_json,
        modules: modules_json,
        achievements,
        threads: threads_json,
        posts: posts_json,
        thread_details: thread_details_json,
    }))
}

#[derive(Debug, sqlx::FromRow)]
struct PostRow {
    id: i64,
    thread: i64,
    author: i64,
    body: String,
    published_at: Option<NaiveDateTime>,
    #[allow(dead_code)]
    parent: Option<i64>,
}

/// `util/thread.py::count_unread`.
async fn count_unread(
    db: &sqlx::SqlitePool,
    user_id: Option<i64>,
    thread_id: i64,
) -> Result<i64, sqlx::Error> {
    let user_id = match user_id {
        None => return Ok(0),
        Some(id) => id,
    };
    let visit: Option<(Option<NaiveDateTime>,)> = sqlx::query_as(
        "SELECT last_visit FROM threads_visits WHERE thread = ? AND \"user\" = ?",
    )
    .bind(thread_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    match visit {
        None => Ok(0),
        Some((None,)) => Ok(0),
        Some((Some(lv),)) => {
            sqlx::query_scalar("SELECT COUNT(*) FROM posts WHERE thread = ? AND published_at > ?")
                .bind(thread_id)
                .bind(lv)
                .fetch_one(db)
                .await
        }
    }
}

// ---------------------------------------------------------------------------
// GET /admin/corrections/{id}
// ---------------------------------------------------------------------------

/// Extract `module<digits>…` → eval id from the raw query string.
/// Python: `re.findall(r'\d+', param)` on the param *name*; the value is the
/// evaluation id (`moduleX_version=Y`).
fn parse_specific_evals(raw: Option<&str>) -> HashMap<i64, i64> {
    let mut out = HashMap::new();
    let raw = match raw {
        Some(r) => r,
        None => return out,
    };
    for pair in raw.split('&') {
        let (name, value) = match pair.split_once('=') {
            Some(p) => p,
            None => continue,
        };
        let mut digits = String::new();
        let mut in_run = false;
        for c in name.chars() {
            if c.is_ascii_digit() {
                digits.push(c);
                in_run = true;
            } else if in_run {
                break;
            }
        }
        if digits.is_empty() {
            continue;
        }
        let value = value.split('#').next().unwrap_or(value);
        if let (Ok(module_id), Ok(eval_id)) =
            (digits.parse::<i64>(), value.parse::<i64>())
        {
            out.insert(module_id, eval_id);
        }
    }
    out
}

/// `GET /admin/corrections/{id}` — one correction. `id` is artificial:
/// `task_id * 100000 + user_id`.
pub async fn get_correction(
    State(state): State<SharedState>,
    user: AuthUser,
    headers: HeaderMap,
    Path(id): Path<i64>,
    RawQuery(raw): RawQuery,
) -> ApiResult<Json<CorrectionResponse>> {
    require_org(&user)?;
    let year = current_year(&state.db, &headers).await?;
    let task_id = id.div_euclid(100000);
    let user_id = id.rem_euclid(100000);

    let sql = format!("{CORR_EVAL_SELECT} WHERE w.year = ? AND t.id = ? AND e.\"user\" = ? ORDER BY m.id, e.id");
    let rows: Vec<CorrEvalRow> = sqlx::query_as::<_, CorrEvalRow>(&sql)
        .bind(year)
        .bind(task_id)
        .bind(user_id)
        .fetch_all(&state.db)
        .await?;

    // 404 when there is no correction for this (task, user)
    if rows.is_empty() {
        return Err(ApiError::NotFound("Opravení neexistuje.".into()));
    }

    let specific_ids = parse_specific_evals(raw.as_deref());
    let mut specific: HashMap<i64, CorrEvalRow> = HashMap::new();
    let mut all_eval_ids: Vec<i64> = rows.iter().map(|r| r.eval_id).collect();
    for (module_id, eval_id) in &specific_ids {
        // Python fetches the requested evaluation by id with no ownership
        // check and shows it for the module when it exists.
        let row: Option<CorrEvalRow> = match rows.iter().find(|r| r.eval_id == *eval_id) {
            Some(r) => Some(r.clone()),
            None => {
                sqlx::query_as(&format!("{CORR_EVAL_SELECT} WHERE e.id = ?"))
                    .bind(eval_id)
                    .fetch_optional(&state.db)
                    .await?
            }
        };
        if let Some(r) = row {
            if !all_eval_ids.contains(&r.eval_id) {
                all_eval_ids.push(r.eval_id);
            }
            specific.insert(*module_id, r);
        }
    }
    let files = files_by_eval(&state.db, &all_eval_ids).await?;

    let thread_id: Option<i64> = sqlx::query_scalar(
        "SELECT thread FROM solution_comments WHERE \"user\" = ? AND task = ?",
    )
    .bind(user_id)
    .bind(task_id)
    .fetch_optional(&state.db)
    .await?;

    let achievements: Vec<i64> = sqlx::query_scalar(
        "SELECT achievement_id FROM user_achievement WHERE user_id = ? AND task_id = ?",
    )
    .bind(user_id)
    .bind(task_id)
    .fetch_all(&state.db)
    .await?;

    let is_corrected = rows
        .iter()
        .any(|r| r.module_autocorrect || r.evaluator.is_some());

    let refs: Vec<&CorrEvalRow> = rows.iter().collect();
    Ok(Json(CorrectionResponse {
        correction: correction_to_json(
            task_id,
            user_id,
            &refs,
            &specific,
            thread_id,
            achievements,
            is_corrected,
            &files,
        ),
    }))
}

// ---------------------------------------------------------------------------
// PUT /admin/corrections/{id}
// ---------------------------------------------------------------------------

/// `PUT /admin/corrections/{id}` — submit an org's correction, then return
/// the updated correction (same shape as GET).
pub async fn update_correction(
    State(state): State<SharedState>,
    user: AuthUser,
    headers: HeaderMap,
    Path(id): Path<i64>,
    RawQuery(raw): RawQuery,
    Json(body): Json<CorrectionUpdateRequest>,
) -> ApiResult<Json<CorrectionResponse>> {
    require_org(&user)?;
    let org_id = user.id.ok_or_else(|| ApiError::BadRequest("Nejste přihlášeni.".into()))?;
    let corr = body.correction;

    process_thread(&state.db, &corr).await?;
    process_achievements(&state.db, &corr).await?;
    for module in &corr.modules {
        process_evaluation(&state.db, &module.evaluation, org_id).await?;
    }

    // the response is the updated correction
    get_correction(State(state), user, headers, Path(id), RawQuery(raw)).await
}

/// `_process_thread`: link/unlink the discussion thread of the correction.
async fn process_thread(
    db: &sqlx::SqlitePool,
    corr: &ksi_models::CorrectionUpdate,
) -> ApiResult<()> {
    let curr: Option<i64> = sqlx::query_scalar(
        "SELECT thread FROM solution_comments WHERE \"user\" = ? AND task = ?",
    )
    .bind(corr.user)
    .bind(corr.task_id)
    .fetch_optional(db)
    .await?;

    match (corr.comment, curr) {
        (Some(thread), None) => {
            sqlx::query(
                "INSERT INTO solution_comments (thread, \"user\", task) VALUES (?, ?, ?)",
            )
            .bind(thread)
            .bind(corr.user)
            .bind(corr.task_id)
            .execute(db)
            .await?;
        }
        (None, Some(_)) => {
            sqlx::query("DELETE FROM solution_comments WHERE \"user\" = ? AND task = ?")
                .bind(corr.user)
                .bind(corr.task_id)
                .execute(db)
                .await?;
        }
        _ => {}
    }
    Ok(())
}

/// `_process_achievements`: replace the user's achievements for the task.
async fn process_achievements(
    db: &sqlx::SqlitePool,
    corr: &ksi_models::CorrectionUpdate,
) -> ApiResult<()> {
    let old: Vec<i64> = sqlx::query_scalar(
        "SELECT achievement_id FROM user_achievement WHERE user_id = ? AND task_id = ?",
    )
    .bind(corr.user)
    .bind(corr.task_id)
    .fetch_all(db)
    .await?;

    if old != corr.achievements {
        sqlx::query("DELETE FROM user_achievement WHERE user_id = ? AND task_id = ?")
            .bind(corr.user)
            .bind(corr.task_id)
            .execute(db)
            .await?;
        for a_id in &corr.achievements {
            sqlx::query(
                "INSERT INTO user_achievement (user_id, achievement_id, task_id) VALUES (?, ?, ?)",
            )
            .bind(corr.user)
            .bind(a_id)
            .bind(corr.task_id)
            .execute(db)
            .await?;
        }
    }
    Ok(())
}

/// `_process_evaluation`: store one module's evaluation.
async fn process_evaluation(
    db: &sqlx::SqlitePool,
    eval: &ksi_models::CorrectionEvaluationUpdate,
    org_id: i64,
) -> ApiResult<()> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM evaluations WHERE id = ?")
        .bind(eval.eval_id)
        .fetch_optional(db)
        .await?;
    if exists.is_none() {
        return Ok(()); // Python silently skips missing evaluations
    }

    let now = Utc::now().naive_utc();
    let now_str = now.format("%Y-%m-%d %H:%M:%S%.6f").to_string();
    let evaluator = eval.corrected_by.unwrap_or(org_id);
    let cheat = eval.cheat.unwrap_or(false);
    // Python formats the points with str(), e.g. 8.0 -> "8.0"
    let points_str = if eval.points.fract() == 0.0 {
        format!("{:.1}", eval.points)
    } else {
        format!("{}", eval.points)
    };
    let report_line = format!("{now_str} : edited by org {org_id} : {points_str} points\n");

    sqlx::query(
        "UPDATE evaluations SET points = ?, time = ?, evaluator = ?, \
         full_report = full_report || ?, cheat = ? WHERE id = ?",
    )
    .bind(eval.points)
    .bind(now)
    .bind(evaluator)
    .bind(report_line)
    .bind(cheat)
    .execute(db)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// GET /admin/correctionInfos
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct CorrectionInfosAllResponse {
    #[serde(rename = "correctionsInfos")]
    infos: Vec<CorrectionInfoTaskJson>,
    waves: Vec<WaveJson>,
    users: Vec<CorrectionInfoUserJson>,
}

fn wave_to_json(w: &Wave, sum_points: f64, tasks_cnt: i64) -> WaveJson {
    let now = Utc::now().naive_utc();
    WaveJson {
        id: w.id,
        index: w.index,
        year: w.year,
        caption: w.caption.clone(),
        public: w.time_published.map(|t| t <= now).unwrap_or(false),
        time_published: iso(&w.time_published),
        sum_points,
        tasks_cnt,
        garant: w.garant,
    }
}

/// Task ids where everything is corrected (mirrors
/// `util/correction.py::tasks_corrected`): tasks with no unevaluated,
/// non-autocorrect evaluations.
async fn tasks_corrected(db: &sqlx::SqlitePool) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT t.id FROM tasks t WHERE NOT EXISTS (
             SELECT 1 FROM evaluations e
             JOIN modules m ON m.id = e.module
             WHERE m.task = t.id AND e.evaluator IS NULL AND m.autocorrect = 0
         )",
    )
    .fetch_all(db)
    .await
}

fn corr_state(
    evaluation_public: bool,
    task_id: i64,
    evaluating: &[i64],
    corrected: &[i64],
) -> String {
    if evaluation_public {
        return "published".into();
    }
    if corrected.contains(&task_id) {
        return "corrected".into();
    }
    if evaluating.contains(&task_id) {
        return "working".into();
    }
    "base".into()
}

#[derive(Debug, sqlx::FromRow)]
struct InfoTaskRow {
    id: i64,
    title: String,
    wave: i64,
    author: Option<i64>,
    evaluation_public: bool,
}

/// `GET /admin/correctionInfos` — tasks, waves and users for the correction
/// UI filter.
pub async fn list_correction_infos(
    State(state): State<SharedState>,
    user: AuthUser,
    headers: HeaderMap,
) -> ApiResult<Json<CorrectionInfosAllResponse>> {
    require_org(&user)?;
    let year = current_year(&state.db, &headers).await?;

    let tasks: Vec<InfoTaskRow> = sqlx::query_as(
        "SELECT t.id, t.title, t.wave, t.author, t.evaluation_public \
         FROM tasks t JOIN waves w ON w.id = t.wave WHERE w.year = ? ORDER BY t.id",
    )
    .bind(year)
    .fetch_all(&state.db)
    .await?;

    let waves: Vec<Wave> = sqlx::query_as(
        "SELECT w.id, w.year, w.\"index\", w.caption, w.garant, w.time_published \
         FROM waves w WHERE w.year = ? \
         AND EXISTS (SELECT 1 FROM tasks t WHERE t.wave = w.id) ORDER BY w.id",
    )
    .bind(year)
    .fetch_all(&state.db)
    .await?;
    let mut waves_json = Vec::with_capacity(waves.len());
    for w in &waves {
        let (sum_points, tasks_cnt) = wave_points(&state.db, w.id).await?;
        waves_json.push(wave_to_json(w, sum_points, tasks_cnt));
    }

    // users active in the year (have evaluations) + all task authors
    let user_rows: Vec<(i64, String, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT DISTINCT u.id, u.first_name, u.last_name, u.role, u.sex, u.profile_picture \
         FROM users u \
         JOIN evaluations e ON e.\"user\" = u.id \
         JOIN modules m ON m.id = e.module \
         JOIN tasks t ON t.id = m.task \
         JOIN waves w ON w.id = t.wave \
         WHERE w.year = ? \
         UNION \
         SELECT u.id, u.first_name, u.last_name, u.role, u.sex, u.profile_picture \
         FROM users u JOIN tasks t ON t.author = u.id \
         ORDER BY 1",
    )
    .bind(year)
    .fetch_all(&state.db)
    .await?;
    let users_json: Vec<CorrectionInfoUserJson> = user_rows
        .into_iter()
        .map(|(id, first_name, last_name, role, sex, profile_picture)| {
            CorrectionInfoUserJson {
                id,
                first_name,
                last_name,
                role,
                profile_picture: profile_picture.filter(|p| {
                    !p.is_empty() && std::path::Path::new(p).is_file()
                }).map(|_| format!("/images/profile/{id}")),
                gender: sex,
            }
        })
        .collect();

    // (task, user) solver pairs in this year
    let solvers: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT t.id, e.\"user\" FROM tasks t \
         JOIN waves w ON w.id = t.wave \
         JOIN modules m ON m.task = t.id \
         JOIN evaluations e ON e.module = m.id \
         WHERE w.year = ? GROUP BY t.id, e.\"user\"",
    )
    .bind(year)
    .fetch_all(&state.db)
    .await?;

    // tasks with any evaluated (org-set) evaluation
    let evaluating: Vec<i64> = sqlx::query_scalar(
        "SELECT DISTINCT t.id FROM tasks t \
         JOIN modules m ON m.task = t.id \
         JOIN evaluations e ON e.module = m.id \
         WHERE e.evaluator IS NOT NULL",
    )
    .fetch_all(&state.db)
    .await?;

    let corrected = tasks_corrected(&state.db).await?;

    let infos: Vec<CorrectionInfoTaskJson> = tasks
        .iter()
        .map(|t| CorrectionInfoTaskJson {
            id: t.id,
            title: t.title.clone(),
            wave: t.wave,
            author: t.author,
            corr_state: corr_state(
                t.evaluation_public,
                t.id,
                &evaluating,
                &corrected,
            ),
            solvers: solvers
                .iter()
                .filter(|(tid, _)| *tid == t.id)
                .map(|(_, uid)| *uid)
                .collect(),
        })
        .collect();

    Ok(Json(CorrectionInfosAllResponse {
        infos,
        waves: waves_json,
        users: users_json,
    }))
}

// ---------------------------------------------------------------------------
// GET /admin/correctionInfos/{id}
// ---------------------------------------------------------------------------

/// `GET /admin/correctionInfos/{id}` — correction info for a single task.
pub async fn get_correction_info(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<ksi_models::CorrectionInfoResponse>> {
    require_org(&user)?;

    let task: Option<InfoTaskRow> = sqlx::query_as(
        "SELECT t.id, t.title, t.wave, t.author, t.evaluation_public \
         FROM tasks t WHERE t.id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    let task = match task {
        None => return Err(ApiError::NotFound("Úloha neexistuje.".into())),
        Some(t) => t,
    };

    let solvers: Vec<i64> = sqlx::query_scalar(
        "SELECT e.\"user\" FROM evaluations e \
         JOIN modules m ON m.id = e.module \
         WHERE m.task = ? GROUP BY e.\"user\"",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    let evaluating: bool = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT 1 FROM evaluations e \
         JOIN modules m ON m.id = e.module \
         WHERE e.evaluator IS NOT NULL AND m.task = ? LIMIT 1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .is_some();

    let corrected = tasks_corrected(&state.db).await?;
    let evaluating_list: Vec<i64> = if evaluating { vec![task.id] } else { vec![] };

    Ok(Json(ksi_models::CorrectionInfoResponse {
        corrections_info: CorrectionInfoTaskJson {
            id: task.id,
            title: task.title,
            wave: task.wave,
            author: task.author,
            corr_state: corr_state(
                task.evaluation_public,
                task.id,
                &evaluating_list,
                &corrected,
            ),
            solvers,
        },
    }))
}

// ---------------------------------------------------------------------------
// PUT /admin/correctionsEmail/{id}
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct EmailRecipient {
    user_id: i64,
    email: String,
    sex: String,
    points: Option<f64>,
}

/// `PUT /admin/correctionsEmail/{id}` — send the "your solution was
/// corrected" e-mails for one task (org only). E-mail is NOT really sent;
/// deliveries are logged via [`LogMailSender`] (TODO: wire up SMTP).
pub async fn send_corrections_email(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(task_id): Path<i64>,
) -> ApiResult<Json<CorrectionsEmailResponse>> {
    require_org(&user)?;
    let org_id = user.id.ok_or_else(|| ApiError::BadRequest("Nejste přihlášeni.".into()))?;

    let task: Option<(String, bool, Option<String>)> = sqlx::query_as(
        "SELECT title, evaluation_public, eval_comment FROM tasks WHERE id = ?",
    )
    .bind(task_id)
    .fetch_optional(&state.db)
    .await?;
    let (title, evaluation_public, eval_comment) = match task {
        None => return Err(ApiError::NotFound("Úloha neexistuje.".into())),
        Some(t) => t,
    };
    // Note: the Python backend sets 400 here but (missing `return`) keeps
    // sending; the port treats a non-published task as a hard error instead.
    if !evaluation_public {
        return Err(ApiError::BadRequest("Úloha nemá zveřejněné opravení.".into()));
    }

    // per-(user, module) max points, summed per user; recipients who opted
    // out of eval notifications are excluded
    let recipients: Vec<EmailRecipient> = sqlx::query_as(
        "SELECT u.id AS user_id, u.email, u.sex, SUM(pm.points) AS points \
         FROM users u \
         LEFT JOIN users_notify un ON un.\"user\" = u.id \
         JOIN (SELECT e.\"user\" AS eu, MAX(CAST(e.points AS REAL)) AS points \
               FROM evaluations e JOIN modules m ON m.id = e.module \
               WHERE m.task = ? GROUP BY e.\"user\", e.module) pm ON pm.eu = u.id \
         WHERE (un.notify_eval = 1 OR un.\"user\" IS NULL) \
         GROUP BY u.id",
    )
    .bind(task_id)
    .fetch_all(&state.db)
    .await?;

    let org: (String, String, String) = sqlx::query_as(
        "SELECT first_name, last_name, sex FROM users WHERE id = ?",
    )
    .bind(org_id)
    .fetch_optional(&state.db)
    .await?
    .unwrap_or_else(|| ("organizátor".into(), String::new(), "male".into()));

    // TODO: real SMTP delivery is not wired up; LogMailSender logs instead.
    let sender = LogMailSender::new("ksi");
    let mut errors: Vec<String> = Vec::new();

    for r in &recipients {
        // the org's comment on this participant's solution, if any
        let comment: Option<(String, String, String, Option<i64>)> = sqlx::query_as(
            "SELECT p.body, c.first_name, c.last_name, c.id \
             FROM solution_comments sc \
             JOIN posts p ON p.thread = sc.thread \
             JOIN users c ON c.id = p.author \
             WHERE sc.\"user\" = ? AND sc.task = ? \
             ORDER BY p.published_at DESC LIMIT 1",
        )
        .bind(r.user_id)
        .bind(task_id)
        .fetch_optional(&state.db)
        .await?;

        let body = email_body(&title, r, &comment, eval_comment.as_deref());
        let msg = MailMessage {
            from: "ksi".into(),
            to: vec![r.email.clone()],
            cc: vec![],
            bcc: vec![],
            subject: format!("KSI Úloha {title} opravena"),
            body_html: body,
            reply_to: None,
        };
        if let Err(e) = sender.send(&msg) {
            errors.push(e.to_string());
        }
    }

    // info e-mail to the org conference
    let conf_body = format!(
        "<p>Úloha {title} je opravena. {} {} právě odeslal{} informační e-mail {} řešitelům.</p>",
        org.0,
        org.1,
        if org.2 == "female" { "a" } else { "" },
        recipients.len(),
    ) + &eval_comment
        .as_deref()
        .map(|c| format!("<p>Společný komentář ke všem opravením:</p> {c}"))
        .unwrap_or_default();
    if let Err(e) = sender.send(&MailMessage {
        from: "ksi".into(),
        to: vec!["ksi-conference".into()],
        cc: vec![],
        bcc: vec![],
        subject: format!("KSI Úloha {title} opravena"),
        body_html: conf_body,
        reply_to: None,
    }) {
        errors.push(e.to_string());
    }

    Ok(Json(CorrectionsEmailResponse {
        count: recipients.len(),
        errors: if errors.is_empty() { None } else { Some(errors) },
    }))
}

/// Faithful Czech body of the per-recipient e-mail (see
/// `endpoint/admin/correctionsEmail.py::_send_single_email`).
fn email_body(
    task_title: &str,
    r: &EmailRecipient,
    comment: &Option<(String, String, String, Option<i64>)>,
    eval_comment: Option<&str>,
) -> String {
    let mut body = format!("<p>Ahoj,<br>opravili jsme tvé řešení úlohy {task_title}.</p>");
    let points = r.points.unwrap_or(0.0);
    if r.sex == "female" {
        body += &format!("<a>Získala jsi <strong>{points:.1} bodů</strong>.</p>");
    } else {
        body += &format!("<a>Získal jsi <strong>{points:.1} bodů</strong>.</p>");
    }
    match comment {
        Some((post_body, first, last, commenter_id)) => {
            let id = commenter_id.unwrap_or(0);
            body += &format!(
                "<p><a href=\"/profil/{id}\"><i>{first} {last}</i></a> komentuje \
                 tvé řešení:</p> {post_body}"
            );
        }
        None => body += "<p>K tvému řešení nebyl přidán žádný komentář.</p>",
    }
    if let Some(c) = eval_comment {
        if !c.is_empty() {
            body += &format!("<p>Společný komentář ke všem opravením:</p> {c}");
        }
    }
    body += "<p>Můžeš si prohlédnout <a href=\"/#vysledky\">výsledkovku</a>, ";
    body += "<p>--<br>Tým KSI</p>";
    body
}

// ---------------------------------------------------------------------------
// GET /admin/corrections/{id}/publish
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct PublishQuery {
    /// `1`/`0` (also accepts true/false); absent → publish.
    public: Option<String>,
}

/// `GET /admin/corrections/{id}/publish` — (un)publish a task's corrections.
pub async fn publish_corrections(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(task_id): Path<i64>,
    Query(q): Query<PublishQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    require_org(&user)?;

    let public = match q.public.as_deref() {
        None => true,
        Some(s) => matches!(s.to_lowercase().as_str(), "1" | "true" | "yes"),
    };

    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM tasks WHERE id = ?")
        .bind(task_id)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
        return Err(ApiError::NotFound("Úloha neexistuje.".into()));
    }

    sqlx::query("UPDATE tasks SET evaluation_public = ? WHERE id = ?")
        .bind(public)
        .bind(task_id)
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({})))
}
