//! Module endpoints: details, submissions, file uploads and code runs.
//!
//! Ports of the Python backend's `endpoint/module.py` (`Module`,
//! `ModuleSubmit`) and `endpoint/runcode.py` (`RunCode`):
//!
//! - `GET /modules/{id}` — module details for the current user
//! - `POST /modules/{id}/submit` — submit a solution (JSON `{"content": …}`)
//! - `POST /modules/{id}/submitFiles` — upload files for a general module
//! - `POST /runCode/{moduleId}/submit` — run code without submitting

use axum::{
    extract::{Multipart, Path, State},
    http::{header, HeaderMap},
    Json,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use ksi_auth::AuthUser;
use ksi_models::{ModuleDetailJson, SubmittedFileJson};
use ksi_sandbox::{
    evaluate as sandbox_evaluate, run_code as sandbox_run_code, IsolateSandbox,
    ProgramSpec, Reporter, RunContext, RunType, SandboxError,
};

use crate::{
    error::{ApiError, ApiResult},
    SharedState,
};

/// Max upload batch size (Python: `MAX_UPLOAD_FILE_SIZE = 25 * 1024**2`).
const MAX_UPLOAD_FILE_SIZE: u64 = 25 * 1024 * 1024;
/// Max files per solution (Python: `MAX_UPLOAD_FILE_COUNT = 20`).
const MAX_UPLOAD_FILE_COUNT: i64 = 20;
/// Default submissions per 24h (Python: `unsuccessful_tries_per_day()` → 20).
const DEFAULT_SUBMIT_RATELIMIT: i64 = 20;

/// Full module row (the shared `ksi_models::Module` lacks `custom`, `data`
/// and `submit_ratelimit`, which the submission flow needs).
#[derive(Debug, Clone, sqlx::FromRow)]
struct ModuleFull {
    id: i64,
    task: i64,
    #[sqlx(rename = "type")]
    module_type: String,
    name: String,
    description: Option<String>,
    max_points: Option<f64>,
    autocorrect: bool,
    custom: bool,
    data: Option<String>,
    submit_ratelimit: Option<i64>,
}

async fn module_by_id(db: &sqlx::SqlitePool, id: i64) -> ApiResult<Option<ModuleFull>> {
    Ok(sqlx::query_as::<_, ModuleFull>(
        "SELECT id, task, type, name, description, max_points, autocorrect,
                custom, data, submit_ratelimit
         FROM modules WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await?)
}

async fn wave_published(db: &sqlx::SqlitePool, task_id: i64) -> ApiResult<Option<chrono::NaiveDateTime>> {
    Ok(sqlx::query_scalar::<_, Option<chrono::NaiveDateTime>>(
        "SELECT w.time_published FROM waves w JOIN tasks t ON t.wave = w.id WHERE t.id = ?",
    )
    .bind(task_id)
    .fetch_optional(db)
    .await?
    .flatten())
}

/// Mirrors `util.task.status(...) == LOCKED` for these endpoints: a task is
/// locked for anonymous users and for non-privileged users while its wave
/// is not published yet.
async fn is_task_locked(
    db: &sqlx::SqlitePool,
    task_id: i64,
    user: &AuthUser,
) -> ApiResult<bool> {
    if !user.is_logged_in() {
        return Ok(true);
    }
    if user.is_org() || user.is_tester() {
        return Ok(false);
    }
    let now = Utc::now().naive_utc();
    Ok(wave_published(db, task_id)
        .await?
        .map(|t| t > now)
        .unwrap_or(true))
}

async fn task_deadline(db: &sqlx::SqlitePool, task_id: i64) -> ApiResult<Option<chrono::NaiveDateTime>> {
    Ok(sqlx::query_scalar::<_, Option<chrono::NaiveDateTime>>(
        "SELECT time_deadline FROM tasks WHERE id = ?",
    )
    .bind(task_id)
    .fetch_optional(db)
    .await?
    .flatten())
}

/// All evaluation ids of `user_id` for `module_id` (Python:
/// `util.module.existing_evaluation`).
async fn existing_evaluations(
    db: &sqlx::SqlitePool,
    module_id: i64,
    user_id: i64,
) -> ApiResult<Vec<i64>> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT id FROM evaluations WHERE user = ? AND module = ?",
    )
    .bind(user_id)
    .bind(module_id)
    .fetch_all(db)
    .await?)
}

// ---------------------------------------------------------------------------
// Custom (per-user generated) assignments
// ---------------------------------------------------------------------------

/// Apply a `modules_custom` row to a module's data/description
/// (mirrors `util/module.py::_apply_custom`).
fn apply_custom(data: &str, description: &str, custom: &ksi_models::ModuleCustom) -> (String, String) {
    let mut data = data.to_string();
    let mut description = custom
        .description
        .clone()
        .unwrap_or_else(|| description.to_string());

    if let Some(cdata) = &custom.data {
        if let (Ok(mut mdata), Ok(cdata_json)) = (
            serde_json::from_str::<Value>(&data),
            serde_json::from_str::<Value>(cdata),
        ) {
            let mdata_has_text_eval = mdata
                .get("text")
                .and_then(|t| t.get("eval_script"))
                .is_some();
            let cdata_has_text_eval = cdata_json
                .get("text")
                .and_then(|t| t.get("eval_script"))
                .is_some();
            if mdata.get("text").is_some() && mdata_has_text_eval && !cdata_has_text_eval {
                // Preserve the 'eval_script' key.
                let mut merged = cdata_json.clone();
                if let (Some(eval_script), Some(obj)) = (
                    mdata.get("text").and_then(|t| t.get("eval_script")).cloned(),
                    merged.as_object_mut(),
                ) {
                    if let Some(text) = obj.get_mut("text").and_then(|t| t.as_object_mut()) {
                        text.insert("eval_script".to_string(), eval_script);
                    }
                }
                data = serde_json::to_string(&merged).unwrap_or(data);
            } else if mdata.get("programming").is_some() {
                if let (Some(map), Some(cmap)) =
                    (mdata.as_object_mut(), cdata_json.as_object())
                {
                    for (k, v) in cmap {
                        map.insert(k.clone(), v.clone());
                    }
                }
                data = serde_json::to_string(&mdata).unwrap_or(data);
            } else {
                data = cdata.clone();
            }
        } else {
            data = cdata.clone();
        }
    }

    if let Some(replace) = &custom.description_replace {
        if let Ok(map) = serde_json::from_str::<Value>(replace) {
            if let Some(obj) = map.as_object() {
                for (k, v) in obj {
                    if let Some(v) = v.as_str() {
                        description = description.replace(k, v);
                    }
                }
            }
        }
    }

    (data, description)
}

