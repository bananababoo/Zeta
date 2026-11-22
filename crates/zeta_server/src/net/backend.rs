//! Moves frames between the TCP transport and the networking library's message buffers, and
//! spawns and despawns 1 connection entity per player socket.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 1.

use std::collections::BTreeSet;
use std::net::SocketAddr;

use bevy_app::{App, AppExit, PostUpdate, PreUpdate, Startup};
use bevy_ecs::prelude::{
    Commands, Component, Entity, IntoScheduleConfigs, MessageReader, MessageWriter, Query, Res,
    ResMut, Resource,
};
use bevy_replicon::prelude::{
    ConnectedClient, DisconnectRequest, RepliconChannels, ServerMessages, ServerState,
    ServerSystems,
};
use bevy_state::state::NextState;

use super::transport::{Connection, ConnectionError, Listener};

/// Largest message the library should build before splitting component changes, in bytes.
/// About 1 internet packet, the size the library is designed around.
const SPLIT_MESSAGE_BYTES: usize = 1200;

/// Address requested by [`super::ServerNetPlugin`].
#[derive(Resource, Clone, Copy, Debug)]
struct ListenAddress(SocketAddr);

/// The bound listening socket, present once listening started.
#[derive(Resource, Debug)]
pub(super) struct ServerListener(pub(super) Listener);

/// The socket of 1 connected client, on its connection entity.
#[derive(Component, Debug)]
struct ClientSocket(Connection);

pub(super) fn build(app: &mut App, listen_address: SocketAddr) {
    app.insert_resource(ListenAddress(listen_address))
        .add_systems(Startup, start_listening)
        .add_systems(
            PreUpdate,
            receive_packets.in_set(ServerSystems::ReceivePackets),
        )
        .add_systems(PostUpdate, send_packets.in_set(ServerSystems::SendPackets));
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "engine systems receive resource handles by value"
)]
fn start_listening(
    mut commands: Commands,
    address: Res<ListenAddress>,
    mut server_state: ResMut<NextState<ServerState>>,
    mut exit: MessageWriter<AppExit>,
) {
    match Listener::bind(address.0) {
        Ok(listener) => {
            match listener.local_address() {
                Ok(bound) => println!("listening for players on {bound}"),
                Err(error) => eprintln!("listening, but the bound address is unknown: {error}"),
            }
            commands.insert_resource(ServerListener(listener));
            server_state.set(ServerState::Running);
        }
        Err(error) => {
            eprintln!("could not listen for players on {}: {error}", address.0);
            exit.write(AppExit::error());
        }
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "engine systems receive resource handles by value"
)]
fn receive_packets(
    mut commands: Commands,
    listener: Option<Res<ServerListener>>,
    channels: Res<RepliconChannels>,
    mut messages: ResMut<ServerMessages>,
    mut sockets: Query<(Entity, &mut ClientSocket)>,
) {
    let Some(listener) = listener else {
        return;
    };
    let (accepted, errors) = listener.0.accept_pending();
    for error in errors {
        eprintln!("could not accept a player connection: {error}");
    }
    for (connection, _) in accepted {
        commands.spawn((
            ConnectedClient {
                max_size: SPLIT_MESSAGE_BYTES,
            },
            ClientSocket(connection),
        ));
    }

    let channel_count = channels.client_channels().len();
    let mut frames = Vec::new();
    for (entity, mut socket) in &mut sockets {
        let mut result = socket.0.receive(&mut frames);
        for frame in frames.drain(..) {
            // The library panics on unknown channels, so a peer naming 1 is cut off first.
            if usize::from(frame.channel) >= channel_count {
                result = Err(ConnectionError::UnknownChannel(frame.channel));
                break;
            }
            messages.insert_received(entity, frame.channel, frame.payload);
        }
        match result {
            Ok(()) => {}
            Err(ConnectionError::Closed) => commands.entity(entity).despawn(),
            Err(error) => {
                eprintln!("dropping player connection {entity}: {error}");
                commands.entity(entity).despawn();
            }
        }
    }
}

