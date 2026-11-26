//! Zeta game client: independent Bevy plugins over replicated components.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 5. Each plugin reads only the
//! replicated components and messages it owns; [`ClientPlugins`] adds them all.

pub mod dice;
pub mod focus;
pub mod frame_pacing;
pub mod inspect;
pub mod inventory;
pub mod layout;
pub mod map;
pub mod movement_input;
pub mod narration_log;
pub mod net;
pub mod planet_io;
pub mod player;
pub mod prompt;
pub mod settings;
pub mod terrain;
pub mod theme;
pub mod turn_banner;

use bevy::app::{PluginGroup, PluginGroupBuilder};

/// Every gameplay plugin of the first playable slice in
/// `docs/prototypes/realism_prototype.md`. Networking is added separately.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClientPlugins;

impl PluginGroup for ClientPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(frame_pacing::FramePacingPlugin)
            .add(theme::ThemePlugin)
            .add(player::LocalPlayerPlugin)
            .add(map::MapPlugin)
            .add(movement_input::MovementInputPlugin)
            .add(prompt::FreeformActionPromptPlugin)
            .add(turn_banner::TurnBubbleBannerPlugin)
            .add(dice::DiceRollPlugin)
            .add(narration_log::NarrationLogPlugin)
            .add(inventory::InventoryPlugin)
            .add(inspect::EntityInspectPlugin)
    }
}
