//! Executes the `WorldEffect` operations of a validated AI Resolution on the server world.
//!
//! Rules: `docs/architecture/ai_orchestration.md` Sections 1 and 3 (the operations and
//! their fields), `docs/design/game_design.md` Section 5 (item awards and removals), and
//! `docs/architecture/technical_architecture.md` Section 2. Every entity a
//! effect names is a Snapshot local identifier, mapped back to its permanent identifier here.

use bevy_ecs::prelude::{Component, Entity, World};
use zeta_core::catalog::{Ability, CatalogId};
use zeta_core::entity::EntityId;
use zeta_core::event_log::ItemChange;
use zeta_core::grid::{GridPos, MapBounds};
use zeta_core::protocol::properties::{
    AbilityScores, ActiveEffects, ArmorClass, CharacterTraits, ClassId, ClassLevels, Concentration,
    ConditionId, ConditionImmunities, Conditions, DamageAffinities, DeathSaves, Health,
    ResourcePools, RulesetId, SubclassId,
};
use zeta_core::protocol::{
    Collider, EntityName, Item, LogEntryKind, NarrationLogEntry, PlayerCharacter, Tags, Visual,
};
use zeta_core::resolution::{LocalId, Settle, Volume, WorldEffect};
use zeta_core::ruleset::{Ruleset, RulesetDomain};
use zeta_core::snapshot::Snapshot;

use super::inventory;
use super::scene::EntitySpec;
use super::turn_bubble;
use super::{EntityIndex, LogSequence, NextEntityId};
use crate::net::messages::{Recipients, ToPlayers};

/// Tag given to a player character the AI DM destroys. Player characters stay in the world
/// so their player keeps control; the prototype defines no character death rules.
pub(crate) const DEFEATED_TAG: &str = "defeated";

/// An unstable process waiting to settle into a final state. Server-only: the time model
/// that would count down `minutes` is outside the prototype, so the state is only stored.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settling(pub(crate) Settle);

/// Sends 1 line of the shared narration and speech log to every player and returns its sequence.
pub(crate) fn broadcast_line(
    world: &mut World,
    kind: LogEntryKind,
    speaker: Option<EntityId>,
    text: String,
    vol: Option<Volume>,
) -> u64 {
    let speaker_name = speaker.and_then(|id| live_name(world, id));
    let sequence = world.resource_mut::<LogSequence>().next();
    world.write_message(ToPlayers {
        recipients: Recipients::All,
        message: NarrationLogEntry {
            sequence,
            kind,
            speaker,
            speaker_name,
            text,
            vol,
        },
    });
    sequence
}

/// The display name of a world entity now.
pub(crate) fn live_name(world: &World, id: EntityId) -> Option<String> {
    let entity = *world.resource::<EntityIndex>().0.get(&id)?;
    world.get::<EntityName>(entity).map(|name| name.0.clone())
}

/// What applying a resolution's effects changed beyond the live components.
#[derive(Debug, Default)]
pub(crate) struct AppliedEffects {
    /// Identifiers given to spawned entities.
    pub(crate) spawned: Vec<EntityId>,
    /// Items gained and lost.
    pub(crate) items: Vec<ItemChange>,
    /// Log sequence numbers emitted by `Narrate` and `Speak` effects.
    pub(crate) log_sequences: Vec<u64>,
}

/// Applies `effects` in order and returns what they spawned and which items changed hands.
pub(crate) fn apply_effects(
    world: &mut World,
    snapshot: &Snapshot,
    effects: &[WorldEffect],
) -> AppliedEffects {
    let mut applied = AppliedEffects::default();
    for effect in effects {
        apply_effect(world, snapshot, effect, &mut applied);
    }
    applied
}

