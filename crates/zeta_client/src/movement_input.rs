//! WASD (tap or hold-to-walk) and click movement, sent to the server as move requests.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 5 and
//! `docs/prototypes/realism_prototype.md`. The server checks every request; this plugin
//! only picks the target square.

use bevy::prelude::*;
use bevy::ui::UiGlobalTransform;
use bevy::window::{PrimaryWindow, RequestRedraw};
use zeta_core::entity::EntityId;
use zeta_core::grid::{GridPos, MapBounds};
use zeta_core::protocol::{MapArea, MoveRequest};

use crate::focus::{
    TextEntryActive, TextEntryUpdate, UiClickConsumed, UiClickUpdate, UiPanel,
    clear_ui_click_consumed, cursor_is_over_any,
};
use crate::layout::{FloatingOrigin, square_at_origin};
use crate::player::LocalPlayer;
use crate::settings::{ClientSettings, MovementKeys};

/// A step to 1 of the 4 adjacent squares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Toward row 0.
    North,
    /// Toward column 0.
    West,
    /// Toward higher rows.
    South,
    /// Toward higher columns.
    East,
}

/// Tracks continuous hold-to-walk repeat timing for movement keys.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
struct MoveRepeatState {
    held: Option<Direction>,
    cooldown_seconds: f32,
}

/// Sends `MoveRequest` from movement keys (tap or hold) and left clicks on the map.
#[derive(Clone, Copy, Debug, Default)]
pub struct MovementInputPlugin;

impl Plugin for MovementInputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ClientSettings>()
            .init_resource::<LocalPlayer>()
            .init_resource::<FloatingOrigin>()
            .init_resource::<TextEntryActive>()
            .init_resource::<UiClickConsumed>()
            .init_resource::<MoveRepeatState>()
            .add_message::<RequestRedraw>()
            .add_systems(First, clear_ui_click_consumed)
            .add_systems(
                Update,
                (
                    keyboard_movement.after(TextEntryUpdate),
                    click_movement.after(UiClickUpdate),
                ),
            );
    }
}

/// The adjacent square in `direction`, or `None` when it leaves the map. Without known
/// bounds only the north and west edges are checked; the server checks the rest.
pub fn step_target(
    position: GridPos,
    direction: Direction,
    bounds: Option<MapBounds>,
) -> Option<GridPos> {
    let target = match direction {
        Direction::North => GridPos::new(position.x, position.y.checked_sub(1)?),
        Direction::West => GridPos::new(position.x.checked_sub(1)?, position.y),
        Direction::South => GridPos::new(position.x, position.y.checked_add(1)?),
        Direction::East => GridPos::new(position.x.checked_add(1)?, position.y),
    };
    match bounds {
        Some(bounds) if !bounds.contains(target.x, target.y) => None,
        _ => Some(target),
    }
}

fn direction_key(direction: Direction, bindings: MovementKeys) -> KeyCode {
    match direction {
        Direction::North => bindings.north,
        Direction::West => bindings.west,
        Direction::South => bindings.south,
        Direction::East => bindings.east,
    }
}

/// The direction of the first movement key pressed this frame.
fn just_pressed_direction(
    keys: &ButtonInput<KeyCode>,
    bindings: MovementKeys,
) -> Option<Direction> {
    [
        (bindings.north, Direction::North),
        (bindings.west, Direction::West),
        (bindings.south, Direction::South),
        (bindings.east, Direction::East),
    ]
    .into_iter()
    .find_map(|(key, direction)| keys.just_pressed(key).then_some(direction))
}

/// The active held movement direction, preferring `preferred` if still pressed.
fn held_direction(
    keys: &ButtonInput<KeyCode>,
    bindings: MovementKeys,
    preferred: Option<Direction>,
) -> Option<Direction> {
    // `ButtonInput::pressed`: bevy_input-0.20.1/src/button_input.rs:157.
    if let Some(dir) = preferred
        && keys.pressed(direction_key(dir, bindings))
    {
        return Some(dir);
    }
    [
        (bindings.north, Direction::North),
        (bindings.west, Direction::West),
        (bindings.south, Direction::South),
        (bindings.east, Direction::East),
    ]
    .into_iter()
    .find_map(|(key, direction)| keys.pressed(key).then_some(direction))
}

