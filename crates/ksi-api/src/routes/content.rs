//! Content, image, task-content, task-details and submission-file endpoints.
//!
//! Ports of `endpoint/content.py`, `endpoint/image.py`, the `TaskDetails`
//! part of `endpoint/task.py` and the `ModuleSubmittedFile` part of
//! `endpoint/module.py`.

use axum::{
    extract::{Multipart, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{Local, NaiveDateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Digest;
use std::collections::{HashMap, HashSet};
use std::path::{Component, PathBuf};

use ksi_auth::AuthUser;
use ksi_models::ContentJson;

use crate::{
    error::{ApiError, ApiResult},
    SharedState,
};

/// Max upload size for `POST /content` (mirrors
/// `util/config.py::MAX_UPLOAD_FILE_SIZE`).
const MAX_UPLOAD_FILE_SIZE: u64 = 25 * 1024 * 1024;

/// Root of the on-disk data tree (`data/content`, `data/task-content`,
/// `data/exec`), mirroring the Python backend's relative `data/…` paths.
/// `KSI_DATA_DIR` overrides it (used by tests).
pub(crate) fn data_root() -> PathBuf {
    if let Ok(dir) = std::env::var("KSI_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let mut p = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    p.push("data");
    p
}

/// Lexically normalize a path (resolve `.`/`..`) without touching the FS.
fn normalize(p: &std::path::Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// Resolve `short` inside `base`, returning `None` when it escapes the box.
/// Mirrors `Content.__parse_short_path` (the `..`-stripping plus the
/// `is_relative_to` check).
fn resolve_in_box(base: &std::path::Path, short: &str) -> Option<PathBuf> {
    let stripped = short.replace("..", "");
    let candidate = normalize(&base.join(&stripped));
    let base_norm = normalize(base);
    if candidate.starts_with(&base_norm) {
        Some(candidate)
    } else {
        None
    }
}

/// Best-effort audit log insert (mirrors `util/logger.py::audit_log`).
pub(crate) async fn audit_log(
    db: &sqlx::SqlitePool,
    scope: &str,
    user_id: Option<i64>,
    message: &str,
    message_meta: Option<Value>,
) {
    let meta = message_meta.map(|m| m.to_string());
    let _ = sqlx::query(
        "INSERT INTO audit_logs (user_id, scope, line, line_meta) VALUES (?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(scope)
    .bind(message)
    .bind(meta)
    .execute(db)
    .await;
}

/// Detect a file's MIME type: content sniffing first (like libmagic),
/// extension guess as fallback.
fn detect_mime(path: &std::path::Path) -> String {
    if let Ok(bytes) = std::fs::read(path) {
        if let Some(kind) = infer::get(&bytes) {
            return kind.mime_type().to_string();
        }
    }
    mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string()
}

/// Serve a file from disk with a detected `Content-Type`.
fn file_response(path: &std::path::Path) -> ApiResult<Response> {
    let bytes =
        std::fs::read(path).map_err(|_| ApiError::NotFound("Soubor neexistuje.".into()))?;
    let mime = detect_mime(path);
    Ok(([(header::CONTENT_TYPE, mime)], bytes).into_response())
}

// ---------------------------------------------------------------------------
// GET /content — file download or directory listing
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct ContentQuery {
    path: Option<String>,
}

/// `GET /content?path=…` — serves a file, or returns a JSON directory
/// listing (`{"content": {"id", "files", "dirs"}}`).
pub async fn get_content(
    State(state): State<SharedState>,
    user: AuthUser,
    Query(q): Query<ContentQuery>,
) -> ApiResult<Response> {
    let short_path = q.path.as_deref().unwrap_or(".").replace("..", "");
    let base = data_root().join("content");
    let file_path = match resolve_in_box(&base, q.path.as_deref().unwrap_or(".")) {
        Some(p) => p,
        None => {
            audit_log(
                &state.db,
                "HACK",
                user.id,
                "Attempt to access content outside box",
                Some(json!({"path": q.path})),
            )
            .await;
            return Err(ApiError::NotFound("Obsah neexistuje.".into()));
        }
    };

    if file_path.is_dir() {
        let id = file_path
            .strip_prefix(normalize(&base))
            .map(|p| {
                let s = p.to_string_lossy().into_owned();
                if s.is_empty() {
                    ".".to_string()
                } else {
                    s
                }
            })
            .unwrap_or_else(|_| short_path.clone());
        let content = ContentJson::list_dir(id, &file_path);
        return Ok(Json(json!({ "content": content })).into_response());
    }

    if !file_path.is_file() {
        let content = ContentJson::empty(short_path);
        return Ok(Json(json!({ "content": content })).into_response());
    }

    file_response(&file_path)
}

// ---------------------------------------------------------------------------
// POST /content — upload files (org only)
// ---------------------------------------------------------------------------

/// `POST /content?path=…` — multipart upload into a content directory.
/// Org only.
pub async fn create_content(
    State(state): State<SharedState>,
    user: AuthUser,
    headers: HeaderMap,
    Query(q): Query<ContentQuery>,
    mut multipart: Multipart,
) -> ApiResult<Response> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest(
            "Upravovat content může pouze organizátor.".into(),
        ));
    }

    let base = data_root().join("content");
    let dir_path = match resolve_in_box(&base, q.path.as_deref().unwrap_or(".")) {
        Some(p) => p,
        None => {
            audit_log(
                &state.db,
                "HACK",
                user.id,
                "Attempt to access content outside box",
                Some(json!({"path": q.path})),
            )
            .await;
            return Err(ApiError::NotFound("Obsah neexistuje.".into()));
        }
    };

    let content_length: Option<u64> = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());
    match content_length {
        None => return Err(ApiError::LengthRequired),
        Some(n) if n > MAX_UPLOAD_FILE_SIZE => {
            return Err(ApiError::PayloadTooLarge("Soubor je příliš velký.".into()))
        }
        _ => {}
    }

    if !dir_path.is_dir() {
        std::fs::create_dir_all(&dir_path)
            .map_err(|e| ApiError::Internal(format!("Nelze vytvořit adresář: {e}")))?;
    }

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("Chyba multipart dat: {e}")))?
    {
        let Some(name) = field.file_name().map(|n| n.to_string()) else {
            continue;
        };
        // Strip directories from the client-supplied name so a part can
        // never escape the target dir.
        let file_name = std::path::Path::new(&name)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "upload".to_string());
        let data = field
            .bytes()
            .await
            .map_err(|e| ApiError::BadRequest(format!("Chyba při čtení dat: {e}")))?;
        if data.len() as u64 > MAX_UPLOAD_FILE_SIZE {
            return Err(ApiError::PayloadTooLarge("Soubor je příliš velký.".into()));
        }
        std::fs::write(dir_path.join(file_name), &data)
            .map_err(|e| ApiError::Internal(format!("Nelze uložit soubor: {e}")))?;
    }

    Ok(Json(json!({})).into_response())
}

