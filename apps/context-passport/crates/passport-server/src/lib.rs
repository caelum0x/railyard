//! Identity Lock server: thin axum handlers over `PassportService`.

pub mod auth;
pub mod dto;
pub mod error;
pub mod handlers;
pub mod receipts;
pub mod router;
pub mod service;
pub mod state;
mod console;

pub use router::build_router;
pub use service::PassportService;
pub use state::AppState;
