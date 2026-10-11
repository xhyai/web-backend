use axum::{
    extract::{Path, State},
    http::header,
    response::{IntoResponse, Response},
    Json,
};
use std::io::{Cursor, Write};

use ksi_auth::AuthUser;
use crate::{error::{ApiError, ApiResult}, SharedState};

/// `GET /admin/subm/task/{task_id}` — download all submissions for a task
/// as a ZIP archive.
///
/// Returns 404 instead of a silent empty ZIP when there is nothing to
/// download, logs warnings for files missing on disk, and includes
/// `SubmittedCode` rows (programming module answers stored in the DB)
/// as `code_<evaluation_id>.txt`.
pub async fn subm_files_task(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(task_id): Path<i64>,
) -> ApiResult<Response> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }

    let modules: Vec<(i64,)> =
        sqlx::query_as("SELECT id FROM modules WHERE task = ?")
            .bind(task_id)
            .fetch_all(&state.db)
            .await?;

    let mut buf = Cursor::new(Vec::new());
    let files_added: usize = {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default();
        let mut count = 0usize;

        for (module_id,) in &modules {
            // users with evaluations for this module
            let users: Vec<(i64, String, String)> = sqlx::query_as(
                "SELECT DISTINCT u.id, u.first_name, u.last_name
                 FROM users u JOIN evaluations e ON e.user = u.id
                 WHERE e.module = ?",
            )
            .bind(module_id)
            .fetch_all(&state.db)
            .await?;

            for (user_id, first_name, last_name) in &users {
                let userdir = format!(
                    "module_{}/{}_{}",
                    module_id,
                    strip_accents(first_name),
                    strip_accents(last_name)
                )
                .replace(' ', "_");

                // submitted files
                let files: Vec<(String, i64)> = sqlx::query_as(
                    "SELECT DISTINCT sf.path, sf.evaluation
                     FROM submitted_files sf
                     JOIN evaluations e ON e.id = sf.evaluation
                     WHERE e.user = ? AND e.module = ?",
                )
                .bind(user_id)
                .bind(module_id)
                .fetch_all(&state.db)
                .await?;

                for (path, _eval_id) in &files {
                    match std::fs::read(path) {
                        Ok(data) => {
                            let name = format!(
                                "{}/{}",
                                userdir,
                                std::path::Path::new(path)
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| "file".into())
                            );
                            zip.start_file(name, options).map_err(|e| {
                                ApiError::Internal(format!("ZIP error: {e}"))
                            })?;
                            zip.write_all(&data).map_err(|e| {
                                ApiError::Internal(format!("ZIP error: {e}"))
                            })?;
                            count += 1;
                        }
                        Err(_) => {
                            tracing::warn!(
                                "submitted file missing on disk, skipping: {path}"
                            );
                        }
                    }
                }

                // submitted code (programming modules) from the DB
                let codes: Vec<(i64, String)> = sqlx::query_as(
                    "SELECT sc.evaluation, sc.code
                     FROM submitted_codes sc
                     JOIN evaluations e ON e.id = sc.evaluation
                     WHERE e.user = ? AND e.module = ?",
                )
                .bind(user_id)
                .bind(module_id)
                .fetch_all(&state.db)
                .await?;

                for (eval_id, code) in &codes {
                    let name = format!("{}/code_{eval_id}.txt", userdir);
                    zip.start_file(name, options).map_err(|e| {
                        ApiError::Internal(format!("ZIP error: {e}"))
                    })?;
                    zip.write_all(code.as_bytes()).map_err(|e| {
                        ApiError::Internal(format!("ZIP error: {e}"))
                    })?;
                    count += 1;
                }
            }
        }

        if count == 0 {
            return Err(ApiError::NotFound(
                "Pro tuto úlohu nebyla nalezena žádná odevzdání ke stažení.".into(),
            ));
        }
        zip.finish().map_err(|e| ApiError::Internal(format!("ZIP error: {e}")))?;
        count
    };

    let _ = files_added;
    let bytes = buf.into_inner();
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip"),
            (
                header::CONTENT_DISPOSITION,
                &format!("inline; filename=\"task_{task_id}.zip\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// Strip Czech/Slovak diacritics for ZIP entry names (mirrors
/// `util.submissions.strip_accents`).
fn strip_accents(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' => 'a',
            'č' => 'c',
            'ď' => 'd',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'ě' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ľ' => 'l',
            'ň' => 'n',
            'ó' | 'ò' | 'ô' | 'ö' => 'o',
            'ř' => 'r',
            'š' => 's',
            'ť' => 't',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ů' => 'u',
            'ý' => 'y',
            'ž' => 'z',
            'Á' | 'À' | 'Â' | 'Ä' => 'A',
            'Č' => 'C',
            'Ď' => 'D',
            'É' | 'È' | 'Ê' | 'Ë' => 'E',
            'Ě' => 'E',
            'Í' | 'Ì' | 'Î' | 'Ï' => 'I',
            'Ľ' => 'L',
            'Ň' => 'N',
            'Ó' | 'Ò' | 'Ô' | 'Ö' => 'O',
            'Ř' => 'R',
            'Š' => 'S',
            'Ť' => 'T',
            'Ú' | 'Ù' | 'Û' | 'Ü' => 'U',
            'Ů' => 'U',
            'Ý' => 'Y',
            'Ž' => 'Z',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accents_stripped() {
        assert_eq!(strip_accents("Příliš žluťoučký kůň"), "Prilis zlutoucky kun");
    }
}

// ---------------------------------------------------------------------------
// Misc admin endpoints (ported from endpoint/admin/*.py)
// ---------------------------------------------------------------------------

use super::content::{audit_log, data_root};
use axum::{
    extract::Query,
    http::{HeaderMap, StatusCode},
};
use chrono::NaiveDateTime;
use ksi_models::{CodeExecution, Evaluation};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::QueryBuilder;

/// `GET /admin/subm/eval/{eval_id}` — all files of one evaluation as a ZIP.
pub async fn subm_files_eval(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(eval_id): Path<i64>,
) -> ApiResult<Response> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }

    let files: Vec<(String,)> =
        sqlx::query_as("SELECT DISTINCT path FROM submitted_files WHERE evaluation = ?")
            .bind(eval_id)
            .fetch_all(&state.db)
            .await?;

    let mut buf = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default();
        for (path,) in &files {
            if let Ok(data) = std::fs::read(path) {
                let name = std::path::Path::new(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "file".into());
                zip.start_file(name, options)
                    .map_err(|e| ApiError::Internal(format!("ZIP error: {e}")))?;
                zip.write_all(&data)
                    .map_err(|e| ApiError::Internal(format!("ZIP error: {e}")))?;
            }
        }
        zip.finish()
            .map_err(|e| ApiError::Internal(format!("ZIP error: {e}")))?;
    }

    let bytes = buf.into_inner();
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip"),
            (
                header::CONTENT_DISPOSITION,
                &format!("inline; filename=\"eval_{eval_id}.zip\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// Read a file as text, or return the Python-style "file does not exist"
/// message (mirrors `EvalCode._file_or_error`).
fn file_or_error(path: &std::path::Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => format!("Soubor {} neexistuje.", path.display()),
    }
}

/// `GET /admin/evalCodes/{id}` — source, merged code, stdout/stderr of one
/// evaluation's execution directory.
pub async fn eval_code(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }

    let code: Option<(i64, String)> =
        sqlx::query_as("SELECT evaluation, code FROM submitted_codes WHERE evaluation = ?")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
    let Some((code_eval, code_text)) = code else {
        // Legacy error shape, kept for parity with the Python endpoint
        // (which also answers 200 here).
        return Ok(Json(json!({"errors": [{"id": 5, "title": "Code not found in db"}]}))
            .into_response());
    };

    let evaluation: Option<(i64, i64, i64)> =
        sqlx::query_as("SELECT id, module, user FROM evaluations WHERE id = ?")
            .bind(code_eval)
            .fetch_optional(&state.db)
            .await?;
    let Some((evaluation_id, module_id, eval_user)) = evaluation else {
        return Ok(Json(json!({"errors": [{"id": 5, "title": "Evaluation not found in db"}]}))
            .into_response());
    };

    let eval_dir = data_root()
        .join("exec")
        .join(format!("module_{module_id}"))
        .join(format!("user_{eval_user}"));

    let source_path = eval_dir.join("source");
    let lines: Vec<String> = std::fs::read_to_string(&source_path)
        .map(|s| s.split('\n').map(|l| l.to_string()).collect())
        .unwrap_or_default();

    if lines.len() >= 2 && (lines[0] != "evaluation" || lines[1] != id.to_string()) {
        return Ok(Json(json!({
            "evalCode": {
                "id": evaluation_id,
                "code": code_text,
                "merged": "Další záznamy o vyhodnocení už nejsou k dispozici, byly nahrazeny novým opravením nebo spuštěním.",
            }
        }))
        .into_response());
    }

    Ok(Json(json!({
        "evalCode": {
            "id": evaluation_id,
            "code": code_text,
            "merged": file_or_error(&eval_dir.join("box").join("run")),
            "stdout": file_or_error(&eval_dir.join("stdout")),
            "stderr": file_or_error(&eval_dir.join("stderr")),
            "merge_stdout": file_or_error(&eval_dir.join("merge.stdout")),
            "check_stdout": file_or_error(&eval_dir.join("check.stdout")),
        }
    }))
    .into_response())
}

