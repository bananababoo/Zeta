//! Executes the `WorldEffect` operations of a validated AI Resolution on the server world.
//!
//! Rules: `docs/architecture/ai_orchestration.md` Sections 1 and 3 (the operations and
//! their fields) and `docs/architecture/technical_architecture.md` Section 2. Every entity a
//! effect names is a Snapshot local identifier, mapped back to its permanent identifier here.

use bevy_ecs::prelude::{Component, World};
use zeta_core::entity::EntityId;
use zeta_core::grid::{GridPos, MapBounds};
use zeta_core::protocol::{
    Collider, EntityName, LogEntryKind, NarrationLogEntry, PlayerCharacter, Tags, Visual,
};
use zeta_core::resolution::{LocalId, Settle, Volume, WorldEffect};
use zeta_core::snapshot::Snapshot;

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

/// Sends 1 line of the shared narration and speech log to every player.
pub(crate) fn broadcast_line(
    world: &mut World,
    kind: LogEntryKind,
    speaker: Option<EntityId>,
    text: String,
    vol: Option<Volume>,
) {
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
}

/// The display name of a world entity now.
pub(crate) fn live_name(world: &World, id: EntityId) -> Option<String> {
    let entity = *world.resource::<EntityIndex>().0.get(&id)?;
    world.get::<EntityName>(entity).map(|name| name.0.clone())
}

/// Applies `effects` in order and returns the identifiers given to spawned entities.
pub(crate) fn apply_effects(
    world: &mut World,
    snapshot: &Snapshot,
    effects: &[WorldEffect],
) -> Vec<EntityId> {
    let mut spawned = Vec::new();
    for effect in effects {
        apply_effect(world, snapshot, effect, &mut spawned);
    }
    spawned
}

fn apply_effect(
    world: &mut World,
    snapshot: &Snapshot,
    effect: &WorldEffect,
    spawned: &mut Vec<EntityId>,
) {
    match effect {
        WorldEffect::Narrate { text } => {
            broadcast_line(world, LogEntryKind::Narration, None, text.clone(), None);
        }
        WorldEffect::Speak { id, text, vol } => {
            if let Some(speaker) = world_id(snapshot, *id) {
                broadcast_line(
                    world,
                    LogEntryKind::Speech,
                    Some(speaker),
                    text.clone(),
                    Some(*vol),
                );
            }
        }
        WorldEffect::Move { id, x, y, .. } => {
            let in_bounds = world
                .get_resource::<MapBounds>()
                .is_some_and(|bounds| bounds.contains(*x, *y));
            if in_bounds {
                with_component::<GridPos>(world, snapshot, *id, |position| {
                    *position = GridPos::new(*x, *y);
                });
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
            let id = world.resource_mut::<NextEntityId>().allocate();
            let spec = EntitySpec {
                name: name.clone(),
                position: GridPos::new(*x, *y),
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
