//! Admin task management endpoints (`GET/POST /admin/atasks`,
//! `/admin/atasks/{id}`, `/deploy`, `/merge`, `/admin/waves/{id}/diff`).
//!
//! Faithful port of `endpoint/admin/task.py`, `taskDeploy.py`, `taskMerge.py`
//! and `waveDiff.py` (plus `util/admin/task.py::createGit` / `admin_to_json`
//! and `generate_icon_variants`). Git operations shell out to
//! the `git` binary via `tokio::process::Command`, mirroring Python's
//! subprocess/GitPython usage; failures become [`ApiError`], never panics.
//!
//! The heavy content-processing half of a deploy (task.json / assignment /
//! modules) lives in [`super::atask_deploy`].

use std::path::{Path, PathBuf};

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::HeaderMap,
    Json,
};
use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use ksi_auth::AuthUser;
use ksi_models::AdminTaskJson;

use super::atask_deploy::{self, DeployCtx, DeployLog, DeployPaths};
use crate::{
    error::{ApiError, ApiResult},
    SharedState,
};

// ---------------------------------------------------------------------------
// Request / response shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub wave: Option<i64>,
    pub fetch_testers: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct FetchTestersQuery {
    pub fetch_testers: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct AtaskEnvelope<T> {
    pub atask: T,
}

#[derive(Debug, Deserialize)]
pub struct CreateAtaskData {
    wave: i64,
    title: String,
    author: i64,
    git_path: String,
    git_branch: String,
    git_commit: Option<String>,
    git_create: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAtaskData {
    wave: i64,
    title: String,
    git_path: String,
    git_branch: String,
    git_commit: String,
    eval_comment: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AtasksResponse {
    pub atasks: Vec<AdminTaskJson>,
}

#[derive(Debug, Serialize)]
pub struct AtaskResponse {
    pub atask: AdminTaskJson,
}

#[derive(Debug, Serialize)]
pub struct DeployStatusResponse {
    pub id: i64,
    pub log: Option<String>,
    pub deploy_date: Option<String>,
    pub deploy_status: String,
}

#[derive(Debug, sqlx::FromRow)]
struct AdminTaskRow {
    id: i64,
    title: String,
    wave: i64,
    author: Option<i64>,
    co_author: Option<i64>,
    git_path: Option<String>,
    git_branch: Option<String>,
    git_commit: Option<String>,
    git_pull_id: Option<i64>,
    deploy_date: Option<NaiveDateTime>,
    deploy_status: String,
    eval_comment: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct WaveRow {
    id: i64,
    year: i64,
    garant: i64,
    time_published: Option<NaiveDateTime>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn iso(dt: &Option<NaiveDateTime>) -> Option<String> {
    dt.map(|d| d.format("%Y-%m-%dT%H:%M:%S%.6f").to_string())
}

/// Current year: `YEAR` header wins, otherwise the newest year
/// (mirrors `Year_fill` in app.py).
async fn current_year(db: &SqlitePool, headers: &HeaderMap) -> ApiResult<i64> {
    if let Some(v) = headers.get("YEAR").and_then(|v| v.to_str().ok()) {
        if let Ok(id) = v.parse::<i64>() {
            return Ok(id);
        }
    }
    sqlx::query_scalar("SELECT id FROM years ORDER BY id DESC LIMIT 1")
        .fetch_one(db)
        .await
        .map_err(|_| ApiError::Internal("Nenalezen žádný ročník.".into()))
}

fn require_org(user: &AuthUser) -> ApiResult<()> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest("Přístup odepřen.".into()));
    }
    Ok(())
}

async fn get_task_row(db: &SqlitePool, id: i64) -> ApiResult<AdminTaskRow> {
    sqlx::query_as(
        "SELECT id, title, wave, author, co_author, git_path, git_branch,
                git_commit, git_pull_id, deploy_date, deploy_status, eval_comment
         FROM tasks WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| ApiError::NotFound("Úloha s tímto ID neexistuje.".into()))
}

async fn get_wave_row(db: &SqlitePool, id: i64) -> ApiResult<WaveRow> {
    sqlx::query_as("SELECT id, year, garant, time_published FROM waves WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Vlna s tímto ID neexistuje.".into()))
}

/// `util.task.max_points` for one task (bonus modules excluded).
async fn task_max_points(db: &SqlitePool, task_id: i64) -> f64 {
    let sum: Option<f64> = sqlx::query_scalar(
        "SELECT CAST(SUM(max_points) AS REAL) FROM modules WHERE task = ? AND bonus = 0",
    )
    .bind(task_id)
    .fetch_optional(db)
    .await
    .unwrap_or(None)
    .flatten();
    sum.unwrap_or(0.0)
}

/// Mirrors `util/admin/task.py::admin_to_json`.
///
/// `testers`/`additional_testers` stay empty: the Python implementation fills
/// them from the GitHub API, which the Rust port deliberately does not call.
async fn admin_task_to_json(db: &SqlitePool, row: &AdminTaskRow) -> AdminTaskJson {
    let mp = task_max_points(db, row.id).await;
    AdminTaskJson {
        id: row.id,
        title: row.title.clone(),
        wave: row.wave,
        author: row.author,
        co_author: row.co_author,
        testers: Vec::new(),
        git_path: row.git_path.clone(),
        git_branch: row.git_branch.clone(),
        git_commit: row.git_commit.clone(),
        git_pull_id: row.git_pull_id,
        deploy_date: iso(&row.deploy_date),
        deploy_status: row.deploy_status.clone(),
        // Python: float(format(amax_points, '.1f'))
        max_score: (mp * 10.0).round() / 10.0,
        eval_comment: row.eval_comment.clone(),
        additional_testers: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Git + lock plumbing (mirrors util/git.py, util/lock.py)
// ---------------------------------------------------------------------------

/// Exclusive lock file (mirrors `lockfile.LockFile`). Removed on drop.
pub struct FileLock {
    path: PathBuf,
}

impl FileLock {
    pub fn acquire(path: PathBuf) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        Ok(Self { path })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Mirrors `util.lock.git_locked`: returns the first held git lock, if any.
fn git_locked(paths: &DeployPaths) -> Option<PathBuf> {
    for lock in [
        paths.task_deploy_lock(),
        paths.wave_diff_lock(),
        paths.task_merge_lock(),
        paths.task_new_lock(),
    ] {
        if lock.exists() {
            return Some(lock);
        }
    }
    None
}

fn git_locked_error(paths: &DeployPaths) -> ApiError {
    let lock = git_locked(paths).map(|p| p.display().to_string()).unwrap_or_default();
    ApiError::Conflict(format!(
        "GIT uzamcen zamkem {lock}\nNekdo momentalne provadi akci s gitem, opakujte prosim akci za 20 sekund."
    ))
}

/// Run `git <args>` in the seminar checkout (mirrors Python's subprocess /
/// GitPython usage). Git failures become `ApiError`, never panics.
async fn git(seminar: &Path, args: &[&str]) -> Result<String, ApiError> {
    let out = tokio::process::Command::new("git")
        .current_dir(seminar)
        .args(args)
        .output()
        .await
        .map_err(|e| ApiError::Internal(format!("git se nepodařilo spustit: {e}")))?;
    if !out.status.success() {
        return Err(ApiError::Internal(format!(
            "git {} selhalo: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// ---------------------------------------------------------------------------
// GET /admin/atasks — list tasks for admin
// ---------------------------------------------------------------------------

/// `GET /admin/atasks` — admin task list, optionally filtered by `?wave=`.
pub async fn list_atasks(
    State(state): State<SharedState>,
    user: AuthUser,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<AtasksResponse>> {
    require_org(&user)?;
    let _fetch_testers = q.fetch_testers.unwrap_or(false);

    let rows: Vec<AdminTaskRow> = if let Some(wave) = q.wave {
        sqlx::query_as(
            "SELECT t.id, t.title, t.wave, t.author, t.co_author, t.git_path, t.git_branch,
                    t.git_commit, t.git_pull_id, t.deploy_date, t.deploy_status, t.eval_comment
             FROM tasks t WHERE t.wave = ? ORDER BY t.id",
        )
        .bind(wave)
        .fetch_all(&state.db)
        .await?
    } else {
        let year = current_year(&state.db, &headers).await?;
        sqlx::query_as(
            "SELECT t.id, t.title, t.wave, t.author, t.co_author, t.git_path, t.git_branch,
                    t.git_commit, t.git_pull_id, t.deploy_date, t.deploy_status, t.eval_comment
             FROM tasks t JOIN waves w ON w.id = t.wave
             WHERE w.year = ? ORDER BY t.id",
        )
        .bind(year)
        .fetch_all(&state.db)
        .await?
    };

    let mut atasks = Vec::with_capacity(rows.len());
    for row in &rows {
        atasks.push(admin_task_to_json(&state.db, row).await);
    }
    Ok(Json(AtasksResponse { atasks }))
}

// ---------------------------------------------------------------------------
// POST /admin/atasks — create task
// ---------------------------------------------------------------------------

/// Mirrors `util/admin/task.py::createGit`: creates the task branch from
/// master, copies the task template, commits and pushes.
///
/// The GitHub pull-request creation is skipped: the Rust port has no GitHub
/// token configured, so `git_pull_id` stays NULL.
async fn create_git(
    paths: &DeployPaths,
    git_path: &str,
    git_branch: &str,
    author_id: i64,
    title: &str,
) -> Result<(Option<String>, Option<i64>), ApiError> {
    let seminar = &paths.seminar;
    git(seminar, &["checkout", "master"]).await?;
    git(seminar, &["pull", "origin"]).await?;

    // Create the new branch.
    git(seminar, &["checkout", "-b", git_branch, "HEAD"]).await?;

    // Copy the template task (target must not exist, like shutil.copytree).
    let template = seminar.join("task-plain");
    let target = seminar.join(git_path);
    if target.exists() {
        return Err(ApiError::BadRequest(format!(
            "Adresář {git_path} v repozitáři již existuje."
        )));
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ApiError::Internal(format!("cannot create directory: {e}")))?;
    }
    atask_deploy::copy_dir_all(&template, &target)
        .map_err(|e| ApiError::Internal(format!("cannot copy task template: {e}")))?;

    // Pre-process repository data: task.json author + assignment.md title.
    let task_json_path = target.join("task.json");
    let raw = std::fs::read_to_string(&task_json_path)
        .map_err(|e| ApiError::Internal(format!("cannot read task.json: {e}")))?;
    let mut data: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| ApiError::Internal(format!("cannot parse task.json: {e}")))?;
    data["author"] = serde_json::json!(author_id);
    std::fs::write(
        &task_json_path,
        serde_json::to_string_pretty(&data).unwrap_or_default(),
    )
    .map_err(|e| ApiError::Internal(format!("cannot write task.json: {e}")))?;

    std::fs::write(
        target.join("assignment.md"),
        format!(
            "{title}.\n\n# {title}\n\nNázev úlohy musí být uvozen `#`, nikoliv podtrhnut rovnítky.\n\n"
        ),
    )
    .map_err(|e| ApiError::Internal(format!("cannot write assignment.md: {e}")))?;

    git(seminar, &["add", git_path]).await?;
    git(seminar, &["commit", "-m", &format!("Nova uloha: {title}")]).await?;
    git(seminar, &["push", "-u", "origin", &format!("{git_branch}:{git_branch}")]).await?;

    let commit = git(seminar, &["rev-parse", "HEAD"]).await?;
    Ok((Some(commit), None))
}

/// `POST /admin/atasks` — create a new task (admins or the wave garant,
/// only before the wave is published).
pub async fn create_atask(
    State(state): State<SharedState>,
    user: AuthUser,
    headers: HeaderMap,
    Json(body): Json<AtaskEnvelope<CreateAtaskData>>,
) -> ApiResult<Json<AtaskResponse>> {
    let data = body.atask;
    let wave = get_wave_row(&state.db, data.wave).await?;

    // Only admins or the wave garant may create tasks.
    if !user.is_logged_in() || (!user.is_admin() && user.id != Some(wave.garant)) {
        return Err(ApiError::Unauthorized("Nedostatečná oprávnění.".into()));
    }

    // Tasks can only be created before the wave is published.
    let now = Utc::now().naive_utc();
    if wave.time_published.map(|tp| now > tp).unwrap_or(false) {
        return Err(ApiError::Forbidden(
            "Po zverejneni vlny nelze vytvaret nove ulohy".into(),
        ));
    }

    let paths = DeployPaths::from_env();
    let (git_commit, github_pull_id): (Option<String>, Option<i64>) =
        if data.git_create.unwrap_or(false) {
        if git_locked(&paths).is_some() {
            return Err(git_locked_error(&paths));
        }
        let _lock = FileLock::acquire(paths.task_new_lock()).map_err(|_| git_locked_error(&paths))?;
        create_git(&paths, &data.git_path, &data.git_branch, data.author, &data.title).await?
        // `_lock` is released here (Drop).
    } else {
        (data.git_commit.clone(), None)
    };

    // First create the discussion thread, then the task.
    let year = current_year(&state.db, &headers).await?;
    let thread_id: i64 = sqlx::query_scalar(
        "INSERT INTO threads (title, public, year) VALUES (?, 1, ?) RETURNING id",
    )
    .bind(&data.title)
    .bind(year)
    .fetch_one(&state.db)
    .await?;

    let task_id: i64 = sqlx::query_scalar(
        "INSERT INTO tasks (wave, title, author, git_path, git_branch, git_commit,
                            git_pull_id, thread, intro, body, evaluation_public, deploy_status)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, '', '', 0, 'default') RETURNING id",
    )
    .bind(data.wave)
    .bind(&data.title)
    .bind(&data.git_path)
    .bind(&data.git_branch)
    .bind(git_commit)
    .bind(github_pull_id)
    .bind(thread_id)
    .fetch_one(&state.db)
    .await?;

    let row = get_task_row(&state.db, task_id).await?;
    Ok(Json(AtaskResponse {
        atask: admin_task_to_json(&state.db, &row).await,
    }))
}

// ---------------------------------------------------------------------------
// GET /admin/atasks/{id} — task detail for admin
// ---------------------------------------------------------------------------

/// `GET /admin/atasks/{id}` — admin detail of one task.
pub async fn get_atask(
    State(state): State<SharedState>,
    user: AuthUser,
    AxumPath(id): AxumPath<i64>,
    Query(q): Query<FetchTestersQuery>,
) -> ApiResult<Json<AtaskResponse>> {
    require_org(&user)?;
    let _fetch_testers = q.fetch_testers.unwrap_or(false);
    let row = get_task_row(&state.db, id).await?;
    Ok(Json(AtaskResponse {
        atask: admin_task_to_json(&state.db, &row).await,
    }))
}

// ---------------------------------------------------------------------------
// PUT /admin/atasks/{id} — update task
// ---------------------------------------------------------------------------

/// `PUT /admin/atasks/{id}` — update a task (admins or the wave garant,
/// only before the wave is published for non-admins).
pub async fn update_atask(
    State(state): State<SharedState>,
    user: AuthUser,
    AxumPath(id): AxumPath<i64>,
    Json(body): Json<AtaskEnvelope<UpdateAtaskData>>,
) -> ApiResult<Json<AtaskResponse>> {
    let data = body.atask;
    let wave = get_wave_row(&state.db, data.wave).await?;

    if !user.is_logged_in() || (!user.is_admin() && user.id != Some(wave.garant)) {
        return Err(ApiError::BadRequest("Přístup odepřen.".into()));
    }

    let task = get_task_row(&state.db, id).await?;

    // Tasks can only be edited before the wave's publication (admins exempt).
    let task_wave = get_wave_row(&state.db, task.wave).await?;
    let now = Utc::now().naive_utc();
    if task_wave.time_published.map(|tp| now > tp).unwrap_or(false) && !user.is_admin() {
        return Err(ApiError::Forbidden(
            "Po zveřejnění vlny nelze úlohu upravovat.".into(),
        ));
    }

    let eval_comment = data.eval_comment.as_ref().map(|c| {
        if c.is_empty() {
            None
        } else {
            Some(c.clone())
        }
    });

    if data.eval_comment.is_some() {
        // An explicitly provided eval_comment (even "") overwrites;
        // "" clears the field to NULL (mirrors Python).
        sqlx::query(
            "UPDATE tasks SET title = ?, git_path = ?, git_branch = ?, git_commit = ?,
                              eval_comment = ? WHERE id = ?",
        )
        .bind(&data.title)
        .bind(&data.git_path)
        .bind(&data.git_branch)
        .bind(&data.git_commit)
        .bind(eval_comment.flatten())
        .bind(id)
        .execute(&state.db)
        .await?;
    } else {
        sqlx::query(
            "UPDATE tasks SET title = ?, git_path = ?, git_branch = ?, git_commit = ? WHERE id = ?",
        )
        .bind(&data.title)
        .bind(&data.git_path)
        .bind(&data.git_branch)
        .bind(&data.git_commit)
        .bind(id)
        .execute(&state.db)
        .await?;
    }

    let row = get_task_row(&state.db, id).await?;
    Ok(Json(AtaskResponse {
        atask: admin_task_to_json(&state.db, &row).await,
    }))
}

// ---------------------------------------------------------------------------
// DELETE /admin/atasks/{id} — delete task
// ---------------------------------------------------------------------------

/// `DELETE /admin/atasks/{id}` — delete a task incl. modules, evaluations,
/// executions, thread and prerequisity (admins only, before publication).
pub async fn delete_atask(
    State(state): State<SharedState>,
    user: AuthUser,
    AxumPath(id): AxumPath<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    // Only admins may delete tasks.
    if !user.is_logged_in() || !user.is_admin() {
        return Err(ApiError::BadRequest("Přístup odepřen.".into()));
    }

    let task = get_task_row(&state.db, id).await?;

    // Tasks can only be deleted before the wave is published.
    let wave = get_wave_row(&state.db, task.wave).await?;
    let now = Utc::now().naive_utc();
    if wave.time_published.map(|tp| now > tp).unwrap_or(false) {
        return Err(ApiError::Forbidden(
            "Po zveřejnění vlny nelze úlohu smazat.".into(),
        ));
    }

    let thread_id: Option<i64> = sqlx::query_scalar("SELECT thread FROM tasks WHERE id = ?")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    let prereq_id: Option<i64> = sqlx::query_scalar("SELECT prerequisite FROM tasks WHERE id = ?")
        .bind(id)
        .fetch_one(&state.db)
        .await?;

    sqlx::query("DELETE FROM code_executions WHERE module IN (SELECT id FROM modules WHERE task = ?)")
        .bind(id)
        .execute(&state.db)
        .await?;
    sqlx::query("DELETE FROM evaluations WHERE module IN (SELECT id FROM modules WHERE task = ?)")
        .bind(id)
        .execute(&state.db)
        .await?;
    sqlx::query("DELETE FROM modules WHERE task = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    if let Some(pr) = prereq_id {
        // Delete the whole prerequisity subtree.
        delete_prereq_tree(&state.db, pr).await?;
        sqlx::query("UPDATE tasks SET prerequisite = NULL WHERE id = ?")
            .bind(id)
            .execute(&state.db)
            .await?;
    }
    sqlx::query("DELETE FROM tasks WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;
    if let Some(th) = thread_id {
        sqlx::query("DELETE FROM threads WHERE id = ?")
            .bind(th)
            .execute(&state.db)
            .await?;
    }

    Ok(Json(serde_json::json!({})))
}

fn delete_prereq_tree<'a>(
    db: &'a SqlitePool,
    root: i64,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ApiResult<()>> + Send + 'a>> {
    Box::pin(async move {
        let children: Vec<i64> = sqlx::query_scalar("SELECT id FROM prerequisities WHERE parent = ?")
            .bind(root)
            .fetch_all(db)
            .await?;
        for child in children {
            delete_prereq_tree(db, child).await?;
        }
        sqlx::query("DELETE FROM prerequisities WHERE id = ?")
            .bind(root)
            .execute(db)
            .await?;
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// POST /admin/atasks/{id}/deploy — deploy task
// ---------------------------------------------------------------------------

/// `POST /admin/atasks/{id}/deploy` — start a background deploy of the task
/// from its git branch (orgs; after publication only admins/the garant).
pub async fn deploy_atask(
    State(state): State<SharedState>,
    user: AuthUser,
    AxumPath(id): AxumPath<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest("Nedostatecna opravneni".into()));
    }

    let task = get_task_row(&state.db, id).await.map_err(|e| match e {
        ApiError::NotFound(_) => ApiError::NotFound("Neexistujici uloha".into()),
        other => other,
    })?;

    // Published tasks may only be deployed by admins and the wave garant.
    let wave = get_wave_row(&state.db, task.wave).await?;
    let now = Utc::now().naive_utc();
    if wave.time_published.map(|tp| now > tp).unwrap_or(false)
        && !user.is_admin()
        && user.id != Some(wave.garant)
    {
        // NOTE: Python answers HTTP_404 here; mirrored.
        return Err(ApiError::NotFound(
            "Po zverejneni ulohy muze deploy provest pouze administrator nebo garant vlny.".into(),
        ));
    }

    if task.git_branch.is_none() || task.git_path.is_none() {
        return Err(ApiError::BadRequest(
            "Uloha nema zadanou gitovskou vetev nebo adresar".into(),
        ));
    }

    let paths = DeployPaths::from_env();
    if git_locked(&paths).is_some() {
        return Err(git_locked_error(&paths));
    }

    sqlx::query("UPDATE tasks SET deploy_status = 'deploying', deploy_date = ? WHERE id = ?")
        .bind(now)
        .bind(id)
        .execute(&state.db)
        .await?;

    let lock = FileLock::acquire(paths.task_deploy_lock()).map_err(|_| git_locked_error(&paths))?;
    let year_id = wave.year;
    // The lock is held for the whole background deploy and released on drop.
    tokio::spawn(run_deploy(state, id, year_id, lock));

    Ok(Json(serde_json::json!({})))
}

/// Background deploy worker (mirrors `util/admin/taskDeploy.py::deploy`).
async fn run_deploy(state: SharedState, task_id: i64, year_id: i64, _lock: FileLock) {
    let paths = DeployPaths::from_env();
    let log = match DeployLog::create(&paths.deploy_log(), task_id) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("cannot create deploy log: {e}");
            set_deploy_status(&state.db, task_id, "error").await;
            return;
        }
    };

    if let Err(e) = deploy_inner(&state, task_id, year_id, &paths, &log).await {
        log.log(&format!("Exception: {e}"));
        set_deploy_status(&state.db, task_id, "error").await;
    }
    log.log("Done");
}

async fn set_deploy_status(db: &SqlitePool, task_id: i64, status: &str) {
    let _ = sqlx::query("UPDATE tasks SET deploy_status = ? WHERE id = ?")
        .bind(status)
        .bind(task_id)
        .execute(db)
        .await;
}

async fn deploy_inner(
    state: &SharedState,
    task_id: i64,
    year_id: i64,
    paths: &DeployPaths,
    log: &DeployLog,
) -> Result<(), atask_deploy::DeployFailure> {
    let fail = |m: String| atask_deploy::DeployFailure(m);

    let task = get_task_row(&state.db, task_id)
        .await
        .map_err(|e| fail(format!("task lookup failed: {e:?}")))?;
    let git_branch = task.git_branch.clone().ok_or_else(|| fail("missing git_branch".into()))?;
    let git_path = task.git_path.clone().ok_or_else(|| fail("missing git_path".into()))?;

    let now = Utc::now().naive_utc();
    sqlx::query("UPDATE tasks SET deploy_status = 'deploying', deploy_date = ? WHERE id = ?")
        .bind(now)
        .bind(task_id)
        .execute(&state.db)
        .await
        .map_err(|e| fail(format!("db error: {e}")))?;

    let seminar = &paths.seminar;

    log.log("Fetching origin...");
    git(seminar, &["fetch", "origin"])
        .await
        .map_err(|e| fail(format!("git fetch failed: {e:?}")))?;

    log.log(&format!("Checking out {git_branch}"));
    git(seminar, &["checkout", &git_branch])
        .await
        .map_err(|e| fail(format!("git checkout failed: {e:?}")))?;

    log.log(&format!("Hard-reseting to origin/{git_branch}"));
    git(seminar, &["reset", "--hard", &format!("origin/{git_branch}")])
        .await
        .map_err(|e| fail(format!("git reset failed: {e:?}")))?;

    let task_dir = seminar.join(&git_path);
    if !task_dir.is_dir() {
        log.log("Repo dir does not exist");
        set_deploy_status(&state.db, task_id, "error").await;
        return Ok(());
    }

    // Point-pad bookkeeping (mirrors Python; the actual point_pad update is
    // disabled upstream, so this only logs).
    let max_points_before = task_max_points(&state.db, task_id).await;
    let point_pad: Option<f64> =
        sqlx::query_scalar("SELECT CAST(point_pad AS REAL) FROM years WHERE id = ?")
            .bind(year_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| fail(format!("db error: {e}")))?
            .flatten();
    log.log(&format!("Current task max points: {max_points_before}"));
    log.log(&format!("Current year point pad: {}", point_pad.unwrap_or(0.0)));

    log.log(&format!("Parsing {}", task_dir.display()));
    let mut ctx = DeployCtx {
        db: &state.db,
        paths: paths.clone(),
        backend_url: atask_deploy::backend_url(),
        log: log.clone(),
        eval_public: true,
        mangled_datadir: String::new(),
        mangled_soldir: String::new(),
    };
    atask_deploy::process_task(&mut ctx, task_id, &task_dir)
        .await
        .map_err(|e| {
            log.log(&format!("Task processing failed: {e}"));
            e
        })?;

    let max_points_now = task_max_points(&state.db, task_id).await;
    let diff = max_points_before - max_points_now;
    log.log(&format!(
        "New task max points: {max_points_now} (before {max_points_before}, diff {diff})"
    ));
    if diff == 0.0 {
        log.log("Point diff is zero, no change is necessary");
    } else if point_pad.unwrap_or(0.0) == 0.0 {
        log.log("The year's point pad is already zero, not modifying it");
    } else {
        // NOTE: Python has the actual point_pad update commented out
        // ("TODO disabled as it conflicts that the point pad is the maximum
        // of the points"), so we only log here as well.
        log.log(&format!(
            "Would set the year's point pad to {} (disabled upstream)",
            (point_pad.unwrap_or(0.0) + diff).max(0.0)
        ));
    }

    // Update git entries in the DB.
    let head_sha = git(seminar, &["rev-parse", "HEAD"])
        .await
        .map_err(|e| fail(format!("git rev-parse failed: {e:?}")))?;
    let time_deadline: Option<NaiveDateTime> =
        sqlx::query_scalar("SELECT time_deadline FROM tasks WHERE id = ?")
            .bind(task_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| fail(format!("db error: {e}")))?
            .flatten();
    let eval_public = ctx.eval_public;
    if time_deadline.map(|td| td > now).unwrap_or(false) {
        sqlx::query("UPDATE tasks SET evaluation_public = ? WHERE id = ?")
            .bind(eval_public)
            .bind(task_id)
            .execute(&state.db)
            .await
            .map_err(|e| fail(format!("db error: {e}")))?;
    } else {
        sqlx::query(
            "UPDATE tasks SET evaluation_public = (evaluation_public OR ?) WHERE id = ?",
        )
        .bind(eval_public)
        .bind(task_id)
        .execute(&state.db)
        .await
        .map_err(|e| fail(format!("db error: {e}")))?;
    }
    sqlx::query("UPDATE tasks SET git_commit = ?, deploy_status = 'done' WHERE id = ?")
        .bind(head_sha)
        .bind(task_id)
        .execute(&state.db)
        .await
        .map_err(|e| fail(format!("db error: {e}")))?;

    // Update the thread name.
    let title: String = sqlx::query_scalar("SELECT title FROM tasks WHERE id = ?")
        .bind(task_id)
        .fetch_one(&state.db)
        .await
        .map_err(|e| fail(format!("db error: {e}")))?;
    let thread: Option<i64> = sqlx::query_scalar("SELECT thread FROM tasks WHERE id = ?")
        .bind(task_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| fail(format!("db error: {e}")))?
        .flatten();
    if let Some(th) = thread {
        sqlx::query("UPDATE threads SET title = ? WHERE id = ?")
            .bind(title)
            .bind(th)
            .execute(&state.db)
            .await
            .map_err(|e| fail(format!("db error: {e}")))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// GET /admin/atasks/{id}/deploy — deploy status
// ---------------------------------------------------------------------------

/// `GET /admin/atasks/{id}/deploy` — deploy status + log tail for a task.
pub async fn deploy_status(
    State(state): State<SharedState>,
    user: AuthUser,
    AxumPath(id): AxumPath<i64>,
) -> ApiResult<Json<DeployStatusResponse>> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest("Přístup odepřen.".into()));
    }
    let task = get_task_row(&state.db, id).await?;

    let mut log_text = None;
    let log_path = DeployPaths::from_env().deploy_log();
    if log_path.is_file() {
        if let Ok(content) = std::fs::read_to_string(&log_path) {
            let mut lines = content.lines();
            if let Some(first) = lines.next() {
                // First line holds the task id (`^(\d*)` in Python).
                let logged_id: String =
                    first.chars().take_while(|c| c.is_ascii_digit()).collect();
                if logged_id == id.to_string() {
                    log_text = Some(lines.collect::<Vec<_>>().join("\n"));
                }
            }
        }
    }

    Ok(Json(DeployStatusResponse {
        id: task.id,
        log: log_text,
        deploy_date: iso(&task.deploy_date),
        deploy_status: task.deploy_status,
    }))
}

// ---------------------------------------------------------------------------
// POST /admin/atasks/{id}/merge — merge task
// ---------------------------------------------------------------------------

/// `POST /admin/atasks/{id}/merge` — merge the task branch into master and
/// delete it (admins or the wave garant).
pub async fn merge_atask(
    State(state): State<SharedState>,
    user: AuthUser,
    AxumPath(id): AxumPath<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let task = get_task_row(&state.db, id).await.map_err(|e| match e {
        ApiError::NotFound(_) => ApiError::NotFound("Neexistujici uloha".into()),
        other => other,
    })?;

    if task.git_path.is_none() || task.git_branch.is_none() {
        return Err(ApiError::BadRequest(
            "Uloha nema zadanou gitovskou vetev nebo adresar".into(),
        ));
    }
    let git_branch = task.git_branch.clone().unwrap();
    if git_branch == "master" {
        return Err(ApiError::BadRequest("Uloha je jiz ve vetvi master".into()));
    }

    let wave = get_wave_row(&state.db, task.wave).await?;

    // Only admins and the wave garant may merge.
    if !user.is_logged_in() || (!user.is_admin() && user.id != Some(wave.garant)) {
        return Err(ApiError::BadRequest("Nedostatecna opravneni".into()));
    }

    let paths = DeployPaths::from_env();
    if git_locked(&paths).is_some() {
        return Err(git_locked_error(&paths));
    }
    let _lock = FileLock::acquire(paths.task_merge_lock()).map_err(|_| git_locked_error(&paths))?;

    // Delete the branch if it exists (cannot delete the branch we are on,
    // so check out master first).
    let branches = git(&paths.seminar, &["branch", "--list", &git_branch]).await?;
    if !branches.trim().is_empty() {
        git(&paths.seminar, &["checkout", "master"]).await?;
        git(&paths.seminar, &["branch", "-D", &git_branch]).await?;
    }

    sqlx::query("UPDATE tasks SET git_branch = 'master' WHERE id = ?")
        .bind(id)
        .execute(&state.db)
        .await?;

    Ok(Json(serde_json::json!({})))
}

// ---------------------------------------------------------------------------
// GET|POST /admin/waves/{id}/diff — wave diff
// ---------------------------------------------------------------------------

/// `GET`/`POST /admin/waves/{id}/diff` — recompute `deploy_status` for every
/// task in the wave by diffing each task directory against its stored
/// `git_commit` (Python only implements POST; both are served here).
pub async fn wave_diff(
    State(state): State<SharedState>,
    user: AuthUser,
    AxumPath(id): AxumPath<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest("Přístup odepřen.".into()));
    }

    let paths = DeployPaths::from_env();
    if git_locked(&paths).is_some() {
        return Err(git_locked_error(&paths));
    }
    let _lock = FileLock::acquire(paths.wave_diff_lock()).map_err(|_| git_locked_error(&paths))?;

    git(&paths.seminar, &["fetch", "origin"]).await?;

    let tasks: Vec<(i64, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, git_branch, git_path, git_commit FROM tasks WHERE wave = ?",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    for (task_id, git_branch, git_path, git_commit) in tasks {
        let (Some(branch), Some(path), Some(commit)) = (git_branch, git_path, git_commit) else {
            set_deploy_status(&state.db, task_id, "default").await;
            continue;
        };

        // Check out the task's branch and pull.
        git(&paths.seminar, &["checkout", &branch]).await?;
        git(&paths.seminar, &["pull", "origin"]).await?;

        if paths.seminar.join(&path).is_dir() {
            let diff = git(&paths.seminar, &["diff", &commit, "HEAD", "--", &path]).await?;
            if !diff.trim().is_empty() {
                set_deploy_status(&state.db, task_id, "diff").await;
            }
        } else {
            set_deploy_status(&state.db, task_id, "default").await;
        }
    }

    Ok(Json(serde_json::json!({})))
}
