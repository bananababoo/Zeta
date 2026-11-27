//! Moves frames between the TCP transport and the networking library's message buffers, and
//! keeps the library's client state and [`ConnectionStatus`] in step with the socket.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 1.

use std::mem;
use std::net::SocketAddr;

use bevy::log::{info, warn};
use bevy::prelude::{
    App, DetectChangesMut, IntoScheduleConfigs, NextState, On, PostUpdate, PreUpdate, Res, ResMut,
    Resource, Startup, State,
};
use bevy_replicon::prelude::{
    ClientMessages, ClientState, ClientSystems, ProtocolMismatch, RepliconChannels,
};

use super::transport::{Connection, ConnectionError, PendingConnection};
use super::{ConnectionDiagnostic, ConnectionStatus};
use crate::settings::CONNECT_TIMEOUT;

/// Address given to [`super::ClientNetPlugin`].
#[derive(Resource, Clone, Copy, Debug)]
struct ServerAddress(SocketAddr);

/// The socket to the server in whatever stage it is.
#[derive(Resource, Debug, Default)]
enum ServerLink {
    /// No socket: not started yet, failed, or ended.
    #[default]
    Closed,
    /// The background connect is running.
    Connecting(PendingConnection),
    /// Connected.
    Open(Connection),
}

pub(super) fn build(app: &mut App, server_address: SocketAddr) {
    app.insert_resource(ServerAddress(server_address))
        .init_resource::<ServerLink>()
        .add_observer(record_protocol_mismatch)
        .add_systems(Startup, start_connecting)
        .add_systems(
            PreUpdate,
            receive_packets.in_set(ClientSystems::ReceivePackets),
        )
        .add_systems(PostUpdate, send_packets.in_set(ClientSystems::SendPackets));
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy observer parameters are passed by value"
)]
fn record_protocol_mismatch(
    _: On<ProtocolMismatch>,
    address: Res<ServerAddress>,
    mut diagnostic: ResMut<ConnectionDiagnostic>,
) {
    let message = format!(
        "protocol mismatch with server at {}: client and server builds do not match",
        address.0
    );
    warn!("{message}");
    diagnostic.server_address = Some(address.0);
    diagnostic.detail = Some(message);
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "engine systems receive resource handles by value"
)]
fn start_connecting(
    address: Res<ServerAddress>,
    mut link: ResMut<ServerLink>,
    mut status: ResMut<ConnectionStatus>,
    mut diagnostic: ResMut<ConnectionDiagnostic>,
) {
    info!("connecting to the server at {}", address.0);
    *link = ServerLink::Connecting(PendingConnection::start(address.0, CONNECT_TIMEOUT));
    *status = ConnectionStatus::Connecting;
    diagnostic.server_address = Some(address.0);
    diagnostic.detail = None;
}

/// The library's client state may only change here, so this system also applies what the
/// send system found.
#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "engine systems receive resource handles by value"
)]
fn receive_packets(
    address: Res<ServerAddress>,
    mut link: ResMut<ServerLink>,
    mut status: ResMut<ConnectionStatus>,
    mut diagnostic: ResMut<ConnectionDiagnostic>,
    state: Res<State<ClientState>>,
    mut next_state: ResMut<NextState<ClientState>>,
    channels: Res<RepliconChannels>,
    mut messages: ResMut<ClientMessages>,
) {
    *link = match mem::take(&mut *link) {
        ServerLink::Closed => ServerLink::Closed,
        ServerLink::Connecting(pending) => match pending.try_finish() {
            Err(pending) => ServerLink::Connecting(pending),
            Ok(Ok(connection)) => {
                info!("connected to the server");
                diagnostic.detail = None;
                ServerLink::Open(connection)
            }
            Ok(Err(error)) => {
                let message = format!("could not connect to server at {} ({error})", address.0);
                warn!("{message}");
                diagnostic.detail = Some(message);
                ServerLink::Closed
            }
        },
        ServerLink::Open(mut connection) => {
            match receive(
                &mut connection,
                channels.server_channels().len(),
                &mut messages,
            ) {
                Ok(()) => ServerLink::Open(connection),
                Err(ConnectionError::Closed) => {
                    info!("the server closed the connection");
                    if diagnostic.detail.is_none() {
                        diagnostic.detail =
                            Some(format!("server at {} closed the connection", address.0));
                    }
                    ServerLink::Closed
                }
                Err(error) => {
                    let message = format!("connection to server at {} lost ({error})", address.0);
                    warn!("{message}");
                    diagnostic.detail = Some(message);
                    ServerLink::Closed
                }
            }
        }
    };

    let (wanted_state, wanted_status) = match *link {
        ServerLink::Closed => (ClientState::Disconnected, ConnectionStatus::Disconnected),
        ServerLink::Connecting(_) => (ClientState::Connecting, ConnectionStatus::Connecting),
        ServerLink::Open(_) => (ClientState::Connected, ConnectionStatus::Connected),
    };
    if **state != wanted_state {
        next_state.set(wanted_state);
    }
    // Startup already began connecting, so a closed link here means the connection is gone.
    status.set_if_neq(wanted_status);
}