// ---------------------------------------------------------------------------
// DELETE /content — delete a file (org only)
// ---------------------------------------------------------------------------

/// `DELETE /content?path=…` — delete a file and prune newly-empty parent
/// directories (up to, but not including, `data/content`). Org only.
pub async fn delete_content(
    State(state): State<SharedState>,
    user: AuthUser,
    Query(q): Query<ContentQuery>,
) -> ApiResult<Response> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest(
            "Upravovat content může pouze organizátor.".into(),
        ));
    }

    let base = data_root().join("content");
    let base_norm = normalize(&base);
    let file_path = match resolve_in_box(&base, q.path.as_deref().unwrap_or(".")) {
        Some(p) => p,
        None => return Err(ApiError::NotFound("Obsah neexistuje.".into())),
    };

    if !file_path.is_file() {
        return Err(ApiError::NotFound("Soubor neexistuje.".into()));
    }

    std::fs::remove_file(&file_path)
        .map_err(|e| ApiError::Internal(format!("Nelze smazat soubor: {e}")))?;

    // Prune empty parent dirs, like Python's `_delete_tree`, but never
    // remove the content root itself.
    let mut dir = file_path.parent().map(|p| p.to_path_buf());
    while let Some(d) = dir {
        if d == base_norm {
            break;
        }
        match std::fs::read_dir(&d) {
            Ok(mut entries) => {
                if entries.next().is_some() {
                    break;
                }
            }
            Err(_) => break,
        }
        if std::fs::remove_dir(&d).is_err() {
            break;
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }

    Ok(Json(json!({})).into_response())
}

// ---------------------------------------------------------------------------
// GET /images/{context}/{id}
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct ImageQuery {
    file: Option<String>,
    module: Option<String>,
    user: Option<String>,
}

/// Serve an image for the `profile`, `codeExecution` or `codeModule`
/// contexts.
pub async fn get_image(
    State(state): State<SharedState>,
    Path((context, id)): Path<(String, String)>,
    Query(q): Query<ImageQuery>,
) -> ApiResult<Response> {
    let image: PathBuf = match context.as_str() {
        "profile" => {
            let id_num: i64 =
                id.parse().map_err(|_| ApiError::BadRequest("Neplatné ID.".into()))?;
            let picture: Option<Option<String>> =
                sqlx::query_scalar("SELECT profile_picture FROM users WHERE id = ?")
                    .bind(id_num)
                    .fetch_optional(&state.db)
                    .await?;
            match picture.flatten() {
                Some(p) if !p.is_empty() => PathBuf::from(p),
                _ => return Err(ApiError::NotFound("Obrázek neexistuje.".into())),
            }
        }
        "codeExecution" => {
            let id_num: i64 =
                id.parse().map_err(|_| ApiError::BadRequest("Neplatné ID.".into()))?;
            let row: Option<(i64, i64)> =
                sqlx::query_as("SELECT user, module FROM code_executions WHERE id = ?")
                    .bind(id_num)
                    .fetch_optional(&state.db)
                    .await?;
            let Some((exec_user, exec_module)) = row else {
                return Err(ApiError::BadRequest("Neplatné ID.".into()));
            };
            let Some(file) = q.file.as_deref() else {
                return Err(ApiError::BadRequest("Chybí parametr file.".into()));
            };
            let file_name = std::path::Path::new(file)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            data_root()
                .join("exec")
                .join(format!("module_{exec_module}"))
                .join(format!("user_{exec_user}"))
                .join(file_name)
        }
        "codeModule" => {
            let (Some(file), Some(module), Some(user)) =
                (q.file.as_deref(), q.module.as_deref(), q.user.as_deref())
            else {
                return Err(ApiError::BadRequest("Chybí parametry.".into()));
            };
            let user_id: i64 =
                user.parse().map_err(|_| ApiError::BadRequest("Neplatné ID.".into()))?;
            let module_id: i64 =
                module.parse().map_err(|_| ApiError::BadRequest("Neplatné ID.".into()))?;
            let file_name = std::path::Path::new(file)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !file_name.ends_with(".png") {
                return Err(ApiError::BadRequest("Neplatný soubor.".into()));
            }
            let image = data_root()
                .join("exec")
                .join(format!("module_{module_id}"))
                .join(format!("user_{user_id}"))
                .join(&file_name);
            let mut resp = file_response_checked(&image)?;
            resp.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-cache"),
            );
            return Ok(resp);
        }
        _ => return Err(ApiError::BadRequest("Neznámý kontext.".into())),
    };

    file_response_checked(&image)
}

