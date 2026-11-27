//! Builds Snapshots from the live replicated components of the server world.
//!
//! Rules: `docs/architecture/ai_orchestration.md` Section 1. Which entities are perceived is
//! decided by `zeta_core::snapshot`; this module only reads the components.

use bevy_ecs::prelude::{Entity, World};
use zeta_core::entity::EntityId;
use zeta_core::grid::GridPos;
use zeta_core::protocol::properties::{
    AbilityScores, ActiveEffects, ArmorClass, Background, CharacterBio, CharacterTraits,
    ClassLevels, CoinPurse, Concentration, ConditionImmunities, Conditions, CreatureType,
    DamageAffinities, DeathSaves, Health, MonsterProfile, Proficiencies, ResourcePools, Senses,
    Size, Species, Speed, Spellbook,
};
use zeta_core::protocol::{Collider, EntityName, PlayerCharacter, Tags, Visual};
use zeta_core::snapshot::{PerceptionSettings, Snapshot, SnapshotEntity, SnapshotProperties};

/// The Snapshot for `actor` from the current world, or `None` if the actor no longer exists.
///
/// No entity blocks sight yet: the prototype defines no sight-blocking tag or component, so
/// every entity is in line of sight and the Snapshot keeps the nearest up to the maximum count.
pub(crate) fn build_snapshot(world: &mut World, actor: EntityId) -> Option<Snapshot> {
    let mut entities = world.query::<(
        Entity,
        &EntityId,
        &GridPos,
        &EntityName,
        &Visual,
        &Tags,
        &Collider,
        Option<&PlayerCharacter>,
    )>();
    let mut actor_entity = None;
    let mut candidates = Vec::new();
    for (handle, &world_id, &position, name, visual, tags, collider, player) in entities.iter(world)
    {
        let entity = SnapshotEntity {
            world_id,
            name: name.0.clone(),
            position,
            tags: tags.0.iter().cloned().collect(),
            passable: collider.passable,
            description: visual.0.clone(),
            player_character: player.is_some(),
            properties: entity_properties(world, handle),
        };
        if world_id == actor {
            actor_entity = Some(entity);
        } else {
            candidates.push(entity);
        }
    }
    Some(Snapshot::build(
        actor_entity?,
        candidates,
        |_| false,
        PerceptionSettings::default(),
    ))
}

/// Collects only the 2024 5e property components attached to `entity`, or `None` if none exist.
fn entity_properties(world: &World, entity: Entity) -> Option<SnapshotProperties> {
    let props = SnapshotProperties {
        health: world.get::<Health>(entity).copied(),
        armor_class: world.get::<ArmorClass>(entity).copied(),
        size: world.get::<Size>(entity).copied(),
        damage_affinities: world.get::<DamageAffinities>(entity).cloned(),
        condition_immunities: world.get::<ConditionImmunities>(entity).cloned(),
        creature_type: world.get::<CreatureType>(entity).copied(),
        ability_scores: world.get::<AbilityScores>(entity).copied(),
        speed: world.get::<Speed>(entity).copied(),
        senses: world.get::<Senses>(entity).copied(),
        proficiencies: world.get::<Proficiencies>(entity).cloned(),
        species: world.get::<Species>(entity).map(|species| species.id),
        background: world.get::<Background>(entity).map(|bg| bg.id),
        class_levels: world.get::<ClassLevels>(entity).cloned(),
        traits: world.get::<CharacterTraits>(entity).cloned(),
        spellbook: world.get::<Spellbook>(entity).cloned(),
        coin_purse: world.get::<CoinPurse>(entity).copied(),
        bio: world.get::<CharacterBio>(entity).cloned(),
        conditions: world.get::<Conditions>(entity).cloned(),
        death_saves: world.get::<DeathSaves>(entity).copied(),
        concentration: world.get::<Concentration>(entity).cloned(),
        active_effects: world.get::<ActiveEffects>(entity).cloned(),
        resource_pools: world.get::<ResourcePools>(entity).cloned(),
        monster_profile: world.get::<MonsterProfile>(entity).copied(),
    };
    (!props.is_empty()).then_some(props)
}

/// The live square of a world entity.
pub(crate) fn live_position(world: &mut World, id: EntityId) -> Option<GridPos> {
    let mut positions = world.query::<(&EntityId, &GridPos)>();
    positions
        .iter(world)
        .find(|(world_id, _)| **world_id == id)
        .map(|(_, position)| *position)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn spawn(world: &mut World, id: u64, x: u32, player: bool) {
        let mut entity = world.spawn((
            EntityId(id),
            GridPos::new(x, 0),
            EntityName(format!("entity {id}")),
            Visual("A test entity.".into()),
            Tags(BTreeSet::from(["wooden".to_owned()])),
            Collider { passable: false },
        ));
        if player {
            entity.insert(PlayerCharacter);
        }
    }

    #[test]
    fn the_snapshot_reads_live_components_with_the_actor_first() {
        let mut world = World::new();
        spawn(&mut world, 5, 3, false);
        spawn(&mut world, 1, 0, true);
        let snapshot = build_snapshot(&mut world, EntityId(1)).expect("actor exists");
        let actor = snapshot.actor().expect("actor present");
        assert_eq!(actor.world_id, EntityId(1));
        assert!(actor.player_character);
        assert_eq!(actor.tags, ["wooden"]);
        assert_eq!(snapshot.entities()[1].world_id, EntityId(5));
        assert!(!snapshot.entities()[1].passable);
    }

    #[test]
    fn a_missing_actor_gives_no_snapshot() {
        let mut world = World::new();
        spawn(&mut world, 5, 3, false);
        assert_eq!(build_snapshot(&mut world, EntityId(1)), None);
        assert_eq!(
            live_position(&mut world, EntityId(5)),
            Some(GridPos::new(3, 0))
        );
    }

    #[test]
    fn snapshot_includes_attached_5e_properties_and_omits_absent_ones() {
        let mut world = World::new();
        spawn(&mut world, 1, 0, true);
        let barrel = world
            .spawn((
                EntityId(2),
                GridPos::new(2, 0),
                EntityName("Barrel".into()),
                Visual("A wooden barrel.".into()),
                Tags(BTreeSet::from(["wooden".to_owned()])),
                Collider { passable: false },
                Health::full(18),
                ArmorClass::new(15),
                Size::Medium,
            ))
            .id();
        let _ = barrel;

        let snapshot = build_snapshot(&mut world, EntityId(1)).expect("actor exists");
        assert_eq!(snapshot.entities()[0].properties, None);
        let barrel_props = snapshot.entities()[1]
            .properties
            .as_ref()
            .expect("barrel properties present");
        assert_eq!(barrel_props.health, Some(Health::full(18)));
        assert_eq!(barrel_props.armor_class, Some(ArmorClass::new(15)));
        assert_eq!(barrel_props.size, Some(Size::Medium));
        assert_eq!(barrel_props.class_levels, None);
    }
}
