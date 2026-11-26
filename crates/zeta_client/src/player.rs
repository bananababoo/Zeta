//! The character this client controls, as assigned by the server.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 5. The server sends
//! `PlayerAssigned` once the player connects; the character entity may replicate before or
//! after that message, so plugins look the character up by `EntityId` every time.

use bevy::prelude::*;
use zeta_core::entity::EntityId;
use zeta_core::protocol::PlayerAssigned;

/// The character this client controls, or `None` before the server assigns one.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalPlayer {
    /// The controlled character.
    pub character: Option<EntityId>,
}

impl LocalPlayer {
    /// Whether `id` is the controlled character.
    pub fn is(&self, id: EntityId) -> bool {
        self.character == Some(id)
    }
}

/// System set in which [`LocalPlayer`] is updated from `PlayerAssigned` messages.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalPlayerUpdate;

/// Keeps [`LocalPlayer`] up to date from `PlayerAssigned` messages.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalPlayerPlugin;

impl Plugin for LocalPlayerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LocalPlayer>()
            .add_systems(Update, assign_local_player.in_set(LocalPlayerUpdate));
    }
}

/// Takes the latest assignment, so a reassignment replaces the previous character.
fn assign_local_player(
    mut assignments: MessageReader<PlayerAssigned>,
    mut local_player: ResMut<LocalPlayer>,
) {
    if let Some(assignment) = assignments.read().last() {
        local_player.character = Some(assignment.character);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<PlayerAssigned>()
            .add_plugins(LocalPlayerPlugin);
        app
    }

    #[test]
    fn local_player_is_unassigned_until_the_server_assigns_a_character() {
        let mut app = test_app();
        app.update();
        assert_eq!(app.world().resource::<LocalPlayer>().character, None);
    }

    #[test]
    fn player_assigned_sets_the_local_character() {
        let mut app = test_app();
        app.world_mut().write_message(PlayerAssigned {
            character: EntityId(7),
        });
        app.update();
        let local_player = app.world().resource::<LocalPlayer>();
        assert_eq!(local_player.character, Some(EntityId(7)));
        assert!(local_player.is(EntityId(7)));
        assert!(!local_player.is(EntityId(8)));
    }

    #[test]
    fn a_later_assignment_replaces_the_earlier_character() {
        let mut app = test_app();
        app.world_mut().write_message(PlayerAssigned {
            character: EntityId(1),
        });
        app.world_mut().write_message(PlayerAssigned {
            character: EntityId(2),
        });
        app.update();
        assert_eq!(
            app.world().resource::<LocalPlayer>().character,
            Some(EntityId(2))
        );
    }
}