/// `GET /admin/evaluations/{id}` — correction info for a single evaluation
/// (mirrors `util/correction.py::corr_eval_to_json`).
pub async fn get_evaluation(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }

    let evaluation: Option<Evaluation> = sqlx::query_as(
        "SELECT id, user, module, evaluator, points, ok, cheat, full_report, time
         FROM evaluations WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    let Some(evaluation) = evaluation else {
        return Err(ApiError::NotFound("Hodnocení neexistuje.".into()));
    };

    let module_type: Option<String> =
        sqlx::query_scalar::<_, Option<String>>("SELECT type FROM modules WHERE id = ?")
            .bind(evaluation.module)
            .fetch_optional(&state.db)
            .await?
            .flatten();

    let mut eval_json = json!({
        "eval_id": evaluation.id,
        "points": evaluation.points,
        "last_modified": evaluation.time.map(|t| t.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()),
        "corrected_by": evaluation.evaluator,
        "full_report": evaluation.full_report,
        "cheat": evaluation.cheat,
    });
    match module_type.as_deref() {
        Some("general") => {
            let files: Vec<(i64, String)> =
                sqlx::query_as("SELECT id, path FROM submitted_files WHERE evaluation = ?")
                    .bind(evaluation.id)
                    .fetch_all(&state.db)
                    .await?;
            eval_json["general"] = json!({
                "files": files.iter().map(|(fid, path)| {
                    let filename = std::path::Path::new(path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    json!({"id": fid, "filename": filename})
                }).collect::<Vec<_>>(),
            });
        }
        Some("programming") => eval_json["programming"] = json!({}),
        Some("quiz") => eval_json["quiz"] = json!({}),
        Some("sortable") => eval_json["sortable"] = json!({}),
        Some("text") => eval_json["text"] = json!({}),
        _ => {}
    }

    Ok(Json(json!({ "evaluation": eval_json })))
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct ExecsQuery {
    user: Option<i64>,
    module: Option<i64>,
    limit: Option<i64>,
    page: Option<i64>,
    from: Option<String>,
    to: Option<String>,
    result: Option<String>,
}

fn push_exec_filters<'a>(qb: &mut QueryBuilder<'a, sqlx::Sqlite>, q: &'a ExecsQuery) {
    if let Some(u) = q.user {
        qb.push(" AND user = ").push_bind(u);
    }
    if let Some(m) = q.module {
        qb.push(" AND module = ").push_bind(m);
    }
    if let Some(from) = q
        .from
        .as_deref()
        .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").ok())
    {
        qb.push(" AND time >= ").push_bind(from);
    }
    if let Some(to) = q
        .to
        .as_deref()
        .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").ok())
    {
        qb.push(" AND time <= ").push_bind(to);
    }
    if let Some(r) = q.result.as_deref() {
        qb.push(" AND result = ").push_bind(r);
    }
}

/// `GET /admin/execs` — paginated code executions with optional filters.
pub async fn list_execs(
    State(state): State<SharedState>,
    user: AuthUser,
    Query(q): Query<ExecsQuery>,
) -> ApiResult<Json<Value>> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }

    let mut count_qb =
        QueryBuilder::<sqlx::Sqlite>::new("SELECT COUNT(*) FROM code_executions WHERE 1 = 1");
    push_exec_filters(&mut count_qb, &q);
    let total: i64 = count_qb
        .build_query_scalar()
        .fetch_one(&state.db)
        .await?;

    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let page = q.page.unwrap_or(0).max(0);

    let mut list_qb = QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT id, module, user, code, result, time, report FROM code_executions WHERE 1 = 1",
    );
    push_exec_filters(&mut list_qb, &q);
    list_qb
        .push(" ORDER BY id DESC LIMIT ")
        .push_bind(limit)
        .push(" OFFSET ")
        .push_bind(limit * page);
    let execs: Vec<CodeExecution> = list_qb
        .build_query_as()
        .fetch_all(&state.db)
        .await?;

    Ok(Json(json!({
        "execs": execs.iter().map(ksi_models::CodeExecutionJson::from).collect::<Vec<_>>(),
        "meta": { "total": total, "page": page },
    })))
}