/// Serve a file, returning 400 (like the Python endpoint) when it is
/// missing on disk.
fn file_response_checked(path: &std::path::Path) -> ApiResult<Response> {
    if !path.is_file() {
        return Err(ApiError::BadRequest("Soubor neexistuje.".into()));
    }
    file_response(path)
}

// ---------------------------------------------------------------------------
// GET /taskContent/{id}, /task-content/{id}/{view}
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct TaskContentQuery {
    path: Option<String>,
}

/// Serve a task-content file (`zadani…`, `reseni…` or `icon` view).
pub async fn get_task_content(
    state: State<SharedState>,
    user: AuthUser,
    Path((id, view)): Path<(String, String)>,
    query: Query<TaskContentQuery>,
) -> ApiResult<Response> {
    task_content_inner(state, user, id, Some(view), query).await
}

/// Legacy `/taskContent/{id}` route (no view segment).
pub async fn get_task_content_legacy(
    state: State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
    query: Query<TaskContentQuery>,
) -> ApiResult<Response> {
    task_content_inner(state, user, id, None, query).await
}

/// Also reachable through the legacy `/taskContent/{id}[/{view}]` routes
/// (the production proxy rewrites those to `/task-content/{id}/{view}`).
async fn task_content_inner(
    State(state): State<SharedState>,
    user: AuthUser,
    id: String,
    view: Option<String>,
    Query(q): Query<TaskContentQuery>,
) -> ApiResult<Response> {
    let Some(view) = view.filter(|v| !v.is_empty()) else {
        return Err(ApiError::BadRequest("Chybí view.".into()));
    };
    if view != "icon" && !view.starts_with("reseni") && !view.starts_with("zadani") {
        return Err(ApiError::BadRequest("Neplatné view.".into()));
    }

    let Some(path_param) = q.path.as_deref() else {
        return Err(ApiError::BadRequest("Chybí parametr path.".into()));
    };

    // `Path(str(id)).name` — sanitize the id/view to their basenames.
    let id_name = std::path::Path::new(&id)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let view_name = std::path::Path::new(&view)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let base = data_root()
        .join("task-content")
        .join(&id_name)
        .join(&view_name);

    let file_path = match resolve_in_box(&base, path_param) {
        Some(p) => p,
        None => {
            audit_log(
                &state.db,
                "HACK",
                user.id,
                "Attempt to access task content outside box",
                Some(json!({"id": id, "view": view, "path": path_param})),
            )
            .await;
            return Err(ApiError::NotFound("Soubor neexistuje.".into()));
        }
    };

    if !file_path.is_file() {
        return Err(ApiError::NotFound("Soubor neexistuje.".into()));
    }

    file_response(&file_path)
}

// ---------------------------------------------------------------------------
// GET /taskDetails/{id}
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct TaskDetailsRow {
    id: i64,
    wave: i64,
    prerequisite: Option<i64>,
    body: String,
    solution: Option<String>,
    thread: Option<i64>,
    time_deadline: Option<NaiveDateTime>,
    evaluation_public: bool,
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn isoformat(dt: &NaiveDateTime) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
}

/// Tasks past their deadline (mirrors `util/task.py::after_deadline`).
async fn after_deadline(db: &sqlx::SqlitePool) -> ApiResult<HashSet<i64>> {
    let now = Utc::now().naive_utc();
    let rows: Vec<(i64,)> = sqlx::query_as("SELECT id FROM tasks WHERE time_deadline < ?")
        .bind(now)
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// `{task_id: fully-submitted module count}` for a user
/// (mirrors `util/task.py::fully_submitted`).
async fn fully_submitted(db: &sqlx::SqlitePool, user_id: i64) -> ApiResult<HashMap<i64, i64>> {
    let max_rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT t.id, COUNT(DISTINCT m.id) FROM tasks t
         LEFT JOIN modules m ON m.task = t.id AND m.bonus = 0
         GROUP BY t.id",
    )
    .fetch_all(db)
    .await?;
    let real_rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT t.id, COUNT(DISTINCT m.id) FROM tasks t
         JOIN modules m ON m.task = t.id
         JOIN evaluations e ON e.module = m.id
         WHERE e.user = ? AND (m.autocorrect = 0 OR e.ok = 1)
         GROUP BY t.id",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    let max_map: HashMap<i64, i64> = max_rows.into_iter().collect();
    let mut out = HashMap::new();
    for (task_id, cnt) in real_rows {
        if max_map.get(&task_id).copied().unwrap_or(0) <= cnt {
            out.insert(task_id, cnt);
        }
    }
    Ok(out)
}

