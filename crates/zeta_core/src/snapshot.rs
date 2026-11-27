//! The Snapshot: every entity the acting character could plausibly perceive.
//!
//! Rules: `docs/architecture/ai_orchestration.md` Section 1. The acting character is always
//! local identifier 0, and the rest follow nearest first, so the same world always gives
//! the same Snapshot.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::entity::EntityId;
use crate::grid::{GridPos, MapBounds, has_line_of_sight};
use crate::protocol::properties::{
    AbilityScores, ActiveEffects, ArmorClass, BackgroundId, CharacterBio, CharacterTraits,
    ClassLevels, CoinPurse, Concentration, ConditionImmunities, Conditions, CreatureType,
    DamageAffinities, DeathSaves, Health, MonsterProfile, Proficiencies, ResourcePools, Senses,
    Size, SpeciesId, Speed, Spellbook,
};
use crate::resolution::LocalId;
use crate::settings::{HEARING_RANGE_SQUARES, SNAPSHOT_MAX_COUNT};

/// Optional 2024 5e ECS property summary attached to a [`SnapshotEntity`].
///
/// Only components actually present on the entity are serialized, so simple props omit
/// character fields and non-breakable terrain omits combat vitals.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotProperties {
    /// Hit points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<Health>,
    /// Armor Class and optional structural damage threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub armor_class: Option<ArmorClass>,
    /// Size category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Size>,
    /// Damage resistances, immunities, and vulnerabilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub damage_affinities: Option<DamageAffinities>,
    /// Condition immunities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition_immunities: Option<ConditionImmunities>,
    /// Creature type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creature_type: Option<CreatureType>,
    /// Core ability scores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ability_scores: Option<AbilityScores>,
    /// Movement speeds in feet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<Speed>,
    /// Special perception ranges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub senses: Option<Senses>,
    /// Saving throw, skill, weapon, armor, tool, and language proficiencies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proficiencies: Option<Proficiencies>,
    /// Playable species.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub species: Option<SpeciesId>,
    /// Character background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<BackgroundId>,
    /// Class levels and hit dice pools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_levels: Option<ClassLevels>,
    /// Features, feats, attunement, and Heroic Inspiration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traits: Option<CharacterTraits>,
    /// Accessible spells across classes, species, feats, and items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spellbook: Option<Spellbook>,
    /// Carried coinage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coin_purse: Option<CoinPurse>,
    /// Roleplay bio notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bio: Option<CharacterBio>,
    /// Active conditions, exhaustion level, and hazards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<Conditions>,
    /// Death saving throw progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub death_saves: Option<DeathSaves>,
    /// Active Concentration state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concentration: Option<Concentration>,
    /// Active buffs, debuffs, stances, and cover modifiers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_effects: Option<ActiveEffects>,
    /// Spell slots, class pools, feature uses, and item charges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_pools: Option<ResourcePools>,
    /// Monster challenge rating and initiative bonus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monster_profile: Option<MonsterProfile>,
}

impl SnapshotProperties {
    /// True when no 5e property component is present.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

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
    /// Optional 2024 5e ECS properties attached to this entity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<SnapshotProperties>,
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

/// Padding in squares around the Snapshot's perceived entities when computing the local
/// coordinate origin and local validation window on large (planetary) maps.
const LOCAL_ORIGIN_PADDING_SQUARES: u32 = 10;
/// Maps whose width and height are both at most this many squares keep origin `(0, 0)` and
/// validate against the full map bounds directly.
const DIRECT_COORDINATE_MAX_EXTENT: u32 = 128;
/// Minimum local validation window width and height on planetary maps.
const MIN_LOCAL_WINDOW_EXTENT: u32 = 40;

/// The entities sent to the AI DM with 1 Freeform Action Prompt.
///
/// The position of an entity in the list is its [`LocalId`]. On large planetary maps,
/// coordinates exposed to the AI DM via [`Self::ai_view`] are shifted by [`Self::origin`] so
/// the model works with small 2-digit integers instead of 7-digit planetary coordinates, and
/// `move`/`spawn` effects are validated against the local window before translating back to
/// world coordinates via [`Self::to_world_pos`]. Recorded in the Event Log, so any stored
/// effect can be traced back to the entity and world square it named.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    entities: Vec<SnapshotEntity>,
    /// World square corresponding to local coordinate `(0, 0)` in [`Self::ai_view`].
    #[serde(default = "default_origin")]
    origin: GridPos,
}