#[allow(
    clippy::too_many_lines,
    reason = "single top-level dispatch over every WorldEffect variant"
)]
fn apply_effect(
    world: &mut World,
    snapshot: &Snapshot,
    effect: &WorldEffect,
    applied: &mut AppliedEffects,
) {
    let spawned = &mut applied.spawned;
    match effect {
        WorldEffect::Narrate { text } => {
            let seq = broadcast_line(world, LogEntryKind::Narration, None, text.clone(), None);
            applied.log_sequences.push(seq);
        }
        WorldEffect::Speak { id, text, vol } => {
            if let Some(speaker) = world_id(snapshot, *id) {
                let seq = broadcast_line(
                    world,
                    LogEntryKind::Speech,
                    Some(speaker),
                    text.clone(),
                    Some(*vol),
                );
                applied.log_sequences.push(seq);
            }
        }
        WorldEffect::Move { id, x, y, .. } => {
            let world_pos = snapshot.to_world_pos(*x, *y);
            let in_bounds = world_pos.is_some_and(|pos| {
                world
                    .get_resource::<MapBounds>()
                    .is_some_and(|bounds| bounds.contains(pos.x, pos.y))
            });
            if let Some(pos) = world_pos
                && in_bounds
            {
                with_component::<GridPos>(world, snapshot, *id, |position| {
                    *position = pos;
                });
            } else {
                eprintln!(
                    "warning: skipped out-of-bounds move effect for local id {id} to local ({x}, {y})"
                );
            }
        }
        WorldEffect::Modify {
            id,
            add,
            remove,
            passable,
            visual,
            settle,
        } => {
            with_component::<Tags>(world, snapshot, *id, |tags| {
                for tag in remove {
                    tags.0.remove(tag);
                }
                tags.0.extend(add.iter().cloned());
            });
            if let Some(passable) = *passable {
                with_component::<Collider>(world, snapshot, *id, |collider| {
                    collider.passable = passable;
                });
            }
            if let Some(visual) = visual {
                with_component::<Visual>(world, snapshot, *id, |current| {
                    current.0.clone_from(visual);
                });
            }
            if let Some(settle) = settle
                && let Some(entity) = engine_entity(world, snapshot, *id)
                && let Ok(mut entity) = world.get_entity_mut(entity)
            {
                entity.insert(Settling(settle.clone()));
            }
        }
        WorldEffect::Spawn {
            name,
            x,
            y,
            passable,
            tags,
            visual,
            settle,
        } => {
            let Some(position) = snapshot.to_world_pos(*x, *y) else {
                eprintln!(
                    "warning: skipped spawn effect for \"{name}\" at overflowing local ({x}, {y})"
                );
                return;
            };
            let id = world.resource_mut::<NextEntityId>().allocate();
            let spec = EntitySpec {
                name: name.clone(),
                position,
                visual: visual.clone(),
                tags: tags.clone(),
                passable: *passable,
            };
            let mut entity = world.spawn(spec.into_bundle(id));
            if let Some(settle) = settle {
                entity.insert(Settling(settle.clone()));
            }
            let entity = entity.id();
            world.resource_mut::<EntityIndex>().0.insert(id, entity);
            spawned.push(id);
        }
        WorldEffect::Destroy { id, .. } => destroy(world, snapshot, *id),
        WorldEffect::Award { .. } | WorldEffect::Take { .. } => {
            applied.items.extend(item_effect(world, snapshot, effect));
        }
        // The Event Log record holds the summary; nothing in the live world changes.
        WorldEffect::History { .. } => {}
        WorldEffect::Bubble { active, ids } => {
            let participants: Vec<EntityId> = ids
                .iter()
                .filter_map(|&id| world_id(snapshot, id))
                .filter(|id| world.resource::<EntityIndex>().0.contains_key(id))
                .collect();
            if *active {
                turn_bubble::start_or_extend(world, &participants);
            } else if ids.is_empty() || !participants.is_empty() {
                turn_bubble::end_bubbles(world, &participants);
            }
        }
        WorldEffect::Damage { .. }
        | WorldEffect::Heal { .. }
        | WorldEffect::TempHp { .. }
        | WorldEffect::Condition { .. }
        | WorldEffect::ApplyEffect { .. }
        | WorldEffect::RemoveEffect { .. }
        | WorldEffect::SpendResource { .. }
        | WorldEffect::RestoreResource { .. }
        | WorldEffect::AdvanceLevel { .. } => {
            apply_property_effect(world, snapshot, effect);
        }
    }
}