/// Load the effective `(data, description)` for a module, generating a
/// per-user assignment via `data/modules/{id}/module-gen` when the module is
/// custom and no usable row exists yet (mirrors `util/module.py::_load_custom`).
async fn load_custom_data(
    db: &sqlx::SqlitePool,
    module: &ModuleFull,
    user: &AuthUser,
) -> (String, String) {
    let data = module.data.clone().unwrap_or_default();
    let description = module.description.clone().unwrap_or_default();
    let user_id = match user.id {
        Some(id) if module.custom => id,
        _ => return (data, description),
    };

    let custom: Option<ksi_models::ModuleCustom> = sqlx::query_as(
        "SELECT module, user, description, description_replace, data, error
         FROM modules_custom WHERE module = ? AND user = ?",
    )
    .bind(module.id)
    .bind(user_id)
    .fetch_optional(db)
    .await
    .unwrap_or(None);

    if let Some(c) = &custom {
        if c.error.is_none() {
            return apply_custom(&data, &description, c);
        }
    }

    // No usable assignment yet: run the module-gen script (10s timeout).
    let gen = std::path::PathBuf::from("data")
        .join("modules")
        .join(module.id.to_string())
        .join("module-gen");
    let error_html = "<div class=\"alert alert-danger\">Chyba při vytváření \
        individuálního zadání, kontaktuj organizátora!</div>";
    if !gen.is_file() {
        return (data, description);
    }

    let abs = std::path::absolute(&gen).unwrap_or(gen);
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new(abs)
            .arg(user_id.to_string())
            .output(),
    )
    .await;
    let output = match output {
        Ok(Ok(o)) => o,
        _ => {
            persist_custom(db, module.id, user_id, None, None, None, Some("Timeout expired!".into())).await;
            return (data, format!("{description}{error_html}"));
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        persist_custom(db, module.id, user_id, None, None, None, Some(stderr)).await;
        return (data, format!("{description}{error_html}"));
    }

    let mut parsed: Value = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(e) => {
            persist_custom(db, module.id, user_id, None, None, None, Some(e.to_string())).await;
            return (data, format!("{description}{error_html}"));
        }
    };
    let obj = match parsed.as_object_mut() {
        Some(o) => o,
        None => {
            persist_custom(db, module.id, user_id, None, None, None, Some("module-gen: invalid JSON".into())).await;
            return (data, format!("{description}{error_html}"));
        }
    };
    // NOTE: mirrors a quirk in the Python code, which stores `description`
    // when the `assignment` key is present.
    let custom_description = if obj.contains_key("assignment") {
        obj.remove("description").map(|d| d.to_string())
    } else {
        None
    };
    let custom_description_replace = obj
        .remove("description_replace")
        .map(|d| d.to_string());
    let custom_data = Some(parsed.to_string());

    persist_custom(
        db,
        module.id,
        user_id,
        custom_description.clone(),
        custom_description_replace.clone(),
        custom_data.clone(),
        None,
    )
    .await;

    let c = ksi_models::ModuleCustom {
        module: module.id,
        user: user_id,
        description: custom_description,
        description_replace: custom_description_replace,
        data: custom_data,
        error: None,
    };
    apply_custom(&data, &description, &c)
}