/// `GET /admin/execs/{id}` — a single code execution.
///
/// Note: the Python endpoint never worked — it references an undefined
/// `sessiton` name. The Rust port implements the clearly intended behavior.
pub async fn get_exec(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }

    let execution: Option<CodeExecution> = sqlx::query_as(
        "SELECT id, module, user, code, result, time, report FROM code_executions WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    let Some(execution) = execution else {
        return Err(ApiError::NotFound("Spuštění neexistuje.".into()));
    };

    Ok(Json(json!(ksi_models::CodeExecutionJson::from(&execution))))
}

// ---------------------------------------------------------------------------
// GET+POST /admin/instanceConfig — instance configuration (admin only)
// ---------------------------------------------------------------------------

/// `GET /admin/instanceConfig` — all non-secret config entries.
pub async fn get_instance_config(
    State(state): State<SharedState>,
    user: AuthUser,
) -> ApiResult<Response> {
    if !user.is_logged_in() || !user.is_admin() {
        return Ok((
            StatusCode::UNAUTHORIZED,
            Json(json!({"result": "error", "error": "Only admin can see the instance configuration."})),
        )
            .into_response());
    }

    let rows: Vec<(String, Option<String>, bool)> =
        sqlx::query_as("SELECT \"key\", value, secret FROM config WHERE secret = 0")
            .fetch_all(&state.db)
            .await?;

    let config: Vec<Value> = rows
        .into_iter()
        .map(|(key, value, secret)| {
            let encrypted = value
                .as_deref()
                .map(|v| v.starts_with("ENCRYPTED:v1:"))
                .unwrap_or(false);
            json!({
                "key": key,
                "value": if encrypted { Value::Null } else { json!(value) },
                "secret": secret,
                "encrypted": encrypted,
            })
        })
        .collect();

    Ok(Json(json!({ "config": config })).into_response())
}

