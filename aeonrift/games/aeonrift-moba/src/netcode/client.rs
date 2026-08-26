//! The predicting lightyear client over real UDP.
//!
//! Subsumes the in-process `client::Client`: lightyear owns the transport and the
//! rollback bookkeeping, while `sim::step` (run by [`super::shared::SharedPlugin`]
//! in `FixedUpdate`) predicts locally. Local intents are written as [`MoveInput`]s
//! in `FixedPreUpdate`; lightyear sends them to the server and replays them on
//! rollback.

use core::net::SocketAddr;

use bevy::prelude::*;
use lightyear::netcode::Key;
use lightyear::prelude::client::input::*;
use lightyear::prelude::client::*;
use lightyear::prelude::input::native::*;
use lightyear::prelude::*;
use lightyear_deterministic_replication::Deterministic;

use super::protocol::{MoveInput, NetPlayer};
use super::shared::{server_addr, SimNet};

/// Adds the predicting UDP client to an app.
pub struct NetcodeClientPlugin {
    pub server_port: u16,
    pub client_addr: SocketAddr,
    pub client_id: u64,
    pub seed: u64,
}

impl Plugin for NetcodeClientPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SimNet::new(self.seed));
        app.init_resource::<ScriptedInput>();
        app.insert_resource(ClientConfig {
            server_port: self.server_port,
            client_addr: self.client_addr,
            client_id: self.client_id,
        });
        app.add_systems(Startup, startup);
        app.add_systems(
            FixedPreUpdate,
            buffer_input.in_set(InputSystems::WriteClientInputs),
        );
        app.add_observer(handle_controlled_spawn);
    }
}

/// The intent the client injects each tick (set by a bot / test / UI). `None`
/// means "hold position".
#[derive(Resource, Default, Clone, Copy)]
pub struct ScriptedInput(pub Option<MoveInput>);

#[derive(Resource, Clone, Copy)]
struct ClientConfig {
    server_port: u16,
    client_addr: SocketAddr,
    client_id: u64,
}

/// Connect to the server over UDP.
fn startup(mut commands: Commands, config: Res<ClientConfig>) -> Result {
    let auth = Authentication::Manual {
        server_addr: server_addr(config.server_port),
        client_id: config.client_id,
        private_key: Key::default(),
        protocol_id: 0,
    };
    let client = commands
        .spawn((
            Client,
            LocalAddr(config.client_addr),
            PeerAddr(server_addr(config.server_port)),
            Link::default(),
            ReplicationReceiver,
            NetcodeClient::new(auth, NetcodeConfig::default())?,
            UdpIo::default(),
            Name::from("AeonriftClient"),
        ))
        .id();
    commands.trigger(Connect { entity: client });
    Ok(())
}

/// Write this tick's local intent into the controlled champion's `ActionState`.
/// Must run in `InputSystems::WriteClientInputs` (FixedPreUpdate).
fn buffer_input(
    scripted: Res<ScriptedInput>,
    mut query: Query<&mut ActionState<MoveInput>, With<InputMarker<MoveInput>>>,
) {
    if let Ok(mut action) = query.single_mut() {
        action.0 = scripted.0.unwrap_or_default();
    }
}

/// When lightyear hands us a controlled champion, mark it as our input source AND
/// opt its predicted copy into the checksum archetype set.
///
/// The `Controlled` entity is the client's *predicted* champion (it carries
/// `PredictionHistory<SimStateHash>`); inserting [`Deterministic`] here is the client
/// half of the checksum bind — without it `ChecksumSendPlugin` would hash no entity
/// and send an empty (0) checksum that the server silently ignores.
fn handle_controlled_spawn(
    trigger: On<Add, Controlled>,
    players: Query<(), (With<NetPlayer>, Without<InputMarker<MoveInput>>)>,
    mut commands: Commands,
) {
    if players.get(trigger.entity).is_ok() {
        commands
            .entity(trigger.entity)
            .insert((InputMarker::<MoveInput>::default(), Deterministic));
    }
}
