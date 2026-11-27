//! Player characters: 1 spawned for each connecting player and removed when they leave.
//!
//! Rules: `docs/prototypes/realism_prototype.md` Section 1 (2 or more players share the
//! test scene) and `docs/architecture/technical_architecture.md` Section 5.

use std::collections::{BTreeMap, BTreeSet};

use bevy_ecs::prelude::{Bundle, Commands, MessageReader, MessageWriter, Query, Res, ResMut};
use zeta_core::catalog::{Ability, ArmorCategory, Skill, WeaponCategory};
use zeta_core::grid::{GridPos, MapBounds};
use zeta_core::protocol::properties::{
    AbilityScores, ActiveEffects, ArmorClass, Background, BackgroundId, CharacterTraits, ClassId,
    ClassLevels, CoinPurse, Concentration, Conditions, CreatureType, DeathSaves, FeatId, Health,
    LanguageId, Proficiencies, ProficiencyLevel, RechargeSchedule, ResourceId, ResourcePool,
    ResourcePools, RulesetId, Size, Species, SpeciesId, Speed,
};
use zeta_core::protocol::{Collider, Creature, PlayerAssigned, PlayerCharacter, TurnBubble};

use super::inventory::starting_inventory;
use super::prompts::PromptRegistry;
use super::scene::{EntitySpec, PLAYER_SPAWN_POSITION, PLAYER_STARTING_ITEMS};
use super::turn_bubble::remove_participant;
use super::{EntityIndex, NextEntityId, PlayerCharacters};
use crate::net::messages::{PlayerConnected, PlayerDisconnected, Recipients, ToPlayers};

/// Update system: spawns a character for each new player and tells that player which one.
#[allow(
    clippy::too_many_arguments,
    reason = "a Bevy system receives each resource and query it uses as a parameter"
)]
pub(crate) fn connect_players(
    mut connections: MessageReader<PlayerConnected>,
    mut commands: Commands,
    mut characters: ResMut<PlayerCharacters>,
    mut index: ResMut<EntityIndex>,
    mut next_id: ResMut<NextEntityId>,
    bounds: Option<Res<MapBounds>>,
    occupants: Query<(&GridPos, &Collider)>,
    mut assignments: MessageWriter<ToPlayers<PlayerAssigned>>,
) {
    let Some(bounds) = bounds else {
        return;
    };
    let mut taken: BTreeSet<GridPos> = occupants
        .iter()
        .filter(|(_, collider)| !collider.passable)
        .map(|(position, _)| *position)
        .collect();
    for connected in connections.read() {
        let connection = connected.connection;
        if let Some(&character) = characters.by_connection.get(&connection) {
            assignments.write(ToPlayers {
                recipients: Recipients::Only(connection),
                message: PlayerAssigned { character },
            });
            continue;
        }
        let Some(position) = free_square_near(PLAYER_SPAWN_POSITION, *bounds, &taken) else {
            eprintln!("no free square is left for a new player character");
            continue;
        };
        taken.insert(position);
        characters.joined = characters.joined.saturating_add(1);
        let id = next_id.allocate();
        let spec = EntitySpec {
            name: format!("Player {}", characters.joined),
            position,
            visual: "An adventurer controlled by a player.".into(),
            tags: Vec::new(),
            passable: false,
        };
        let entity = commands
            .spawn((
                spec.into_bundle(id),
                Creature,
                PlayerCharacter,
                starting_inventory(PLAYER_STARTING_ITEMS),
                starter_character_properties(),
            ))
            .id();
        index.0.insert(id, entity);
        characters.by_connection.insert(connection, id);
        assignments.write(ToPlayers {
            recipients: Recipients::Only(connection),
            message: PlayerAssigned { character: id },
        });
    }
}

/// Default 2024 SRD 5.2.1 Level 1 Human Soldier Fighter property bundle for new players.
fn starter_character_properties() -> impl Bundle {
    let mut features = BTreeSet::new();
    for id in ["second_wind", "weapon_mastery", "fighting_style"] {
        if let Ok(ruleset_id) = RulesetId::new(id) {
            features.insert(ruleset_id);
        }
    }
    let mut pools = ResourcePools::default();
    pools.class_pools.insert(
        ResourceId::SecondWind,
        ResourcePool {
            current: 2,
            max: 2,
            recharge: RechargeSchedule::ShortRest,
        },
    );
    (
        (
            Health::full(13),
            ArmorClass::new(16),
            Size::Medium,
            CreatureType::Humanoid,
            AbilityScores {
                strength: 16,
                dexterity: 14,
                constitution: 16,
                intelligence: 10,
                wisdom: 12,
                charisma: 8,
            },
            Speed::walk_only(30),
            Species {
                id: SpeciesId::Human,
            },
            Background {
                id: BackgroundId::Soldier,
            },
        ),
        (
            ClassLevels::single(ClassId::Fighter, 1, None),
            Proficiencies {
                saving_throws: BTreeSet::from([Ability::Strength, Ability::Constitution]),
                skills: BTreeMap::from([
                    (Skill::Athletics, ProficiencyLevel::Proficient),
                    (Skill::Perception, ProficiencyLevel::Proficient),
                ]),
                armor: BTreeSet::from([
                    ArmorCategory::Light,
                    ArmorCategory::Medium,
                    ArmorCategory::Heavy,
                    ArmorCategory::Shield,
                ]),
                weapons: BTreeSet::from([WeaponCategory::Simple, WeaponCategory::Martial]),
                weapon_ids: BTreeSet::new(),
                weapon_masteries: BTreeSet::new(),
                tools: BTreeSet::new(),
                languages: BTreeSet::from([LanguageId::Common]),
            },
            CharacterTraits {
                features,
                feats: BTreeSet::from([FeatId::Defense]),
                attuned_items: Vec::new(),
                heroic_inspiration: true,
            },
            CoinPurse {
                gp: 15,
                ..CoinPurse::default()
            },
            Conditions::default(),
            DeathSaves::default(),
            Concentration::default(),
            ActiveEffects::default(),
            pools,
        ),
    )
}

