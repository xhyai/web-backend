use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use ksi_auth::AuthUser;
use crate::{
    error::{ApiError, ApiResult},
    mail::{LogMailSender, MailMessage, MailSender},
    SharedState,
};
use ksi_models::{
    parse_feedback, FeedbackJson, FeedbackRow, IncomingCategory, StoredCategory,
};

async fn config_value(db: &SqlitePool, key: &str, default: &str) -> String {
    sqlx::query_scalar::<_, Option<String>>("SELECT value FROM config WHERE \"key\" = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .flatten()
        .unwrap_or_else(|| default.to_string())
}

async fn fetch_feedback(
    db: &SqlitePool,
    user_id: i64,
    task_id: i64,
) -> Result<Option<FeedbackRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT user, task, content, \"lastUpdated\" FROM feedbacks WHERE user = ? AND task = ?",
    )
    .bind(user_id)
    .bind(task_id)
    .fetch_optional(db)
    .await
}

async fn task_exists(db: &SqlitePool, task_id: i64) -> Result<bool, sqlx::Error> {
    let id: Option<i64> = sqlx::query_scalar("SELECT id FROM tasks WHERE id = ?")
        .bind(task_id)
        .fetch_optional(db)
        .await?;
    Ok(id.is_some())
}

// ---------------------------------------------------------------------------
// GET /feedbacks/{id} — task feedback of the current user (id = task id)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct TaskFeedbackResponse {
    pub feedback: FeedbackJson,
}

pub async fn get_task_feedback(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(task_id): Path<i64>,
) -> ApiResult<Json<TaskFeedbackResponse>> {
    if !user.is_logged_in() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }
    let user_id = user.id.unwrap();

    match fetch_feedback(&state.db, user_id, task_id).await? {
        Some(f) => Ok(Json(TaskFeedbackResponse {
            feedback: FeedbackJson::from_row(&f),
        })),
        None => {
            if !task_exists(&state.db, task_id).await? {
                return Err(ApiError::NotFound("Úloha s tímto ID neexistuje.".into()));
            }
            Ok(Json(TaskFeedbackResponse {
                feedback: FeedbackJson::empty(task_id, user_id),
            }))
        }
    }
}

// ---------------------------------------------------------------------------
// PUT /feedbacks/{id} — create or update the current user's task feedback
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct UpdateFeedbackBody {
    pub feedback: UpdateFeedback,
}

#[derive(Debug, Deserialize)]
pub struct UpdateFeedback {
    pub categories: Vec<IncomingCategory>,
}

pub async fn update_task_feedback(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(task_id): Path<i64>,
    Json(body): Json<UpdateFeedbackBody>,
) -> ApiResult<Json<TaskFeedbackResponse>> {
    if !user.is_logged_in() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }
    let user_id = user.id.unwrap();

    let stored = parse_feedback(body.feedback.categories).map_err(|e| ApiError::BadRequest(e.message()))?;
    let content = serde_json::to_string_pretty(&stored)
        .map_err(|e| ApiError::Internal(format!("serializace feedbacku selhala: {e}")))?;
    let now = Utc::now().naive_utc();

    let exists = fetch_feedback(&state.db, user_id, task_id).await?.is_some();
    if exists {
        sqlx::query(
            "UPDATE feedbacks SET content = ?, \"lastUpdated\" = ? WHERE user = ? AND task = ?",
        )
        .bind(&content)
        .bind(now)
        .bind(user_id)
        .bind(task_id)
        .execute(&state.db)
        .await?;
    } else {
        sqlx::query(
            "INSERT INTO feedbacks (user, task, content, \"lastUpdated\")
             VALUES (?, ?, ?, ?)",
        )
        .bind(user_id)
        .bind(task_id)
        .bind(&content)
        .bind(now)
        .execute(&state.db)
        .await?;
    }

    let row = fetch_feedback(&state.db, user_id, task_id)
        .await?
        .expect("just-written feedback");
    Ok(Json(TaskFeedbackResponse {
        feedback: FeedbackJson::from_row(&row),
    }))
}

// ---------------------------------------------------------------------------
// DELETE /feedbacks/{id}
// ---------------------------------------------------------------------------

