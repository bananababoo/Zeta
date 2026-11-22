//! Messages between the server world and the network layer, so the world never imports the
//! networking library.

use bevy_app::{App, Plugin};
use bevy_ecs::prelude::{Component, Entity, Message};
use zeta_core::protocol::{
    DiceRollShown, EndTurnRequest, FreeformActionPrompt, MoveRequest, NarrationLogEntry,
    PlayerAssigned, PromptStatusUpdate,
};

/// 1 player's connection: the engine entity the network layer spawned for it. The server
/// world may attach its own components to that entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlayerConnection(pub Entity);

/// A player finished connecting and can receive messages.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayerConnected {
    /// The new connection.
    pub connection: PlayerConnection,
}

/// A player disconnected.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayerDisconnected {
    /// The closed connection.
    pub connection: PlayerConnection,
}

/// A message a player sent.
#[derive(Message, Clone, Debug, PartialEq, Eq)]
pub struct FromPlayer<M: Send + Sync + 'static> {
    /// Who sent it.
    pub connection: PlayerConnection,
    /// What was sent.
    pub message: M,
}

/// Who receives a server message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recipients {
    /// Every connected player.
    All,
    /// 1 player.
    Only(PlayerConnection),
}

/// A message for players.
#[derive(Message, Clone, Debug, PartialEq, Eq)]
pub struct ToPlayers<M: Send + Sync + 'static> {
    /// Who receives it.
    pub recipients: Recipients,
    /// What is sent.
    pub message: M,
}

/// Marks a server entity for replication to players.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Networked;

/// Registers the messages above without the networking library, for the server world's tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct NetMessagesPlugin;

impl Plugin for NetMessagesPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<PlayerConnected>()
            .add_message::<PlayerDisconnected>()
            .add_message::<FromPlayer<MoveRequest>>()
            .add_message::<FromPlayer<FreeformActionPrompt>>()
            .add_message::<FromPlayer<EndTurnRequest>>()
            .add_message::<ToPlayers<PlayerAssigned>>()
            .add_message::<ToPlayers<NarrationLogEntry>>()
            .add_message::<ToPlayers<DiceRollShown>>()
            .add_message::<ToPlayers<PromptStatusUpdate>>();
    }
}
