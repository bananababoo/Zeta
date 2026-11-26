//! Player characters: 1 spawned for each connecting player and removed when they leave.
//!
//! Rules: `docs/prototypes/realism_prototype.md` Section 1 (2 or more players share the
//! test scene) and `docs/architecture/technical_architecture.md` Section 5.

use std::collections::BTreeSet;

use bevy_ecs::prelude::{Commands, MessageReader, MessageWriter, Query, Res, ResMut};
use zeta_core::grid::{GridPos, MapBounds};
use zeta_core::protocol::{Collider, Creature, PlayerAssigned, PlayerCharacter, TurnBubble};

use super::inventory::starting_inventory;
use super::prompts::PromptRegistry;
use super::scene::{EntitySpec, PLAYER_SPAWN_POSITION, PLAYER_STARTING_ITEMS};
use super::turn_bubble::remove_participant;
use super::{EntityIndex, NextEntityId, PlayerCharacters};
use crate::net::messages::{PlayerConnected, PlayerDisconnected, Recipients, ToPlayers};

/// Update system: spawns a character for each new player and tells that player which one.
#[allow(
    clippy::too_many_arguments,
    reason = "a Bevy system receives each resource and query it uses as a parameter"
)]
pub(crate) fn connect_players(
    mut connections: MessageReader<PlayerConnected>,
    mut commands: Commands,
    mut characters: ResMut<PlayerCharacters>,
    mut index: ResMut<EntityIndex>,
    mut next_id: ResMut<NextEntityId>,
    bounds: Option<Res<MapBounds>>,
    occupants: Query<(&GridPos, &Collider)>,
    mut assignments: MessageWriter<ToPlayers<PlayerAssigned>>,
) {
    let Some(bounds) = bounds else {
        return;
    };
    let mut taken: BTreeSet<GridPos> = occupants
        .iter()
        .filter(|(_, collider)| !collider.passable)
        .map(|(position, _)| *position)
        .collect();
    for connected in connections.read() {
        let connection = connected.connection;
        if let Some(&character) = characters.by_connection.get(&connection) {
            assignments.write(ToPlayers {
                recipients: Recipients::Only(connection),
                message: PlayerAssigned { character },
            });
            continue;
        }
        let Some(position) = free_square_near(PLAYER_SPAWN_POSITION, *bounds, &taken) else {
            eprintln!("no free square is left for a new player character");
            continue;
        };
        taken.insert(position);
        characters.joined = characters.joined.saturating_add(1);
        let id = next_id.allocate();
        let spec = EntitySpec {
            name: format!("Player {}", characters.joined),
            position,
            visual: "An adventurer controlled by a player.".into(),
            tags: Vec::new(),
            passable: false,
        };
        let entity = commands
            .spawn((
                spec.into_bundle(id),
                Creature,
                PlayerCharacter,
                starting_inventory(PLAYER_STARTING_ITEMS),
            ))
            .id();
        index.0.insert(id, entity);
        characters.by_connection.insert(connection, id);
        assignments.write(ToPlayers {
            recipients: Recipients::Only(connection),
            message: PlayerAssigned { character: id },
        });
    }
}

/// Update system: removes a departing player's character from the world and Turn Bubbles.
pub(crate) fn disconnect_players(
    mut disconnections: MessageReader<PlayerDisconnected>,
    mut commands: Commands,
    mut characters: ResMut<PlayerCharacters>,
    mut index: ResMut<EntityIndex>,
    mut bubbles: Query<&mut TurnBubble>,
    mut prompts: ResMut<PromptRegistry>,
) {
    for disconnected in disconnections.read() {
        let connection = disconnected.connection;
        prompts.forget_connection(connection);
        let Some(character) = characters.by_connection.remove(&connection) else {
            continue;
        };
        for mut bubble in &mut bubbles {
            remove_participant(&mut bubble, character);
        }
        if let Some(entity) = index.0.remove(&character) {
            commands.entity(entity).despawn();
        }
    }
}

/// The free square nearest `origin`, ties broken by row and then column.
fn free_square_near(
    origin: GridPos,
    bounds: MapBounds,
    taken: &BTreeSet<GridPos>,
) -> Option<GridPos> {
    (0..bounds.height)
        .flat_map(|y| (0..bounds.width).map(move |x| GridPos::new(x, y)))
        .filter(|square| !taken.contains(square))
        .min_by_key(|square| (origin.grid_distance(*square), square.y, square.x))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: MapBounds = MapBounds {
        width: 5,
        height: 5,
    };

    #[test]
    fn the_spawn_square_is_used_when_free() {
        let origin = GridPos::new(2, 2);
        assert_eq!(
            free_square_near(origin, BOUNDS, &BTreeSet::new()),
            Some(origin)
        );
    }

    #[test]
    fn a_taken_spawn_square_gives_the_nearest_free_one() {
        let origin = GridPos::new(2, 2);
        let taken = BTreeSet::from([origin, GridPos::new(1, 1)]);
        assert_eq!(
            free_square_near(origin, BOUNDS, &taken),
            Some(GridPos::new(2, 1))
        );
    }

    #[test]
    fn a_full_map_has_no_free_square() {
        let taken: BTreeSet<GridPos> = (0..5)
            .flat_map(|y| (0..5).map(move |x| GridPos::new(x, y)))
            .collect();
        assert_eq!(free_square_near(GridPos::new(0, 0), BOUNDS, &taken), None);
    }
}