#[derive(Debug, Deserialize)]
pub(crate) struct InstanceConfigBody {
    key: Option<String>,
    value: Option<Value>,
}

/// `POST /admin/instanceConfig` — set a config entry.
pub async fn set_instance_config(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(body): Json<InstanceConfigBody>,
) -> ApiResult<Response> {
    if !user.is_logged_in() || !user.is_admin() {
        return Ok((
            StatusCode::UNAUTHORIZED,
            Json(json!({"result": "error", "error": "Only admin can change the instance configuration."})),
        )
            .into_response());
    }

    let (Some(key), Some(value)) = (body.key, body.value) else {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({"result": "error", "error": "Missing key or value."})),
        )
            .into_response());
    };
    let value_str = match &value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };

    let previous: Option<String> =
        sqlx::query_scalar::<_, Option<String>>("SELECT value FROM config WHERE key = ? AND secret = 0")
            .bind(&key)
            .fetch_optional(&state.db)
            .await?
            .flatten();
    audit_log(
        &state.db,
        "CONFIG",
        user.id,
        &format!("Changed instance config {key}"),
        Some(json!({"key": key, "previous": previous})),
    )
    .await;

    sqlx::query(
        "INSERT INTO config (\"key\", value, secret) VALUES (?, ?, 0)
         ON CONFLICT(\"key\") DO UPDATE SET value = excluded.value",
    )
    .bind(&key)
    .bind(&value_str)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({})).into_response())
}

