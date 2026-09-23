//! Shared application state.

use std::sync::Arc;

use rago_db::Db;
use rago_graphql::RagoSchema;
use rago_mail::{CaptureTransport, EmailTransport};

/// Axum application state: the database handle, the GraphQL schema, and the
/// injectable email transport.
///
/// `mailer` is the active [`EmailTransport`]. By default it is a
/// [`CaptureTransport`] (also exposed via `capture` so rendered emails are
/// inspectable at `/emails`). Built with `--features live`, it becomes the real
/// SMTP transport and `capture` is `None` — "drop credentials → live" with no
/// code change.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub schema: RagoSchema,
    pub mailer: Arc<dyn EmailTransport>,
    pub capture: Option<Arc<CaptureTransport>>,
}

impl AppState {
    /// Default state: an in-memory capture mailer (inspectable at `/emails`).
    pub fn new(db: Db) -> Self {
        let capture = Arc::new(CaptureTransport::new());
        let schema = rago_graphql::build_schema_with_mailer(db.clone(), capture.clone());
        Self {
            db,
            schema,
            mailer: capture.clone(),
            capture: Some(capture),
        }
    }

    /// Live state: the real SMTP transport (owner-gated, `--features live`).
    #[cfg(feature = "live")]
    pub fn new_live(db: Db) -> Result<Self, rago_mail::MailError> {
        let mailer = Arc::new(rago_mail::SmtpTransport::from_env()?);
        let schema = rago_graphql::build_schema_with_mailer(db.clone(), mailer.clone());
        Ok(Self {
            db,
            schema,
            mailer,
            capture: None,
        })
    }
}
