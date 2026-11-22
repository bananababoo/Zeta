//! The replicated Turn Bubble and the request that ends a turn.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 5. Protocol rules for every
//! type here are in the parent module docs.

use serde::{Deserialize, Serialize};

use crate::entity::EntityId;

/// An active Turn Bubble: participants act in a fixed order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct TurnBubble {
    /// Participants in turn order.
    pub order: Vec<EntityId>,
    /// Index into `order` of the participant whose turn it is.
    pub current: u32,
    /// Rounds completed since the Turn Bubble started.
    pub completed_rounds: u32,
    /// Whether the current participant has used their 1 movement.
    pub movement_used: bool,
    /// Whether the current participant has used their 1 Freeform Action Prompt.
    pub prompt_used: bool,
}

/// Client to server: end the player's turn in a Turn Bubble.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Message))]
pub struct EndTurnRequest;

#[cfg(test)]
mod tests {
    use super::super::assert_json_round_trip;
    use super::*;

    #[test]
    fn turn_types_round_trip_through_json() {
        assert_json_round_trip(&TurnBubble {
            order: vec![EntityId(1), EntityId(2)],
            current: 1,
            completed_rounds: 3,
            movement_used: true,
            prompt_used: false,
        });
        assert_json_round_trip(&EndTurnRequest);
    }
}