pub async fn delete_task_feedback(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(task_id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    if !user.is_logged_in() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }
    let user_id = user.id.unwrap();

    if fetch_feedback(&state.db, user_id, task_id).await?.is_none() {
        return Err(ApiError::NotFound("Feedback s tímto ID neexistuje.".into()));
    }
    sqlx::query("DELETE FROM feedbacks WHERE user = ? AND task = ?")
        .bind(user_id)
        .bind(task_id)
        .execute(&state.db)
        .await?;
    Ok(Json(serde_json::json!({})))
}

// ---------------------------------------------------------------------------
// POST /feedbacks — create a new task feedback (and notify orgs by e-mail)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateFeedbackBody {
    pub feedback: CreateFeedback,
}

#[derive(Debug, Deserialize)]
pub struct CreateFeedback {
    #[serde(rename = "taskId")]
    pub task_id: i64,
    pub categories: Vec<IncomingCategory>,
}

pub async fn create_task_feedback(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(body): Json<CreateFeedbackBody>,
) -> ApiResult<Json<TaskFeedbackResponse>> {
    if !user.is_logged_in() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }
    let user_id = user.id.unwrap();
    let task_id = body.feedback.task_id;

    if !task_exists(&state.db, task_id).await? {
        return Err(ApiError::NotFound("Úloha s tímto ID neexistuje.".into()));
    }

    let stored =
        parse_feedback(body.feedback.categories.clone()).map_err(|e| ApiError::BadRequest(e.message()))?;
    let content = serde_json::to_string_pretty(&stored)
        .map_err(|e| ApiError::Internal(format!("serializace feedbacku selhala: {e}")))?;
    let now = Utc::now().naive_utc();

    sqlx::query(
        "INSERT INTO feedbacks (user, task, content, \"lastUpdated\")
         VALUES (?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(task_id)
    .bind(&content)
    .bind(now)
    .execute(&state.db)
    .await?;

    // Best-effort org notification; Python swallows every exception here.
    let _ = notify_orgs_about_feedback(&state.db, task_id, user_id, &stored).await;

    let row = fetch_feedback(&state.db, user_id, task_id)
        .await?
        .expect("just-inserted feedback");
    Ok(Json(TaskFeedbackResponse {
        feedback: FeedbackJson::from_row(&row),
    }))
}

/// Render one category answer like `FeedbackTask._get_answer_string`:
/// stars/line become ★/☆, other answers are whitespace-collapsed text.
/// Returns the rendered answer and whether it counts as a "text answer".
fn answer_string(category: &StoredCategory) -> (String, bool) {
    match category.ftype.as_str() {
        "stars" | "line" => {
            let n = category
                .answer
                .as_i64()
                .or_else(|| category.answer.as_u64().map(|v| v as i64))
                .unwrap_or(0)
                .clamp(0, 5) as usize;
            ("★".repeat(n) + &"☆".repeat(5 - n), false)
        }
        _ => {
            let raw = category.answer.as_str().unwrap_or("");
            let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
            // Python: `answer.count(' ') > 14` — at least 15 words.
            let has_text = collapsed.split(' ').count() > 15;
            (collapsed, has_text)
        }
    }
}

/// Mirror of `FeedbacksTask._send_feedback_email_to_orgs`: e-mail the task
/// author (CC wave garant and co-author) when the feedback contains a text
/// answer. Sending is best-effort (see [`crate::mail`]).
async fn notify_orgs_about_feedback(
    db: &SqlitePool,
    task_id: i64,
    user_id: i64,
    stored: &[StoredCategory],
) -> Result<(), String> {
    if !stored.iter().any(|c| answer_string(c).1) {
        return Ok(());
    }

    let task: Option<(String, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT title, author, co_author FROM tasks WHERE id = ?",
    )
    .bind(task_id)
    .fetch_optional(db)
    .await
    .map_err(|e| e.to_string())?;
    let (title, author, co_author) = task.ok_or_else(|| "task zmizel".to_string())?;

    let author_email: Option<String> = sqlx::query_scalar("SELECT email FROM users WHERE id = ?")
        .bind(author)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?
        .flatten();
    let author_email = match author_email {
        Some(e) => e,
        None => return Ok(()),
    };

    let garant_email: Option<String> = sqlx::query_scalar(
        "SELECT u.email FROM users u
         JOIN waves w ON w.garant = u.id
         JOIN tasks t ON t.wave = w.id
         WHERE t.id = ?",
    )
    .bind(task_id)
    .fetch_optional(db)
    .await
    .map_err(|e| e.to_string())?
    .flatten();

    let mut cc: Vec<String> = Vec::new();
    if let Some(g) = garant_email {
        if g != author_email {
            cc.push(g);
        }
    }
    if let Some(co) = co_author {
        let co_email: Option<String> = sqlx::query_scalar("SELECT email FROM users WHERE id = ?")
            .bind(co)
            .fetch_optional(db)
            .await
            .map_err(|e| e.to_string())?
            .flatten();
        if let Some(e) = co_email {
            cc.push(e);
        }
    }

    let user_name: Option<(String, String)> =
        sqlx::query_as("SELECT first_name, last_name FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(db)
            .await
            .map_err(|e| e.to_string())?;
    let (first_name, last_name) = user_name.unwrap_or_default();

    let feedback_text = stored
        .iter()
        .map(|c| {
            let (answer, _) = answer_string(c);
            format!("<p><i>\"{}\"</i>:</p><p>{}</p>", c.text, answer)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let web_url = config_value(db, "web_url", "https://ksi.fi.muni.cz/").await;
    let mail_sign = config_value(db, "mail_sign", "").await;
    let prefix = config_value(db, "mail_subject_prefix", "[KSI]").await;

    let sender = LogMailSender::default();
    let msg = MailMessage {
        from: "ksi".into(),
        to: vec![author_email],
        cc,
        bcc: vec![],
        subject: format!("{prefix} Nový feedback k úloze {title}"),
        body_html: format!(
            "<p>Ahoj,<br/>k tvé úloze <a href=\"{web_url}/ulohy/{task_id}\">{title}</a> \
             byl přidán nový feedback od <b>{first_name} {last_name}:</b></p>\
             {feedback_text}{mail_sign}"
        ),
        reply_to: None,
    };
    sender.send(&msg).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// POST /feedback — send a free-form feedback e-mail to the orgs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct FeedbackEmailBody {
    pub body: String,
    pub email: Option<String>,
}

#[derive(Serialize)]
pub struct FeedbackEmailResponse {
    pub result: &'static str,
}

/// Mirror of `endpoint/feedback_email.py::FeedbackEmail`: forwards free-form
/// feedback text to the addresses in `feedback_recipients`.
pub async fn send_feedback_email(
    State(state): State<SharedState>,
    _user: AuthUser,
    Json(body): Json<FeedbackEmailBody>,
) -> ApiResult<Response> {
    if body.body.is_empty() {
        // Python returns early without setting a result (empty 200).
        return Ok(StatusCode::OK.into_response());
    }

    let recipients: Vec<String> =
        sqlx::query_scalar("SELECT email FROM feedback_recipients ORDER BY email")
            .fetch_all(&state.db)
            .await?;

    let reply_to = match body.email {
        Some(e) if !e.is_empty() => Some(e),
        _ => {
            let conf = config_value(&state.db, "ksi_conf", "").await;
            if conf.is_empty() {
                None
            } else {
                Some(conf)
            }
        }
    };

    let easter_egg: Option<String> = sqlx::query_scalar(
        "SELECT body FROM mail_eastereggs ORDER BY RANDOM() LIMIT 1",
    )
    .fetch_optional(&state.db)
    .await?;
    let egg_html = easter_egg
        .map(|b| format!("<hr><p>PS: {b}</p>"))
        .unwrap_or_default();

    let prefix = config_value(&state.db, "mail_subject_prefix", "[KSI]").await;
    let sender = LogMailSender::default();
    let msg = MailMessage {
        from: "ksi".into(),
        to: recipients,
        cc: vec![],
        bcc: vec![],
        subject: format!("{prefix} Zpetna vazba"),
        body_html: format!("<p>{}</p>{egg_html}", body.body),
        reply_to,
    };
    // Python's mail queue swallows send errors; the log sender never fails.
    let _ = sender.send(&msg);

    Ok(Json(FeedbackEmailResponse { result: "ok" }).into_response())
}
