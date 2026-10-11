//! `POST /admin/e-mail` — send a bulk e-mail to participants (admin only).
//!
//! Faithful port of `endpoint/admin/email.py` **except delivery**: real SMTP
//! is not wired up, so the recipients are resolved exactly like in Python,
//! the message is handed to [`LogMailSender`](crate::mail::LogMailSender)
//! (which logs it instead of sending), and `{"count": n}` is returned.
//!
//! Deviations from Python (all consequences of missing SMTP/config):
//! * `KarlikSign` — the mail sign-off text comes from server config which
//!   this port does not have, so it is *not* appended to the body.
//! * Per-recipient unsubscribe links and the org BCC copies are skipped; the
//!   `users_notify` opt-out filter (notify_ksi / notify_events) *is* applied.
//! * The audit log entry is emitted via `tracing::info!` instead of the
//!   `audit_logs` table.
//! * `successful_participant_percentage` uses the documented Python default
//!   (60), since the config value is not plumbed through in this port.

use std::collections::HashMap;

use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::json;

use ksi_auth::AuthUser;
use crate::{
    error::{ApiError, ApiResult},
    mail::{LogMailSender, MailMessage, MailSender},
    SharedState,
};

/// Default `successful_participant_percentage` (see `util/config.py`).
const SUCCESSFUL_PARTICIPANT_PERCENTAGE: f64 = 60.0;

/// Request envelope: `{"e-mail": {...}}`.
#[derive(Debug, Deserialize)]
pub struct EmailSendRequest {
    #[serde(rename = "e-mail")]
    email: EmailSend,
}

/// Field names mirror the Python spec (capitalised keys).
#[derive(Debug, Deserialize)]
struct EmailSend {
    #[serde(rename = "Subject")]
    subject: String,
    #[serde(rename = "Body")]
    body: String,
    #[serde(rename = "Reply-To", default)]
    reply_to: Option<String>,
    #[serde(rename = "To")]
    to: Vec<i64>,
    #[serde(rename = "Bcc", default)]
    bcc: Vec<String>,
    #[serde(rename = "Gender", default)]
    gender: Option<String>,
    #[serde(rename = "KarlikSign", default)]
    #[allow(dead_code)]
    karlik_sign: bool,
    #[serde(rename = "Easteregg", default)]
    easteregg: bool,
    #[serde(rename = "Successful", default)]
    successful: bool,
    #[serde(rename = "Category", default)]
    category: Option<String>,
    #[serde(rename = "Type", default)]
    mail_type: Option<String>,
}

#[derive(Debug, Clone)]
struct Recipient {
    id: i64,
    email: String,
    sex: String,
}

/// `POST /admin/e-mail`
pub async fn send_email(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(payload): Json<EmailSendRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if !user.is_logged_in() || !user.is_org() {
        return Err(ApiError::BadRequest("Nejste přihlášeni.".into()));
    }

    let data = payload.email;
    if data.to.is_empty() {
        return Err(ApiError::BadRequest("Prázdný seznam ročníků.".into()));
    }

    tracing::info!(
        scope = "MAIL",
        user_id = user.id,
        subject = %data.subject,
        "admin sent an e-mail (logged, not delivered)"
    );

    // ---- recipient selection (mirrors endpoint/admin/email.py) ----
    let mut recipients: HashMap<i64, Recipient> = if data.successful {
        let mut map = HashMap::new();
        for year_id in &data.to {
            for r in successful_participants(&state, *year_id).await? {
                map.insert(r.id, r);
            }
        }
        map
    } else {
        active_participants(&state, &data.to).await?
    };

    if let Some(gender) = &data.gender {
        if gender != "both" {
            recipients.retain(|_, r| &r.sex == gender);
        }
    }

    if let Some(category) = &data.category {
        if category == "hs" || category == "other" {
            let (min_year, max_year) = year_ends(&state, &data.to).await?;
            let finishes = school_finishes(&state).await?;
            recipients.retain(|id, _| match finishes.get(id) {
                Some(finish) => {
                    if category == "hs" {
                        *finish >= min_year
                    } else {
                        *finish < max_year
                    }
                }
                None => false,
            });
        }
    }

    // ---- filter unsubscribed (users_notify; rows missing => filtered out,
    // mirroring the Python dict membership test) ----
    let mail_type = data.mail_type.as_deref().unwrap_or("ksi");
    let notify_col = match mail_type {
        "events" => "notify_events",
        _ => "notify_ksi",
    };
    let ids: Vec<i64> = recipients.keys().copied().collect();
    let subscribed: Vec<i64> = if ids.is_empty() {
        vec![]
    } else {
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT user FROM users_notify WHERE user IN ({placeholders}) AND {notify_col} = 1"
        );
        let mut q = sqlx::query_scalar::<_, i64>(&sql);
        for id in &ids {
            q = q.bind(id);
        }
        q.fetch_all(&state.db).await?
    };
    recipients.retain(|id, _| subscribed.contains(id));

    // ---- body ----
    let mut body = data.body.clone();
    // KarlikSign: no sign-off text available without mail config — skipped
    // (documented above).
    if data.easteregg {
        if let Some(egg) = random_easteregg(&state).await? {
            body.push_str(&format!("<hr><p>PS: {egg}</p>"));
        }
    }

    // ---- "send" (logged, not delivered) ----
    let to: Vec<String> = recipients.values().map(|r| r.email.clone()).collect();
    let msg = MailMessage {
        from: "ksi".to_string(),
        to,
        cc: vec![],
        bcc: data.bcc.clone(),
        subject: data.subject,
        body_html: format!("<html>{body}</html>"),
        reply_to: data.reply_to.filter(|s| !s.is_empty()),
    };
    LogMailSender::default()
        .send(&msg)
        .map_err(|e| ApiError::Internal(format!("Chyba při odesílání e-mailu: {e}")))?;

    Ok(Json(json!({ "count": recipients.len() })))
}

