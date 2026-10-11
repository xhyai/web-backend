//! E-mail sending abstraction (mirrors `util/mail.py`).
//!
//! **IMPORTANT: real SMTP is NOT wired up.** The Python backend queues mail
//! in a background thread and delivers it through `smtplib`. In this port,
//! [`MailSender`] is a trait and the only shipped implementation is
//! [`LogMailSender`], which logs every e-mail via `tracing` instead of
//! sending it. To enable real delivery, implement [`MailSender`] with an
//! SMTP client (e.g. `lettre`) and inject it where [`LogMailSender`] is
//! used — no route code has to change.
//!
//! Anything that would require SMTP-adjacent configuration (per-recipient
//! unsubscribe links, the org BCC copies, the Karlík sign-off, the plaintext
//! `multipart/alternative` rendering) is skipped and documented in
//! [`routes::email`](crate::routes::email).

/// A single e-mail to be delivered.
#[derive(Debug, Clone)]
pub struct MailMessage {
    /// Envelope sender.
    pub from: String,
    /// Primary recipients.
    pub to: Vec<String>,
    /// Carbon-copy recipients.
    pub cc: Vec<String>,
    /// Blind-carbon-copy recipients.
    pub bcc: Vec<String>,
    pub subject: String,
    /// HTML body.
    pub body_html: String,
    /// Optional `Reply-To` header value.
    pub reply_to: Option<String>,
}

/// Error returned by a [`MailSender`].
#[derive(Debug)]
pub struct MailSendError(pub String);

impl std::fmt::Display for MailSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "odeslání e-mailu selhalo: {}", self.0)
    }
}

impl std::error::Error for MailSendError {}

/// Delivers a [`MailMessage`]. Implementations decide the transport.
pub trait MailSender: Send + Sync {
    fn send(&self, msg: &MailMessage) -> Result<(), MailSendError>;
}

/// Faithful stub: logs the e-mail instead of sending it.
///
/// Mirrors the Python behaviour of redirecting mail to a `.eml` file when no
/// sender is configured — here the message goes to the application log, and
/// the call always succeeds (the Python queue silently swallows send errors
/// as well).
#[derive(Debug, Clone)]
pub struct LogMailSender {
    /// Envelope sender used for the log record.
    pub from: String,
}

impl LogMailSender {
    pub fn new(from: impl Into<String>) -> Self {
        Self { from: from.into() }
    }
}

impl Default for LogMailSender {
    fn default() -> Self {
        Self::new("ksi")
    }
}

impl MailSender for LogMailSender {
    fn send(&self, msg: &MailMessage) -> Result<(), MailSendError> {
        tracing::info!(
            from = %self.from,
            to_count = msg.to.len(),
            to = ?msg.to,
            cc = ?msg.cc,
            bcc = ?msg.bcc,
            subject = %msg.subject,
            reply_to = ?msg.reply_to,
            body_len = msg.body_html.len(),
            "e-mail NOT actually sent (SMTP not wired up); see ksi-api::mail docs"
        );
        tracing::debug!(body = %msg.body_html, "e-mail body (not sent)");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_sender_accepts_message() {
        let sender = LogMailSender::default();
        let msg = MailMessage {
            from: "ksi".into(),
            to: vec!["a@example.com".into()],
            cc: vec![],
            bcc: vec!["org@example.com".into()],
            subject: "Ahoj".into(),
            body_html: "<p>Ahoj</p>".into(),
            reply_to: None,
        };
        sender.send(&msg).unwrap();
    }
}
