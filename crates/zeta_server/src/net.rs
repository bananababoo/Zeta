//! Network layer of the server: the only module that imports the networking library.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 1, and
//! `docs/architecture/code_style.md` Section 11 (pre-1.0 dependencies stay in 1 module).
//! [`ServerNetPlugin`] replicates every `Networked` entity, registers the shared protocol,
//! runs the TCP transport, and turns library messages into the [`messages`] types the server
//! world reads and writes.

mod backend;
pub mod messages;
mod transport;

use std::net::SocketAddr;

use bevy_app::{App, Plugin, PostUpdate, PreUpdate};
use bevy_ecs::component::Mutable;
use bevy_ecs::lifecycle::{Add, Remove};
use bevy_ecs::message::Messages;
use bevy_ecs::observer::On;
use bevy_ecs::prelude::{
    Component, IntoScheduleConfigs, Message, MessageWriter, Query, ResMut, With,
};
use bevy_replicon::prelude::{
    AppRuleExt, AuthorizedClient, Channel, ClientId, ClientMessageAppExt, FromClient, Replicated,
    RepliconSharedPlugin, SendTargets, ServerMessageAppExt, ServerMessagePlugin, ServerPlugin,
    ServerSystems, ToClients,
};
use bevy_state::app::StatesPlugin;
use bevy_time::TimePlugin;
use serde::Serialize;
use serde::de::DeserializeOwned;
use zeta_core::protocol::{Delivery, ProtocolRegistrar, register_protocol};

use self::messages::{
    FromPlayer, NetMessagesPlugin, Networked, PlayerConnected, PlayerConnection,
    PlayerDisconnected, Recipients, ToPlayers,
};

/// Runs the server side of the network: listens for players on `listen_address`, replicates
/// `Networked` entities to them, and exchanges the protocol messages through
/// [`FromPlayer`] and [`ToPlayers`].
///
/// Adds [`NetMessagesPlugin`] itself, and the engine's state and time plugins when missing.
/// When the address cannot be bound the app exits with an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerNetPlugin {
    /// Address the server accepts player connections on.
    pub listen_address: SocketAddr,
}

impl Plugin for ServerNetPlugin {
    fn build(&self, app: &mut App) {
        // The networking library keeps its connection status in engine states and times its
        // acknowledgement cleanup with engine time, so both must exist before it is added.
        if !app.is_plugin_added::<StatesPlugin>() {
            app.add_plugins(StatesPlugin);
        }
        if !app.is_plugin_added::<TimePlugin>() {
            app.add_plugins(TimePlugin);
        }
        // Added 1 by 1 rather than as the library's plugin group, because building the whole
        // workspace enables the client side of the library here too.
        app.add_plugins((
            RepliconSharedPlugin::default(),
            // Replicates once per server update; the server loop already runs at a fixed rate.
            ServerPlugin::new(PostUpdate),
            ServerMessagePlugin,
            NetMessagesPlugin,
        ));
        register_protocol(&mut ServerRegistrar(app));
        app.register_required_components::<Networked, Replicated>()
            .add_observer(announce_connected)
            .add_observer(announce_disconnected);
        backend::build(app, self.listen_address);
    }
}

/// Registers each protocol entry with the library and bridges its messages to the Zeta types.
struct ServerRegistrar<'a>(&'a mut App);

impl ProtocolRegistrar for ServerRegistrar<'_> {
    fn component<C>(&mut self)
    where
        C: Component<Mutability = Mutable> + Serialize + DeserializeOwned,
    {
        self.0.replicate::<C>();
    }

    fn client_message<M>(&mut self, delivery: Delivery)
    where
        M: Message + Serialize + DeserializeOwned,
    {
        self.0
            .add_client_message::<M>(channel(delivery))
            .add_message::<FromPlayer<M>>()
            .add_systems(
                PreUpdate,
                forward_from_clients::<M>.after(ServerSystems::Receive),
            );
    }

    fn server_message<M>(&mut self, delivery: Delivery)
    where
        M: Message + Serialize + DeserializeOwned,
    {
        self.0
            .add_server_message::<M>(channel(delivery))
            .add_message::<ToPlayers<M>>()
            .add_systems(
                PostUpdate,
                forward_to_clients::<M>.before(ServerSystems::Send),
            );
    }
}

fn channel(delivery: Delivery) -> Channel {
    match delivery {
        Delivery::Ordered => Channel::Ordered,
        Delivery::Unordered => Channel::Unordered,
    }
}

/// The library marks a client authorized once its protocol matches; only then is it a player.
#[allow(
    clippy::needless_pass_by_value,
    reason = "engine observers receive their trigger by value"
)]
fn announce_connected(
    add: On<Add<AuthorizedClient>>,
    mut connected: MessageWriter<PlayerConnected>,
) {
    connected.write(PlayerConnected {
        connection: PlayerConnection(add.entity),
    });
}

/// Runs when the connection entity despawns, so only announced players are announced as gone.
#[allow(
    clippy::needless_pass_by_value,
    reason = "engine observers receive their trigger by value"
)]
fn announce_disconnected(
    remove: On<Remove<AuthorizedClient>>,
    mut disconnected: MessageWriter<PlayerDisconnected>,
) {
    disconnected.write(PlayerDisconnected {
        connection: PlayerConnection(remove.entity),
    });
}

fn forward_from_clients<M: Message>(
    mut from_clients: ResMut<Messages<FromClient<M>>>,
    authorized: Query<(), With<AuthorizedClient>>,
    mut from_players: MessageWriter<FromPlayer<M>>,
) {
    for FromClient { client_id, message } in from_clients.drain() {
        // Messages the server sends itself have no connection, and unauthorized clients are
        // not players yet.
        let Some(entity) = client_id.entity() else {
            continue;
        };
        if authorized.contains(entity) {
            from_players.write(FromPlayer {
                connection: PlayerConnection(entity),
                message,
            });
        }
    }
}

fn forward_to_clients<M: Message>(
    mut to_players: ResMut<Messages<ToPlayers<M>>>,
    mut to_clients: MessageWriter<ToClients<M>>,
) {
    for ToPlayers {
        recipients,
        message,
    } in to_players.drain()
    {
        let targets = match recipients {
            Recipients::All => SendTargets::All,
            Recipients::Only(PlayerConnection(entity)) => {
                SendTargets::Single(ClientId::Client(entity))
            }
        };
        to_clients.write(ToClients { targets, message });
    }
}