/// IDs of tasks the user has corrected (mirrors `util/task.py::corrected`).
async fn corrected(db: &sqlx::SqlitePool, user_id: i64) -> ApiResult<HashSet<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT DISTINCT t.id FROM tasks t
         JOIN modules m ON m.task = t.id
         JOIN evaluations e ON e.module = m.id
         WHERE t.evaluation_public = 1 AND e.user = ?",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// IDs of fully auto-corrected tasks (mirrors
/// `util/task.py::autocorrected_full`).
async fn autocorrected_full(db: &sqlx::SqlitePool, user_id: i64) -> ApiResult<HashSet<i64>> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT mc.task_id FROM
           (SELECT t.id AS task_id, COUNT(DISTINCT m.id) AS cnt FROM tasks t
            JOIN modules m ON m.task = t.id WHERE m.bonus = 0 GROUP BY t.id) mc
         JOIN
           (SELECT t.id AS task_id, COUNT(DISTINCT m.id) AS cnt FROM tasks t
            JOIN modules m ON m.task = t.id
            JOIN evaluations e ON e.module = m.id
            WHERE e.user = ? AND (m.autocorrect = 0 OR e.ok = 1)
            GROUP BY t.id) rc ON rc.task_id = mc.task_id AND rc.cnt = mc.cnt",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

enum PrereqExpr {
    Atomic(i64),
    And(Vec<PrereqExpr>),
    Or(Vec<PrereqExpr>),
}

/// Load a prerequisite tree (mirrors `util/prerequisite.py`).
fn load_prereq<'a>(
    db: &'a sqlx::SqlitePool,
    id: i64,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ApiResult<PrereqExpr>> + Send + 'a>> {
    Box::pin(async move {
        let row: Option<(String, Option<i64>)> =
            sqlx::query_as("SELECT type, task FROM prerequisities WHERE id = ?")
                .bind(id)
                .fetch_optional(db)
                .await?;
        let (ptype, task) = row.unwrap_or_else(|| ("ATOMIC".to_string(), None));
        match ptype.as_str() {
            "AND" | "OR" => {
                let kids: Vec<(i64,)> =
                    sqlx::query_as("SELECT id FROM prerequisities WHERE parent = ?")
                        .bind(id)
                        .fetch_all(db)
                        .await?;
                let mut exprs = Vec::new();
                for (kid,) in kids {
                    exprs.push(load_prereq(db, kid).await?);
                }
                if ptype == "AND" {
                    Ok(PrereqExpr::And(exprs))
                } else {
                    Ok(PrereqExpr::Or(exprs))
                }
            }
            _ => Ok(PrereqExpr::Atomic(task.unwrap_or(-1))),
        }
    })
}

fn eval_prereq(expr: &PrereqExpr, active: &HashSet<i64>) -> bool {
    match expr {
        PrereqExpr::Atomic(t) => active.contains(t),
        PrereqExpr::And(items) => items.iter().all(|e| eval_prereq(e, active)),
        PrereqExpr::Or(items) => items.iter().any(|e| eval_prereq(e, active)),
    }
}

/// Task visibility state (mirrors `util/task.py::status`).
async fn task_status(db: &sqlx::SqlitePool, task: &TaskDetailsRow, user: &AuthUser) -> ApiResult<String> {
    let published: Option<Option<NaiveDateTime>> =
        sqlx::query_scalar("SELECT time_published FROM waves WHERE id = ?")
            .bind(task.wave)
            .fetch_optional(db)
            .await?;
    let now = Utc::now().naive_utc();
    let wave_public = published.flatten().map(|t| t <= now).unwrap_or(false);

    if !user.is_logged_in() {
        let adeadline = after_deadline(db).await?;
        let open = (task.prerequisite.is_none() || adeadline.contains(&task.id)) && wave_public;
        return Ok(if open { "base" } else { "locked" }.to_string());
    }
    let role = user.role.as_deref().unwrap_or("");
    if !wave_public && !matches!(role, "org" | "admin" | "tester") {
        return Ok("locked".to_string());
    }
    let uid = user.id.expect("logged in");
    let corr = corrected(db, uid).await?;
    let acfull = autocorrected_full(db, uid).await?;
    if corr.contains(&task.id) && acfull.contains(&task.id) {
        return Ok("done".to_string());
    }
    let fsubmitted = fully_submitted(db, uid).await?;
    if fsubmitted.contains_key(&task.id) {
        return Ok("correcting".to_string());
    }
    let mut active = after_deadline(db).await?;
    active.extend(fsubmitted.keys());
    if active.contains(&task.id) {
        return Ok("base".to_string());
    }
    let prereq_ok = match task.prerequisite {
        None => true,
        Some(pid) => eval_prereq(&load_prereq(db, pid).await?, &active),
    };
    if prereq_ok || matches!(role, "org" | "admin" | "tester") {
        Ok("base".to_string())
    } else {
        Ok("locked".to_string())
    }
}

/// Whether the task solution is visible (mirrors
/// `util/task.py::solution_public`).
fn solution_public(status: &str, task: &TaskDetailsRow, user: &AuthUser) -> bool {
    let role = user.role.as_deref().unwrap_or("");
    ((task.time_deadline.is_some()
        && (status == "done"
            || task.time_deadline.map(|dl| dl < Utc::now().naive_utc()).unwrap_or(false)))
        || matches!(role, "org" | "admin" | "tester"))
}

