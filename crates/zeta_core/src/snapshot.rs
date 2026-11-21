//! The Snapshot: every entity the acting character could plausibly perceive.
//!
//! Rules: `docs/architecture/ai_orchestration.md` Section 1. The acting character is always
//! local identifier 0, and the rest follow nearest first, so the same world always gives
//! the same Snapshot.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::entity::EntityId;
use crate::grid::{GridPos, has_line_of_sight};
use crate::resolution::LocalId;
use crate::settings::{HEARING_RANGE_SQUARES, SNAPSHOT_MAX_COUNT};

/// 1 entity as the Snapshot records it: the fixed field set plus server-only fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotEntity {
    /// Permanent identity. Server-only; the AI DM sees the local identifier instead.
    pub world_id: EntityId,
    /// Display name.
    pub name: String,
    /// Where the entity stands.
    pub position: GridPos,
    /// Free-form tags such as `burning` or `wooden`.
    pub tags: Vec<String>,
    /// Whether creatures can move through the entity's square.
    pub passable: bool,
    /// 1-sentence description.
    pub description: String,
    /// Whether a player controls the entity. Server-only; player characters stay free to
    /// move while locked.
    pub player_character: bool,
}

/// How far perception reaches when building a Snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PerceptionSettings {
    /// Entities within this many squares are heard even without line of sight.
    pub hearing_range_squares: u32,
    /// Maximum Snapshot size, including the acting character. Values below 1 count as 1.
    pub max_count: usize,
}

impl Default for PerceptionSettings {
    fn default() -> Self {
        Self {
            hearing_range_squares: HEARING_RANGE_SQUARES,
            max_count: SNAPSHOT_MAX_COUNT,
        }
    }
}

/// The entities sent to the AI DM with 1 Freeform Action Prompt.
///
/// The position of an entity in the list is its [`LocalId`]. Recorded in the Event Log, so
/// any stored effect can be traced back to the entity it named.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    entities: Vec<SnapshotEntity>,
}

/// 1 entity in the form the AI DM receives: the fixed field set from
/// `docs/architecture/ai_orchestration.md` Section 1 and nothing else.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AiSnapshotEntity<'a> {
    /// Local identifier for this request.
    pub id: LocalId,
    /// Display name.
    pub name: &'a str,
    /// Column.
    pub x: u32,
    /// Row.
    pub y: u32,
    /// Free-form tags.
    pub tags: &'a [String],
    /// Whether creatures can move through it.
    pub passable: bool,
    /// 1-sentence description.
    pub description: &'a str,
}

impl Snapshot {
    /// Builds the Snapshot for `actor` from every entity that might be perceived.
    ///
    /// An entity is perceived if it is within hearing range or in line of sight. Candidates
    /// repeating the actor or another candidate's `world_id` are dropped. When more are
    /// perceived than `max_count` allows, the nearest are kept, ties broken by `world_id`.
    pub fn build(
        actor: SnapshotEntity,
        candidates: impl IntoIterator<Item = SnapshotEntity>,
        blocks_sight: impl Fn(GridPos) -> bool,
        settings: PerceptionSettings,
    ) -> Self {
        let origin = actor.position;
        let is_perceived = |entity: &SnapshotEntity| {
            origin.grid_distance(entity.position) <= settings.hearing_range_squares
                || has_line_of_sight(origin, entity.position, &blocks_sight)
        };
        let mut seen = BTreeSet::from([actor.world_id]);
        let mut perceived: Vec<SnapshotEntity> = candidates
            .into_iter()
            .filter(|entity| seen.insert(entity.world_id) && is_perceived(entity))
            .collect();
        perceived.sort_by_key(|entity| (origin.grid_distance(entity.position), entity.world_id));

        let largest_count = usize::try_from(LocalId::MAX).unwrap_or(usize::MAX);
        let max_count = settings.max_count.clamp(1, largest_count);
        perceived.truncate(max_count - 1);
        let mut entities = Vec::with_capacity(perceived.len() + 1);
        entities.push(actor);
        entities.extend(perceived);
        Self { entities }
    }

    /// Number of entities, including the acting character.
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    /// Always false: the acting character is always present.
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// The acting character.
    pub fn actor(&self) -> Option<&SnapshotEntity> {
        self.entities.first()
    }

