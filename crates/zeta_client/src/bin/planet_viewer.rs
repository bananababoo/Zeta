//! Standalone interactive Sera planet viewer.
//!
//! Usage: `cargo run -p zeta_client --bin planet_viewer --offline`
//!
//! Controls:
//! - `W`/`A`/`S`/`D` or Arrow keys: pan smoothly across Sera (hold `Shift` for 4x speed)
//! - `Q`/`E` or `-`/`+`: zoom out / in
//! - `Space`: toggle `ElevationShaded` vs `LandOceanMask`

use bevy::prelude::*;
use bevy::window::WindowResolution;
use zeta_client::planet_stream::{
    PlanetCameraFocus, PlanetRenderMode, PlanetStreamPlugin, PlanetWorld, focus_tile,
    pan_focus_view,
};
use zeta_core::world::sample_tile;

const WINDOW_WIDTH: u32 = 1280;
const WINDOW_HEIGHT: u32 = 720;
const BASE_PAN_PIXELS_PER_SEC: f32 = 340.0;
const FAST_PAN_MULTIPLIER: f32 = 4.0;
const ZOOM_FACTOR_PER_SEC: f32 = 2.0;
const MIN_TILE_PIXEL_SIZE: f32 = 1.5;
const MAX_TILE_PIXEL_SIZE: f32 = 24.0;

#[derive(Component)]
struct PlanetHudText;

fn main() -> AppExit {
    App::new()
        .add_plugins(
            DefaultPlugins
                .build()
                .set(zeta_client::terrain::client_asset_plugin())
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "Zeta — Sera Planet Viewer".to_owned(),
                        // `WindowResolution::new`: bevy_window-0.20.1/src/window.rs:923.
                        resolution: WindowResolution::new(WINDOW_WIDTH, WINDOW_HEIGHT),
                        ..default()
                    }),
                    ..default()
                }),
        )
        // `ClearColor`: bevy_camera-0.20.1/src/clear_color.rs:56.
        .insert_resource(ClearColor(Color::srgb(0.05, 0.09, 0.14)))
        .insert_resource(PlanetCameraFocus::sera_seam())
        .add_plugins(PlanetStreamPlugin)
        .add_systems(Startup, spawn_hud_overlay)
        .add_systems(Update, (handle_viewer_controls, update_hud_overlay).chain())
        .run()
}

fn spawn_hud_overlay(mut commands: Commands) {
    // `Node`: bevy_ui-0.20.1/src/ui_node.rs:485.
    // `PositionType::Absolute`: bevy_ui-0.20.1/src/ui_node.rs:1763.
    // `UiRect::axes`: bevy_ui-0.20.1/src/geometry.rs:693.
    // `Val::Px`: bevy_ui-0.20.1/src/geometry.rs:32.
    // `BackgroundColor`: bevy_ui-0.20.1/src/ui_node.rs:2576.
    // `Text`: bevy_ui-0.20.1/src/widget/text.rs:128.
    // `TextFont`: bevy_text-0.20.1/src/text.rs:671.
    // `TextColor`: bevy_text-0.20.1/src/text.rs:1405.
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(12.0),
                left: Val::Px(12.0),
                padding: UiRect::axes(Val::Px(12.0), Val::Px(8.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.06, 0.08, 0.11, 0.82)),
        ))
        .with_child((
            PlanetHudText,
            Text::new("Loading Sera..."),
            TextFont {
                // `FontSize::Px`: bevy_text-0.20.1/src/text.rs:783.
                font_size: FontSize::Px(14.0),
                ..default()
            },
            TextColor(Color::srgb(0.95, 0.94, 0.90)),
        ));
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn handle_viewer_controls(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut focus: ResMut<PlanetCameraFocus>,
    mut mode: ResMut<PlanetRenderMode>,
) {
    // `ButtonInput::just_pressed`: bevy_input-0.20.1/src/button_input.rs:188.
    // `KeyCode::Space`: bevy_input-0.20.1/src/keyboard.rs:425.
    if keys.just_pressed(KeyCode::Space) {
        *mode = mode.toggled();
    }

    // `Time::delta_secs`: bevy_time-0.20.1/src/time.rs:283.
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }

    // `ButtonInput::pressed`: bevy_input-0.20.1/src/button_input.rs:157.
    let mut pan_dir = Vec2::ZERO;
    if keys.pressed(KeyCode::KeyA) || keys.pressed(KeyCode::ArrowLeft) {
        pan_dir.x -= 1.0;
    }
    if keys.pressed(KeyCode::KeyD) || keys.pressed(KeyCode::ArrowRight) {
        pan_dir.x += 1.0;
    }
    if keys.pressed(KeyCode::KeyW) || keys.pressed(KeyCode::ArrowUp) {
        pan_dir.y -= 1.0;
    }
    if keys.pressed(KeyCode::KeyS) || keys.pressed(KeyCode::ArrowDown) {
        pan_dir.y += 1.0;
    }

    if pan_dir != Vec2::ZERO {
        let boost = if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
            FAST_PAN_MULTIPLIER
        } else {
            1.0
        };
        let tiles_per_sec =
            (BASE_PAN_PIXELS_PER_SEC * boost) / focus.tile_pixel_size.max(MIN_TILE_PIXEL_SIZE);
        let delta = pan_dir.normalize() * tiles_per_sec * dt;
        pan_focus_view(&mut focus, delta);
    }

    let zoom_in = keys.pressed(KeyCode::KeyE)
        || keys.pressed(KeyCode::Equal)
        || keys.pressed(KeyCode::NumpadAdd);
    let zoom_out = keys.pressed(KeyCode::KeyQ)
        || keys.pressed(KeyCode::Minus)
        || keys.pressed(KeyCode::NumpadSubtract);

    if zoom_in && !zoom_out {
        focus.tile_pixel_size = (focus.tile_pixel_size * ZOOM_FACTOR_PER_SEC.powf(dt))
            .clamp(MIN_TILE_PIXEL_SIZE, MAX_TILE_PIXEL_SIZE);
    } else if zoom_out && !zoom_in {
        focus.tile_pixel_size = (focus.tile_pixel_size / ZOOM_FACTOR_PER_SEC.powf(dt))
            .clamp(MIN_TILE_PIXEL_SIZE, MAX_TILE_PIXEL_SIZE);
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn update_hud_overlay(
    focus: Res<PlanetCameraFocus>,
    mode: Res<PlanetRenderMode>,
    world: Option<Res<PlanetWorld>>,
    mut hud_text: Query<&mut Text, With<PlanetHudText>>,
) {
    let Ok(mut text) = hud_text.single_mut() else {
        return;
    };
    let tile = focus_tile(&focus);
    let elev_line = if let Some(world) = world {
        let sample = sample_tile(world.maps(), tile);
        let kind = if sample.is_water { "Ocean" } else { "Land" };
        format!(
            "Elevation: {} ft (step {} ft, {kind})",
            sample.elevation_ft, sample.elevation_step_ft
        )
    } else {
        "Elevation: unavailable".to_owned()
    };

    text.0 = format!(
        "Face: {} (view rot {:?})\n\
         Tile: ({}, {})  Chunk: ({}, {})\n\
         {elev_line}\n\
         Mode: {}  Zoom: {:.1} px/tile\n\
         WASD/Arrows: Pan (Shift: 4x) | Q/E,+/-: Zoom | Space: Mode",
        tile.face,
        focus.view_rotation,
        tile.x,
        tile.y,
        focus.anchor.cx,
        focus.anchor.cy,
        mode.label(),
        focus.tile_pixel_size,
    );
}
