//! Network layer of the client: the only module that imports the networking library.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 1, and
//! `docs/architecture/code_style.md` Section 11 (pre-1.0 dependencies stay in 1 module).
//! [`ClientNetPlugin`] connects to the server, registers the shared protocol, and runs the TCP
//! transport. Replicated components then appear on local entities, the client sends a protocol
//! message by writing it as an engine message (for example `MoveRequest`), and reads what the
//! server sent the same way (for example `PlayerAssigned`).

mod backend;
mod transport;

use std::net::SocketAddr;

use bevy::ecs::component::Mutable;
use bevy::prelude::{App, Component, Message, Plugin, Resource};
use bevy::state::app::StatesPlugin;
use bevy_replicon::prelude::{
    AppRuleExt, Channel, ClientMessageAppExt, ClientMessagePlugin, ClientPlugin,
    RepliconSharedPlugin, ServerMessageAppExt,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use zeta_core::protocol::{Delivery, ProtocolRegistrar, register_protocol};

/// Runs the client side of the network: connects to `server_address` once at startup,
/// replicates the server's entities into this world, and exchanges the protocol messages.
///
/// Add it after the engine's default plugins; it adds the engine's state plugin itself when
/// missing. [`ConnectionStatus`] reports how the connection is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientNetPlugin {
    /// Address of the server to connect to.
    pub server_address: SocketAddr,
}

/// How the connection to the server is doing, for the interface to show.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// Waiting for the server to answer.
    #[default]
    Connecting,
    /// Connected; replicated entities and server messages arrive.
    Connected,
    /// The connect failed or the connection ended. The client does not reconnect on its own.
    Disconnected,
}

impl Plugin for ClientNetPlugin {
    fn build(&self, app: &mut App) {
        // The networking library keeps its connection status in engine states.
        if !app.is_plugin_added::<StatesPlugin>() {
            app.add_plugins(StatesPlugin);
        }
        // Added 1 by 1 rather than as the library's plugin group, because building the whole
        // workspace enables the server side of the library here too.
        app.add_plugins((
            RepliconSharedPlugin::default(),
            ClientPlugin,
            ClientMessagePlugin,
        ));
        register_protocol(&mut ClientRegistrar(app));
        app.init_resource::<ConnectionStatus>();
        backend::build(app, self.server_address);
    }
}

/// Registers each protocol entry with the library. The library already maps the messages onto
/// plain engine messages on the client, so no bridging is needed.
struct ClientRegistrar<'a>(&'a mut App);

impl ProtocolRegistrar for ClientRegistrar<'_> {
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
        self.0.add_client_message::<M>(channel(delivery));
    }

    fn server_message<M>(&mut self, delivery: Delivery)
    where
        M: Message + Serialize + DeserializeOwned,
    {
        self.0.add_server_message::<M>(channel(delivery));
    }
}

fn channel(delivery: Delivery) -> Channel {
    match delivery {
        Delivery::Ordered => Channel::Ordered,
        Delivery::Unordered => Channel::Unordered,
    }
}