    /// The entity with a local identifier.
    pub fn get(&self, id: LocalId) -> Option<&SnapshotEntity> {
        self.entities.get(usize::try_from(id).ok()?)
    }

    /// Whether a local identifier names an entity in this Snapshot.
    pub fn contains(&self, id: LocalId) -> bool {
        self.get(id).is_some()
    }

    /// Every entity in local identifier order.
    pub fn entities(&self) -> &[SnapshotEntity] {
        &self.entities
    }

    /// The Snapshot as the AI DM receives it.
    pub fn ai_view(&self) -> Vec<AiSnapshotEntity<'_>> {
        // `build` caps the Snapshot at `LocalId::MAX` entities, so the range never runs out.
        (0..LocalId::MAX)
            .zip(&self.entities)
            .map(|(id, entity)| AiSnapshotEntity {
                id,
                name: &entity.name,
                x: entity.position.x,
                y: entity.position.y,
                tags: &entity.tags,
                passable: entity.passable,
                description: &entity.description,
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn entity(world_id: u64, x: u32, y: u32) -> SnapshotEntity {
        SnapshotEntity {
            world_id: EntityId(world_id),
            name: format!("entity {world_id}"),
            position: GridPos::new(x, y),
            tags: vec![],
            passable: true,
            description: "A test entity.".into(),
            player_character: false,
        }
    }

    fn settings(hearing: u32, max_count: usize) -> PerceptionSettings {
        PerceptionSettings {
            hearing_range_squares: hearing,
            max_count,
        }
    }

    fn world_ids(snapshot: &Snapshot) -> Vec<u64> {
        snapshot
            .entities()
            .iter()
            .map(|entity| entity.world_id.0)
            .collect()
    }

    #[test]
    fn actor_is_local_id_zero_and_others_follow_nearest_first() {
        let candidates = [entity(7, 9, 0), entity(3, 2, 0), entity(5, 5, 0)];
        let snapshot = Snapshot::build(entity(1, 0, 0), candidates, |_| false, settings(0, 10));
        assert_eq!(world_ids(&snapshot), [1, 3, 5, 7]);
    }

    #[test]
    fn hidden_entities_beyond_hearing_range_are_left_out() {
        let wall = |square: GridPos| square.x == 3;
        let candidates = [entity(2, 2, 0), entity(3, 6, 0)];
        let snapshot = Snapshot::build(entity(1, 0, 0), candidates, wall, settings(4, 10));
        assert_eq!(world_ids(&snapshot), [1, 2]);
    }

    #[test]
    fn hidden_entities_within_hearing_range_are_included() {
        let wall = |square: GridPos| square.x == 3;
        let candidates = [entity(2, 6, 0)];
        let snapshot = Snapshot::build(entity(1, 0, 0), candidates, wall, settings(6, 10));
        assert_eq!(world_ids(&snapshot), [1, 2]);
    }

    #[test]
    fn maximum_count_keeps_the_nearest_and_includes_the_actor() {
        let candidates = [entity(4, 3, 0), entity(2, 1, 0), entity(3, 2, 0)];
        let snapshot = Snapshot::build(entity(1, 0, 0), candidates, |_| false, settings(0, 3));
        assert_eq!(world_ids(&snapshot), [1, 2, 3]);
    }

    #[test]
    fn zero_maximum_count_still_holds_the_actor() {
        let snapshot = Snapshot::build(
            entity(1, 0, 0),
            [entity(2, 1, 0)],
            |_| false,
            settings(0, 0),
        );
        assert_eq!(world_ids(&snapshot), [1]);
    }

    #[test]
    fn repeated_entities_appear_once() {
        let candidates = [entity(1, 0, 0), entity(2, 1, 0), entity(2, 1, 0)];
        let snapshot = Snapshot::build(entity(1, 0, 0), candidates, |_| false, settings(0, 10));
        assert_eq!(world_ids(&snapshot), [1, 2]);
    }

    #[test]
    fn ai_view_holds_only_the_fixed_field_set() {
        let snapshot = Snapshot::build(entity(1, 4, 2), [], |_| false, settings(0, 10));
        let json = serde_json::to_value(snapshot.ai_view()).expect("serializes");
        assert_eq!(
            json,
            serde_json::json!([{
                "id": 0,
                "name": "entity 1",
                "x": 4,
                "y": 2,
                "tags": [],
                "passable": true,
                "description": "A test entity."
            }])
        );
    }
}