/// Upsert a `modules_custom` row (mirrors the Python `_load_custom` persistence).
async fn persist_custom(
    db: &sqlx::SqlitePool,
    module_id: i64,
    user_id: i64,
    description: Option<String>,
    description_replace: Option<String>,
    data: Option<String>,
    error: Option<String>,
) {
    let exists: Option<i64> = sqlx::query_scalar(
        "SELECT module FROM modules_custom WHERE module = ? AND user = ?",
    )
    .bind(module_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
    .unwrap_or(None);
    if exists.is_some() {
        let _ = sqlx::query(
            "UPDATE modules_custom SET description = ?, description_replace = ?, data = ?, error = ?
             WHERE module = ? AND user = ?",
        )
        .bind(description)
        .bind(description_replace)
        .bind(data)
        .bind(error)
        .bind(module_id)
        .bind(user_id)
        .execute(db)
        .await;
    } else {
        let _ = sqlx::query(
            "INSERT INTO modules_custom (module, user, description, description_replace, data, error)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(module_id)
        .bind(user_id)
        .bind(description)
        .bind(description_replace)
        .bind(data)
        .bind(error)
        .execute(db)
        .await;
    }
}

// ---------------------------------------------------------------------------
// GET /modules/{id}
// ---------------------------------------------------------------------------

/// Salted hash used for the `edulint_source_id` (mirrors
/// `util.programming.to_json`).
fn edulint_source_id(task_id: i64, module_id: i64, user_id: i64) -> String {
    let salt = std::env::var("KSI_SALT").unwrap_or_default();
    let seminar = std::env::var("KSI_SEMINAR_NAME_SHORT").unwrap_or_else(|_| "KSI".into());
    let mut hasher = Sha256::new();
    hasher.update(format!("{salt}{user_id}"));
    let hashed = format!("{:x}", hasher.finalize());
    format!("{seminar}:taskId_{task_id}:moduleId_{module_id}:userId_{hashed}")
}

/// Build the full module JSON (mirrors `util/module.py::to_json`).
async fn module_to_json(
    db: &sqlx::SqlitePool,
    module: &ModuleFull,
    user: &AuthUser,
) -> ApiResult<ModuleDetailJson> {
    let (data, description) = load_custom_data(db, module, user).await;
    let data_json: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
    let user_id = user.id.unwrap_or(0);

    // Best evaluation of this user for this module.
    let best: Option<(i64, bool, Option<f64>, Option<chrono::NaiveDateTime>, i64)> =
        sqlx::query_as(
            "SELECT e.id, e.ok, CAST(e.points AS REAL), e.time, t.evaluation_public
             FROM evaluations e
             JOIN modules m ON m.id = e.module
             JOIN tasks t ON t.id = m.task
             WHERE e.user = ? AND e.module = ?
             ORDER BY e.ok DESC, e.points DESC, e.time DESC
             LIMIT 1",
        )
        .bind(user_id)
        .bind(module.id)
        .fetch_optional(db)
        .await?;

    let state = match &best {
        Some((_, ok, _, _, _)) => {
            if *ok {
                "correct"
            } else {
                "incorrect"
            }
        }
        None => "blank",
    }
    .to_string();
    let score = match &best {
        Some((_, _, _, _, eval_public)) if *eval_public == 1 => Some(module.id as f64),
        _ => None,
    };

    let mut out = ModuleDetailJson {
        id: module.id,
        module_type: module.module_type.clone(),
        name: module.name.clone(),
        // swagger requires a string description; fall back to "".
        description: Some(description),
        autocorrect: module.autocorrect,
        max_score: module.max_points,
        state,
        score,
        ..Default::default()
    };

    match module.module_type.as_str() {
        "programming" => {
            let prog = data_json.get("programming");
            let default_code = prog
                .and_then(|p| p.get("default_code"))
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            let mut code = default_code.clone();
            let mut last_time = None;
            let mut last_origin = None;

            if let Some((eval_id, _, _, time, _)) = &best {
                let submitted: Option<String> = sqlx::query_scalar(
                    "SELECT code FROM submitted_codes WHERE evaluation = ? LIMIT 1",
                )
                .bind(eval_id)
                .fetch_optional(db)
                .await?
                .flatten();
                if let Some(c) = submitted {
                    code = c;
                    last_time = time.map(|t| t.format("%Y-%m-%dT%H:%M:%S%.6f").to_string());
                    last_origin = Some("evaluation".to_string());
                }
            }
            if last_origin.is_none() {
                let exec: Option<(String, Option<chrono::NaiveDateTime>)> = sqlx::query_as(
                    "SELECT code, time FROM code_executions
                     WHERE module = ? AND user = ? ORDER BY time DESC LIMIT 1",
                )
                .bind(module.id)
                .bind(user_id)
                .fetch_optional(db)
                .await?;
                if let Some((c, t)) = exec {
                    code = c;
                    last_time = t.map(|t| t.format("%Y-%m-%dT%H:%M:%S%.6f").to_string());
                    last_origin = Some("execution".to_string());
                }
            }

            out.code = Some(code);
            out.default_code = Some(default_code);
            out.edulint_source_id = Some(edulint_source_id(module.task, module.id, user_id));
            out.last_time = last_time;
            out.last_origin = last_origin;
        }
        "quiz" => {
            if let Some(questions) = data_json.get("quiz").and_then(|q| q.as_array()) {
                out.questions = Some(
                    questions
                        .iter()
                        .map(|q| {
                            json!({
                                "type": q.get("type"),
                                "question": q.get("question"),
                                "text": q.get("text").and_then(|t| t.as_str()).unwrap_or(""),
                                "options": q.get("options"),
                            })
                        })
                        .collect(),
                );
            }
        }
        "sortable" => {
            if let Some(s) = data_json.get("sortable") {
                out.sortable_list = Some(json!({
                    "fixed": s.get("fixed"),
                    "movable": s.get("movable"),
                }));
            }
        }
        "general" => {
            let files: Vec<(i64, String)> = sqlx::query_as(
                "SELECT f.id, f.path FROM submitted_files f
                 JOIN evaluations e ON e.id = f.evaluation
                 WHERE e.user = ? AND e.module = ?",
            )
            .bind(user_id)
            .bind(module.id)
            .fetch_all(db)
            .await?;
            out.submitted_files = Some(
                files
                    .into_iter()
                    .map(|(id, path)| SubmittedFileJson {
                        id,
                        filename: std::path::Path::new(&path)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or(path),
                    })
                    .collect(),
            );
        }
        "text" => {
            if let Some(text) = data_json.get("text") {
                if let Some(qs) = text.get("questions").and_then(|q| q.as_array()) {
                    out.fields = Some(
                        qs.iter()
                            .filter_map(|q| q.as_str().map(str::to_string))
                            .collect(),
                    );
                } else if let Some(inputs) = text.get("inputs").and_then(|i| i.as_u64()) {
                    out.fields = Some(
                        (1..=inputs)
                            .map(|i| format!("Otázka {i}"))
                            .collect(),
                    );
                }
            }
        }
        _ => {}
    }

    Ok(out)
}

#[derive(serde::Serialize)]
pub struct ModuleResponse {
    pub module: ModuleDetailJson,
}

/// `GET /modules/{id}` — module details for the current user.
pub async fn get_module(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<ModuleResponse>> {
    if !user.is_logged_in() {
        return Err(ApiError::BadRequest(
            "Modul s tímto ID neexistuje.".into(),
        ));
    }

    let module = module_by_id(&state.db, id).await?;
    let module = match module {
        None => return Err(ApiError::NotFound("Modul s tímto ID neexistuje.".into())),
        Some(m) => m,
    };

    if is_task_locked(&state.db, module.task, &user).await? {
        return Err(ApiError::Forbidden("Modul je uzamčen.".into()));
    }

    Ok(Json(ModuleResponse {
        module: module_to_json(&state.db, &module, &user).await?,
    }))
}

// ---------------------------------------------------------------------------
// POST /modules/{id}/submit
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SubmitBody {
    pub content: Value,
}

/// Grant an achievement from an `action add_achievement <id>` line
/// (mirrors `util/module.py::perform_action`).
async fn perform_action(db: &sqlx::SqlitePool, module_task: i64, user_id: i64, action: &str) {
    let parts: Vec<&str> = action.split_whitespace().collect();
    if parts.len() < 2 || parts[0] != "action" || parts[1] != "add_achievement" {
        return;
    }
    let achievement_id: i64 = match parts.get(2).and_then(|s| s.parse().ok()) {
        Some(id) => id,
        None => return,
    };
    let already: Option<i64> = sqlx::query_scalar(
        "SELECT achievement_id FROM user_achievement WHERE user_id = ? AND achievement_id = ?",
    )
    .bind(user_id)
    .bind(achievement_id)
    .fetch_optional(db)
    .await
    .unwrap_or(None);
    if already.is_some() {
        return;
    }
    let _ = sqlx::query(
        "INSERT INTO user_achievement (user_id, achievement_id, task_id) VALUES (?, ?, ?)",
    )
    .bind(user_id)
    .bind(achievement_id)
    .bind(module_task)
    .execute(db)
    .await;
}

/// Evaluate a quiz submission (mirrors `util/quiz.py::evaluate`).
fn evaluate_quiz(module: &ModuleFull, data: &Value) -> (bool, String) {
    let mut report = format!(
        "=== Evaluating quiz id '{}' for task id '{}' ===\n\n",
        module.id, module.task
    );
    report += &format!(" Raw data: {data}\n");
    report += " Evaluation:\n";

    let mut overall = true;
    let empty = Vec::new();
    let questions = serde_json::from_str::<Value>(&module.data.clone().unwrap_or_default())
        .ok()
        .and_then(|v| v.get("quiz").cloned())
        .and_then(|q| q.as_array().cloned())
        .unwrap_or(empty);

    for (i, question) in questions.iter().enumerate() {
        let answers_user: Vec<i64> = data
            .get(i)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_i64().or_else(|| x.as_str().and_then(|s| s.parse().ok())))
                    .collect()
            })
            .unwrap_or_default();
        let correct: Vec<i64> = question
            .get("correct")
            .and_then(|c| c.as_array())
            .map(|arr| arr.iter().filter_map(|x| x.as_i64()).collect())
            .unwrap_or_default();
        let is_correct = answers_user == correct;
        report += &format!(
            "  [{}] Question {i} -- user answers: {answers_user:?}, correct answers: {correct:?}\n",
            if is_correct { 'y' } else { 'n' }
        );
        overall &= is_correct;
    }

    report += &format!("\n Overall result: [{}]", if overall { 'y' } else { 'n' });
    (overall, report)
}

