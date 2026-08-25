//! # `ai` — deterministic opponents.
//!
//! [`deploy_bot`] is the reusable bot policy: a pure function from match state to this
//! tick's deploys. It drives the bot-vs-bot runner, PvE, and the test opponents.

pub mod deploy_bot;

pub use deploy_bot::{bot_deploy_for, bot_deploys, BotDifficulty};
