//! Replicated components and client-server messages shared by the server and the client.
//!
//! Rules: `docs/architecture/technical_architecture.md` Sections 1 and 5.
//!
//! Type definitions live in 1 submodule per gameplay domain ([`world`], [`turn`], [`actions`],
//! [`log`]); a new milestone adds a new submodule. Every type is re-exported here, so callers
//! write `zeta_core::protocol::MoveRequest` whatever file defines it.
//!
//! Registration stays in 1 place, [`register_protocol`], on purpose. The networking library
//! numbers each type by the order it is registered, and refuses a client whose list differs
//! from the server's. 1 fixed list keeps that order independent of plugin add order.
//!
//! Rules for every network type:
//! - Sent format: fields and variants are only ever added, never renamed, reordered, or removed.
//! - Structs and unit-only enums only. The networking library's binary format cannot read
//!   internally tagged enums.
//! - New registrations go at the end of [`register_protocol`], and the order test gains the
//!   same line.

pub mod actions;
pub mod log;
pub mod turn;
pub mod world;

pub use actions::{
    FreeformActionPrompt, MoveRequest, PlayerAssigned, PromptState, PromptStatusUpdate,
};
pub use log::{DiceRollShown, LogEntryKind, NarrationLogEntry};
pub use turn::{EndTurnRequest, TurnBubble};
pub use world::{Collider, EntityName, MapArea, PlayerCharacter, Tags, Visual};

#[cfg(feature = "bevy")]
use serde::Serialize;

#[cfg(feature = "bevy")]
use crate::entity::EntityId;
#[cfg(feature = "bevy")]
use crate::grid::GridPos;

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

/// Shared by the submodule tests: serializes to JSON and back, and expects the same value.
#[cfg(test)]
fn assert_json_round_trip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(value).expect("serializes");
    let parsed: T = serde_json::from_str(&json).expect("parses back");
    assert_eq!(&parsed, value, "{json}");
}

#[cfg(all(test, feature = "bevy"))]
mod tests {
    use serde::de::DeserializeOwned;

    use super::*;

    #[derive(Default)]
    struct RecordingRegistrar(Vec<String>);

    fn short_name<T>() -> &'static str {
        let full = std::any::type_name::<T>();
        full.rsplit("::").next().unwrap_or(full)
    }

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
