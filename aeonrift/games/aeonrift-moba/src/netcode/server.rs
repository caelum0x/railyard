//! The authoritative lightyear server over real UDP.
//!
//! Subsumes the in-process `server::Server`: lightyear owns the transport, input
//! buffering, and `effect_tick` scheduling, while `sim::step` (run by
//! [`super::shared::SharedPlugin`] in `FixedUpdate`) stays the authoritative
//! kernel. On connect the server spawns the replicated champion entity.

use bevy::prelude::*;
use lightyear::prelude::server::*;
use lightyear::prelude::*;
use lightyear_deterministic_replication::Deterministic;

use super::protocol::{NetChampion, NetPlayer, SimStateHash};
use super::shared::{server_addr, SimNet, CHAMPION_ID};

/// How often the server flushes replication updates to clients.
const SEND_INTERVAL: core::time::Duration = core::time::Duration::from_millis(20);

/// Adds the authoritative UDP server to an app.
pub struct NetcodeServerPlugin {
    pub port: u16,
    pub seed: u64,
}

impl Plugin for NetcodeServerPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SimNet::new(self.seed));
        app.insert_resource(ReplicationMetadata::new(SEND_INTERVAL));
        app.insert_resource(ServerPort(self.port));
        app.add_systems(Startup, startup);
        app.add_observer(handle_new_client);
        app.add_observer(handle_connected);
    }
}

#[derive(Resource, Clone, Copy)]
struct ServerPort(u16);

/// Start the UDP server listening on the configured loopback port.
fn startup(mut commands: Commands, port: Res<ServerPort>) {
    let server = commands
        .spawn((
            NetcodeServer::new(NetcodeConfig::default()),
            LocalAddr(server_addr(port.0)),
            ServerUdpIo::default(),
            Name::from("AeonriftServer"),
        ))
        .id();
    commands.trigger(Start { entity: server });
}

/// New link => enable replication to that client.
fn handle_new_client(trigger: On<Add, LinkOf>, mut commands: Commands) {
    commands
        .entity(trigger.entity)
        .insert((ReplicationSender, Name::from("ClientOf")));
}

/// On a confirmed connection, spawn the replicated + client-predicted champion.
fn handle_connected(
    trigger: On<Add, Connected>,
    query: Query<&RemoteId, With<ClientOf>>,
    sim: Res<SimNet>,
    mut commands: Commands,
) {
    let Ok(client_id) = query.get(trigger.entity) else {
        return;
    };
    let client_id = client_id.0;

    let (x, y, hp, team) = sim
        .world
        .champion(CHAMPION_ID)
        .map(|c| (c.pos.x, c.pos.y, c.health.current, c.team))
        .unwrap_or((0.0, 0.0, 0, 0));
    let hash = sim::state_hash(&sim.world);

    let entity = commands
        .spawn((
            NetChampion { x, y, hp, team },
            // The checksum-bound determinism fingerprint (see `protocol::SimStateHash`).
            SimStateHash(hash),
            // Opt this authoritative champion into lightyear's checksum archetype set;
            // without this marker `ChecksumReceivePlugin` would hash nothing and every
            // client checksum would be compared against an empty (0) history.
            Deterministic,
            NetPlayer(client_id.to_bits()),
            // Server-authoritative replication + client-side prediction for the
            // controlling client (= aeonrift's predict/reconcile, now over UDP).
            Replicate::to_clients(NetworkTarget::All),
            PredictionTarget::to_clients(NetworkTarget::Single(client_id)),
            ControlledBy {
                owner: trigger.entity,
                lifetime: Default::default(),
            },
            Name::from("Champion"),
        ))
        .id();
    info!("spawned champion {entity:?} for client {client_id:?}");
}