/// Evaluate a sortable submission (mirrors `util/sortable.py::evaluate`).
fn evaluate_sortable(module: &ModuleFull, data: &Value) -> (bool, String) {
    let mut report = format!(
        "=== Evaluating sortable id '{}' for task id '{}' ===\n\n",
        module.id, module.task
    );
    report += &format!(" Raw data: {data}\n");
    report += " Evaluation:\n";

    let data_json: Value =
        serde_json::from_str(&module.data.clone().unwrap_or_default()).unwrap_or(Value::Null);
    let correct = data_json
        .get("sortable")
        .and_then(|s| s.get("correct"))
        .cloned()
        .unwrap_or(Value::Null);
    let result = correct
        .as_array()
        .map(|arr| arr.iter().any(|c| c == data))
        .unwrap_or(false);

    report += &format!("  User order: {data}\n");
    report += &format!("  Correct order: {correct}\n");
    report += &format!("\n Overall result: [{}]", if result { 'y' } else { 'n' });
    (result, report)
}

/// Evaluate a text submission (mirrors `util/text.py::evaluate`).
/// Returns `(result, report)` where result is `ok`/`nok`/`error`;
/// `message`/`score` are extracted when present.
async fn evaluate_text(
    module: &ModuleFull,
    data: &Value,
    user_id: i64,
) -> Result<(String, Option<String>, Option<f64>, String), SandboxError> {
    let mut report = format!(
        "=== Evaluating text id '{}' for task id '{}' ===\n\n",
        module.id, module.task
    );
    report += &format!("Raw data: {data}\n");
    report += "Evaluation:\n";

    let data_json: Value =
        serde_json::from_str(&module.data.clone().unwrap_or_default()).unwrap_or(Value::Null);
    let text = match data_json.get("text") {
        Some(t) => t,
        None => {
            report += "No eval method specified!\n";
            return Ok((
                "error".into(),
                Some("Není dostupná žádná metoda opravení!".into()),
                None,
                report,
            ));
        }
    };

    let answers: Vec<String> = data
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|v| v.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();

    if let Some(diff) = text.get("diff").and_then(|d| d.as_array()) {
        report += "Diff used!\n";
        let ignore_case = text.get("ignore_case").and_then(|v| v.as_bool()).unwrap_or(false);
        let mut result = answers.len() == diff.len();
        for (expected, given) in diff.iter().zip(answers.iter()) {
            let mut s1 = expected.as_str().unwrap_or("").trim().to_string();
            let mut s2 = given.trim().to_string();
            if ignore_case {
                s1 = s1.to_lowercase();
                s2 = s2.to_lowercase();
            }
            result &= s1 == s2;
        }
        return Ok((
            if result { "ok" } else { "nok" }.into(),
            None,
            None,
            report,
        ));
    }

    if let Some(eval_script) = text.get("eval_script").and_then(|s| s.as_str()) {
        return eval_text_by_script(eval_script, &answers, user_id, &mut report).await;
    }

    report += "No eval method specified!\n";
    Ok((
        "error".into(),
        Some("Není dostupná žádná metoda opravení!".into()),
        None,
        report,
    ))
}

