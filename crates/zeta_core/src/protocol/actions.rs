//! Player action requests and the server replies about them.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 5. Protocol rules for every
//! type here are in the parent module docs.

use serde::{Deserialize, Serialize};

use crate::entity::EntityId;
use crate::grid::GridPos;
use crate::protocol::inventory::StackId;

/// Client to server: move the player's character toward a square.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Message))]
pub struct MoveRequest {
    /// Destination square.
    pub target: GridPos,
}

/// Client to server: the text of 1 Freeform Action Prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Message))]
pub struct FreeformActionPrompt {
    /// What the player typed; untrusted.
    pub text: String,
    /// The carried tool stack the player chose `Utilize` on, if any. The server looks up
    /// that tool's checks in the item catalog itself.
    #[serde(default)]
    pub utilize: Option<StackId>,
}

/// Server to 1 client: the character this player controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Message))]
pub struct PlayerAssigned {
    /// The controlled character.
    pub character: EntityId,
}

/// Where a player's Freeform Action Prompt stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptState {
    /// The AI DM is resolving it.
    Resolving,
    /// It waits behind a `PendingAiResolution` lock.
    Queued,
    /// Its effects were applied.
    Applied,
    /// Nothing was applied; `reason` says why.
    Rejected,
}

/// Server to 1 client: progress of that player's latest Freeform Action Prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Message))]
pub struct PromptStatusUpdate {
    /// Current state.
    pub state: PromptState,
    /// Explanation shown to the player, when rejected.
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::super::assert_json_round_trip;
    use super::*;

    #[test]
    fn action_messages_round_trip_through_json() {
        assert_json_round_trip(&MoveRequest {
            target: GridPos { x: 4, y: 7 },
        });
        assert_json_round_trip(&FreeformActionPrompt {
            text: "I kick the barrel".to_owned(),
            utilize: None,
        });
        assert_json_round_trip(&FreeformActionPrompt {
            text: "I paint the goblin".to_owned(),
            utilize: Some(StackId(2)),
        });
        assert_json_round_trip(&PlayerAssigned {
            character: EntityId(9),
        });
        for state in [
            PromptState::Resolving,
            PromptState::Queued,
            PromptState::Applied,
            PromptState::Rejected,
        ] {
            assert_json_round_trip(&PromptStatusUpdate {
                state,
                reason: None,
            });
        }
    }

    #[test]
    fn prompt_state_uses_snake_case_names() {
        assert_eq!(
            serde_json::to_string(&PromptState::Rejected).expect("serializes"),
            "\"rejected\""
        );
    }
}