#[derive(Debug, sqlx::FromRow)]
struct ModuleRow {
    id: i64,
    task: i64,
    #[sqlx(rename = "type")]
    module_type: String,
    name: String,
    description: Option<String>,
    autocorrect: bool,
    max_points: Option<f64>,
    data: Option<String>,
}

fn config_value<'a>(db: &'a sqlx::SqlitePool, key: &'a str) -> impl std::future::Future<Output = String> + 'a {
    async move {
        sqlx::query_scalar::<_, Option<String>>("SELECT value FROM config WHERE key = ?")
            .bind(key)
            .fetch_optional(db)
            .await
            .ok()
            .flatten()
            .flatten()
            .unwrap_or_default()
    }
}

/// Module JSON (mirrors `util/module.py::to_json`, best-effort for the
/// per-type content; custom modules are rendered from their base fields —
/// the Python backend executes stored Python code for those, which the Rust
/// port deliberately does not do).
async fn module_to_json(
    db: &sqlx::SqlitePool,
    m: &ModuleRow,
    task_eval_public: bool,
    user_id: Option<i64>,
    task_id: i64,
) -> ApiResult<Value> {
    let best: Option<(i64, bool, Option<NaiveDateTime>)> = sqlx::query_as(
        "SELECT id, ok, time FROM evaluations
         WHERE user = ? AND module = ?
         ORDER BY ok DESC, points DESC, time DESC LIMIT 1",
    )
    .bind(user_id)
    .bind(m.id)
    .fetch_optional(db)
    .await?;

    let mut v = json!({
        "id": m.id,
        "type": m.module_type,
        "name": m.name,
        "description": m.description,
        "autocorrect": m.autocorrect,
        "max_score": m.max_points,
        "state": match &best {
            None => "blank",
            Some((_, ok, _)) => if *ok { "correct" } else { "incorrect" },
        },
        "score": match &best {
            Some(_) if task_eval_public => json!(m.id),
            _ => Value::Null,
        },
    });

    if let Err(e) = module_extra_json(db, m, user_id, task_id, &best, &mut v).await {
        let desc = v.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string();
        v["description"] = json!(format!(
            "{desc}<pre><code><strong>Module parsing error:</strong><br>{e}</code></pre>"
        ));
    }

    Ok(v)
}