/// Run a text module's eval script (mirrors `util/text.py::eval_text`).
async fn eval_text_by_script(
    eval_script: &str,
    answers: &[String],
    user_id: i64,
    report: &mut String,
) -> Result<(String, Option<String>, Option<f64>, String), SandboxError> {
    let dir = std::env::temp_dir().join(format!("ksi-text-{}", uuid::Uuid::new_v4().simple()));
    tokio::fs::create_dir_all(&dir).await?;
    let stdout_path = dir.join("stdout");
    let stderr_path = dir.join("stderr");

    let abs = std::path::absolute(eval_script).unwrap_or_else(|_| eval_script.into());
    let stdout_file = std::fs::File::create(&stdout_path)?;
    let stderr_file = std::fs::File::create(&stderr_path)?;
    let status = tokio::process::Command::new(abs)
        .args(answers)
        .stdout(stdout_file)
        .stderr(stderr_file)
        .current_dir(&dir)
        .env("KSI_USER", user_id.to_string())
        .status()
        .await;

    let result = match status {
        Ok(s) if s.success() => "ok",
        _ => "nok",
    }
    .to_string();

    *report += "Stdout:\n";
    if let Ok(out) = tokio::fs::read_to_string(&stdout_path).await {
        *report += &out;
    }

    let stderr_len = tokio::fs::metadata(&stderr_path).await.map(|m| m.len()).unwrap_or(0);
    if stderr_len > 0 {
        *report += "Eval script returned nonempty stderr:\n";
        if let Ok(err) = tokio::fs::read_to_string(&stderr_path).await {
            *report += &err;
        }
        let _ = tokio::fs::remove_dir_all(&dir).await;
        return Err(SandboxError::CheckError(
            "Eval script returned non-empty stderr!".into(),
        ));
    }

    let mut message = None;
    let mut score = None;
    let result_path = dir.join("eval.out");
    if result_path.is_file() {
        if let Ok(raw) = tokio::fs::read_to_string(&result_path).await {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                message = v.get("message").and_then(|m| m.as_str()).map(str::to_string);
                if result == "ok" {
                    score = v
                        .get("score")
                        .and_then(|s| s.as_f64())
                        .map(|s| (s * 10.0).round() / 10.0);
                }
            }
        }
    }

    let _ = tokio::fs::remove_dir_all(&dir).await;
    Ok((result, message, score, std::mem::take(report)))
}

/// `POST /modules/{id}/submit` — submit a solution.
/// Returns a `ModuleSubmitResponse`-shaped JSON (`result`/`report`/`error`/…).
pub async fn submit_module(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<SubmitBody>,
) -> ApiResult<Json<Value>> {
    if !user.is_logged_in() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }
    let user_id = user.id.unwrap();

    let module = module_by_id(&state.db, id).await?;
    let mut module = match module {
        None => return Err(ApiError::NotFound("Neexistující modul".into())),
        Some(m) => m,
    };

    // Per-user generated assignment overrides the module data.
    let (custom_data, _) = load_custom_data(&state.db, &module, &user).await;
    module.data = Some(custom_data);

    // No submissions after the deadline.
    let now = Utc::now().naive_utc();
    if let Some(deadline) = task_deadline(&state.db, module.task).await? {
        if deadline < now {
            return Ok(Json(json!({
                "result": "error",
                "error": "Nelze odevzdat po termínu odevzdání úlohy",
            })));
        }
    }

    if module.module_type == "general" {
        return Ok(Json(json!({
            "result": "error",
            "error": "Pro nahrání souborů použijte /modules/{id}/submitFiles",
        })));
    }

    // Submission rate limit (orgs are exempt).
    if !user.is_org() {
        let day_ago = now - chrono::Duration::days(1);
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM evaluations
             WHERE user = ? AND module = ? AND time >= ?",
        )
        .bind(user_id)
        .bind(id)
        .bind(day_ago)
        .fetch_one(&state.db)
        .await?;
        let ratelimit = module.submit_ratelimit.unwrap_or(DEFAULT_SUBMIT_RATELIMIT);
        if count >= ratelimit {
            return Ok(Json(json!({
                "result": "error",
                "error": format!(
                    "Překročen limit odevzdání ({ratelimit} odevzdání / 24 hodin)."
                ),
            })));
        }
    }

    let data = &body.content;
    let max_points = module.max_points.unwrap_or(0.0);

    // Evaluate according to the module type.
    let (result, message, score, report) = match module.module_type.as_str() {
        "programming" => {
            return submit_programming(&state.db, &module, &user, data).await;
        }
        "text" => {
            let mut reporter = String::new();
            match evaluate_text(&module, data, user_id).await {
                Ok((r, m, s, rep)) => (r, m, s, rep),
                Err(_) => {
                    reporter += "Při opravování nastala výjimka, kontaktuj organizátora!";
                    ("error".into(), None, None, reporter)
                }
            }
        }
        "quiz" => {
            let (ok, rep) = evaluate_quiz(&module, data);
            (if ok { "ok" } else { "nok" }.into(), None, None, rep)
        }
        "sortable" => {
            let (ok, rep) = evaluate_sortable(&module, data);
            (if ok { "ok" } else { "nok" }.into(), None, None, rep)
        }
        other => {
            return Err(ApiError::Internal(format!(
                "Nepodporovaný typ modulu: {other}"
            )));
        }
    };

    let score = score.unwrap_or_else(|| if result == "ok" { max_points } else { 0.0 });
    let ok = result == "ok";

    let eval_id: i64 = sqlx::query_scalar(
        "INSERT INTO evaluations (user, module, points, ok, full_report, time)
         VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(user_id)
    .bind(module.id)
    .bind(score)
    .bind(ok)
    .bind(&report)
    .bind(now)
    .fetch_one(&state.db)
    .await?;
    let _ = eval_id;

    for line in report.lines() {
        if line.starts_with("action ") {
            perform_action(&state.db, module.task, user_id, line.trim()).await;
        }
    }

    let mut out = json!({ "result": result, "report": report });
    if let Some(m) = message {
        out["message"] = json!(m);
    }
    // The report is only visible to orgs (mirrors the Python stripping).
    if !user.is_org() {
        out.as_object_mut().and_then(|o| o.remove("report"));
    }
    Ok(Json(out))
}

