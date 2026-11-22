//! Zeta game client: independent Bevy plugins over replicated components.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 5.

pub mod dice;
pub mod focus;
pub mod map;
pub mod movement_input;
pub mod narration_log;
pub mod net;
pub mod player;
pub mod prompt;
pub mod settings;
pub mod turn_banner;