/// Per-type module content for [`module_to_json`]. Errors are collected as a
/// string so the caller can append the "Module parsing error" note, like the
/// Python backend does.
async fn module_extra_json(
    db: &sqlx::SqlitePool,
    m: &ModuleRow,
    user_id: Option<i64>,
    task_id: i64,
    best: &Option<(i64, bool, Option<NaiveDateTime>)>,
    v: &mut Value,
) -> Result<(), String> {
    match m.module_type.as_str() {
        "general" => {
            let files: Vec<(i64, String)> = sqlx::query_as(
                "SELECT sf.id, sf.path FROM submitted_files sf
                 JOIN evaluations e ON e.id = sf.evaluation
                 WHERE e.user = ? AND e.module = ?",
            )
            .bind(user_id)
            .bind(m.id)
            .fetch_all(db)
            .await
            .map_err(|e| e.to_string())?;
            v["submitted_files"] = json!(files
                .iter()
                .map(|(id, path)| {
                    let filename = std::path::Path::new(path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    json!({"id": id, "filename": filename})
                })
                .collect::<Vec<_>>());
            Ok(())
        }
        "programming" => {
            let data: Value = serde_json::from_str(m.data.as_deref().unwrap_or("{}"))
                .map_err(|e| e.to_string())?;
            let default_code = data
                .pointer("/programming/default_code")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            let mut code = default_code.clone();
            let mut last_origin: Option<&str> = None;
            let mut last_time: Option<NaiveDateTime> = None;
            if let Some((eval_id, _, eval_time)) = best {
                let submitted: Option<String> =
                    sqlx::query_scalar("SELECT code FROM submitted_codes WHERE evaluation = ?")
                        .bind(eval_id)
                        .fetch_optional(db)
                        .await
                        .map_err(|e| e.to_string())?
                        .flatten();
                if let Some(sc) = submitted {
                    code = sc;
                    last_time = *eval_time;
                    last_origin = Some("evaluation");
                }
            } else if user_id.is_some() {
                let exec: Option<(Option<String>, Option<NaiveDateTime>)> = sqlx::query_as(
                    "SELECT code, time FROM code_executions
                     WHERE module = ? AND user = ?
                     ORDER BY time DESC LIMIT 1",
                )
                .bind(m.id)
                .bind(user_id)
                .fetch_optional(db)
                .await
                .map_err(|e| e.to_string())?;
                if let Some((exec_code, exec_time)) = exec {
                    if let Some(c) = exec_code {
                        code = c;
                    }
                    last_time = exec_time;
                    last_origin = Some("execution");
                }
            }
            // edulint_source_id mirrors util/programming.py::to_json
            let salt = config_value(db, "salt").await;
            let short = config_value(db, "seminar_name_short").await;
            let uid = user_id.unwrap_or(0);
            let mut hasher = sha2::Sha256::new();
            hasher.update(format!("{salt}{uid}").as_bytes());
            let hash = hex_encode(&hasher.finalize());
            v["code"] = json!(code);
            v["default_code"] = json!(default_code);
            v["edulint_source_id"] =
                json!(format!("{short}:taskId_{task_id}:moduleId_{}:userId_{hash}", m.id));
            if let Some(t) = last_time {
                v["last_datetime"] = json!(isoformat(&t));
            }
            if let Some(o) = last_origin {
                v["last_origin"] = json!(o);
            }
            Ok(())
        }
        "quiz" => {
            let data: Value = serde_json::from_str(m.data.as_deref().unwrap_or("{}"))
                .map_err(|e| e.to_string())?;
            let questions = data
                .pointer("/quiz")
                .and_then(|q| q.as_array())
                .cloned()
                .unwrap_or_default();
            v["questions"] = json!(questions
                .iter()
                .map(|q| json!({
                    "type": q.get("type"),
                    "question": q.get("question"),
                    "text": q.get("text").and_then(|t| t.as_str()).unwrap_or(""),
                    "options": q.get("options"),
                }))
                .collect::<Vec<_>>());
            Ok(())
        }
        "sortable" => {
            let data: Value = serde_json::from_str(m.data.as_deref().unwrap_or("{}"))
                .map_err(|e| e.to_string())?;
            v["sortable_list"] = json!({
                "fixed": data.pointer("/sortable/fixed"),
                "movable": data.pointer("/sortable/movable"),
            });
            Ok(())
        }
        "text" => {
            let data: Value = serde_json::from_str(m.data.as_deref().unwrap_or("{}"))
                .map_err(|e| e.to_string())?;
            let fields: Vec<Value> = match data.pointer("/text/questions") {
                Some(qs) => qs.as_array().cloned().unwrap_or_default(),
                None => {
                    let inputs = data.pointer("/text/inputs").and_then(|i| i.as_u64()).unwrap_or(0);
                    (1..=inputs).map(|i| json!(format!("Otázka {i}"))).collect()
                }
            };
            v["fields"] = json!(fields);
            Ok(())
        }
        _ => Ok(()),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, sqlx::FromRow)]
struct AchievementRow {
    id: i64,
    title: String,
    picture: String,
    description: Option<String>,
    year: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
struct ThreadRow {
    id: i64,
    title: Option<String>,
    year: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct PostRow {
    id: i64,
    thread: i64,
    author: i64,
    body: String,
    published_at: Option<NaiveDateTime>,
}

/// Count unread posts for a user in a thread (mirrors
/// `util/thread.py::count_unread`).
async fn count_unread(db: &sqlx::SqlitePool, user_id: Option<i64>, thread_id: i64) -> ApiResult<i64> {
    let Some(uid) = user_id else {
        return Ok(0);
    };
    let visit: Option<(NaiveDateTime, Option<NaiveDateTime>)> = sqlx::query_as(
        "SELECT last_visit, last_last_visit FROM threads_visits WHERE thread = ? AND user = ?",
    )
    .bind(thread_id)
    .bind(uid)
    .fetch_optional(db)
    .await?;
    let Some((last_visit, _)) = visit else {
        return Ok(0);
    };
    let cnt: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM posts WHERE thread = ? AND published_at > ?")
        .bind(thread_id)
        .bind(last_visit)
        .fetch_one(db)
        .await?;
    Ok(cnt)
}

async fn thread_to_json(db: &sqlx::SqlitePool, t: &ThreadRow, user_id: Option<i64>) -> ApiResult<Value> {
    let posts_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM posts WHERE thread = ?")
        .bind(t.id)
        .fetch_one(db)
        .await?;
    let unread = count_unread(db, user_id, t.id).await?;
    Ok(json!({
        "id": t.id,
        "title": t.title,
        "unread": unread,
        "posts_count": posts_count,
        "details": t.id,
        "year": t.year,
    }))
}

async fn thread_details_to_json(db: &sqlx::SqlitePool, thread_id: i64) -> ApiResult<Value> {
    let roots: Vec<(i64,)> =
        sqlx::query_as("SELECT id FROM posts WHERE thread = ? AND parent IS NULL ORDER BY id")
            .bind(thread_id)
            .fetch_all(db)
            .await?;
    Ok(json!({
        "id": thread_id,
        "root_posts": roots.iter().map(|(id,)| id).collect::<Vec<_>>(),
    }))
}

async fn post_to_json(db: &sqlx::SqlitePool, p: &PostRow, user_id: Option<i64>) -> ApiResult<Value> {
    let is_new = match user_id {
        None => false,
        Some(uid) => {
            let visit: Option<(NaiveDateTime, Option<NaiveDateTime>)> = sqlx::query_as(
                "SELECT last_visit, last_last_visit FROM threads_visits WHERE thread = ? AND user = ?",
            )
            .bind(p.thread)
            .bind(uid)
            .fetch_optional(db)
            .await?;
            match visit {
                None => true,
                Some((_, None)) => true,
                Some((_, Some(last_last))) => match p.published_at {
                    None => true,
                    Some(pub_at) => last_last < pub_at,
                },
            }
        }
    };
    // `Post.reactions` is a self-referential relationship → child posts.
    let reactions: Vec<(i64,)> = sqlx::query_as("SELECT id FROM posts WHERE parent = ? ORDER BY id")
        .bind(p.id)
        .fetch_all(db)
        .await?;
    Ok(json!({
        "id": p.id,
        "thread": p.thread,
        "author": p.author,
        "body": p.body,
        "published_at": p.published_at.as_ref().map(isoformat),
        "reaction": reactions.iter().map(|(id,)| id).collect::<Vec<_>>(),
        "is_new": is_new,
    }))
}

/// `GET /taskDetails/{id}` — task details with modules, scores,
/// achievements and discussion threads.
pub async fn get_task_details(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let task: Option<TaskDetailsRow> = sqlx::query_as(
        "SELECT id, wave, prerequisite, body, solution, thread, time_deadline,
                evaluation_public
         FROM tasks WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    let Some(task) = task else {
        return Err(ApiError::NotFound("Úloha s tímto ID neexistuje.".into()));
    };

    let status = task_status(&state.db, &task, &user).await?;
    if status == "locked" {
        return Err(ApiError::Forbidden("Úloha uzamčena.".into()));
    }
    let uid = user.id;

    // achievements for this user+task
    let achievements: Vec<AchievementRow> = if let Some(uid) = uid {
        sqlx::query_as(
            "SELECT a.id, a.title, a.picture, a.description, a.year
             FROM achievements a
             JOIN user_achievement ua ON ua.achievement_id = a.id
             WHERE ua.user_id = ? AND ua.task_id = ?",
        )
        .bind(uid)
        .bind(task.id)
        .fetch_all(&state.db)
        .await?
    } else {
        Vec::new()
    };

    // per-module max points + evaluator for this user
    let scores: Vec<(i64, Option<f64>, Option<i64>)> = sqlx::query_as(
        "SELECT m.id, MAX(e.points), MAX(e.evaluator)
         FROM modules m
         JOIN evaluations e ON e.module = m.id
         WHERE m.task = ? AND e.user = ?
         GROUP BY m.id",
    )
    .bind(task.id)
    .bind(uid)
    .fetch_all(&state.db)
    .await?;

    // best scores across participants
    let best_scores: Vec<(i64, f64)> = sqlx::query_as(
        "SELECT u.id, SUM(pm.points) FROM
           (SELECT e.user AS user_id, MAX(e.points) AS points
            FROM evaluations e
            JOIN modules m ON m.id = e.module
            JOIN tasks t ON t.id = m.task
            WHERE m.task = ? AND t.evaluation_public = 1
            GROUP BY e.module, e.user) pm
         JOIN users u ON u.id = pm.user_id
         WHERE u.role = 'participant'
         GROUP BY pm.user_id
         ORDER BY SUM(pm.points) DESC
         LIMIT 100",
    )
    .bind(task.id)
    .fetch_all(&state.db)
    .await?;

    // comment thread for this user
    let comment_thread: Option<i64> = if let Some(uid) = uid {
        sqlx::query_scalar("SELECT thread FROM solution_comments WHERE task = ? AND user = ?")
            .bind(task.id)
            .bind(uid)
            .fetch_optional(&state.db)
            .await?
            .flatten()
    } else {
        None
    };

    // modules
    let modules: Vec<ModuleRow> = sqlx::query_as(
        "SELECT id, task, type, name, description, autocorrect, max_points, data
         FROM modules WHERE task = ? ORDER BY \"order\", id",
    )
    .bind(task.id)
    .fetch_all(&state.db)
    .await?;
    let mut modules_json = Vec::with_capacity(modules.len());
    for m in &modules {
        modules_json.push(module_to_json(&state.db, m, task.evaluation_public, uid, task.id).await?);
    }

    // threads: task thread + comment thread
    let mut thread_ids: Vec<i64> = Vec::new();
    if let Some(t) = task.thread {
        thread_ids.push(t);
    }
    if let Some(ct) = comment_thread {
        if !thread_ids.contains(&ct) {
            thread_ids.push(ct);
        }
    }
    let mut threads_json = Vec::new();
    let mut thread_details_json = Vec::new();
    let mut posts_json = Vec::new();
    for tid in &thread_ids {
        let t: Option<ThreadRow> =
            sqlx::query_as("SELECT id, title, year FROM threads WHERE id = ?")
                .bind(tid)
                .fetch_optional(&state.db)
                .await?;
        let Some(t) = t else { continue };
        threads_json.push(thread_to_json(&state.db, &t, uid).await?);
        thread_details_json.push(thread_details_to_json(&state.db, t.id).await?);
        let posts: Vec<PostRow> = sqlx::query_as(
            "SELECT id, thread, author, body, published_at FROM posts WHERE thread = ? ORDER BY id",
        )
        .bind(t.id)
        .fetch_all(&state.db)
        .await?;
        for p in &posts {
            posts_json.push(post_to_json(&state.db, p, uid).await?);
        }
    }

    Ok(Json(json!({
        "taskDetails": {
            "id": task.id,
            "body": task.body,
            "thread": task.thread,
            "modules": modules.iter().map(|m| m.id).collect::<Vec<_>>(),
            "best_scores": best_scores.iter().map(|(u, _)| u).collect::<Vec<_>>(),
            "comment": if task.evaluation_public { comment_thread } else { None },
            "solution": if solution_public(&status, &task, &user) { task.solution } else { None },
            "achievements": achievements.iter().map(|a| a.id).collect::<Vec<_>>(),
        },
        "modules": modules_json,
        "moduleScores": scores.iter().filter_map(|(mid, points, evaluator)| {
            points.map(|p| json!({
                "id": mid,
                "is_corrected": true,
                "score": round1(p),
                "reviewed_by": evaluator,
            }))
        }).collect::<Vec<_>>(),
        "achievements": achievements.iter().map(|a| json!({
            "id": a.id,
            "title": a.title,
            "active": true,
            "picture": a.picture,
            "description": a.description,
            "persistent": a.year.is_none(),
            "year": a.year,
        })).collect::<Vec<_>>(),
        "userScores": best_scores.iter().map(|(u, total)| json!({
            "id": u, "user": u, "score": round1(*total),
        })).collect::<Vec<_>>(),
        "threads": threads_json,
        "threadDetails": thread_details_json,
        "posts": posts_json,
    })))
}

// ---------------------------------------------------------------------------
// /submFiles/{id} — single submitted file
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct SubmittedFileRow {
    id: i64,
    evaluation: i64,
    path: String,
}

/// Access check for a submitted file (mirrors
/// `ModuleSubmittedFile._get_submitted_file`).
///
/// Note: the Python code reads `user.is_org` *without calling it*, which is
/// always truthy — i.e. any logged-in user could fetch any file. The Rust
/// port implements the clearly intended check (`user.is_org()`).
async fn check_subm_file(
    db: &sqlx::SqlitePool,
    user: &AuthUser,
    id: i64,
) -> ApiResult<SubmittedFileRow> {
    if !user.is_logged_in() {
        return Err(ApiError::Forbidden("Nejste přihlášeni.".into()));
    }
    let file: Option<SubmittedFileRow> =
        sqlx::query_as("SELECT id, evaluation, path FROM submitted_files WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await?;
    let Some(file) = file else {
        return Err(ApiError::NotFound("Soubor neexistuje.".into()));
    };
    let eval_user: Option<i64> = sqlx::query_scalar("SELECT user FROM evaluations WHERE id = ?")
        .bind(file.evaluation)
        .fetch_optional(db)
        .await?
        .flatten();
    if eval_user == user.id || user.is_org() {
        Ok(file)
    } else {
        Err(ApiError::Forbidden("K tomuto souboru nemáte oprávnění.".into()))
    }
}

/// `GET /submFiles/{id}` — download a single submitted file.
pub async fn get_subm_file(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    let file = check_subm_file(&state.db, &user, id).await?;
    let path = std::path::Path::new(&file.path);
    if !path.is_file() {
        return Err(ApiError::NotFound("Soubor neexistuje.".into()));
    }
    file_response(path)
}

/// `DELETE /submFiles/{id}` — delete a submitted file (before deadline).
/// If the evaluation has no files left, the evaluation itself is removed.
pub async fn delete_subm_file(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    let file = match check_subm_file(&state.db, &user, id).await {
        Ok(f) => f,
        Err(ApiError::Forbidden(_)) => {
            return Ok((
                StatusCode::FORBIDDEN,
                Json(json!({"status": "error", "error": "K tomuto souboru nemáte oprávnění"})),
            )
                .into_response())
        }
        Err(ApiError::NotFound(_)) => {
            return Ok((
                StatusCode::NOT_FOUND,
                Json(json!({"status": "error", "error": "Soubor nenalezen na serveru"})),
            )
                .into_response())
        }
        Err(e) => return Err(e),
    };

    // Files can only be deleted before the task deadline.
    let deadline: Option<Option<NaiveDateTime>> = sqlx::query_scalar(
        "SELECT t.time_deadline FROM tasks t
         JOIN modules m ON m.task = t.id
         JOIN evaluations e ON e.module = m.id
         WHERE e.id = ?",
    )
    .bind(file.evaluation)
    .fetch_optional(&state.db)
    .await?;
    if let Some(Some(dl)) = deadline {
        if dl < Utc::now().naive_utc() {
            return Ok(Json(json!({
                "result": "error",
                "error": "Nelze smazat soubory po termínu odevzdání úlohy",
            }))
            .into_response());
        }
    }

    if std::fs::remove_file(&file.path).is_err() {
        return Ok(Json(json!({
            "status": "error",
            "error": "Soubor se nepodařilo odstranit z filesystému",
        }))
        .into_response());
    }

    let db_result: Result<(), sqlx::Error> = async {
        let mut tx = state.db.begin().await?;
        let report_line = format!(
            "{} : removed file {}\n",
            Local::now().format("%Y-%m-%d %H:%M:%S%.6f"),
            file.path
        );
        sqlx::query("UPDATE evaluations SET full_report = full_report || ? WHERE id = ?")
            .bind(&report_line)
            .bind(file.evaluation)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM submitted_files WHERE id = ?")
            .bind(file.id)
            .execute(&mut *tx)
            .await?;
        let remaining: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM submitted_files WHERE evaluation = ?")
                .bind(file.evaluation)
                .fetch_one(&mut *tx)
                .await?;
        if remaining == 0 {
            sqlx::query("DELETE FROM evaluations WHERE id = ?")
                .bind(file.evaluation)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }
    .await;

    if db_result.is_err() {
        return Ok(Json(json!({
            "status": "error",
            "error": "Záznam o souboru se nepodařilo odstranit z databáze",
        }))
        .into_response());
    }

    Ok(Json(json!({"status": "ok"})).into_response())
}
