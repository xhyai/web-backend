//! `POST /registration` and `POST /forgottenPassword`.
//!
//! Faithful port of `endpoint/registration.py` (`Registration`) and
//! `endpoint/user.py` (`ForgottenPassword`) **except delivery**: real SMTP
//! is not wired up, so both e-mails are handed to
//! [`LogMailSender`](crate::mail::LogMailSender) (which logs them instead
//! of sending), exactly like `routes::email` does.
//!
//! Deviations from Python:
//! * Validation errors (malformed e-mail, unknown gender, empty password)
//!   are returned as `200 {"error": "..."}` like the duplicate-e-mail case
//!   (Python would 500 on the DB enum constraint instead).
//! * The three manual clean-up deletes on mid-registration failure are a
//!   single DB transaction — same net effect, no half-created users.
//! * The `source_ip` in the password-reset audit log is not recorded
//!   (the handler has no access to the peer address).
//! * `github`/`discord` from the registration payload are ignored, exactly
//!   like in Python (the endpoint never stores them).

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::{ApiError, ApiResult},
    mail::{LogMailSender, MailMessage, MailSender},
    SharedState,
};

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

/// `util/user_notify.py::new_token` — 40 random chars (sha256 hex digest
/// truncated to `TOKEN_LENGTH = 40` in Python; here two uuids, same length).
fn new_auth_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
        + &uuid::Uuid::new_v4().simple().to_string()[..8]
}

/// Random 16-char password with the same alphabet as Python:
/// `random.SystemRandom().choice(ascii_uppercase + digits + ascii_lowercase)`.
fn new_random_password() -> String {
    const ALPHABET: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyz";
    uuid::Uuid::new_v4()
        .as_bytes()
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

// Python `util/config.py` defaults, used because this port has no mail
// config plumbed through (same approach as `routes::email`).
const MAIL_SUBJECT_PREFIX: &str = "[KSI]";
const SEMINAR_NAME: &str = "Korespondenční seminář z informatiky";
const MAIL_REGISTRATION_WELCOME: &str =
    "Korespondenčním semináři z informatiky Fakulty informatiky Masarykovy univerzity.";
const KSI_WEB: &str = "https://ksi.fi.muni.cz/";

// ---------------------------------------------------------------------------
// POST /registration
// ---------------------------------------------------------------------------

/// Mirrors the swagger `RegistrationRequest` schema.
#[derive(Debug, Deserialize)]
pub struct RegistrationRequest {
    pub email: String,
    pub nick_name: Option<String>,
    pub first_name: String,
    pub last_name: String,
    pub gender: String,
    pub short_info: String,
    pub github: Option<String>,
    pub discord: Option<String>,
    pub addr_street: String,
    pub addr_city: String,
    pub addr_zip: String,
    pub addr_country: String,
    pub school_name: String,
    pub school_street: String,
    pub school_city: String,
    pub school_zip: String,
    pub school_country: String,
    /// swagger declares this `double`; Python truncates with `int(...)`.
    pub school_finish: f64,
    pub tshirt_size: String,
    pub notify_eval: Option<bool>,
    pub notify_response: Option<bool>,
    pub notify_ksi: Option<bool>,
    pub notify_events: Option<bool>,
    pub password: String,
    pub referral: Option<String>,
}

/// Mirrors the swagger `PossibleErrorDict` schema: `{}` on success,
/// `{"error": "..."}` on any user-facing failure.
#[derive(Debug, Serialize)]
pub struct RegistrationResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn reg_error(msg: &str) -> Json<RegistrationResult> {
    Json(RegistrationResult {
        error: Some(msg.into()),
    })
}

/// `POST /registration` — create a new user, their profile and notification
/// settings, then (stub-)send the welcome e-mail.
pub async fn register(
    State(state): State<SharedState>,
    Json(req): Json<RegistrationRequest>,
) -> ApiResult<Json<RegistrationResult>> {
    // ---- input validation (Python had none here and 500'd instead) ----
    if !req.email.contains('@') {
        return Ok(reg_error("Neplatný e-mail."));
    }
    if !matches!(req.gender.as_str(), "male" | "female" | "other") {
        return Ok(reg_error("Neplatné pohlaví."));
    }
    if req.password.is_empty() {
        return Ok(reg_error("Heslo nesmí být prázdné."));
    }
    if req.first_name.trim().is_empty() || req.last_name.trim().is_empty() {
        return Ok(reg_error("Jméno a příjmení nesmí být prázdné."));
    }

    let existing: Option<i64> = sqlx::query_scalar("SELECT id FROM users WHERE email = ?")
        .bind(&req.email)
        .fetch_optional(&state.db)
        .await?;
    if existing.is_some() {
        return Ok(reg_error("Na tento e-mail je již někdo zaregistrovaný."));
    }

    let password_hash = bcrypt::hash(&req.password, bcrypt::DEFAULT_COST)
        .map_err(|e| ApiError::Internal(format!("Chyba při hashování hesla: {e}")))?;

    // Python committed user → profile → notify separately and deleted the
    // earlier rows on failure; a transaction is the same thing, atomically.
    let mut tx = state.db.begin().await?;

    let user_id: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, first_name, last_name, nick_name, sex, password, short_info, role, enabled)
         VALUES (?, ?, ?, ?, ?, ?, ?, 'participant', 1)
         RETURNING id",
    )
    .bind(&req.email)
    .bind(&req.first_name)
    .bind(&req.last_name)
    .bind(req.nick_name.as_deref().unwrap_or(""))
    .bind(&req.gender)
    .bind(&password_hash)
    .bind(&req.short_info)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        tracing::error!("registration: cannot create user: {e}");
        ApiError::Internal("Nelze vytvořit uživatele, kontaktuj prosím orga.".into())
    })?;

    sqlx::query(
        "INSERT INTO profiles (user_id, addr_street, addr_city, addr_zip, addr_country,
                               school_name, school_street, school_city, school_zip, school_country,
                               school_finish, tshirt_size, referral)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(&req.addr_street)
    .bind(&req.addr_city)
    .bind(&req.addr_zip)
    .bind(req.addr_country.to_lowercase())
    .bind(&req.school_name)
    .bind(&req.school_street)
    .bind(&req.school_city)
    .bind(&req.school_zip)
    .bind(req.school_country.to_lowercase())
    .bind(req.school_finish as i64)
    .bind(req.tshirt_size.to_uppercase())
    .bind(req.referral.as_deref().unwrap_or("{}"))
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        tracing::error!("registration: cannot create profile: {e}");
        ApiError::Internal("Nelze vytvořit profil, kontaktuj prosím orga.".into())
    })?;

    sqlx::query(
        "INSERT INTO users_notify (user, auth_token, notify_eval, notify_response, notify_ksi, notify_events)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(new_auth_token())
    .bind(req.notify_eval.unwrap_or(true))
    .bind(req.notify_response.unwrap_or(true))
    .bind(req.notify_ksi.unwrap_or(true))
    .bind(req.notify_events.unwrap_or(true))
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        tracing::error!("registration: cannot create notify record: {e}");
        ApiError::Internal("Nelze vytvořit notifikační záznam, kontaktuj prosím orga.".into())
    })?;

    tx.commit().await?;

    // ---- welcome e-mail (logged, not sent — no SMTP wired up) ----
    let msg = MailMessage {
        from: "ksi".to_string(),
        to: vec![req.email.clone()],
        cc: vec![],
        bcc: vec![],
        subject: format!("{MAIL_SUBJECT_PREFIX} Potvrzení registrace {SEMINAR_NAME}"),
        body_html: format!(
            "Ahoj!<br/>{MAIL_REGISTRATION_WELCOME} Nyní můžeš začít řešit naplno. \
             Stačí se přihlásit na {KSI_WEB} pomocí e-mailu a zvoleného hesla. \
             Přejeme ti hodně úspěchů při řešení semináře!"
        ),
        reply_to: None,
    };
    // Python only logged mail failures; the LogMailSender never fails.
    if let Err(e) = LogMailSender::default().send(&msg) {
        tracing::error!("registration: welcome e-mail failed: {e}");
    }

    Ok(Json(RegistrationResult { error: None }))
}

