//! Client layout, timing, and keybinding values.
//!
//! Every constant here is a placeholder: `docs/architecture/technical_architecture.md`
//! Section 5 leaves visual layout, fonts, and keybindings undecided, and
//! `docs/architecture/code_style.md` Section 6 keeps undecided numbers in this module.
//! Plugins read them through [`ClientSettings`], so tests can use other values.

use bevy::prelude::{Color, KeyCode, Resource};

/// Placeholder: color of even grid squares, until map art exists. Replaced when sprites are
/// decided (`docs/architecture/technical_architecture.md` Section 2).
pub const GRID_LIGHT_COLOR: Color = Color::srgb(0.22, 0.26, 0.22);

/// Placeholder: color of odd grid squares, until map art exists. Replaced when sprites are
/// decided (`docs/architecture/technical_architecture.md` Section 2).
pub const GRID_DARK_COLOR: Color = Color::srgb(0.18, 0.22, 0.18);

/// Placeholder: color of player characters, until sprites exist. Replaced when sprites are
/// decided (`docs/architecture/technical_architecture.md` Section 2).
pub const PLAYER_CHARACTER_COLOR: Color = Color::srgb(0.25, 0.45, 0.9);

/// Placeholder: color of the character this client controls, until sprites exist. Replaced
/// when sprites are decided (`docs/architecture/technical_architecture.md` Section 2).
pub const LOCAL_PLAYER_COLOR: Color = Color::srgb(0.45, 0.8, 1.0);

/// Placeholder: color of entities creatures cannot pass through, until sprites exist.
/// Replaced when sprites are decided (`docs/architecture/technical_architecture.md`
/// Section 2).
pub const IMPASSABLE_COLOR: Color = Color::srgb(0.35, 0.3, 0.28);

/// Placeholder: color of every other entity, until sprites exist. Replaced when sprites are
/// decided (`docs/architecture/technical_architecture.md` Section 2).
pub const OTHER_ENTITY_COLOR: Color = Color::srgb(0.8, 0.5, 0.25);

/// Placeholder: width and height of 1 grid square in world units. Replaced when the client
/// layout is decided (`docs/architecture/technical_architecture.md` Section 5).
pub const TILE_SIZE: f32 = 32.0;

/// Placeholder: seconds a speech bubble stays above its speaker. Replaced when the client
/// layout is decided (`docs/architecture/technical_architecture.md` Section 5).
pub const SPEECH_BUBBLE_SECONDS: f32 = 4.0;

/// Placeholder: seconds the d20 tumbles before it settles on the natural roll. Replaced when
/// the client layout is decided (`docs/architecture/technical_architecture.md` Section 5).
pub const DICE_ANIMATION_SECONDS: f32 = 1.2;

/// Placeholder: seconds between face changes while the d20 tumbles. Replaced when the client
/// layout is decided (`docs/architecture/technical_architecture.md` Section 5).
pub const DICE_FACE_SECONDS: f32 = 0.06;

/// Placeholder: seconds a settled roll stays on screen before the next roll or hiding.
/// Replaced when the client layout is decided
/// (`docs/architecture/technical_architecture.md` Section 5).
pub const DICE_RESULT_SECONDS: f32 = 3.0;

/// Placeholder: lines the shared narration log keeps; older lines are dropped. Replaced when
/// the client layout is decided (`docs/architecture/technical_architecture.md` Section 5).
pub const NARRATION_LOG_MAX_LINES: usize = 200;

/// Placeholder: logical pixels the narration log scrolls per mouse wheel line. Replaced when
/// the client layout is decided (`docs/architecture/technical_architecture.md` Section 5).
pub const NARRATION_LOG_SCROLL_PIXELS_PER_LINE: f32 = 20.0;

/// Placeholder: characters a Freeform Action Prompt may hold in the text box. The server
/// validates the text again. Replaced when prompt limits are decided
/// (`docs/architecture/ai_orchestration.md`).
pub const PROMPT_MAX_CHARACTERS: usize = 500;

/// Placeholder: key that moves the local character 1 square north. Replaced when keybindings
/// are decided (`docs/architecture/technical_architecture.md` Section 5).
pub const MOVE_NORTH_KEY: KeyCode = KeyCode::KeyW;

/// Placeholder: key that moves the local character 1 square west. Replaced when keybindings
/// are decided (`docs/architecture/technical_architecture.md` Section 5).
pub const MOVE_WEST_KEY: KeyCode = KeyCode::KeyA;

/// Placeholder: key that moves the local character 1 square south. Replaced when keybindings
/// are decided (`docs/architecture/technical_architecture.md` Section 5).
pub const MOVE_SOUTH_KEY: KeyCode = KeyCode::KeyS;

/// Placeholder: key that moves the local character 1 square east. Replaced when keybindings
/// are decided (`docs/architecture/technical_architecture.md` Section 5).
pub const MOVE_EAST_KEY: KeyCode = KeyCode::KeyD;

/// Placeholder: key that ends the local player's turn in a Turn Bubble. Replaced when
/// keybindings are decided (`docs/architecture/technical_architecture.md` Section 5).
pub const END_TURN_KEY: KeyCode = KeyCode::Space;

/// Layout, timing, and keybinding values the client plugins read. `Default` uses the
/// placeholder constants of this module.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct ClientSettings {
    /// Width and height of 1 grid square in world units.
    pub tile_size: f32,
    /// Seconds a speech bubble stays above its speaker.
    pub speech_bubble_seconds: f32,
    /// Seconds the d20 tumbles before it settles.
    pub dice_animation_seconds: f32,
    /// Seconds between face changes while the d20 tumbles.
    pub dice_face_seconds: f32,
    /// Seconds a settled roll stays on screen.
    pub dice_result_seconds: f32,
    /// Lines the narration log keeps.
    pub narration_log_max_lines: usize,
    /// Logical pixels the narration log scrolls per mouse wheel line.
    pub narration_log_scroll_pixels_per_line: f32,
    /// Characters the Freeform Action Prompt text box accepts.
    pub prompt_max_characters: usize,
    /// Movement keys in the order north, west, south, east.
    pub movement_keys: MovementKeys,
    /// Key that ends the local player's turn.
    pub end_turn_key: KeyCode,
}

/// The 4 keys that move the local character 1 square.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MovementKeys {
    /// Moves north, toward row 0.
    pub north: KeyCode,
    /// Moves west, toward column 0.
    pub west: KeyCode,
    /// Moves south.
    pub south: KeyCode,
    /// Moves east.
    pub east: KeyCode,
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            tile_size: TILE_SIZE,
            speech_bubble_seconds: SPEECH_BUBBLE_SECONDS,
            dice_animation_seconds: DICE_ANIMATION_SECONDS,
            dice_face_seconds: DICE_FACE_SECONDS,
            dice_result_seconds: DICE_RESULT_SECONDS,
            narration_log_max_lines: NARRATION_LOG_MAX_LINES,
            narration_log_scroll_pixels_per_line: NARRATION_LOG_SCROLL_PIXELS_PER_LINE,
            prompt_max_characters: PROMPT_MAX_CHARACTERS,
            movement_keys: MovementKeys {
                north: MOVE_NORTH_KEY,
                west: MOVE_WEST_KEY,
                south: MOVE_SOUTH_KEY,
                east: MOVE_EAST_KEY,
            },
            end_turn_key: END_TURN_KEY,
        }
    }
}
