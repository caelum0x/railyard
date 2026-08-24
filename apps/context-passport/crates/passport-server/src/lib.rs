//! Character Lock Passport server: thin axum handlers over `PassportService`.

pub mod auth;
pub mod dto;
pub mod error;
pub mod handlers;
pub mod router;
pub mod service;
pub mod state;

pub use router::build_router;
pub use service::PassportService;
pub use state::AppState;