/// Programming submission: store the code, optionally auto-evaluate it in the
/// sandbox, and record the evaluation (mirrors `_evaluate_code`).
async fn submit_programming(
    db: &sqlx::SqlitePool,
    module: &ModuleFull,
    user: &AuthUser,
    data: &Value,
) -> ApiResult<Json<Value>> {
    let user_id = user.id.unwrap();
    let code = data.as_str().unwrap_or("").to_string();
    let now = Utc::now().naive_utc();

    // Without autocorrect all submissions share a single evaluation row.
    let existing = existing_evaluations(db, module.id, user_id).await?;
    let evaluation_id: i64 = if !module.autocorrect && !existing.is_empty() {
        let eid = existing[0];
        sqlx::query("UPDATE evaluations SET time = ? WHERE id = ?")
            .bind(now)
            .bind(eid)
            .execute(db)
            .await?;
        eid
    } else {
        sqlx::query_scalar(
            "INSERT INTO evaluations (user, module, ok, full_report, time)
             VALUES (?, ?, 0, '', ?) RETURNING id",
        )
        .bind(user_id)
        .bind(module.id)
        .bind(now)
        .fetch_one(db)
        .await?
    };

    sqlx::query("INSERT INTO submitted_codes (evaluation, code) VALUES (?, ?)")
        .bind(evaluation_id)
        .bind(&code)
        .execute(db)
        .await?;

    if !module.autocorrect {
        return Ok(Json(json!({ "result": "ok" })));
    }

    let mut reporter = Reporter::new(50 * 1000);
    let spec = match ProgramSpec::from_data(
        &serde_json::from_str(&module.data.clone().unwrap_or_default()).unwrap_or(Value::Null),
    ) {
        Ok(s) => s,
        Err(_) => {
            reporter.push("Unsupported programming version\n");
            let result = json!({
                "result": "error",
                "message": "Opravení této úlohy není webovým systémem podporováno.",
            });
            return finish_programming_eval(db, module, user, evaluation_id, result, &reporter, 0.0, false).await;
        }
    };

    let sandbox = IsolateSandbox::new();
    let ctx = RunContext {
        module_id: module.id,
        user_id,
        run_id: evaluation_id,
        run_type: RunType::Eval,
        module_lib_path: std::path::PathBuf::from("data/module_lib"),
        store_base: std::path::PathBuf::from("data/exec"),
    };

    let outcome = match sandbox_evaluate(&sandbox, &spec, &ctx, &code, &mut reporter).await {
        Ok(o) => o,
        Err(SandboxError::NotAvailable(msg)) => {
            // isolate is not installed: 503, not a crash.
            return Err(ApiError::ServiceUnavailable(format!(
                "Spouštění kódu není na tomto serveru dostupné ({msg})."
            )));
        }
        Err(SandboxError::NoFreeBox) => {
            let result = json!({
                "result": "error",
                "message": "Přesáhnut maximální počet souběžně běžících opravení, zkuste to za chvíli.",
            });
            return finish_programming_eval(db, module, user, evaluation_id, result, &reporter, 0.0, false).await;
        }
        Err(e) => {
            reporter.push("Zachycena chyba:\n");
            reporter.push(&format!("{e:?}\n"));
            let result = json!({
                "result": "error",
                "message": "Nastala chyba při vykonávání kódu, zkus to prosím znovu později \
                    a v případě přetrvávající chyby kontaktuj organizátora",
            });
            return finish_programming_eval(db, module, user, evaluation_id, result, &reporter, 0.0, false).await;
        }
    };

    let max_points = module.max_points.unwrap_or(0.0);
    let ok = outcome.result == "ok";
    let score = match outcome.score {
        Some(s) if s <= max_points && s >= 0.0 => s,
        _ => {
            if ok {
                max_points
            } else {
                0.0
            }
        }
    };

    let mut result = json!({ "result": outcome.result, "stdout": outcome.stdout });
    if let Some(m) = outcome.message {
        result["message"] = json!(m);
    }
    if outcome.result != "error" {
        result["score"] = json!(score);
    }

    for action in &outcome.actions {
        perform_action(db, module.task, user_id, action).await;
    }

    finish_programming_eval(db, module, user, evaluation_id, result, &reporter, score, ok).await
}