#[expect(
    clippy::too_many_arguments,
    reason = "a Bevy system lists every resource and query it reads as a parameter"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn keyboard_movement(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    settings: Res<ClientSettings>,
    text_entry: Res<TextEntryActive>,
    local_player: Res<LocalPlayer>,
    mut repeat: ResMut<MoveRepeatState>,
    characters: Query<(&EntityId, &GridPos)>,
    areas: Query<&MapArea>,
    mut requests: MessageWriter<MoveRequest>,
    mut redraws: MessageWriter<RequestRedraw>,
) {
    if text_entry.0 {
        repeat.held = None;
        repeat.cooldown_seconds = 0.0;
        return;
    }

    let step_direction =
        if let Some(direction) = just_pressed_direction(&keys, settings.movement_keys) {
            repeat.held = Some(direction);
            repeat.cooldown_seconds = settings.move_hold_initial_delay_seconds;
            redraws.write(RequestRedraw);
            Some(direction)
        } else if let Some(direction) = held_direction(&keys, settings.movement_keys, repeat.held) {
            redraws.write(RequestRedraw);
            if repeat.held == Some(direction) {
                // `Time::delta_secs`: bevy_time-0.20.1/src/time.rs:283.
                repeat.cooldown_seconds -= time.delta_secs();
                if repeat.cooldown_seconds <= 0.0 {
                    repeat.cooldown_seconds = settings.move_hold_repeat_interval_seconds;
                    Some(direction)
                } else {
                    None
                }
            } else {
                repeat.held = Some(direction);
                repeat.cooldown_seconds = settings.move_hold_initial_delay_seconds;
                None
            }
        } else {
            repeat.held = None;
            repeat.cooldown_seconds = 0.0;
            None
        };

    let Some(direction) = step_direction else {
        return;
    };
    let Some((_, position)) = characters.iter().find(|(id, _)| local_player.is(**id)) else {
        return;
    };
    let bounds = areas.iter().next().map(|area| area.0);
    if let Some(target) = step_target(*position, direction, bounds) {
        requests.write(MoveRequest { target });
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "a Bevy system lists every resource and query it reads as a parameter"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn click_movement(
    buttons: Res<ButtonInput<MouseButton>>,
    settings: Res<ClientSettings>,
    origin: Res<FloatingOrigin>,
    text_entry: Res<TextEntryActive>,
    click_consumed: Res<UiClickConsumed>,
    local_player: Res<LocalPlayer>,
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<(&Camera, &GlobalTransform), With<Camera2d>>,
    panels: Query<(&ComputedNode, &UiGlobalTransform), With<UiPanel>>,
    areas: Query<&MapArea>,
    mut requests: MessageWriter<MoveRequest>,
) {
    if text_entry.0
        || click_consumed.0
        || local_player.character.is_none()
        || !buttons.just_pressed(MouseButton::Left)
    {
        return;
    }
    if cursor_is_over_any(&window, panels.iter()) {
        return;
    }
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let (camera, camera_transform) = *camera;
    let Ok(world) = camera.viewport_to_world_2d(camera_transform, cursor) else {
        return;
    };
    let Some(area) = areas.iter().next() else {
        return;
    };
    if let Some(target) = square_at_origin(world, area.0, settings.tile_size, *origin) {
        requests.write(MoveRequest { target });
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::time::TimeUpdateStrategy;

    use super::*;

    const BOUNDS: MapBounds = MapBounds {
        width: 5,
        height: 5,
    };

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<MoveRequest>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
                100,
            )))
            .add_plugins(MovementInputPlugin);
        app.world_mut().spawn(MapArea(BOUNDS));
        app.world_mut().spawn((EntityId(1), GridPos::new(2, 2)));
        app.world_mut().spawn((EntityId(2), GridPos::new(0, 0)));
        app.world_mut().resource_mut::<LocalPlayer>().character = Some(EntityId(1));
        app
    }

    fn press(app: &mut App, key: KeyCode) {
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(key);
        app.update();
    }

    fn sent(app: &App) -> Vec<MoveRequest> {
        let messages = app.world().resource::<Messages<MoveRequest>>();
        messages.iter_current_update_messages().copied().collect()
    }

    #[test]
    fn step_target_moves_to_the_adjacent_square() {
        let center = GridPos::new(2, 2);
        assert_eq!(
            step_target(center, Direction::North, Some(BOUNDS)),
            Some(GridPos::new(2, 1))
        );
        assert_eq!(
            step_target(center, Direction::West, Some(BOUNDS)),
            Some(GridPos::new(1, 2))
        );
        assert_eq!(
            step_target(center, Direction::South, Some(BOUNDS)),
            Some(GridPos::new(2, 3))
        );
        assert_eq!(
            step_target(center, Direction::East, Some(BOUNDS)),
            Some(GridPos::new(3, 2))
        );
    }

    #[test]
    fn step_target_never_leaves_the_map() {
        assert_eq!(
            step_target(GridPos::new(0, 0), Direction::North, None),
            None
        );
        assert_eq!(step_target(GridPos::new(0, 0), Direction::West, None), None);
        assert_eq!(
            step_target(GridPos::new(4, 4), Direction::South, Some(BOUNDS)),
            None
        );
        assert_eq!(
            step_target(GridPos::new(4, 4), Direction::East, Some(BOUNDS)),
            None
        );
        assert_eq!(
            step_target(GridPos::new(u32::MAX, 0), Direction::East, None),
            None
        );
    }

    #[test]
    fn a_movement_key_requests_the_adjacent_square_of_the_local_character() {
        let mut app = test_app();
        press(&mut app, KeyCode::KeyW);
        assert_eq!(
            sent(&app),
            vec![MoveRequest {
                target: GridPos::new(2, 1)
            }]
        );
    }

    #[test]
    fn holding_a_movement_key_repeats_steps_and_stops_when_typing() {
        let mut app = test_app();
        press(&mut app, KeyCode::KeyD);
        assert_eq!(sent(&app).len(), 1);

        // Simulate clearing just_pressed while keeping KeyD held down across ticks.
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .clear_just_pressed(KeyCode::KeyD);
        let mut repeated = 0;
        for _ in 0..5 {
            app.update();
            repeated += sent(&app).len();
        }
        assert!(
            repeated >= 2,
            "expected repeated hold steps, got {repeated}"
        );

        // Typing immediately suppresses repeat steps.
        app.world_mut().resource_mut::<TextEntryActive>().0 = true;
        for _ in 0..3 {
            app.update();
            assert_eq!(sent(&app), Vec::<MoveRequest>::new());
        }
    }

    #[test]
    fn movement_keys_do_nothing_while_typing() {
        let mut app = test_app();
        app.world_mut().resource_mut::<TextEntryActive>().0 = true;
        press(&mut app, KeyCode::KeyD);
        assert_eq!(sent(&app), Vec::<MoveRequest>::new());
    }

    #[test]
    fn movement_keys_do_nothing_before_a_character_is_assigned() {
        let mut app = test_app();
        app.world_mut().resource_mut::<LocalPlayer>().character = None;
        press(&mut app, KeyCode::KeyS);
        assert_eq!(sent(&app), Vec::<MoveRequest>::new());
    }

    #[test]
    fn movement_keys_do_not_request_squares_outside_the_map() {
        let mut app = test_app();
        app.world_mut().resource_mut::<LocalPlayer>().character = Some(EntityId(2));
        press(&mut app, KeyCode::KeyA);
        assert_eq!(sent(&app), Vec::<MoveRequest>::new());
    }
}
