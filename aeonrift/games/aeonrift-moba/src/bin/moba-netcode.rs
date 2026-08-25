//! Real-UDP lightyear netcode demo for the AEONRIFT MOBA.
//!
//! Runs the authoritative server or a predicting client over real UDP loopback,
//! both driving the SAME untouched `sim` deterministic kernel.
//!
//!   cargo +1.96.1 run -p aeonrift-moba --features netcode --bin moba-netcode -- server
//!   cargo +1.96.1 run -p aeonrift-moba --features netcode --bin moba-netcode -- client
//!
//! (feature-gated; the default build never compiles this.)

use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use aeonrift_moba::netcode::{build_client_app, build_server_app};
use bevy::log::LogPlugin;

const SERVER_PORT: u16 = 6001;
const SEED: u64 = 42;

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "server".to_string());
    match mode.as_str() {
        "server" => {
            let mut app = build_server_app(SERVER_PORT, SEED);
            app.add_plugins(LogPlugin::default());
            println!("aeonrift moba-netcode: UDP server listening on 127.0.0.1:{SERVER_PORT}");
            app.run();
        }
        "client" => {
            let client_addr: SocketAddr =
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6002);
            let mut app = build_client_app(SERVER_PORT, client_addr, 1, SEED);
            app.add_plugins(LogPlugin::default());
            println!("aeonrift moba-netcode: UDP client connecting to 127.0.0.1:{SERVER_PORT}");
            app.run();
        }
        other => {
            eprintln!("unknown mode {other:?}; use `server` or `client`");
            std::process::exit(2);
        }
    }
}
