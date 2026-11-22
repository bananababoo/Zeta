//! The prototype test scene: a 20x20 map with a river, a bridge, an oil barrel, and a goblin.
//!
//! Rules: `docs/prototypes/realism_prototype.md` Section 1. The scene is temporary test
//! content, so its layout and text are placeholders, not Lore.

use bevy_ecs::prelude::{Bundle, Commands, ResMut};
use zeta_core::entity::EntityId;
use zeta_core::grid::{GridPos, MapBounds};
use zeta_core::protocol::{Collider, EntityName, MapArea, Tags, Visual};

use super::{EntityIndex, NextEntityId};
use crate::net::messages::Networked;

/// Size of the prototype test scene.
pub const TEST_SCENE_BOUNDS: MapBounds = MapBounds {
    width: 20,
    height: 20,
};

/// Place name the AI DM receives with every request in the test scene.
pub const TEST_SCENE_PLACE_NAME: &str = "Prototype Test Scene";

/// Column of the river that splits the map.
const RIVER_COLUMN: u32 = 10;
/// Row where the bridge crosses the river.
const BRIDGE_ROW: u32 = 10;
/// Where the oil barrel stands, west of the bridge.
const OIL_BARREL_POSITION: GridPos = GridPos::new(8, 9);
/// Where the goblin stands, east of the bridge.
const GOBLIN_POSITION: GridPos = GridPos::new(12, 10);
/// Where new player characters appear, or the nearest free square to it.
pub(crate) const PLAYER_SPAWN_POSITION: GridPos = GridPos::new(4, 10);

/// The fields every world entity starts with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EntitySpec {
    /// Display name.
    pub(crate) name: String,
    /// Starting square.
    pub(crate) position: GridPos,
    /// 1-sentence description.
    pub(crate) visual: String,
    /// Starting tags.
    pub(crate) tags: Vec<String>,
    /// Whether creatures can enter its square.
    pub(crate) passable: bool,
}

impl EntitySpec {
    fn new(name: &str, position: GridPos, visual: &str, tags: &[&str], passable: bool) -> Self {
        Self {
            name: name.into(),
            position,
            visual: visual.into(),
            tags: tags.iter().map(|&tag| tag.into()).collect(),
            passable,
        }
    }

    /// The replicated components of a world entity with identifier `id`.
    pub(crate) fn into_bundle(self, id: EntityId) -> impl Bundle {
        (
            id,
            self.position,
            EntityName(self.name),
            Visual(self.visual),
            Tags(self.tags.into_iter().collect()),
            Collider {
                passable: self.passable,
            },
            Networked,
        )
    }
}

/// Every entity of the test scene, in spawn order.
pub(crate) fn test_scene_entities() -> Vec<EntitySpec> {
    let mut entities: Vec<EntitySpec> = (0..TEST_SCENE_BOUNDS.height)
        .filter(|&row| row != BRIDGE_ROW)
        .map(|row| {
            EntitySpec::new(
                "River",
                GridPos::new(RIVER_COLUMN, row),
                "Fast, cold river water too deep to wade.",
                &["water", "deep"],
                false,
            )
        })
        .collect();
    entities.push(EntitySpec::new(
        "Wooden Bridge",
        GridPos::new(RIVER_COLUMN, BRIDGE_ROW),
        "A narrow wooden footbridge over the river.",
        &["wooden"],
        true,
    ));
    entities.push(EntitySpec::new(
        "Oil Barrel",
        OIL_BARREL_POSITION,
        "A wooden barrel of lamp oil, sealed with a cork.",
        &["wooden", "flammable"],
        false,
    ));
    entities.push(EntitySpec::new(
        "Goblin",
        GOBLIN_POSITION,
        "A wary goblin watching the far end of the bridge.",
        &[],
        false,
    ));
    entities
}

/// Startup system: the map bounds and every test scene entity.
pub(crate) fn spawn_test_scene(
    mut commands: Commands,
    mut index: ResMut<EntityIndex>,
    mut next_id: ResMut<NextEntityId>,
) {
    commands.insert_resource(TEST_SCENE_BOUNDS);
    commands.spawn((MapArea(TEST_SCENE_BOUNDS), Networked));
    for spec in test_scene_entities() {
        let id = next_id.allocate();
        let entity = commands.spawn(spec.into_bundle(id)).id();
        index.0.insert(id, entity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_river_is_crossable_only_at_the_bridge() {
        let entities = test_scene_entities();
        for row in 0..TEST_SCENE_BOUNDS.height {
            let square = GridPos::new(RIVER_COLUMN, row);
            let on_square: Vec<&EntitySpec> = entities
                .iter()
                .filter(|entity| entity.position == square)
                .collect();
            assert_eq!(on_square.len(), 1, "row {row}");
            assert_eq!(on_square[0].passable, row == BRIDGE_ROW, "row {row}");
        }
    }

    #[test]
    fn every_entity_is_inside_the_map() {
        for entity in test_scene_entities() {
            assert!(
                TEST_SCENE_BOUNDS.contains(entity.position.x, entity.position.y),
                "{entity:?}"
            );
        }
    }
}