const fn default_origin() -> GridPos {
    GridPos::new(0, 0)
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
    /// Optional 2024 5e ECS property summary when attached to the entity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<&'a SnapshotProperties>,
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
        let origin = compute_local_origin(&entities);
        Self { entities, origin }
    }

    /// World square corresponding to local `(0, 0)` in [`Self::ai_view`].
    pub const fn origin(&self) -> GridPos {
        self.origin
    }

    /// Translates a world square into Snapshot-local coordinates, if inside the shifted frame.
    pub const fn to_local_pos(&self, world: GridPos) -> Option<(u32, u32)> {
        let Some(x) = world.x.checked_sub(self.origin.x) else {
            return None;
        };
        let Some(y) = world.y.checked_sub(self.origin.y) else {
            return None;
        };
        Some((x, y))
    }

    /// Translates Snapshot-local coordinates from an AI DM effect back to a world [`GridPos`].
    pub const fn to_world_pos(&self, local_x: u32, local_y: u32) -> Option<GridPos> {
        let Some(x) = self.origin.x.checked_add(local_x) else {
            return None;
        };
        let Some(y) = self.origin.y.checked_add(local_y) else {
            return None;
        };
        Some(GridPos::new(x, y))
    }

    /// Valid local coordinate bounds for `move` and `spawn` effects against `world_bounds`.
    ///
    /// Small maps (at most `128x128` at origin `(0, 0)`) return `world_bounds` directly.
    /// Large planetary maps bound effects to a tight window around the Snapshot's entities so
    /// coordinate transcription typos are rejected and retried instead of teleporting entities
    /// across the planet.
    pub fn local_bounds(&self, world_bounds: MapBounds) -> MapBounds {
        if self.origin == GridPos::new(0, 0)
            && world_bounds.width <= DIRECT_COORDINATE_MAX_EXTENT
            && world_bounds.height <= DIRECT_COORDINATE_MAX_EXTENT
        {
            return world_bounds;
        }
        let max_local_x = self
            .entities
            .iter()
            .map(|entity| entity.position.x.saturating_sub(self.origin.x))
            .max()
            .unwrap_or(0);
        let max_local_y = self
            .entities
            .iter()
            .map(|entity| entity.position.y.saturating_sub(self.origin.y))
            .max()
            .unwrap_or(0);
        let avail_w = world_bounds.width.saturating_sub(self.origin.x);
        let avail_h = world_bounds.height.saturating_sub(self.origin.y);
        let width = max_local_x
            .saturating_add(LOCAL_ORIGIN_PADDING_SQUARES)
            .saturating_add(1)
            .max(MIN_LOCAL_WINDOW_EXTENT)
            .min(avail_w);
        let height = max_local_y
            .saturating_add(LOCAL_ORIGIN_PADDING_SQUARES)
            .saturating_add(1)
            .max(MIN_LOCAL_WINDOW_EXTENT)
            .min(avail_h);
        MapBounds { width, height }
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

    /// The Snapshot as the AI DM receives it (coordinates shifted by [`Self::origin`]).
    pub fn ai_view(&self) -> Vec<AiSnapshotEntity<'_>> {
        // `build` caps the Snapshot at `LocalId::MAX` entities, so the range never runs out.
        (0..LocalId::MAX)
            .zip(&self.entities)
            .map(|(id, entity)| AiSnapshotEntity {
                id,
                name: &entity.name,
                x: entity.position.x.saturating_sub(self.origin.x),
                y: entity.position.y.saturating_sub(self.origin.y),
                tags: &entity.tags,
                passable: entity.passable,
                description: &entity.description,
                properties: entity.properties.as_ref(),
            })
            .collect()
    }
}

/// Keeps origin `(0, 0)` when all Snapshot entities already live near the world origin;
/// shifts planetary coordinates so the AI DM sees small 2-digit coordinates.
fn compute_local_origin(entities: &[SnapshotEntity]) -> GridPos {
    let max_x = entities.iter().map(|e| e.position.x).max().unwrap_or(0);
    let max_y = entities.iter().map(|e| e.position.y).max().unwrap_or(0);
    if max_x < DIRECT_COORDINATE_MAX_EXTENT && max_y < DIRECT_COORDINATE_MAX_EXTENT {
        return GridPos::new(0, 0);
    }
    let min_x = entities.iter().map(|e| e.position.x).min().unwrap_or(0);
    let min_y = entities.iter().map(|e| e.position.y).min().unwrap_or(0);
    GridPos::new(
        min_x.saturating_sub(LOCAL_ORIGIN_PADDING_SQUARES),
        min_y.saturating_sub(LOCAL_ORIGIN_PADDING_SQUARES),
    )
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
            properties: None,
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

    #[test]
    fn ai_view_includes_only_attached_5e_properties() {
        let mut actor = entity(1, 4, 2);
        actor.properties = Some(SnapshotProperties {
            health: Some(Health::full(18)),
            armor_class: Some(ArmorClass::new(15)),
            ..SnapshotProperties::default()
        });
        let snapshot = Snapshot::build(actor, [], |_| false, settings(0, 10));
        let json = serde_json::to_value(snapshot.ai_view()).expect("serializes");
        assert_eq!(
            json[0]["properties"],
            serde_json::json!({
                "health": { "current": 18, "max": 18, "temporary": 0 },
                "armor_class": { "base": 15, "damage_threshold": 0 }
            })
        );
    }

    #[test]
    fn planetary_coordinates_shift_into_small_local_window_and_round_trip() {
        let actor = entity(1, 1_819_652, 318_474);
        let barrel = entity(2, 1_819_656, 318_473);
        let snapshot = Snapshot::build(actor, [barrel], |_| false, settings(10, 20));
        assert_eq!(snapshot.origin(), GridPos::new(1_819_642, 318_463));

        let view = snapshot.ai_view();
        assert_eq!((view[0].x, view[0].y), (10, 11));
        assert_eq!((view[1].x, view[1].y), (14, 10));
        assert_eq!(
            snapshot.to_world_pos(view[1].x, view[1].y),
            Some(GridPos::new(1_819_656, 318_473))
        );

        let face = MapBounds {
            width: 2_097_152,
            height: 2_097_152,
        };
        let local = snapshot.local_bounds(face);
        assert!(local.contains(15, 10));
        assert!(!local.contains(1_819_656, 1_819_656));
    }
}
