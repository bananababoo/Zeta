//! The prototype test scene anchored inside Sera (`PosX`): the 20x20 River Crossing authored
//! region (river, wooden bridge, oil barrel, and goblin) embedded inside the continuous
//! `2^21 x 2^21` planetary face so players can walk straight out into Sera with zero loading
//! zones.
//!
//! Rules: `docs/prototypes/realism_prototype.md` Section 1 and
//! `docs/architecture/world_geography.md` Sections 1–4. The scene is temporary test content,
//! so its layout and text are placeholders, not Lore.

use bevy_ecs::prelude::{Bundle, Commands, ResMut};
use zeta_core::entity::EntityId;
use zeta_core::grid::{GridPos, MapBounds};
use zeta_core::protocol::{Collider, EntityName, MapArea, Tags, Visual};
use zeta_core::world::FACE_TILES;

use super::inventory::starting_inventory;
use super::{EntityIndex, NextEntityId};
use crate::net::messages::Networked;

/// Planetary top-left tile anchor of the River Crossing authored region on Sera (`PosX`),
/// matching `assets/world/regions/river_crossing.json`.
pub const RIVER_CROSSING_ORIGIN: GridPos = GridPos::new(1_819_648, 318_464);

/// Height (and width) of the River Crossing authored region in 5-foot squares.
const RIVER_CROSSING_SIZE: u32 = 20;

/// Size of the playable Sera cube-sphere face (`2^21 x 2^21` 5-foot squares).
pub const TEST_SCENE_BOUNDS: MapBounds = MapBounds {
    width: FACE_TILES,
    height: FACE_TILES,
};

/// Translates a River Crossing local `(x, y)` offset into its planetary `PosX` [`GridPos`].
#[must_use]
pub(super) const fn river_crossing_pos(local_x: u32, local_y: u32) -> GridPos {
    GridPos::new(
        RIVER_CROSSING_ORIGIN.x + local_x,
        RIVER_CROSSING_ORIGIN.y + local_y,
    )
}

/// Place name the AI DM receives with every request in the test scene.
pub const TEST_SCENE_PLACE_NAME: &str = "Prototype Test Scene";

/// Region-local column of the river that splits River Crossing.
const RIVER_COLUMN: u32 = 10;
/// Region-local row where the bridge crosses the river.
const BRIDGE_ROW: u32 = 10;
/// Where the oil barrel stands, west of the bridge.
const OIL_BARREL_POSITION: GridPos = river_crossing_pos(8, 9);
/// Where the goblin stands, east of the bridge.
const GOBLIN_POSITION: GridPos = river_crossing_pos(12, 10);
/// Where new player characters appear, or the nearest free square to it.
pub(crate) const PLAYER_SPAWN_POSITION: GridPos = river_crossing_pos(4, 10);
/// Catalog items and quantities every new player character carries.
pub(crate) const PLAYER_STARTING_ITEMS: &[(&str, u32)] = &[
    ("longsword", 1),
    ("rope", 1),
    ("rations", 2),
    ("painters_supplies", 1),
];
/// Catalog items and quantities the goblin carries, so `Give` has a recipient.
pub(crate) const GOBLIN_STARTING_ITEMS: &[(&str, u32)] = &[("scimitar", 1)];
/// Display name of the goblin.
const GOBLIN_NAME: &str = "Goblin";

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
    let mut entities: Vec<EntitySpec> = (0..RIVER_CROSSING_SIZE)
        .filter(|&row| row != BRIDGE_ROW)
        .map(|row| {
            EntitySpec::new(
                "River",
                river_crossing_pos(RIVER_COLUMN, row),
                "Fast, cold river water too deep to wade.",
                &["water", "deep"],
                false,
            )
        })
        .collect();
    entities.push(EntitySpec::new(
        "Wooden Bridge",
        river_crossing_pos(RIVER_COLUMN, BRIDGE_ROW),
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
        GOBLIN_NAME,
        GOBLIN_POSITION,
        "A wary goblin watching the far end of the bridge.",
        &[],
        false,
    ));
    entities
}

/// Startup system: the map bounds and every test scene entity, the goblin with its items.
pub(crate) fn spawn_test_scene(
    mut commands: Commands,
    mut index: ResMut<EntityIndex>,
    mut next_id: ResMut<NextEntityId>,
) {
    commands.insert_resource(TEST_SCENE_BOUNDS);
    commands.spawn((MapArea(TEST_SCENE_BOUNDS), Networked));
    for spec in test_scene_entities() {
        let id = next_id.allocate();
        let is_goblin = spec.name == GOBLIN_NAME;
        let mut entity = commands.spawn(spec.into_bundle(id));
        if is_goblin {
            entity.insert((
                zeta_core::protocol::Creature,
                starting_inventory(GOBLIN_STARTING_ITEMS),
            ));
        }
        index.0.insert(id, entity.id());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_river_is_crossable_only_at_the_bridge() {
        let entities = test_scene_entities();
        for row in 0..RIVER_CROSSING_SIZE {
            let square = river_crossing_pos(RIVER_COLUMN, row);
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