// ---------------------------------------------------------------------------
// POST /forgottenPassword
// ---------------------------------------------------------------------------

/// Mirrors the swagger `ForgottenPasswordRequest` schema.
#[derive(Debug, Deserialize)]
pub struct ForgottenPasswordRequest {
    pub email: String,
}

/// Mirrors the swagger `ForgottenPasswordResult` schema.
#[derive(Debug, Serialize)]
pub struct ForgottenPasswordResult {
    pub result: String,
}

fn fp_response(status: StatusCode, result: &str) -> Response {
    (
        status,
        Json(ForgottenPasswordResult {
            result: result.into(),
        }),
    )
        .into_response()
}

/// `POST /forgottenPassword` — generate a new random password for the user
/// with the given e-mail and (stub-)send it to them.
pub async fn forgotten_password(
    State(state): State<SharedState>,
    Json(req): Json<ForgottenPasswordRequest>,
) -> ApiResult<Response> {
    let user: Option<(i64, String)> =
        sqlx::query_as("SELECT id, role FROM users WHERE email = ?")
            .bind(&req.email)
            .fetch_optional(&state.db)
            .await?;

    let (user_id, role) = match user {
        Some(u) => u,
        // Python: 400 + {"result": "error"}.
        None => return Ok(fp_response(StatusCode::BAD_REQUEST, "error")),
    };

    let new_password = new_random_password();
    let password_hash = bcrypt::hash(&new_password, bcrypt::DEFAULT_COST)
        .map_err(|e| ApiError::Internal(format!("Chyba při hashování hesla: {e}")))?;

    // Python `util/logger.py::audit_log(scope="AUTH", message="Password reset")`.
    sqlx::query(
        "INSERT INTO audit_logs (user_id, scope, line, line_meta) VALUES (?, 'AUTH', 'Password reset', ?)",
    )
    .bind(user_id)
    .bind(serde_json::json!({ "role": role }).to_string())
    .execute(&state.db)
    .await?;

    sqlx::query("UPDATE users SET password = ? WHERE id = ?")
        .bind(&password_hash)
        .bind(user_id)
        .execute(&state.db)
        .await?;

    // ---- new-password e-mail (logged, not sent — no SMTP wired up) ----
    // NOTE: the plaintext password ends up in the application log via the
    // LogMailSender stub. This mirrors the Python behaviour of e-mailing it
    // to the user; never enable this log line with a real SMTP transport
    // that also archives mail, and replace the stub before production use.
    let msg = MailMessage {
        from: "ksi".to_string(),
        to: vec![req.email.clone()],
        cc: vec![],
        bcc: vec![],
        subject: format!("{MAIL_SUBJECT_PREFIX} Nové heslo"),
        body_html: format!(
            "Ahoj,<br/>na základě tvé žádosti ti bylo vygenerováno nové \
             heslo: {new_password}<br/><br/>{SEMINAR_NAME}"
        ),
        reply_to: None,
    };
    if let Err(e) = LogMailSender::default().send(&msg) {
        tracing::error!("forgottenPassword: e-mail failed: {e}");
    }

    Ok(fp_response(StatusCode::OK, "ok"))
}