/// Executes 1 typed 2024 5e property `WorldEffect` with server-authoritative rules math.
#[allow(
    clippy::too_many_lines,
    reason = "1 match dispatching the 9 typed property verbs in a single place"
)]
fn apply_property_effect(world: &mut World, snapshot: &Snapshot, effect: &WorldEffect) {
    match effect {
        WorldEffect::Damage {
            id,
            amount,
            damage_type,
        } => {
            let Some(entity) = engine_entity(world, snapshot, *id) else {
                return;
            };
            let threshold = world
                .get::<ArmorClass>(entity)
                .map_or(0, |ac| ac.damage_threshold);
            let affinities = world.get::<DamageAffinities>(entity).cloned();
            let active = world.get::<ActiveEffects>(entity).cloned();
            let Some(outcome) = world.get_mut::<Health>(entity).map(|mut hp| {
                hp.apply_damage(
                    *amount,
                    *damage_type,
                    threshold,
                    affinities.as_ref(),
                    active.as_ref(),
                )
            }) else {
                eprintln!("warning: damage effect ignored because local id {id} has no Health");
                return;
            };
            if outcome.reached_zero {
                clear_concentration(world, entity);
                if world.get::<DeathSaves>(entity).is_some() {
                    let immunities = world.get::<ConditionImmunities>(entity).cloned();
                    ensure_conditions(world, entity);
                    if let Some(mut conds) = world.get_mut::<Conditions>(entity) {
                        conds.apply_changes(
                            &[ConditionId::Unconscious],
                            &[],
                            0,
                            &[],
                            &[],
                            immunities.as_ref(),
                        );
                    }
                }
            }
        }
        WorldEffect::Heal { id, amount } => {
            let Some(entity) = engine_entity(world, snapshot, *id) else {
                return;
            };
            let revived = world.get_mut::<Health>(entity).is_some_and(|mut hp| {
                let was_zero = hp.is_zero();
                let healed = hp.apply_heal(*amount);
                was_zero && healed > 0
            });
            if revived {
                if let Some(mut saves) = world.get_mut::<DeathSaves>(entity) {
                    saves.reset();
                }
                if let Some(mut conds) = world.get_mut::<Conditions>(entity) {
                    conds.apply_changes(&[], &[ConditionId::Unconscious], 0, &[], &[], None);
                }
            }
        }
        WorldEffect::TempHp { id, amount } => {
            with_component::<Health>(world, snapshot, *id, |hp| hp.apply_temp_hp(*amount));
        }
        WorldEffect::Condition {
            id,
            add,
            remove,
            exhaustion_delta,
            add_hazards,
            remove_hazards,
        } => {
            let Some(entity) = engine_entity(world, snapshot, *id) else {
                return;
            };
            let immunities = world.get::<ConditionImmunities>(entity).cloned();
            if !add.is_empty() || !add_hazards.is_empty() || *exhaustion_delta > 0 {
                ensure_conditions(world, entity);
            }
            let broke = world
                .get_mut::<Conditions>(entity)
                .is_some_and(|mut conds| {
                    conds.apply_changes(
                        add,
                        remove,
                        *exhaustion_delta,
                        add_hazards,
                        remove_hazards,
                        immunities.as_ref(),
                    )
                });
            if broke {
                clear_concentration(world, entity);
            }
        }
        WorldEffect::ApplyEffect {
            id,
            effect,
            concentrate,
        } => {
            let Some(entity) = engine_entity(world, snapshot, *id) else {
                return;
            };
            if *concentrate {
                clear_concentration(world, entity);
                let next_conc = Concentration {
                    active_effect_id: Some(effect.id.clone()),
                    source_id: effect.source.clone(),
                    remaining_rounds: effect.duration_rounds,
                };
                if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
                    entity_mut.insert(next_conc);
                }
            }
            if world.get::<ActiveEffects>(entity).is_none()
                && let Ok(mut entity_mut) = world.get_entity_mut(entity)
            {
                entity_mut.insert(ActiveEffects::default());
            }
            if let Some(mut active) = world.get_mut::<ActiveEffects>(entity) {
                active.upsert(effect.clone());
            }
        }
        WorldEffect::RemoveEffect { id, effect_id } => {
            let Some(entity) = engine_entity(world, snapshot, *id) else {
                return;
            };
            if let Some(mut active) = world.get_mut::<ActiveEffects>(entity) {
                active.remove(effect_id);
            }
            if let Some(mut conc) = world.get_mut::<Concentration>(entity)
                && conc.active_effect_id.as_deref() == Some(effect_id.as_str())
            {
                conc.clear();
            }
        }
        WorldEffect::SpendResource { id, target, amount } => {
            if let Some(entity) = engine_entity(world, snapshot, *id) {
                if let Some(mut pools) = world.get_mut::<ResourcePools>(entity) {
                    if !pools.spend(target, *amount) {
                        eprintln!(
                            "warning: spend_resource ({target:?}, amount {amount}) could not spend on local id {id}"
                        );
                    }
                } else {
                    eprintln!(
                        "warning: spend_resource ignored because local id {id} has no ResourcePools"
                    );
                }
            }
        }
        WorldEffect::RestoreResource { id, target, amount } => {
            if let Some(entity) = engine_entity(world, snapshot, *id) {
                if let Some(mut pools) = world.get_mut::<ResourcePools>(entity) {
                    if !pools.restore(target, *amount) {
                        eprintln!(
                            "warning: restore_resource ({target:?}, amount {amount}) could not restore on local id {id}"
                        );
                    }
                } else {
                    eprintln!(
                        "warning: restore_resource ignored because local id {id} has no ResourcePools"
                    );
                }
            }
        }
        WorldEffect::AdvanceLevel {
            id,
            class,
            subclass,
        } => {
            let Some(entity) = engine_entity(world, snapshot, *id) else {
                return;
            };
            advance_entity_level(world, entity, *class, *subclass);
        }
        _ => {}
    }
}