/// `GET /admin/monitoring-dashboard` — monitoring dashboard URL from config.
pub async fn monitoring_dashboard(
    State(state): State<SharedState>,
    user: AuthUser,
) -> ApiResult<Response> {
    if !user.is_logged_in() || !user.is_org() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!("Nedostatecna opravneni")),
        )
            .into_response());
    }

    let url: Option<String> =
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT value FROM config WHERE key = 'monitoring_dashboard_url'",
        )
        .fetch_optional(&state.db)
        .await?
        .flatten();

    Ok(Json(json!({ "url": url })).into_response())
}

// ---------------------------------------------------------------------------
// GET /admin/user-export — CSV of all participants of a year
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct ExportRow {
    id: i64,
    last_name: String,
    first_name: String,
    email: String,
    sex: String,
    addr_street: String,
    addr_city: String,
    addr_zip: String,
    addr_country: String,
    school_name: String,
    school_street: String,
    school_city: String,
    school_zip: String,
    school_country: String,
    school_finish: i64,
    tshirt_size: String,
    total_score: Option<f64>,
    tasks_cnt: i64,
    cheat: Option<i64>,
}

/// Format a float like Python's `str(float)`.
fn py_float(x: f64) -> String {
    format!("{x:?}")
}

/// `GET /admin/user-export` — CSV of all solvers of the year (from the
/// `year` header, or the latest year).
pub async fn user_export(
    State(state): State<SharedState>,
    user: AuthUser,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }

    let year_id: Option<i64> = match headers
        .get("year")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse().ok())
    {
        Some(id) => Some(id),
        None => {
            sqlx::query_scalar::<_, Option<i64>>("SELECT MAX(id) FROM years")
                .fetch_optional(&state.db)
                .await?
                .flatten()
        }
    };
    let Some(year_id) = year_id else {
        return Err(ApiError::BadRequest("Neznámý ročník.".into()));
    };

    let year_row: Option<(String, i64)> =
        sqlx::query_as("SELECT year, point_pad FROM years WHERE id = ?")
            .bind(year_id)
            .fetch_optional(&state.db)
            .await?;
    let Some((year_name, point_pad)) = year_row else {
        return Err(ApiError::BadRequest("Neznámý ročník.".into()));
    };

    // Score per (user, module), evaluation_public modules only.
    // (Inlined as a subquery below via a temp view-like CTE.)
    let rows: Vec<ExportRow> = sqlx::query_as(
        "SELECT u.id, u.last_name, u.first_name, u.email, u.sex,
                p.addr_street, p.addr_city, p.addr_zip, p.addr_country,
                p.school_name, p.school_street, p.school_city, p.school_zip, p.school_country,
                p.school_finish, p.tshirt_size,
                SUM(pu.points) AS total_score, tp.tasks_cnt AS tasks_cnt, MAX(pu.cheat) AS cheat
         FROM users u
         JOIN (SELECT e.user AS user_id, e.module AS module_id,
                      MAX(e.points) AS points, MAX(e.cheat) AS cheat
               FROM evaluations e
               JOIN modules m ON m.id = e.module
               JOIN tasks t ON t.id = m.task
               JOIN waves w ON w.id = t.wave
               WHERE t.evaluation_public = 1 AND w.year = ?
               GROUP BY e.user, e.module) pu ON pu.user_id = u.id
         JOIN (SELECT e.user AS user_id, COUNT(DISTINCT t.id) AS tasks_cnt
               FROM evaluations e
               JOIN modules m ON m.id = e.module
               JOIN tasks t ON t.id = m.task
               JOIN waves w ON w.id = t.wave
               WHERE w.year = ?
               GROUP BY e.user) tp ON tp.user_id = u.id
         JOIN profiles p ON p.user_id = u.id
         WHERE u.role = 'participant' AND tp.tasks_cnt > 0
         GROUP BY u.id
         ORDER BY total_score DESC, u.last_name, u.first_name",
    )
    .bind(year_id)
    .bind(year_id)
    .fetch_all(&state.db)
    .await?;

    // year_end: e.g. "2026/2027" -> 2027
    let year_end: i64 = year_name
        .replace(' ', "")
        .split('/')
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
        + 1;

    let (users_hs, users_other): (Vec<&ExportRow>, Vec<&ExportRow>) = rows
        .iter()
        .partition(|r| r.school_finish >= year_end);

    let sum_points_real: f64 = sqlx::query_scalar::<_, Option<f64>>(
        "SELECT SUM(m.max_points) FROM modules m
         JOIN tasks t ON t.id = m.task
         JOIN waves w ON w.id = t.wave
         WHERE w.year = ? AND m.bonus = 0",
    )
    .bind(year_id)
    .fetch_optional(&state.db)
    .await?
    .flatten()
    .unwrap_or(0.0);
    let sum_points_bonus_real: f64 = sqlx::query_scalar::<_, Option<f64>>(
        "SELECT SUM(m.max_points) FROM modules m
         JOIN tasks t ON t.id = m.task
         JOIN waves w ON w.id = t.wave
         WHERE w.year = ?",
    )
    .bind(year_id)
    .fetch_optional(&state.db)
    .await?
    .flatten()
    .unwrap_or(0.0);
    let sum_points = sum_points_real.max(point_pad as f64);

    let percentage: f64 = sqlx::query_scalar::<_, Option<String>>(
        "SELECT value FROM config WHERE key = 'successful_participant_percentage'",
    )
    .fetch_optional(&state.db)
    .await?
    .flatten()
    .and_then(|s| s.parse::<f64>().ok())
    .unwrap_or(60.0);

    fn stringify(users: &[&ExportRow], sum_points: f64, percentage: f64) -> String {
        let mut res = String::new();
        let mut order = 0i64;
        let mut last_points = f64::NAN;
        for (i, r) in users.iter().enumerate() {
            let points = r.total_score.unwrap_or(0.0);
            let cheat = r.cheat.unwrap_or(0) != 0;
            if points != last_points {
                order = i as i64 + 1;
                last_points = points;
            }
            let successful = points >= (percentage / 100.0) * sum_points && !cheat;
            res.push_str(&format!(
                "{};{};{};{};{};{};{};{};{};{};{};{};{};{};{};{};{};{};{};{}\n",
                order,
                r.last_name,
                r.first_name,
                py_float(points),
                if successful { "A" } else { "N" },
                if cheat { "A" } else { "N" },
                r.email,
                r.sex,
                r.addr_street,
                r.addr_city,
                r.addr_zip,
                r.addr_country,
                r.school_name,
                r.school_street,
                r.school_city,
                r.school_zip,
                r.school_country,
                r.school_finish,
                r.tshirt_size,
                r.id,
            ));
        }
        res
    }

    let table_header = "Pořadí;Příjmení;Jméno;Body;Úspěšný řešitel;Podvod;E-mail;Pohlaví;Ulice;Město;PSČ;Země;Škola;Adresa školy;Město školy;PSČ školy;Země školy;Rok maturity;Velikost trička;ID\n";

    let mut out = String::new();
    out.push_str(&format!(
        "Celkem bodů: {}, včetně bonusových úloh: {}, bodová vycpávka: {}\n",
        py_float(sum_points_real),
        py_float(sum_points_bonus_real),
        point_pad
    ));
    out.push_str("Středoškoláci\n");
    out.push_str(table_header);
    out.push_str(&stringify(&users_hs, sum_points, percentage));
    out.push_str("\nOstatní\n");
    out.push_str(table_header);
    out.push_str(&stringify(&users_other, sum_points, percentage));

    Ok((
        [
            (header::CONTENT_TYPE, "text/csv"),
            (
                header::CONTENT_DISPOSITION,
                &format!("inline; filename=\"resitele_{year_id}.csv\""),
            ),
        ],
        out,
    )
        .into_response())
}
