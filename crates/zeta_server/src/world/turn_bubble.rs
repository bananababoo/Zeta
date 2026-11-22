//! Turn Bubble rules: fixed turn order, 1 movement and 1 Freeform Action Prompt per turn,
//! and the AI DM acting for non-player characters on their turns.
//!
//! Rules: `docs/prototypes/realism_prototype.md` Section 2. Area Clocks
//! (`docs/architecture/technical_architecture.md` Section 4) are outside the prototype, so
//! `completed_rounds` is only counted here.

use bevy_ecs::prelude::{Entity, MessageReader, MessageWriter, Query, Res, ResMut, With, World};
use zeta_core::entity::EntityId;
use zeta_core::protocol::{EndTurnRequest, NarrationLogEntry, PlayerCharacter, TurnBubble};

use super::{EntityIndex, LogSequence, PlayerCharacters, notice};
use crate::net::messages::{FromPlayer, Networked, ToPlayers};

/// Whether a participant may take a turn-limited action now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TurnPermission {
    /// The entity is in no Turn Bubble, so real-time rules apply.
    Free,
    /// It is the entity's turn and the action is still unused.
    Allowed,
    /// Another participant's turn is running.
    NotYourTurn,
    /// The entity already used this action during its turn.
    AlreadyUsed,
}

impl TurnPermission {
    /// The notice shown to a player refused by this permission, if refused.
    pub(crate) const fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Free | Self::Allowed => None,
            Self::NotYourTurn => Some("Wait for your turn in the Turn Bubble."),
            Self::AlreadyUsed => Some("You already used that this turn."),
        }
    }
}

/// The participant whose turn it is.
pub(crate) fn current_participant(bubble: &TurnBubble) -> Option<EntityId> {
    bubble
        .order
        .get(usize::try_from(bubble.current).ok()?)
        .copied()
}

/// Whether `participant` may take the action whose use `used` reports.
pub(crate) fn permission(
    bubble: Option<&TurnBubble>,
    participant: EntityId,
    used: impl Fn(&TurnBubble) -> bool,
) -> TurnPermission {
    match bubble {
        None => TurnPermission::Free,
        Some(bubble) if current_participant(bubble) != Some(participant) => {
            TurnPermission::NotYourTurn
        }
        Some(bubble) if used(bubble) => TurnPermission::AlreadyUsed,
        Some(_) => TurnPermission::Allowed,
    }
}

/// Passes the turn to the next participant, counting a round when the order wraps.
pub(crate) fn advance(bubble: &mut TurnBubble) {
    let next = bubble.current.saturating_add(1);
    if usize::try_from(next).map_or(true, |next| next >= bubble.order.len()) {
        bubble.current = 0;
        bubble.completed_rounds = bubble.completed_rounds.saturating_add(1);
    } else {
        bubble.current = next;
    }
    bubble.movement_used = false;
    bubble.prompt_used = false;
}

/// Adds participants at the end of the order, skipping any already in it.
pub(crate) fn add_participants(bubble: &mut TurnBubble, participants: &[EntityId]) {
    for &participant in participants {
        if !bubble.order.contains(&participant) {
            bubble.order.push(participant);
        }
    }
}

/// Removes a participant. If it was their turn, the next participant's turn starts.
pub(crate) fn remove_participant(bubble: &mut TurnBubble, participant: EntityId) {
    let Some(removed) = bubble.order.iter().position(|&id| id == participant) else {
        return;
    };
    bubble.order.remove(removed);
    let current = usize::try_from(bubble.current).unwrap_or(usize::MAX);
    if removed < current {
        bubble.current = bubble.current.saturating_sub(1);
    } else if removed == current {
        bubble.movement_used = false;
        bubble.prompt_used = false;
        if current >= bubble.order.len() {
            bubble.current = 0;
            bubble.completed_rounds = bubble.completed_rounds.saturating_add(1);
        }
    }
}

/// A new Turn Bubble with `participants` in order, the first acting first.
pub(crate) fn new_bubble(participants: &[EntityId]) -> TurnBubble {
    let mut bubble = TurnBubble {
        order: Vec::new(),
        current: 0,
        completed_rounds: 0,
        movement_used: false,
        prompt_used: false,
    };
    add_participants(&mut bubble, participants);
    bubble
}

/// Starts a Turn Bubble with `participants`, or adds them to the bubble already holding any
/// of them.
pub(crate) fn start_or_extend(world: &mut World, participants: &[EntityId]) {
    if participants.is_empty() {
        return;
    }
    let mut bubbles = world.query::<&mut TurnBubble>();
    if let Some(mut bubble) = bubbles
        .iter_mut(world)
        .find(|bubble| participants.iter().any(|id| bubble.order.contains(id)))
    {
        add_participants(&mut bubble, participants);
        return;
    }
    world.spawn((new_bubble(participants), Networked));
}

/// Ends every Turn Bubble holding any of `participants`, or every bubble when empty.
pub(crate) fn end_bubbles(world: &mut World, participants: &[EntityId]) {
    let mut bubbles = world.query::<(Entity, &TurnBubble)>();
    let ended: Vec<Entity> = bubbles
        .iter(world)
        .filter(|(_, bubble)| {
            participants.is_empty() || participants.iter().any(|id| bubble.order.contains(id))
        })
        .map(|(entity, _)| entity)
        .collect();
    for entity in ended {
        world.despawn(entity);
    }
}

