//! Replicated components that describe world entities: name, look, tags, and collision.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 5. Protocol rules for every
//! type here are in the parent module docs.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::grid::MapBounds;

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

/// Marks a living or once-living creature (player character, NPC, or monster), as opposed to
/// an inanimate object or terrain feature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct Creature;

#[cfg(test)]
mod tests {
    use super::super::assert_json_round_trip;
    use super::*;

    #[test]
    fn world_components_round_trip_through_json() {
        assert_json_round_trip(&MapArea(MapBounds {
            width: 20,
            height: 20,
        }));
        assert_json_round_trip(&EntityName("Goblin".to_owned()));
        assert_json_round_trip(&Visual("A rusty oil barrel.".to_owned()));
        assert_json_round_trip(&Tags(BTreeSet::from(["burning".to_owned()])));
        assert_json_round_trip(&Collider { passable: false });
        assert_json_round_trip(&PlayerCharacter);
        assert_json_round_trip(&Creature);
    }
}