/// Successful participants of a year (mirrors
/// `util/user.py::successful_participants`): participants whose summed
/// best-per-module points reach `SUCCESSFUL_PARTICIPANT_PERCENTAGE` % of
/// `max(year max points, point_pad)` and who have no cheat flag.
async fn successful_participants(
    state: &SharedState,
    year_id: i64,
) -> ApiResult<Vec<Recipient>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        email: String,
        sex: String,
        sum_points: Option<f64>,
        cheat: i64,
    }

    let rows: Vec<Row> = sqlx::query_as(
        "SELECT u.id, u.email, u.sex,
                CAST(SUM(pm.points) AS REAL) AS sum_points,
                MAX(pm.cheat) AS cheat
         FROM users u
         JOIN (
             SELECT e.user AS user_id,
                    MAX(e.points) AS points,
                    MAX(e.cheat) AS cheat
             FROM evaluations e
             JOIN modules m ON m.id = e.module
             JOIN tasks t ON t.id = m.task
             JOIN waves w ON w.id = t.wave
             WHERE w.year = ? AND t.evaluation_public = 1
             GROUP BY e.user, e.module
         ) pm ON pm.user_id = u.id
         WHERE u.role = 'participant'
         GROUP BY u.id",
    )
    .bind(year_id)
    .fetch_all(&state.db)
    .await?;

    let max_points: f64 = sqlx::query_scalar(
        "SELECT COALESCE(CAST(SUM(m.max_points) AS REAL), 0)
         FROM modules m
         JOIN tasks t ON t.id = m.task
         JOIN waves w ON w.id = t.wave
         WHERE w.year = ? AND m.bonus = 0",
    )
    .bind(year_id)
    .fetch_one(&state.db)
    .await?;

    let point_pad: f64 = sqlx::query_scalar(
        "SELECT COALESCE(CAST(point_pad AS REAL), 0) FROM years WHERE id = ?",
    )
    .bind(year_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0.0);

    let threshold = (SUCCESSFUL_PARTICIPANT_PERCENTAGE / 100.0) * max_points.max(point_pad);

    Ok(rows
        .into_iter()
        .filter(|r| r.sum_points.unwrap_or(0.0) >= threshold && r.cheat == 0)
        .map(|r| Recipient {
            id: r.id,
            email: r.email,
            sex: r.sex,
        })
        .collect())
}

/// Participants active (any evaluation) in the given years (mirrors
/// `util/user.py::active_years_all` + role/year filter).
async fn active_participants(
    state: &SharedState,
    year_ids: &[i64],
) -> ApiResult<HashMap<i64, Recipient>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        email: String,
        sex: String,
    }

    let placeholders = year_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT DISTINCT u.id, u.email, u.sex
         FROM users u
         JOIN evaluations e ON e.user = u.id
         JOIN modules m ON m.id = e.module
         JOIN tasks t ON t.id = m.task
         JOIN waves w ON w.id = t.wave
         WHERE w.year IN ({placeholders}) AND u.role = 'participant'"
    );
    let mut q = sqlx::query_as::<_, Row>(&sql);
    for id in year_ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(&state.db).await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.id,
                Recipient {
                    id: r.id,
                    email: r.email.clone(),
                    sex: r.sex,
                },
            )
        })
        .collect())
}

/// `year_end` for the earliest and latest requested year (mirrors
/// `util/year.py::year_end`: first part of "YYYY/YYYY" + 1).
async fn year_ends(state: &SharedState, year_ids: &[i64]) -> ApiResult<(i64, i64)> {
    let placeholders = year_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!("SELECT year FROM years WHERE id IN ({placeholders})");
    let mut q = sqlx::query_scalar::<_, Option<String>>(&sql);
    for id in year_ids {
        q = q.bind(id);
    }
    let mut ends: Vec<i64> = q
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .filter_map(|y| {
            y.and_then(|s| {
                s.replace(' ', "")
                    .split('/')
                    .next()
                    .and_then(|p| p.parse::<i64>().ok())
                    .map(|v| v + 1)
            })
        })
        .collect();
    ends.sort_unstable();
    match (ends.first(), ends.last()) {
        (Some(&min), Some(&max)) => Ok((min, max)),
        _ => Err(ApiError::BadRequest("Neznámý ročník.".into())),
    }
}

/// `{user_id: school_finish}` for all profiles (mirrors the `finish` dict).
async fn school_finishes(state: &SharedState) -> ApiResult<HashMap<i64, i64>> {
    let rows: Vec<(i64, i64)> =
        sqlx::query_as("SELECT user_id, school_finish FROM profiles")
            .fetch_all(&state.db)
            .await?;
    Ok(rows.into_iter().collect())
}

/// One random easter egg (mirrors `util/mail.py::easteregg`).
async fn random_easteregg(state: &SharedState) -> ApiResult<Option<String>> {
    let egg: Option<String> =
        sqlx::query_scalar("SELECT body FROM mail_eastereggs ORDER BY RANDOM() LIMIT 1")
            .fetch_optional(&state.db)
            .await?;
    Ok(egg)
}