fn receive(
    connection: &mut Connection,
    channel_count: usize,
    messages: &mut ClientMessages,
) -> Result<(), ConnectionError> {
    let mut frames = Vec::new();
    let result = connection.receive(&mut frames);
    for frame in frames {
        // The library panics on unknown channels, so a server naming 1 is cut off first.
        if usize::from(frame.channel) >= channel_count {
            return Err(ConnectionError::UnknownChannel(frame.channel));
        }
        messages.insert_received(frame.channel, frame.payload);
    }
    result
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "engine systems receive resource handles by value"
)]
fn send_packets(
    address: Res<ServerAddress>,
    mut link: ResMut<ServerLink>,
    mut diagnostic: ResMut<ConnectionDiagnostic>,
    mut messages: ResMut<ClientMessages>,
) {
    let ServerLink::Open(connection) = &mut *link else {
        // Nothing can be sent without a connection, so queued messages are dropped.
        messages.drain_sent().for_each(drop);
        return;
    };
    let mut result = Ok(());
    for (channel, payload) in messages.drain_sent() {
        if result.is_ok() {
            result = u8::try_from(channel)
                .map_err(|_| ConnectionError::ChannelOutOfRange(channel))
                .and_then(|channel| connection.queue(channel, &payload));
        }
    }
    if let Err(error) = result.and_then(|()| connection.flush()) {
        if matches!(error, ConnectionError::Closed) {
            if diagnostic.detail.is_none() {
                diagnostic.detail = Some(format!("server at {} closed the connection", address.0));
            }
        } else {
            let message = format!("connection to server at {} lost ({error})", address.0);
            warn!("{message}");
            diagnostic.detail = Some(message);
        }
        // The receive system moves the library's state to disconnected next update.
        *link = ServerLink::Closed;
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    use bevy::app::TaskPoolPlugin;
    use bevy::prelude::{Messages, On};
    use bevy_replicon::postcard_utils;
    use bevy_replicon::prelude::{ProtocolHash, ProtocolMismatch};
    use zeta_core::grid::GridPos;
    use zeta_core::protocol::MoveRequest;
    use zeta_core::wire::{Frame, FrameDecoder, MAX_PAYLOAD_BYTES, encode_frame};

    use super::super::ClientNetPlugin;
    use super::*;

    const DEADLINE: Duration = Duration::from_secs(5);

    // Client channels in registration order: the library's acknowledgements, its protocol
    // check, then the client messages of `register_protocol`.
    const PROTOCOL_HASH_CHANNEL: u8 = 1;
    const MOVE_REQUEST_CHANNEL: u8 = 2;
    // Server channels: entity updates, entity mutations, then the library's protocol mismatch.
    const PROTOCOL_MISMATCH_CHANNEL: u8 = 2;

    #[derive(Resource, Default)]
    struct MismatchCount(usize);

    fn client_app(server_address: SocketAddr) -> App {
        let mut app = App::new();
        app.add_plugins((
            TaskPoolPlugin::default(),
            ClientNetPlugin { server_address },
        ))
        .init_resource::<MismatchCount>()
        .add_observer(
            |_: On<ProtocolMismatch>, mut count: ResMut<MismatchCount>| {
                count.0 += 1;
            },
        );
        app.finish();
        app.cleanup();
        app
    }

    fn status(app: &App) -> ConnectionStatus {
        *app.world().resource::<ConnectionStatus>()
    }

    fn update_until(app: &mut App, deadline: Duration, mut done: impl FnMut(&mut App) -> bool) {
        let start = Instant::now();
        loop {
            app.update();
            if done(app) {
                return;
            }
            assert!(start.elapsed() < deadline, "condition never became true");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// A client app connected to a raw server socket.
    fn connected_client() -> (App, TcpStream) {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).expect("binds");
        let mut app = client_app(listener.local_addr().expect("has an address"));
        app.update();
        let (peer, _) = listener.accept().expect("accepts");
        update_until(&mut app, DEADLINE, |app| {
            status(app) == ConnectionStatus::Connected
        });
        (app, peer)
    }

    fn send_frame(peer: &mut TcpStream, channel: u8, payload: &[u8]) {
        let mut bytes = Vec::new();
        encode_frame(channel, payload, MAX_PAYLOAD_BYTES, &mut bytes).expect("fits");
        peer.write_all(&bytes).expect("writes");
    }

    fn postcard<T: serde::Serialize>(value: &T) -> Vec<u8> {
        let mut bytes = Vec::new();
        postcard_utils::to_extend_mut(value, &mut bytes).expect("serializes");
        bytes
    }

    fn read_frame_on(peer: &mut TcpStream, channel: u8) -> Frame {
        peer.set_read_timeout(Some(DEADLINE)).expect("sets timeout");
        let mut decoder = FrameDecoder::default();
        let mut chunk = [0; 4096];
        loop {
            while let Some(frame) = decoder.next_frame() {
                let frame = frame.expect("valid frame");
                if frame.channel == channel {
                    return frame;
                }
            }
            let count = peer
                .read(&mut chunk)
                .expect("client sends before the deadline");
            assert!(count > 0, "client closed the connection");
            decoder.push(&chunk[..count]);
        }
    }

    #[test]
    fn status_starts_as_connecting() {
        let app = client_app(SocketAddr::from(([127, 0, 0, 1], 9)));
        assert_eq!(status(&app), ConnectionStatus::Connecting);
    }

    #[test]
    fn a_failed_connect_reports_disconnected() {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).expect("binds");
        let address = listener.local_addr().expect("has an address");
        drop(listener);
        let mut app = client_app(address);
        // A refused connect can take a few seconds on some systems.
        update_until(&mut app, DEADLINE * 2, |app| {
            status(app) == ConnectionStatus::Disconnected
        });
        let state = app.world().resource::<State<ClientState>>();
        assert_eq!(**state, ClientState::Disconnected);
        let diagnostic = app.world().resource::<ConnectionDiagnostic>();
        assert_eq!(diagnostic.server_address, Some(address));
        assert!(
            diagnostic
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains(&address.to_string())),
            "expected failure diagnostic naming {address}, got {diagnostic:?}"
        );
    }

    #[test]
    #[ignore = "needs a loopback TCP connect, which the agent sandbox denies; run with --ignored"]
    fn connecting_reports_connected_and_sends_the_protocol_hash() {
        let (app, mut peer) = connected_client();
        let state = app.world().resource::<State<ClientState>>();
        assert_eq!(**state, ClientState::Connected);
        let hash = *app.world().resource::<ProtocolHash>();
        let frame = read_frame_on(&mut peer, PROTOCOL_HASH_CHANNEL);
        assert_eq!(frame.payload, postcard(&hash));
    }

    #[test]
    #[ignore = "needs a loopback TCP connect, which the agent sandbox denies; run with --ignored"]
    fn a_move_request_written_on_the_client_reaches_the_server() {
        let (mut app, mut peer) = connected_client();
        let request = MoveRequest {
            target: GridPos { x: 2, y: 5 },
        };
        app.world_mut()
            .resource_mut::<Messages<MoveRequest>>()
            .write(request);
        app.update();
        let frame = read_frame_on(&mut peer, MOVE_REQUEST_CHANNEL);
        assert_eq!(frame.payload, postcard(&request));
    }

    #[test]
    #[ignore = "needs a loopback TCP connect, which the agent sandbox denies; run with --ignored"]
    fn a_server_frame_reaches_the_library() {
        let (mut app, mut peer) = connected_client();
        send_frame(&mut peer, PROTOCOL_MISMATCH_CHANNEL, &[]);
        update_until(&mut app, DEADLINE, |app| {
            app.world().resource::<MismatchCount>().0 == 1
        });
    }

    #[test]
    #[ignore = "needs a loopback TCP connect, which the agent sandbox denies; run with --ignored"]
    fn the_server_closing_reports_disconnected() {
        let (mut app, peer) = connected_client();
        drop(peer);
        update_until(&mut app, DEADLINE, |app| {
            status(app) == ConnectionStatus::Disconnected
        });
        let state = app.world().resource::<State<ClientState>>();
        assert_eq!(**state, ClientState::Disconnected);
        let diagnostic = app.world().resource::<ConnectionDiagnostic>();
        assert!(
            diagnostic
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("closed")),
            "expected close diagnostic, got {diagnostic:?}"
        );
    }

    #[test]
    #[ignore = "needs a loopback TCP connect, which the agent sandbox denies; run with --ignored"]
    fn a_frame_on_an_unknown_channel_disconnects() {
        let (mut app, mut peer) = connected_client();
        send_frame(&mut peer, u8::MAX, b"junk");
        update_until(&mut app, DEADLINE, |app| {
            status(app) == ConnectionStatus::Disconnected
        });
    }
}
