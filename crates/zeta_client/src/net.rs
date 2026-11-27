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

use std::net::{AddrParseError, Ipv4Addr, SocketAddr};

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
use zeta_core::settings::DEFAULT_SERVER_PORT;

/// Parses the server address from an optional environment value, defaulting to local
/// `127.0.0.1` on [`DEFAULT_SERVER_PORT`].
///
/// # Errors
/// Returns [`AddrParseError`] when `value` is present but not a valid `host:port` socket address.
pub fn parse_server_address(value: Option<&str>) -> Result<SocketAddr, AddrParseError> {
    value.map_or_else(
        || Ok(SocketAddr::from((Ipv4Addr::LOCALHOST, DEFAULT_SERVER_PORT))),
        str::parse,
    )
}

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

/// Target address and latest failure detail for [`ConnectionStatus`]
/// (`docs/architecture/code_style.md` Section 7).
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionDiagnostic {
    /// Address the client is connecting or connected to.
    pub server_address: Option<SocketAddr>,
    /// Concise failure cause when disconnected or rejected, such as refused connect or
    /// protocol mismatch.
    pub detail: Option<String>,
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
        app.init_resource::<ConnectionStatus>()
            .init_resource::<ConnectionDiagnostic>();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_server_is_local_on_the_default_port() {
        let address = parse_server_address(None).expect("the default parses");
        assert_eq!(
            address,
            SocketAddr::from((Ipv4Addr::LOCALHOST, DEFAULT_SERVER_PORT))
        );
    }

    #[test]
    fn the_environment_value_overrides_the_default() {
        let address = parse_server_address(Some("10.0.0.5:9000")).expect("a valid address");
        assert_eq!(address, SocketAddr::from(([10, 0, 0, 5], 9000)));
    }

    #[test]
    fn an_invalid_environment_value_is_an_error() {
        assert!(parse_server_address(Some("not an address")).is_err());
    }
}
