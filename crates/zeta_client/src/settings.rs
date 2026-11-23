//! Client layout, timing, and keybinding values.
//!
//! Every constant here is a placeholder: `docs/architecture/technical_architecture.md`
//! Section 5 leaves visual layout, fonts, and keybindings undecided, and
//! `docs/architecture/code_style.md` Section 6 keeps undecided numbers in this module.
//! Plugins read them through [`ClientSettings`], so tests can use other values.

use std::time::Duration;

use bevy::prelude::{Color, KeyCode, Resource};

/// Placeholder: how long a connect may take before the client gives up. Replaced when hosting
/// is set up (`docs/architecture/technical_architecture.md` Section 1).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Placeholder: bytes read from the server per update at most, so 1 large burst cannot stall
/// a frame. Replaced when hosting is set up (`docs/architecture/technical_architecture.md`
/// Section 1).
pub const MAX_RECEIVE_BYTES_PER_UPDATE: usize = 4 << 20;

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

/// Placeholder: key that opens and closes the inventory panel. Replaced when keybindings are
/// decided (`docs/architecture/technical_architecture.md` Section 5).
pub const INVENTORY_KEY: KeyCode = KeyCode::KeyI;

/// Placeholder: logical pixels the inventory panel scrolls per mouse wheel line. Replaced when
/// the client layout is decided (`docs/architecture/technical_architecture.md` Section 5).
pub const INVENTORY_SCROLL_PIXELS_PER_LINE: f32 = 20.0;

/// Placeholder: width and height of 1 inventory slot square in logical pixels. Replaced when
/// the client layout is decided (`docs/architecture/technical_architecture.md` Section 5).
pub const INVENTORY_SLOT_SIZE_PIXELS: f32 = 40.0;

/// Placeholder: logical pixels between inventory slot squares. Replaced when the client layout
/// is decided (`docs/architecture/technical_architecture.md` Section 5).
pub const INVENTORY_SLOT_GAP_PIXELS: f32 = 4.0;

/// Placeholder: logical pixels between a slot and its tooltip or action menu. Replaced when
/// the client layout is decided (`docs/architecture/technical_architecture.md` Section 5).
pub const INVENTORY_POPUP_GAP_PIXELS: f32 = 6.0;

/// Placeholder: width of the border that marks an equipped slot, in logical pixels. Replaced
/// when the client layout is decided (`docs/architecture/technical_architecture.md`
/// Section 5).
pub const INVENTORY_EQUIPPED_BORDER_PIXELS: f32 = 3.0;

/// Placeholder: color of the border that marks an equipped slot. Replaced when the client
/// layout is decided (`docs/architecture/technical_architecture.md` Section 5).
pub const INVENTORY_EQUIPPED_BORDER_COLOR: Color = Color::srgb(1.0, 0.85, 0.3);

/// Placeholder: color of weapon slots. Replaced when item art is decided
/// (`docs/architecture/technical_architecture.md` Section 2).
pub const INVENTORY_WEAPON_COLOR: Color = Color::srgb(0.6, 0.22, 0.2);

/// Placeholder: color of armor and shield slots. Replaced when item art is decided
/// (`docs/architecture/technical_architecture.md` Section 2).
pub const INVENTORY_ARMOR_COLOR: Color = Color::srgb(0.25, 0.35, 0.6);

/// Placeholder: color of tool slots. Replaced when item art is decided
/// (`docs/architecture/technical_architecture.md` Section 2).
pub const INVENTORY_TOOL_COLOR: Color = Color::srgb(0.55, 0.42, 0.2);

/// Placeholder: color of every other slot, including custom items. Replaced when item art is
/// decided (`docs/architecture/technical_architecture.md` Section 2).
pub const INVENTORY_GEAR_COLOR: Color = Color::srgb(0.3, 0.45, 0.3);
/// Placeholder: longest wait between updates while the window has focus and nothing happens.
/// Input wakes the client at once; this bounds how late server messages and timers are seen.
/// Replaced when client performance targets are decided
/// (`docs/architecture/technical_architecture.md` Section 5).
pub const FOCUSED_UPDATE_WAIT: Duration = Duration::from_millis(100);

/// Placeholder: longest wait between updates while the window is out of focus. Replaced when
/// client performance targets are decided (`docs/architecture/technical_architecture.md`
/// Section 5).
pub const UNFOCUSED_UPDATE_WAIT: Duration = Duration::from_millis(250);

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
    /// Key that opens and closes the inventory panel.
    pub inventory_key: KeyCode,
    /// Logical pixels the inventory panel scrolls per mouse wheel line.
    pub inventory_scroll_pixels_per_line: f32,
    /// Size, spacing, and colors of the inventory slot grid.
    pub inventory_slots: InventorySlotSettings,
    /// Longest wait between updates while the window has focus.
    pub focused_update_wait: Duration,
    /// Longest wait between updates while the window is out of focus.
    pub unfocused_update_wait: Duration,
}

/// Size, spacing, and colors of the inventory slot grid, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InventorySlotSettings {
    /// Width and height of 1 slot square.
    pub size_pixels: f32,
    /// Space between slot squares.
    pub gap_pixels: f32,
    /// Space between a slot and its tooltip or action menu.
    pub popup_gap_pixels: f32,
    /// Width of the border that marks an equipped slot.
    pub equipped_border_pixels: f32,
    /// Color of the border that marks an equipped slot.
    pub equipped_border_color: Color,
    /// Color of weapon slots.
    pub weapon_color: Color,
    /// Color of armor and shield slots.
    pub armor_color: Color,
    /// Color of tool slots.
    pub tool_color: Color,
    /// Color of every other slot, including custom items.
    pub gear_color: Color,
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
            inventory_key: INVENTORY_KEY,
            inventory_scroll_pixels_per_line: INVENTORY_SCROLL_PIXELS_PER_LINE,
            inventory_slots: InventorySlotSettings {
                size_pixels: INVENTORY_SLOT_SIZE_PIXELS,
                gap_pixels: INVENTORY_SLOT_GAP_PIXELS,
                popup_gap_pixels: INVENTORY_POPUP_GAP_PIXELS,
                equipped_border_pixels: INVENTORY_EQUIPPED_BORDER_PIXELS,
                equipped_border_color: INVENTORY_EQUIPPED_BORDER_COLOR,
                weapon_color: INVENTORY_WEAPON_COLOR,
                armor_color: INVENTORY_ARMOR_COLOR,
                tool_color: INVENTORY_TOOL_COLOR,
                gear_color: INVENTORY_GEAR_COLOR,
            },
            focused_update_wait: FOCUSED_UPDATE_WAIT,
            unfocused_update_wait: UNFOCUSED_UPDATE_WAIT,
        }
    }
}
