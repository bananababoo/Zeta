//! Lines of the shared narration and speech log, and visible dice rolls.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 5. Protocol rules for every
//! type here are in the parent module docs.

use serde::{Deserialize, Serialize};

use crate::entity::EntityId;
use crate::resolution::{D20, Volume};

/// The kind of 1 shared log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogEntryKind {
    /// A player's declared Freeform Action Prompt.
    Action,
    /// AI DM narration.
    Narration,
    /// An entity speaking.
    Speech,
    /// A system notice, such as a request to rephrase.
    Notice,
}

/// Server to clients: 1 line of the shared narration and speech log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Message))]
pub struct NarrationLogEntry {
    /// Shared with [`DiceRollShown`], so clients can order lines across message types.
    pub sequence: u64,
    /// What kind of line this is.
    pub kind: LogEntryKind,
    /// The acting or speaking entity, if any.
    pub speaker: Option<EntityId>,
    /// Display name of `speaker` when the line was written.
    pub speaker_name: Option<String>,
    /// The line itself.
    pub text: String,
    /// Speech volume, only for speech.
    pub vol: Option<Volume>,
}

/// Server to clients: a visible d20 check roll.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Message))]
pub struct DiceRollShown {
    /// Shared with [`NarrationLogEntry`].
    pub sequence: u64,
    /// The rolling character.
    pub actor: EntityId,
    /// The natural roll.
    pub natural: D20,
    /// The character's modifier.
    pub modifier: i8,
    /// The Difficulty Class set by the AI DM.
    pub dc: u8,
    /// Why the check was needed.
    pub reason: String,
    /// Whether the check succeeded.
    pub success: bool,
}

#[cfg(test)]
mod tests {
    use super::super::assert_json_round_trip;
    use super::*;

    #[test]
    fn log_messages_round_trip_through_json() {
        assert_json_round_trip(&NarrationLogEntry {
            sequence: 5,
            kind: LogEntryKind::Speech,
            speaker: Some(EntityId(3)),
            speaker_name: Some("Goblin".to_owned()),
            text: "Halt!".to_owned(),
            vol: Some(Volume::Shout),
        });
        assert_json_round_trip(&DiceRollShown {
            sequence: 6,
            actor: EntityId(9),
            natural: D20::new(17).expect("valid face"),
            modifier: -1,
            dc: 12,
            reason: "Kick the barrel over".to_owned(),
            success: true,
        });
    }

    #[test]
    fn log_entry_kind_uses_snake_case_names() {
        assert_eq!(
            serde_json::to_string(&LogEntryKind::Notice).expect("serializes"),
            "\"notice\""
        );
    }
}