/// Persist the evaluation row after auto-correction and shape the response.
async fn finish_programming_eval(
    db: &sqlx::SqlitePool,
    module: &ModuleFull,
    user: &AuthUser,
    evaluation_id: i64,
    mut result: Value,
    reporter: &Reporter,
    score: f64,
    ok: bool,
) -> ApiResult<Json<Value>> {
    let now = Utc::now().naive_utc();
    let mut full_report: String = sqlx::query_scalar("SELECT full_report FROM evaluations WHERE id = ?")
        .bind(evaluation_id)
        .fetch_one(db)
        .await?;
    full_report += &format!("{now} : {}\n", reporter.report_truncated());

    sqlx::query("UPDATE evaluations SET points = ?, ok = ?, full_report = ? WHERE id = ?")
        .bind(score)
        .bind(ok)
        .bind(&full_report)
        .bind(evaluation_id)
        .execute(db)
        .await?;

    if user.is_org() {
        result["report"] = json!(reporter.report_truncated());
    }
    let _ = module;
    Ok(Json(result))
}

// ---------------------------------------------------------------------------
// POST /modules/{id}/submitFiles
// ---------------------------------------------------------------------------

/// Guess a file's MIME type: content sniffing first (like Python's `magic`),
/// then the filename extension, defaulting to `application/octet-stream`.
fn guess_mime(filename: &str, bytes: &[u8]) -> String {
    if let Some(kind) = infer::get(bytes) {
        return kind.mime_type().to_string();
    }
    mime_guess::from_path(filename)
        .first_raw()
        .unwrap_or("application/octet-stream")
        .to_string()
}

/// `POST /modules/{id}/submitFiles` — upload files for a general module
/// (mirrors `ModuleSubmit._upload_files`).
pub async fn submit_files(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(id): Path<i64>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> ApiResult<Json<Value>> {
    if !user.is_logged_in() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }
    let user_id = user.id.unwrap();

    let module = module_by_id(&state.db, id).await?;
    let module = match module {
        None => return Err(ApiError::NotFound("Neexistující modul".into())),
        Some(m) => m,
    };
    if module.module_type != "general" {
        return Ok(Json(json!({
            "result": "error",
            "error": "Soubory lze nahrávat pouze k modulům typu general.",
        })));
    }

    // No uploads after the deadline.
    let now = Utc::now().naive_utc();
    if let Some(deadline) = task_deadline(&state.db, module.task).await? {
        if deadline < now {
            return Ok(Json(json!({
                "result": "error",
                "error": "Nelze odevzdat po termínu odevzdání úlohy",
            })));
        }
    }

    // Requests without a declared length are rejected (Python: 411),
    // oversized batches are rejected up-front (Python: 413).
    let content_length: Option<u64> = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());
    let content_length = match content_length {
        Some(n) => n,
        None => {
            return Ok(Json(json!({
                "result": "error",
                "error": "Nelze nahrát neukončený stream.",
            })));
        }
    };
    if content_length > MAX_UPLOAD_FILE_SIZE {
        return Err(ApiError::PayloadTooLarge(
            "Maximální velikost dávky je 20 MB.".into(),
        ));
    }

    // Reuse the existing evaluation when present, else create a new one.
    let existing = existing_evaluations(&state.db, module.id, user_id).await?;
    let (evaluation_id, mut report) = if let Some(&eid) = existing.first() {
        let full_report: String =
            sqlx::query_scalar("SELECT full_report FROM evaluations WHERE id = ?")
                .bind(eid)
                .fetch_one(&state.db)
                .await?;
        sqlx::query("UPDATE evaluations SET time = ? WHERE id = ?")
            .bind(now)
            .bind(eid)
            .execute(&state.db)
            .await?;
        (eid, full_report)
    } else {
        report_new_evaluation(&state, module.id, module.task, user_id).await?
    };

    // Only a limited number of files may be uploaded per solution.
    let file_cnt: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM submitted_files WHERE evaluation = ?",
    )
    .bind(evaluation_id)
    .fetch_one(&state.db)
    .await?;
    if existing.is_empty() && file_cnt > MAX_UPLOAD_FILE_COUNT {
        return Err(ApiError::BadRequest(
            "K řešení lze nahrát nejvýše 20 souborů.".into(),
        ));
    }

    let dir = std::path::PathBuf::from("data")
        .join("submissions")
        .join(format!("module_{}", module.id))
        .join(format!("user_{user_id}"));
    tokio::fs::create_dir_all(&dir).await.map_err(|_| {
        ApiError::BadRequest("Chyba 42, kontaktuj orga.".into())
    })?;
    if !dir.is_dir() {
        return Err(ApiError::BadRequest("Chyba 42, kontaktuj orga.".into()));
    }

    let mut total: u64 = 0;
    while let Some(field) = multipart.next_field().await.map_err(|e| {
        ApiError::BadRequest(format!("Neplatná multipart data: {e}"))
    })? {
        let filename = field
            .file_name()
            .map(str::to_string)
            .unwrap_or_else(|| "file".to_string());
        // Keep the filename inside the submission dir (no path traversal).
        let filename = std::path::Path::new(&filename)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        let bytes = field.bytes().await.map_err(|e| {
            ApiError::BadRequest(format!("Chyba při čtení souboru: {e}"))
        })?;
        total += bytes.len() as u64;
        if total > MAX_UPLOAD_FILE_SIZE {
            return Err(ApiError::PayloadTooLarge(
                "Maximální velikost dávky je 20 MB.".into(),
            ));
        }

        let path = dir.join(&filename);
        tokio::fs::write(&path, &bytes).await.map_err(|_| {
            ApiError::Internal("Soubor se nepodařilo uložit.".into())
        })?;
        let mime = guess_mime(&filename, &bytes);
        let path_str = path.to_string_lossy().into_owned();

        report += &format!(
            "{now} :  [y] uploaded file: '{filename}' (mime: {mime}) to file {path_str}\n"
        );

        // Don't duplicate the DB row when the file is already recorded.
        let in_db: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM submitted_files WHERE evaluation = ? AND path = ?",
        )
        .bind(evaluation_id)
        .bind(&path_str)
        .fetch_optional(&state.db)
        .await?;
        if in_db.is_none() {
            sqlx::query(
                "INSERT INTO submitted_files (evaluation, mime, path) VALUES (?, ?, ?)",
            )
            .bind(evaluation_id)
            .bind(&mime)
            .bind(&path_str)
            .execute(&state.db)
            .await?;
        }
    }

    sqlx::query("UPDATE evaluations SET full_report = ? WHERE id = ?")
        .bind(&report)
        .bind(evaluation_id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({ "result": "ok" })))
}