fn ensure_conditions(world: &mut World, entity: Entity) {
    if world.get::<Conditions>(entity).is_none()
        && let Ok(mut entity_mut) = world.get_entity_mut(entity)
    {
        entity_mut.insert(Conditions::default());
    }
}

/// Clears active [`Concentration`] on `entity` and removes its concentrated [`ActiveEffect`].
fn clear_concentration(world: &mut World, entity: Entity) {
    let old_effect_id = world.get_mut::<Concentration>(entity).and_then(|mut conc| {
        let old = conc.active_effect_id.clone();
        conc.clear();
        old
    });
    if let Some(effect_id) = old_effect_id
        && let Some(mut active) = world.get_mut::<ActiveEffects>(entity)
    {
        active.remove(&effect_id);
    }
}

/// Adds 1 level in `class`, updates hit points using 2024 fixed progression, and grants
/// level-1 class features from [`Ruleset::srd`] on the first level in that class.
fn advance_entity_level(
    world: &mut World,
    entity: Entity,
    class: ClassId,
    subclass: Option<SubclassId>,
) {
    let first_level_in_class = world
        .get::<ClassLevels>(entity)
        .is_none_or(|levels| !levels.entries.iter().any(|entry| entry.class == class));
    if world.get::<ClassLevels>(entity).is_none()
        && let Ok(mut entity_mut) = world.get_entity_mut(entity)
    {
        entity_mut.insert(ClassLevels::default());
    }
    let Some(new_total_level) = world.get_mut::<ClassLevels>(entity).and_then(|mut levels| {
        levels
            .add_level(class, subclass)
            .then(|| levels.total_level())
    }) else {
        return;
    };

    let active = world.get::<ActiveEffects>(entity).cloned();
    let con_mod = world.get::<AbilityScores>(entity).map_or(0, |scores| {
        scores.modifier(Ability::Constitution, active.as_ref())
    });
    let die_gain = if new_total_level == 1 {
        i16::from(class.hit_die_sides())
    } else {
        i16::from(class.hit_die_sides() / 2 + 1)
    };
    let hp_gain = u16::try_from((die_gain + i16::from(con_mod)).max(1)).unwrap_or(1);
    if let Some(mut hp) = world.get_mut::<Health>(entity) {
        hp.max = hp.max.saturating_add(hp_gain);
        hp.current = hp.current.saturating_add(hp_gain).min(hp.max);
    }

    if first_level_in_class
        && let Some(class_entry) = Ruleset::srd().get(RulesetDomain::Class, class.as_str())
        && let Some(feature_ids) = class_entry
            .data
            .as_ref()
            .and_then(|data| data.get("level_1_features"))
            .and_then(serde_json::Value::as_array)
    {
        if world.get::<CharacterTraits>(entity).is_none()
            && let Ok(mut entity_mut) = world.get_entity_mut(entity)
        {
            entity_mut.insert(CharacterTraits::default());
        }
        if let Some(mut traits) = world.get_mut::<CharacterTraits>(entity) {
            for feature in feature_ids {
                if let Some(raw_id) = feature.as_str()
                    && let Ok(ruleset_id) = RulesetId::new(raw_id)
                {
                    traits.features.insert(ruleset_id);
                }
            }
        }
    }
}

