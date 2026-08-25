//! REAL-UDP NETCODE GATE (M1.5), behind the `netcode` feature.
//!
//! Spins up an authoritative lightyear server and a predicting client as two
//! in-process Bevy apps talking over **real UDP loopback**, then drives a move
//! input and asserts:
//!   1. the client actually connects over UDP and receives the replicated champion
//!      (end-to-end transport works), and
//!   2. the client's champion converges toward the server's authoritative
//!      position after the move input flows over the wire and through `sim::step`.
//!
//! Run: `cargo +1.96.1 test -p aeonrift-moba --features netcode --test netcode_udp`
//!
//! This is inherently timing-dependent (real sockets + async IO), so it is
//! generous with warmup iterations. If it proves flaky in a constrained CI
//! sandbox, it can be `--ignored`; the feature-compile + the `moba-netcode` bin
//! remain the primary proofs.
#![cfg(feature = "netcode")]

use std::time::Duration;

use aeonrift_moba::netcode::{
    build_client_app, build_server_app, MoveInput, NetChampion, ScriptedInput, SimNet, CHAMPION_ID,
};
use core::net::{IpAddr, Ipv4Addr, SocketAddr};

const SERVER_PORT: u16 = 6111;
const SEED: u64 = 42;

/// Pump both apps once with a short real-time gap so UDP packets can traverse the
/// loopback interface between frames.
fn pump(server: &mut bevy::app::App, client: &mut bevy::app::App) {
    server.update();
    client.update();
    std::thread::sleep(Duration::from_millis(5));
}

#[test]
fn client_connects_and_converges_over_real_udp() {
    let client_addr: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6112);
    let mut server = build_server_app(SERVER_PORT, SEED);
    let mut client = build_client_app(SERVER_PORT, client_addr, 1, SEED);

    // Manually driving `update()` (instead of `run()`) means we must run plugin
    // `finish`/`cleanup` ourselves — that's where lightyear/replicon insert their
    // channel-map and transport resources.
    for app in [&mut server, &mut client] {
        app.finish();
        app.cleanup();
    }

    // Warm up: let the netcode handshake + first replication land.
    let mut replicated = false;
    for _ in 0..400 {
        pump(&mut server, &mut client);
        let mut q = client.world_mut().query::<&NetChampion>();
        if q.iter(client.world()).next().is_some() {
            replicated = true;
            break;
        }
    }
    assert!(
        replicated,
        "client never received a replicated champion over UDP (handshake/replication failed)"
    );

    // Drive a move toward a far target and let it flow over the wire + sim::step.
    client
        .world_mut()
        .insert_resource(ScriptedInput(Some(MoveInput::move_to(sim::Vec2::new(
            40.0, 0.0,
        )))));

    for _ in 0..600 {
        pump(&mut server, &mut client);
    }

    // The authoritative server sim must have advanced the champion toward +x.
    let server_pos = server
        .world()
        .resource::<SimNet>()
        .world
        .champion(CHAMPION_ID)
        .expect("server champion exists")
        .pos;
    assert!(
        server_pos.x > 1.0,
        "server sim did not move the champion from the input (x={}); \
         inputs are not reaching sim::step over UDP",
        server_pos.x
    );

    // The client must have received the authoritative movement (convergence).
    let mut q = client.world_mut().query::<&NetChampion>();
    let client_x = q
        .iter(client.world())
        .map(|c| c.x)
        .next()
        .expect("client has a replicated champion");
    assert!(
        client_x > 0.5,
        "client champion did not converge toward the server's authoritative x \
         (client_x={client_x}, server_x={})",
        server_pos.x
    );
}
