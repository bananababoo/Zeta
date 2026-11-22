//! Replicated components and client-server messages shared by the server and the client.
//!
//! Rules: `docs/architecture/technical_architecture.md` Sections 1 and 5. These are a sent
//! format: fields and variants are only ever added. Network types are structs and unit-only
//! enums, because the networking library's binary format cannot read internally tagged enums.
//! [`register_protocol`] fixes the registration order both sides must share, so new entries
//! go at the end.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::entity::EntityId;
use crate::grid::{GridPos, MapBounds};
use crate::resolution::{D20, Volume};

/// The playable area of the map, on 1 replicated entity. A separate component because
/// `MapBounds` is a server resource, and resources are components in the game engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct MapArea(pub MapBounds);

/// Display name of a world entity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct EntityName(pub String);

/// What an entity looks like now: the 1-sentence description in the Snapshot, and the text
/// the client shows until a sprite exists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct Visual(pub String);

/// Free-form narrative states such as `burning`, shown as overlay badges.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct Tags(pub BTreeSet<String>);

/// Whether creatures can move through the entity's square.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct Collider {
    /// True when creatures can enter the square.
    pub passable: bool,
}

/// Marks an entity a player controls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct PlayerCharacter;

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
}

/// Client to server: end the player's turn in a Turn Bubble.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Message))]
pub struct EndTurnRequest;

/// Server to 1 client: the character this player controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Message))]
pub struct PlayerAssigned {
    /// The controlled character.
    pub character: EntityId,
}

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

/// How a message travels, without naming the networking library.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Reliable and in order.
    Ordered,
    /// Reliable, any order.
    Unordered,
}

/// Registers network types with the networking library. Implemented by the server and
/// client network modules, so this crate never depends on that library.
#[cfg(feature = "bevy")]
pub trait ProtocolRegistrar {
    /// Replicates a component from server to clients.
    fn component<C>(&mut self)
    where
        C: bevy_ecs::prelude::Component<Mutability = bevy_ecs::component::Mutable>
            + Serialize
            + serde::de::DeserializeOwned;
    /// Registers a message clients send to the server.
    fn client_message<M>(&mut self, delivery: Delivery)
    where
        M: bevy_ecs::prelude::Message + Serialize + serde::de::DeserializeOwned;
    /// Registers a message the server sends to clients.
    fn server_message<M>(&mut self, delivery: Delivery)
    where
        M: bevy_ecs::prelude::Message + Serialize + serde::de::DeserializeOwned;
}

/// Registers every network type in the 1 order both sides share. Append only.
#[cfg(feature = "bevy")]
pub fn register_protocol(registrar: &mut impl ProtocolRegistrar) {
    registrar.component::<EntityId>();
    registrar.component::<GridPos>();
    registrar.component::<MapArea>();
    registrar.component::<EntityName>();
    registrar.component::<Visual>();
    registrar.component::<Tags>();
    registrar.component::<Collider>();
    registrar.component::<PlayerCharacter>();
    registrar.component::<TurnBubble>();
    registrar.client_message::<MoveRequest>(Delivery::Ordered);
    registrar.client_message::<FreeformActionPrompt>(Delivery::Ordered);
    registrar.client_message::<EndTurnRequest>(Delivery::Ordered);
    registrar.server_message::<PlayerAssigned>(Delivery::Ordered);
    registrar.server_message::<NarrationLogEntry>(Delivery::Ordered);
    registrar.server_message::<DiceRollShown>(Delivery::Ordered);
    registrar.server_message::<PromptStatusUpdate>(Delivery::Ordered);
}

#[cfg(test)]
mod tests {
    use serde::de::DeserializeOwned;

    use super::*;

    fn assert_json_round_trip<T>(value: &T)
    where
        T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).expect("serializes");
        let parsed: T = serde_json::from_str(&json).expect("parses back");
        assert_eq!(&parsed, value, "{json}");
    }

    #[test]
    fn every_replicated_component_round_trips_through_json() {
        assert_json_round_trip(&MapArea(MapBounds {
            width: 20,
            height: 20,
        }));
        assert_json_round_trip(&EntityName("Goblin".to_owned()));
        assert_json_round_trip(&Visual("A rusty oil barrel.".to_owned()));
        assert_json_round_trip(&Tags(BTreeSet::from(["burning".to_owned()])));
        assert_json_round_trip(&Collider { passable: false });
        assert_json_round_trip(&PlayerCharacter);
        assert_json_round_trip(&TurnBubble {
            order: vec![EntityId(1), EntityId(2)],
            current: 1,
            completed_rounds: 3,
            movement_used: true,
            prompt_used: false,
        });
    }

    #[test]
    fn every_message_round_trips_through_json() {
        assert_json_round_trip(&MoveRequest {
            target: GridPos { x: 4, y: 7 },
        });
        assert_json_round_trip(&FreeformActionPrompt {
            text: "I kick the barrel".to_owned(),
        });
        assert_json_round_trip(&EndTurnRequest);
        assert_json_round_trip(&PlayerAssigned {
            character: EntityId(9),
        });
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
    fn unit_enums_use_snake_case_names() {
        assert_eq!(
            serde_json::to_string(&LogEntryKind::Notice).expect("serializes"),
            "\"notice\""
        );
        assert_eq!(
            serde_json::to_string(&PromptState::Rejected).expect("serializes"),
            "\"rejected\""
        );
    }

    #[cfg(feature = "bevy")]
    #[derive(Default)]
    struct RecordingRegistrar(Vec<String>);

    #[cfg(feature = "bevy")]
    fn short_name<T>() -> &'static str {
        let full = std::any::type_name::<T>();
        full.rsplit("::").next().unwrap_or(full)
    }

    #[cfg(feature = "bevy")]
    impl ProtocolRegistrar for RecordingRegistrar {
        fn component<C>(&mut self)
        where
            C: bevy_ecs::prelude::Component<Mutability = bevy_ecs::component::Mutable>
                + Serialize
                + DeserializeOwned,
        {
            self.0.push(format!("component {}", short_name::<C>()));
        }

        fn client_message<M>(&mut self, delivery: Delivery)
        where
            M: bevy_ecs::prelude::Message + Serialize + DeserializeOwned,
        {
            self.0
                .push(format!("client {} {delivery:?}", short_name::<M>()));
        }

        fn server_message<M>(&mut self, delivery: Delivery)
        where
            M: bevy_ecs::prelude::Message + Serialize + DeserializeOwned,
        {
            self.0
                .push(format!("server {} {delivery:?}", short_name::<M>()));
        }
    }

    /// The order is the network identity of each type; changing it breaks every older client.
    #[cfg(feature = "bevy")]
    #[test]
    fn registration_order_is_fixed() {
        let mut registrar = RecordingRegistrar::default();
        register_protocol(&mut registrar);
        assert_eq!(
            registrar.0,
            [
                "component EntityId",
                "component GridPos",
                "component MapArea",
                "component EntityName",
                "component Visual",
                "component Tags",
                "component Collider",
                "component PlayerCharacter",
                "component TurnBubble",
                "client MoveRequest Ordered",
                "client FreeformActionPrompt Ordered",
                "client EndTurnRequest Ordered",
                "server PlayerAssigned Ordered",
                "server NarrationLogEntry Ordered",
                "server DiceRollShown Ordered",
                "server PromptStatusUpdate Ordered",
            ]
        );
    }
}