/// Applies an award or take effect and returns the items that changed hands.
fn item_effect(world: &mut World, snapshot: &Snapshot, effect: &WorldEffect) -> Vec<ItemChange> {
    match effect {
        WorldEffect::Award {
            id,
            item,
            custom,
            qty,
        } => {
            // Validation guarantees exactly 1 of `item` and `custom`, and a known catalog id.
            let item = match (item, custom) {
                (Some(catalog_id), None) => {
                    CatalogId::new(catalog_id.as_str()).ok().map(Item::Catalog)
                }
                (None, Some(custom)) => Some(Item::Custom(custom.clone())),
                _ => None,
            };
            match (world_id(snapshot, *id), item) {
                (Some(holder), Some(item)) => inventory::award(world, holder, item, *qty),
                _ => Vec::new(),
            }
        }
        WorldEffect::Take {
            id,
            item,
            custom_name,
            qty,
        } => {
            let Some(holder) = world_id(snapshot, *id) else {
                return Vec::new();
            };
            match (item, custom_name) {
                (Some(catalog_id), None) => inventory::take(world, holder, *qty, |held| {
                    held.catalog_id()
                        .is_some_and(|held| held.as_str() == catalog_id.as_str())
                }),
                (None, Some(name)) => inventory::take(
                    world,
                    holder,
                    *qty,
                    |held| matches!(held, Item::Custom(custom) if custom.name.eq_ignore_ascii_case(name)),
                ),
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

fn destroy(world: &mut World, snapshot: &Snapshot, local: LocalId) {
    let Some(id) = world_id(snapshot, local) else {
        return;
    };
    let Some(entity) = engine_entity(world, snapshot, local) else {
        return;
    };
    if world.get::<PlayerCharacter>(entity).is_some() {
        if let Some(mut tags) = world.get_mut::<Tags>(entity) {
            tags.0.insert(DEFEATED_TAG.into());
        }
        return;
    }
    world.resource_mut::<EntityIndex>().0.remove(&id);
    world.despawn(entity);
    turn_bubble::remove_everywhere(world, id);
}

/// The permanent identifier behind a Snapshot local identifier.
fn world_id(snapshot: &Snapshot, local: LocalId) -> Option<EntityId> {
    snapshot.get(local).map(|entity| entity.world_id)
}

/// The engine entity behind a Snapshot local identifier, if it still exists.
fn engine_entity(
    world: &World,
    snapshot: &Snapshot,
    local: LocalId,
) -> Option<bevy_ecs::prelude::Entity> {
    let id = world_id(snapshot, local)?;
    world.resource::<EntityIndex>().0.get(&id).copied()
}

/// Changes 1 component of the entity a local identifier names, if both still exist.
fn with_component<C: Component<Mutability = bevy_ecs::component::Mutable>>(
    world: &mut World,
    snapshot: &Snapshot,
    local: LocalId,
    change: impl FnOnce(&mut C),
) {
    if let Some(entity) = engine_entity(world, snapshot, local)
        && let Some(mut component) = world.get_mut::<C>(entity)
    {
        change(&mut component);
    }
}