fn send_packets(
    mut commands: Commands,
    mut messages: ResMut<ServerMessages>,
    mut sockets: Query<(Entity, &mut ClientSocket)>,
    mut disconnects: MessageReader<DisconnectRequest>,
) {
    let mut closing = BTreeSet::new();
    for (client, channel, payload) in messages.drain_sent() {
        let Ok((_, mut socket)) = sockets.get_mut(client) else {
            continue;
        };
        let queued = u8::try_from(channel)
            .map_err(|_| ConnectionError::ChannelOutOfRange(channel))
            .and_then(|channel| socket.0.queue(channel, &payload));
        if let Err(error) = queued {
            eprintln!("dropping player connection {client}: {error}");
            closing.insert(client);
        }
    }
    for (entity, mut socket) in &mut sockets {
        if let Err(error) = socket.0.flush() {
            if !matches!(error, ConnectionError::Closed) {
                eprintln!("dropping player connection {entity}: {error}");
            }
            closing.insert(entity);
        }
    }
    // Requested disconnects close after the flush above, so their last messages go out first.
    closing.extend(disconnects.read().map(|request| request.client));
    for entity in closing {
        if sockets.contains(entity) {
            commands.entity(entity).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::{Duration, Instant};

    use bevy_app::TaskPoolPlugin;
    use bevy_ecs::message::Messages;
    use bevy_ecs::prelude::With;
    use bevy_replicon::postcard_utils;
    use bevy_replicon::prelude::{AuthorizedClient, ProtocolHash};
    use zeta_core::entity::EntityId;
    use zeta_core::grid::GridPos;
    use zeta_core::protocol::{MoveRequest, PlayerAssigned};
    use zeta_core::wire::{Frame, FrameDecoder, MAX_PAYLOAD_BYTES, encode_frame};

    use super::super::ServerNetPlugin;
    use super::super::messages::{
        FromPlayer, Networked, PlayerConnected, PlayerConnection, PlayerDisconnected, Recipients,
        ToPlayers,
    };
    use super::*;

    const DEADLINE: Duration = Duration::from_secs(5);

    // Client channels in registration order: the library's acknowledgements, its protocol
    // check, then the client messages of `register_protocol`.
    const PROTOCOL_HASH_CHANNEL: u8 = 1;
    const MOVE_REQUEST_CHANNEL: u8 = 2;
    // Server channel 0 carries entity updates.
    const UPDATES_CHANNEL: u8 = 0;

    #[derive(Resource, Default)]
    struct Seen {
        connected: Vec<PlayerConnection>,
        disconnected: Vec<PlayerConnection>,
        moves: Vec<FromPlayer<MoveRequest>>,
    }

    fn record(
        mut seen: ResMut<Seen>,
        mut connected: MessageReader<PlayerConnected>,
        mut disconnected: MessageReader<PlayerDisconnected>,
        mut moves: MessageReader<FromPlayer<MoveRequest>>,
    ) {
        seen.connected
            .extend(connected.read().map(|message| message.connection));
        seen.disconnected
            .extend(disconnected.read().map(|message| message.connection));
        seen.moves.extend(moves.read().cloned());
    }

    fn server_app() -> (App, SocketAddr) {
        let mut app = App::new();
        app.add_plugins((
            TaskPoolPlugin::default(),
            ServerNetPlugin {
                listen_address: SocketAddr::from(([127, 0, 0, 1], 0)),
            },
        ))
        .init_resource::<Seen>()
        .add_systems(bevy_app::Update, record);
        app.finish();
        app.cleanup();
        app.update();
        let address = app
            .world()
            .resource::<ServerListener>()
            .0
            .local_address()
            .expect("bound");
        (app, address)
    }

    fn update_until(app: &mut App, mut done: impl FnMut(&mut App) -> bool) {
        let start = Instant::now();
        while !done(app) {
            assert!(start.elapsed() < DEADLINE, "condition never became true");
            app.update();
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn connected_clients(app: &mut App) -> Vec<Entity> {
        app.world_mut()
            .query_filtered::<Entity, With<ConnectedClient>>()
            .iter(app.world())
            .collect()
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

    /// Connects a raw peer and passes the library's protocol check, as a real client would.
    fn authorized_peer(app: &mut App, address: SocketAddr) -> (TcpStream, Entity) {
        let mut peer = TcpStream::connect(address).expect("connects");
        let hash = *app.world().resource::<ProtocolHash>();
        send_frame(&mut peer, PROTOCOL_HASH_CHANNEL, &postcard(&hash));
        update_until(app, |app| {
            !app.world().resource::<Seen>().connected.is_empty()
        });
        let PlayerConnection(entity) = app.world().resource::<Seen>().connected[0];
        (peer, entity)
    }

    fn read_frames_until(peer: &mut TcpStream, mut done: impl FnMut(&Frame) -> bool) {
        peer.set_read_timeout(Some(DEADLINE)).expect("sets timeout");
        let mut decoder = FrameDecoder::default();
        let mut chunk = [0; 4096];
        loop {
            while let Some(frame) = decoder.next_frame() {
                if done(&frame.expect("valid frame")) {
                    return;
                }
            }
            let count = peer
                .read(&mut chunk)
                .expect("server sends before the deadline");
            assert!(count > 0, "server closed the connection");
            decoder.push(&chunk[..count]);
        }
    }

    #[test]
    fn server_reaches_running_state_after_binding() {
        let (app, _) = server_app();
        let state = app
            .world()
            .resource::<bevy_state::state::State<ServerState>>();
        assert_eq!(*state.get(), ServerState::Running);
    }

    #[test]
    #[ignore = "needs a loopback TCP connect, which the agent sandbox denies; run with --ignored"]
    fn raw_peer_gets_a_connection_entity_that_despawns_when_it_closes() {
        let (mut app, address) = server_app();
        let peer = TcpStream::connect(address).expect("connects");
        update_until(&mut app, |app| connected_clients(app).len() == 1);
        drop(peer);
        update_until(&mut app, |app| connected_clients(app).is_empty());
        // It never passed the protocol check, so it was never a player.
        assert_eq!(app.world().resource::<Seen>().connected, Vec::new());
        assert_eq!(app.world().resource::<Seen>().disconnected, Vec::new());
    }

    #[test]
    #[ignore = "needs a loopback TCP connect, which the agent sandbox denies; run with --ignored"]
    fn protocol_check_makes_a_player_and_closing_announces_its_departure() {
        let (mut app, address) = server_app();
        let (peer, entity) = authorized_peer(&mut app, address);
        assert!(app.world().get::<AuthorizedClient>(entity).is_some());
        drop(peer);
        update_until(&mut app, |app| {
            !app.world().resource::<Seen>().disconnected.is_empty()
        });
        assert_eq!(
            app.world().resource::<Seen>().disconnected,
            [PlayerConnection(entity)]
        );
    }

    #[test]
    #[ignore = "needs a loopback TCP connect, which the agent sandbox denies; run with --ignored"]
    fn a_players_move_request_arrives_as_from_player() {
        let (mut app, address) = server_app();
        let (mut peer, entity) = authorized_peer(&mut app, address);
        let request = MoveRequest {
            target: GridPos { x: 3, y: 4 },
        };
        send_frame(&mut peer, MOVE_REQUEST_CHANNEL, &postcard(&request));
        update_until(&mut app, |app| {
            !app.world().resource::<Seen>().moves.is_empty()
        });
        assert_eq!(
            app.world().resource::<Seen>().moves,
            [FromPlayer {
                connection: PlayerConnection(entity),
                message: request,
            }]
        );
    }

    #[test]
    #[ignore = "needs a loopback TCP connect, which the agent sandbox denies; run with --ignored"]
    fn messages_from_a_peer_before_the_protocol_check_are_ignored() {
        let (mut app, address) = server_app();
        let mut peer = TcpStream::connect(address).expect("connects");
        let request = MoveRequest {
            target: GridPos { x: 1, y: 1 },
        };
        send_frame(&mut peer, MOVE_REQUEST_CHANNEL, &postcard(&request));
        update_until(&mut app, |app| connected_clients(app).len() == 1);
        for _ in 0..10 {
            app.update();
        }
        assert_eq!(app.world().resource::<Seen>().moves, Vec::new());
    }

    #[test]
    #[ignore = "needs a loopback TCP connect, which the agent sandbox denies; run with --ignored"]
    fn a_message_for_1_player_reaches_that_peer() {
        let (mut app, address) = server_app();
        let (mut peer, entity) = authorized_peer(&mut app, address);
        let assigned = PlayerAssigned {
            character: EntityId(42),
        };
        app.world_mut()
            .resource_mut::<Messages<ToPlayers<PlayerAssigned>>>()
            .write(ToPlayers {
                recipients: Recipients::Only(PlayerConnection(entity)),
                message: assigned,
            });
        app.update();
        let expected = postcard(&assigned);
        read_frames_until(&mut peer, |frame| frame.payload.ends_with(&expected));
    }

    #[test]
    #[ignore = "needs a loopback TCP connect, which the agent sandbox denies; run with --ignored"]
    fn networked_entities_replicate_to_players() {
        let (mut app, address) = server_app();
        let (mut peer, _) = authorized_peer(&mut app, address);
        app.world_mut().spawn((Networked, EntityId(7)));
        app.update();
        read_frames_until(&mut peer, |frame| frame.channel == UPDATES_CHANNEL);
    }

    #[test]
    #[ignore = "needs a loopback TCP connect, which the agent sandbox denies; run with --ignored"]
    fn a_frame_on_an_unknown_channel_drops_the_connection() {
        let (mut app, address) = server_app();
        let mut peer = TcpStream::connect(address).expect("connects");
        update_until(&mut app, |app| connected_clients(app).len() == 1);
        send_frame(&mut peer, u8::MAX, b"junk");
        update_until(&mut app, |app| connected_clients(app).is_empty());
    }

    #[test]
    fn binding_a_taken_address_exits_with_an_error() {
        let (_running, address) = server_app();
        let mut app = App::new();
        app.add_plugins((
            TaskPoolPlugin::default(),
            ServerNetPlugin {
                listen_address: address,
            },
        ));
        app.finish();
        app.cleanup();
        app.update();
        assert!(app.should_exit().is_some_and(|exit| exit.is_error()));
    }
}
