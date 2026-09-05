//! # rago-mail
//!
//! Transactional email for Rago — faithful port of the `*_mailer` layer: email
//! builders that render the same billing data (invoice, credit note, receipt,
//! dunning) into an email body, plus an injectable [`EmailTransport`] so the
//! whole path is verified against an in-memory transport in tests. A live SMTP
//! transport is owner-gated (not built here).

pub mod builders;

use std::sync::Mutex;

use async_trait::async_trait;
use thiserror::Error;

/// A rendered email.
#[derive(Debug, Clone)]
pub struct Email {
    pub to: String,
    pub from: String,
    pub subject: String,
    pub html_body: String,
    pub text_body: String,
}

#[derive(Debug, Error)]
pub enum MailError {
    #[error("mail transport error: {0}")]
    Transport(String),
}

/// An injectable email transport (`ActionMailer` delivery method).
#[async_trait]
pub trait EmailTransport: Send + Sync {
    async fn send(&self, email: &Email) -> Result<(), MailError>;
}

/// In-memory transport that captures sent emails (for tests/verification).
#[derive(Default)]
pub struct CaptureTransport {
    sent: Mutex<Vec<Email>>,
}

impl CaptureTransport {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn sent(&self) -> Vec<Email> {
        self.sent.lock().unwrap().clone()
    }
    pub fn last(&self) -> Option<Email> {
        self.sent.lock().unwrap().last().cloned()
    }
}

#[async_trait]
impl EmailTransport for CaptureTransport {
    async fn send(&self, email: &Email) -> Result<(), MailError> {
        self.sent.lock().unwrap().push(email.clone());
        Ok(())
    }
}

// ---- live SMTP transport (owner-gated behind the `live` feature) -----------

/// Live SMTP email transport (owner-gated behind the `live` feature).
///
/// Reads SMTP settings from the environment (`RAGO_SMTP_HOST`,
/// `RAGO_SMTP_PORT`, `RAGO_SMTP_USERNAME`, `RAGO_SMTP_PASSWORD`) via
/// [`SmtpTransport::from_env`]; builds a multipart (plain + HTML) message and
/// delivers it over STARTTLS. Drop credentials in and it sends for real with
/// zero code change.
#[cfg(feature = "live")]
pub struct SmtpTransport {
    mailer: lettre::AsyncSmtpTransport<lettre::Tokio1Executor>,
}

#[cfg(feature = "live")]
impl SmtpTransport {
    /// Build the transport from `RAGO_SMTP_*` environment variables.
    pub fn from_env() -> Result<Self, MailError> {
        let host = std::env::var("RAGO_SMTP_HOST").map_err(|_| MailError::Transport("RAGO_SMTP_HOST not set".into()))?;
        let port: u16 = std::env::var("RAGO_SMTP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(587);
        let username = std::env::var("RAGO_SMTP_USERNAME").unwrap_or_default();
        let password = std::env::var("RAGO_SMTP_PASSWORD").unwrap_or_default();

        let creds = lettre::transport::smtp::authentication::Credentials::new(username, password);
        let mailer = lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::starttls_relay(&host)
            .map_err(|e| MailError::Transport(e.to_string()))?
            .port(port)
            .credentials(creds)
            .build();
        Ok(Self { mailer })
    }
}

#[cfg(feature = "live")]
#[async_trait]
impl EmailTransport for SmtpTransport {
    async fn send(&self, email: &Email) -> Result<(), MailError> {
        use lettre::message::{header::ContentType, MultiPart, SinglePart};
        use lettre::{AsyncTransport, Message};

        let message = Message::builder()
            .from(email.from.parse().map_err(|e| MailError::Transport(format!("bad from: {e}")))?)
            .to(email.to.parse().map_err(|e| MailError::Transport(format!("bad to: {e}")))?)
            .subject(&email.subject)
            .multipart(
                MultiPart::alternative()
                    .singlepart(SinglePart::builder().header(ContentType::TEXT_PLAIN).body(email.text_body.clone()))
                    .singlepart(SinglePart::builder().header(ContentType::TEXT_HTML).body(email.html_body.clone())),
            )
            .map_err(|e| MailError::Transport(e.to_string()))?;

        self.mailer.send(message).await.map_err(|e| MailError::Transport(e.to_string()))?;
        Ok(())
    }
}