/// Create the evaluation row for a fresh file upload (mirrors the Python
/// report header in `_upload_files`).
async fn report_new_evaluation(
    state: &SharedState,
    module_id: i64,
    task_id: i64,
    user_id: i64,
) -> ApiResult<(i64, String)> {
    let now = Utc::now().naive_utc();
    let report = format!(
        "{now} : === Uploading files for module id '{module_id}' for task id '{task_id}' ===\n"
    );
    let evaluation_id: i64 = sqlx::query_scalar(
        "INSERT INTO evaluations (user, module, ok, full_report, time)
         VALUES (?, ?, 1, ?, ?) RETURNING id",
    )
    .bind(user_id)
    .bind(module_id)
    .bind(&report)
    .bind(now)
    .fetch_one(&state.db)
    .await?;
    Ok((evaluation_id, report))
}

// ---------------------------------------------------------------------------
// POST /runCode/{moduleId}/submit
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RunCodeBody {
    pub content: Value,
}

/// `POST /runCode/{moduleId}/submit` — run code in the sandbox without
/// submitting it (mirrors `endpoint/runcode.py::RunCode`).
pub async fn run_code(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(module_id): Path<i64>,
    Json(body): Json<RunCodeBody>,
) -> ApiResult<Json<Value>> {
    if !user.is_logged_in() {
        return Err(ApiError::Unauthorized("Nejste přihlášeni.".into()));
    }
    let user_id = user.id.unwrap();
    let code = body.content.as_str().unwrap_or("").to_string();

    let module = module_by_id(&state.db, module_id).await?;
    let mut module = match module {
        None => return Err(ApiError::NotFound("Modul neexistuje.".into())),
        Some(m) => m,
    };
    let (custom_data, _) = load_custom_data(&state.db, &module, &user).await;
    module.data = Some(custom_data);

    if is_task_locked(&state.db, module.task, &user).await? {
        return Err(ApiError::Forbidden("Úloha je uzamčena.".into()));
    }

    let now = Utc::now().naive_utc();
    let execution_id: i64 = sqlx::query_scalar(
        "INSERT INTO code_executions (module, user, code, result, time, report)
         VALUES (?, ?, ?, 'error', ?, '') RETURNING id",
    )
    .bind(module.id)
    .bind(user_id)
    .bind(&code)
    .bind(now)
    .fetch_one(&state.db)
    .await?;

    let mut reporter = Reporter::new(50 * 1000);
    let outcome = run_code_inner(&module, user_id, execution_id, &code, &mut reporter).await;

    let (result, exec_result) = match outcome {
        Ok(o) => ("ok", o),
        Err(SandboxError::NotAvailable(msg)) => {
            // isolate missing → 503, recorded on the execution row.
            let detail = format!("Spouštění kódu není na tomto serveru dostupné ({msg}).");
            sqlx::query("UPDATE code_executions SET result = 'error', report = ? WHERE id = ?")
                .bind(&detail)
                .bind(execution_id)
                .execute(&state.db)
                .await?;
            return Err(ApiError::ServiceUnavailable(detail));
        }
        Err(e) => {
            reporter.push(&format!("{e}\n"));
            reporter.push(&format!("{e:?}\n"));
            (
                "error",
                ksi_sandbox::ExecOutcome {
                    stdout: String::new(),
                    cheat: false,
                },
            )
        }
    };

    let mut response = if result == "ok" {
        json!({ "result": "ok", "stdout": exec_result.stdout })
    } else {
        json!({
            "result": "error",
            "stdout": exec_result.stdout,
            "message": "Kód se nepodařilo spustit, kontaktujte organizátora.",
        })
    };
    if user.is_org() {
        response["report"] = json!(reporter.report_truncated());
    }

    sqlx::query("UPDATE code_executions SET result = ?, report = ? WHERE id = ?")
        .bind(result)
        .bind(reporter.report_truncated())
        .execute(&state.db)
        .await?;

    Ok(Json(response))
}

/// The sandbox run itself, shared by the route above.
async fn run_code_inner(
    module: &ModuleFull,
    user_id: i64,
    execution_id: i64,
    code: &str,
    reporter: &mut Reporter,
) -> Result<ksi_sandbox::ExecOutcome, SandboxError> {
    let spec = ProgramSpec::from_data(
        &serde_json::from_str(&module.data.clone().unwrap_or_default()).unwrap_or(Value::Null),
    )
    .map_err(|_| {
        reporter.push("Unsupported programming version\n");
        SandboxError::UnsupportedVersion("unsupported".into())
    })?;

    let sandbox = IsolateSandbox::new();
    let ctx = RunContext {
        module_id: module.id,
        user_id,
        run_id: execution_id,
        run_type: RunType::Exec,
        module_lib_path: std::path::PathBuf::from("data/module_lib"),
        store_base: std::path::PathBuf::from("data/exec"),
    };
    sandbox_run_code(&sandbox, &spec, &ctx, code, reporter).await
}