/// Removes a participant from every Turn Bubble.
pub(crate) fn remove_everywhere(world: &mut World, participant: EntityId) {
    let mut bubbles = world.query::<&mut TurnBubble>();
    for mut bubble in bubbles.iter_mut(world) {
        if bubble.order.contains(&participant) {
            remove_participant(&mut bubble, participant);
        }
    }
}

/// The Turn Bubble holding `participant`, if any.
pub(crate) fn bubble_of(world: &mut World, participant: EntityId) -> Option<TurnBubble> {
    let mut bubbles = world.query::<&TurnBubble>();
    bubbles
        .iter(world)
        .find(|bubble| bubble.order.contains(&participant))
        .cloned()
}

/// Applies `change` to the Turn Bubble holding `participant`, if any.
pub(crate) fn update_bubble_of(
    world: &mut World,
    participant: EntityId,
    change: impl FnOnce(&mut TurnBubble),
) {
    let mut bubbles = world.query::<&mut TurnBubble>();
    if let Some(mut bubble) = bubbles
        .iter_mut(world)
        .find(|bubble| bubble.order.contains(&participant))
    {
        change(&mut bubble);
    }
}

/// Ends Turn Bubbles with no player character left, and returns the non-player characters
/// whose turn it is in the rest.
pub(crate) fn tidy_and_list_npc_turns(world: &mut World) -> Vec<EntityId> {
    let mut player_characters = world.query_filtered::<&EntityId, With<PlayerCharacter>>();
    let players: Vec<EntityId> = player_characters.iter(world).copied().collect();
    let index: Vec<EntityId> = world.resource::<EntityIndex>().0.keys().copied().collect();

    let mut bubbles = world.query::<(Entity, &mut TurnBubble)>();
    let mut ended = Vec::new();
    let mut npc_turns = Vec::new();
    for (entity, mut bubble) in bubbles.iter_mut(world) {
        let missing: Vec<EntityId> = bubble
            .order
            .iter()
            .copied()
            .filter(|id| !index.contains(id))
            .collect();
        for id in missing {
            remove_participant(&mut bubble, id);
        }
        if !bubble.order.iter().any(|id| players.contains(id)) {
            ended.push(entity);
        } else if let Some(current) = current_participant(&bubble)
            && !players.contains(&current)
        {
            npc_turns.push(current);
        }
    }
    for entity in ended {
        world.despawn(entity);
    }
    npc_turns
}

/// Update system: a player ending their turn passes it to the next participant.
#[allow(
    clippy::needless_pass_by_value,
    reason = "a Bevy system receives each resource it uses as a parameter, by value"
)]
pub(crate) fn handle_end_turn_requests(
    mut requests: MessageReader<FromPlayer<EndTurnRequest>>,
    characters: Res<PlayerCharacters>,
    mut bubbles: Query<&mut TurnBubble>,
    mut sequence: ResMut<LogSequence>,
    mut notices: MessageWriter<ToPlayers<NarrationLogEntry>>,
) {
    for request in requests.read() {
        let Some(&character) = characters.by_connection.get(&request.connection) else {
            continue;
        };
        let Some(mut bubble) = bubbles
            .iter_mut()
            .find(|bubble| bubble.order.contains(&character))
        else {
            notices.write(notice(
                &mut sequence,
                request.connection,
                "You are not in a Turn Bubble.",
            ));
            continue;
        };
        if current_participant(&bubble) == Some(character) {
            advance(&mut bubble);
        } else {
            notices.write(notice(
                &mut sequence,
                request.connection,
                "It is not your turn.",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(values: &[u64]) -> Vec<EntityId> {
        values.iter().map(|&value| EntityId(value)).collect()
    }

    #[test]
    fn advancing_past_the_last_participant_counts_a_round_and_resets_the_turn() {
        let mut bubble = new_bubble(&ids(&[1, 2]));
        bubble.movement_used = true;
        advance(&mut bubble);
        assert_eq!(current_participant(&bubble), Some(EntityId(2)));
        assert!(!bubble.movement_used);
        advance(&mut bubble);
        assert_eq!(current_participant(&bubble), Some(EntityId(1)));
        assert_eq!(bubble.completed_rounds, 1);
    }

    #[test]
    fn only_the_current_participant_may_act_once() {
        let mut bubble = new_bubble(&ids(&[1, 2]));
        let moved = |bubble: &TurnBubble| bubble.movement_used;
        assert_eq!(
            permission(Some(&bubble), EntityId(2), moved),
            TurnPermission::NotYourTurn
        );
        assert_eq!(
            permission(Some(&bubble), EntityId(1), moved),
            TurnPermission::Allowed
        );
        bubble.movement_used = true;
        assert_eq!(
            permission(Some(&bubble), EntityId(1), moved),
            TurnPermission::AlreadyUsed
        );
        assert_eq!(permission(None, EntityId(1), moved), TurnPermission::Free);
    }

    #[test]
    fn removing_participants_keeps_the_right_turn() {
        let mut bubble = new_bubble(&ids(&[1, 2, 3]));
        advance(&mut bubble);
        remove_participant(&mut bubble, EntityId(1));
        assert_eq!(current_participant(&bubble), Some(EntityId(2)));
        remove_participant(&mut bubble, EntityId(2));
        assert_eq!(current_participant(&bubble), Some(EntityId(3)));
        remove_participant(&mut bubble, EntityId(3));
        assert_eq!(current_participant(&bubble), None);
    }

    #[test]
    fn participants_join_once() {
        let mut bubble = new_bubble(&ids(&[1, 2, 1]));
        add_participants(&mut bubble, &ids(&[2, 3]));
        assert_eq!(bubble.order, ids(&[1, 2, 3]));
    }
}