/// Update system: removes a departing player's character from the world and Turn Bubbles.
pub(crate) fn disconnect_players(
    mut disconnections: MessageReader<PlayerDisconnected>,
    mut commands: Commands,
    mut characters: ResMut<PlayerCharacters>,
    mut index: ResMut<EntityIndex>,
    mut bubbles: Query<&mut TurnBubble>,
    mut prompts: ResMut<PromptRegistry>,
) {
    for disconnected in disconnections.read() {
        let connection = disconnected.connection;
        prompts.forget_connection(connection);
        let Some(character) = characters.by_connection.remove(&connection) else {
            continue;
        };
        for mut bubble in &mut bubbles {
            remove_participant(&mut bubble, character);
        }
        if let Some(entity) = index.0.remove(&character) {
            commands.entity(entity).despawn();
        }
    }
}

/// The free square nearest `origin`, ties broken by row and then column.
///
/// Expands ring-by-ring in Chebyshev distance so spawning on a planetary `2^21 x 2^21` face
/// finishes in microseconds instead of scanning every square.
fn free_square_near(
    origin: GridPos,
    bounds: MapBounds,
    taken: &BTreeSet<GridPos>,
) -> Option<GridPos> {
    if bounds.width == 0 || bounds.height == 0 {
        return None;
    }
    let last_col = bounds.width - 1;
    let last_row = bounds.height - 1;
    let span_col = origin.x.max(last_col.saturating_sub(origin.x));
    let span_row = origin.y.max(last_row.saturating_sub(origin.y));
    let max_radius = span_col.max(span_row);

    for radius in 0..=max_radius {
        let col_min = origin.x.saturating_sub(radius);
        let col_max = origin.x.saturating_add(radius).min(last_col);
        let row_min = origin.y.saturating_sub(radius);
        let row_max = origin.y.saturating_add(radius).min(last_row);
        if col_min > col_max || row_min > row_max {
            continue;
        }
        let top_row = origin.y.checked_sub(radius);
        let bottom_row = origin.y.checked_add(radius);
        for row in row_min..=row_max {
            if Some(row) == top_row || Some(row) == bottom_row {
                for col in col_min..=col_max {
                    let candidate = GridPos::new(col, row);
                    if origin.grid_distance(candidate) == radius && !taken.contains(&candidate) {
                        return Some(candidate);
                    }
                }
            } else {
                for col in [col_min, col_max] {
                    let candidate = GridPos::new(col, row);
                    if origin.grid_distance(candidate) == radius && !taken.contains(&candidate) {
                        return Some(candidate);
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: MapBounds = MapBounds {
        width: 5,
        height: 5,
    };

    #[test]
    fn the_spawn_square_is_used_when_free() {
        let origin = GridPos::new(2, 2);
        assert_eq!(
            free_square_near(origin, BOUNDS, &BTreeSet::new()),
            Some(origin)
        );
    }

    #[test]
    fn a_taken_spawn_square_gives_the_nearest_free_one() {
        let origin = GridPos::new(2, 2);
        let taken = BTreeSet::from([origin, GridPos::new(1, 1)]);
        assert_eq!(
            free_square_near(origin, BOUNDS, &taken),
            Some(GridPos::new(2, 1))
        );
    }

    #[test]
    fn a_full_map_has_no_free_square() {
        let taken: BTreeSet<GridPos> = (0..5)
            .flat_map(|y| (0..5).map(move |x| GridPos::new(x, y)))
            .collect();
        assert_eq!(free_square_near(GridPos::new(0, 0), BOUNDS, &taken), None);
    }

    #[test]
    fn planetary_face_spawn_search_is_instant() {
        let planetary = MapBounds {
            width: zeta_core::world::FACE_TILES,
            height: zeta_core::world::FACE_TILES,
        };
        let origin = GridPos::new(1_819_652, 318_474);
        let taken = BTreeSet::from([origin]);
        assert_eq!(
            free_square_near(origin, planetary, &taken),
            Some(GridPos::new(origin.x - 1, origin.y - 1))
        );
    }
}
