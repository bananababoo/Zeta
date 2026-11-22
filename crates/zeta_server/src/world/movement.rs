//! Direct movement: a player's move request walks their character along a grid path.
//!
//! Rules: `docs/prototypes/realism_prototype.md` Section 2, the `PendingAiResolution` lock in
//! `docs/architecture/technical_architecture.md` Section 2, and the path rules in
//! `zeta_core::movement`.

use std::collections::BTreeSet;

use bevy_ecs::prelude::{MessageReader, MessageWriter, Query, Res, ResMut};
use zeta_core::entity::EntityId;
use zeta_core::grid::{GridPos, MapBounds};
use zeta_core::movement::{PathError, find_path};
use zeta_core::pending_resolution::ChangeKind;
use zeta_core::protocol::{Collider, MoveRequest, NarrationLogEntry, TurnBubble};

use super::turn_bubble::permission;
use super::{LogSequence, PlayerCharacters, notice};
use crate::bridge::ResolutionLocksResource;
use crate::net::messages::{FromPlayer, ToPlayers};
use crate::settings::MOVEMENT_SQUARES_PER_REQUEST;

/// Update system: moves each requesting player's character, or tells the player why not.
#[allow(
    clippy::too_many_arguments,
    clippy::needless_pass_by_value,
    reason = "a Bevy system receives each resource and query it uses as a parameter, by value"
)]
pub(crate) fn handle_move_requests(
    mut requests: MessageReader<FromPlayer<MoveRequest>>,
    characters: Res<PlayerCharacters>,
    locks: Res<ResolutionLocksResource>,
    bounds: Option<Res<MapBounds>>,
    mut positions: Query<(&EntityId, &mut GridPos, &Collider)>,
    mut bubbles: Query<&mut TurnBubble>,
    mut sequence: ResMut<LogSequence>,
    mut notices: MessageWriter<ToPlayers<NarrationLogEntry>>,
) {
    let Some(bounds) = bounds else {
        return;
    };
    for request in requests.read() {
        let connection = request.connection;
        let Some(&character) = characters.by_connection.get(&connection) else {
            continue;
        };
        if !locks.0.may_change(character, ChangeKind::Movement) {
            notices.write(notice(
                &mut sequence,
                connection,
                "Your character cannot move while an action involving them resolves.",
            ));
            continue;
        }
        let mut bubble = bubbles
            .iter_mut()
            .find(|bubble| bubble.order.contains(&character));
        let turn = permission(bubble.as_deref(), character, |bubble| bubble.movement_used);
        if let Some(refusal) = turn.refusal() {
            notices.write(notice(&mut sequence, connection, refusal));
            continue;
        }

        let blocked: BTreeSet<GridPos> = positions
            .iter()
            .filter(|(id, _, collider)| **id != character && !collider.passable)
            .map(|(_, position, _)| *position)
            .collect();
        let Some((_, mut position, _)) = positions.iter_mut().find(|(id, _, _)| **id == character)
        else {
            continue;
        };
        match find_path(
            *position,
            request.message.target,
            *bounds,
            MOVEMENT_SQUARES_PER_REQUEST,
            |square| blocked.contains(&square),
        ) {
            Ok(path) => {
                if let Some(&last) = path.last() {
                    *position = last;
                    if let Some(bubble) = bubble.as_mut() {
                        bubble.movement_used = true;
                    }
                }
            }
            Err(error) => {
                notices.write(notice(&mut sequence, connection, refusal_text(error)));
            }
        }
    }
}

/// What a player is told when no path exists.
fn refusal_text(error: PathError) -> String {
    match error {
        PathError::OutsideMap { .. } => "That square is outside the map.".into(),
        PathError::Blocked { .. } => "Something blocks that square.".into(),
        PathError::Unreachable { max_squares } => {
            format!("No path of at most {max_squares} squares reaches that square.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_name_the_reason() {
        assert!(refusal_text(PathError::Blocked { x: 1, y: 1 }).contains("blocks"));
        assert!(refusal_text(PathError::Unreachable { max_squares: 6 }).contains('6'));
    }
}
