//! Real-UDP netcode via **lightyear 0.29** (Bevy 0.19), behind the `netcode`
//! feature.
//!
//! This is the M1.5 graduation of aeonrift's in-process predict/reconcile spine:
//! it REPLACES the in-process channel with real UDP while KEEPING `sim-core` as
//! the untouched deterministic kernel. The default build (feature off) still uses
//! the in-process `server::Server` / `client::Client` spine — so the `no_desync`
//! gate and the whole default workspace build stay green; this module only
//! compiles when `--features netcode` is set.
//!
//! ## The lightyear <-> aeonrift-spine mapping
//! - [`protocol::MoveInput`] — inputs-only native input == `sim::net::StampedInput`.
//! - `PredictionTarget` + rollback == `client::Client::{predict,reconcile}`.
//! - [`shared::SimChecksum`] (`sim::state_hash`) == `ChecksumPlugin` desync guard.
//! - `FixedUpdate` at `sim::TICK_HZ` == the fixed `DT` tick.
//! - lightyear input-delay config == the `effect_tick` rewrite.
//!
//! ## Run it (two processes, real UDP loopback)
//! ```text
//! cargo +1.96.1 run -p aeonrift-moba --features netcode --bin moba-netcode -- server
//! cargo +1.96.1 run -p aeonrift-moba --features netcode --bin moba-netcode -- client
//! ```

pub mod client;
pub mod protocol;
pub mod server;
pub mod shared;

use core::net::SocketAddr;

use bevy::prelude::*;
use lightyear::prelude::client::ClientPlugins;
use lightyear::prelude::server::ServerPlugins;

pub use client::{NetcodeClientPlugin, ScriptedInput};
pub use protocol::{MoveInput, NetChampion, NetPlayer, ProtocolPlugin};
pub use server::NetcodeServerPlugin;
pub use shared::{server_addr, tick_duration, SharedPlugin, SimChecksum, SimNet, CHAMPION_ID};

/// A headless Bevy app skeleton (no window, no log subscriber). Two of these can
/// coexist in one process for an in-process UDP-loopback test.
fn base_app() -> App {
    let mut app = App::new();
    // The headless plugin set lightyear needs (mirrors the example harness), minus
    // any global log subscriber so two apps can coexist in one process (tests).
    app.add_plugins(MinimalPlugins);
    app.add_plugins(bevy::transform::TransformPlugin);
    app.add_plugins(bevy::state::app::StatesPlugin);
    app.add_plugins(bevy::diagnostic::DiagnosticsPlugin);
    app
}

/// Build a headless authoritative UDP server app listening on `port`.
pub fn build_server_app(port: u16, seed: u64) -> App {
    let mut app = base_app();
    // Client/Server plugins first, then the protocol (ProtocolPlugin must follow).
    app.add_plugins(ServerPlugins {
        tick_duration: tick_duration(),
    });
    app.add_plugins(SharedPlugin);
    app.add_plugins(NetcodeServerPlugin { port, seed });
    app
}

/// Build a headless predicting UDP client app connecting to `server_port`.
pub fn build_client_app(
    server_port: u16,
    client_addr: SocketAddr,
    client_id: u64,
    seed: u64,
) -> App {
    let mut app = base_app();
    app.add_plugins(ClientPlugins {
        tick_duration: tick_duration(),
    });
    app.add_plugins(SharedPlugin);
    app.add_plugins(NetcodeClientPlugin {
        server_port,
        client_addr,
        client_id,
        seed,
    });
    app
}
