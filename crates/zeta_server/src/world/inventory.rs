//! Carried items: the deterministic item verbs players send, and the item awards and
//! removals of AI DM effects.
//!
//! Rules: `docs/design/game_design.md` Section 5 (generic verbs, no AI call for item use) and `docs/architecture/ai_orchestration.md` Section 1. `Utilize`
//! never arrives here: the client sends it as a Freeform Action Prompt with the item
//! attached. Every verb and effect that moves items is written to the Event Log; items an
//! entity carries vanish unrecorded when the entity is destroyed or its player leaves.

use bevy_ecs::prelude::{MessageReader, ResMut, Resource, World};
use zeta_core::catalog::{Catalog, CatalogId};
use zeta_core::entity::EntityId;
use zeta_core::event_log::{AppliedChange, EventCause, ItemChange, ItemChangeKind, ItemVerbCause};
use zeta_core::grid::GridPos;
use zeta_core::item_rules::{available_verbs, display_name};
use zeta_core::pending_resolution::ChangeKind;
use zeta_core::protocol::{
    Collider, Inventory, InventoryError, Item, ItemStack, ItemVerb, ItemVerbRequest, LogEntryKind,
};

use super::effects::broadcast_line;
use super::scene::EntitySpec;
use super::snapshot::live_position;
use super::{EntityIndex, EventLogWriter, LogSequence, NextEntityId, PlayerCharacters, notice};
use crate::bridge::ResolutionLocksResource;
use crate::net::messages::{FromPlayer, PlayerConnection};

/// Refusal when the request names a stack the character does not carry.
const NOT_CARRIED: &str = "You no longer carry that item.";

/// Item verbs received this tick, waiting for [`run_item_verbs`].
#[derive(Resource, Default, Debug)]
pub(crate) struct IncomingItemVerbs(Vec<(PlayerConnection, ItemVerbRequest)>);

/// Update system: collects item verb requests for [`run_item_verbs`].
pub(crate) fn collect_item_verbs(
    mut requests: MessageReader<FromPlayer<ItemVerbRequest>>,
    mut incoming: ResMut<IncomingItemVerbs>,
) {
    for request in requests.read() {
        incoming
            .0
            .push((request.connection, request.message.clone()));
    }
}

/// What 1 performed verb changed.
struct VerbOutcome {
    /// Action line shown to every player.
    line: String,
    /// Entities the verb spawned.
    spawned: Vec<EntityId>,
    /// Items gained and lost.
    items: Vec<ItemChange>,
}

/// Update system: performs each collected verb, or tells the player why not, and records
/// every performed verb in the Event Log.
pub(crate) fn run_item_verbs(world: &mut World) {
    let incoming = std::mem::take(&mut world.resource_mut::<IncomingItemVerbs>().0);
    for (connection, request) in incoming {
        let Some(&actor) = world
            .resource::<PlayerCharacters>()
            .by_connection
            .get(&connection)
        else {
            continue;
        };
        let position = live_position(world, actor);
        match perform(world, actor, &request) {
            Ok(outcome) => {
                broadcast_line(world, LogEntryKind::Action, Some(actor), outcome.line, None);
                let cause = EventCause::ItemVerb(ItemVerbCause { actor, request });
                let change = AppliedChange {
                    resolution: None,
                    branch: None,
                    spawned: outcome.spawned,
                    items: outcome.items,
                };
                world.resource_mut::<EventLogWriter>().record(
                    position.unwrap_or_else(|| GridPos::new(0, 0)),
                    cause,
                    change,
                );
            }
            Err(refusal) => {
                let line = notice(
                    &mut world.resource_mut::<LogSequence>(),
                    connection,
                    refusal,
                );
                world.write_message(line);
            }
        }
    }
}

/// Performs 1 verb for `actor`, or returns the refusal shown to the player.
fn perform(
    world: &mut World,
    actor: EntityId,
    request: &ItemVerbRequest,
) -> Result<VerbOutcome, String> {
    let catalog = Catalog::srd();
    if !may_change(world, actor) {
        return Err(
            "Your items cannot change while an action involving your character resolves."
                .to_owned(),
        );
    }
    let stack = read_inventory(world, actor, |inventory| {
        inventory.stack(request.stack).cloned()
    })
    .flatten()
    .ok_or(NOT_CARRIED)?;
    let name = display_name(stack.item(), catalog);
    if !available_verbs(&stack, catalog).contains(&request.verb) {
        return Err(format!("The {name} cannot be used that way."));
    }
    match request.verb {
        ItemVerb::Equip | ItemVerb::Unequip => {
            let equip = request.verb == ItemVerb::Equip;
            let changed = change_inventory(world, actor, |inventory| {
                inventory.set_equipped(stack.id(), equip)
            })?;
            let (verb, kind) = if equip {
                ("Equips", ItemChangeKind::Equipped)
            } else {
                ("Puts away", ItemChangeKind::Unequipped)
            };
            Ok(VerbOutcome {
                line: format!("{verb} the {name}."),
                spawned: Vec::new(),
                items: vec![ItemChange {
                    holder: actor,
                    kind,
                    stack: changed,
                }],
            })
        }
        ItemVerb::Consume => {
            let removed =
                change_inventory(world, actor, |inventory| inventory.remove(stack.id(), 1))?;
            Ok(VerbOutcome {
                line: format!("Uses up 1 {name}."),
                spawned: Vec::new(),
                items: vec![lost(actor, removed)],
            })
        }
        ItemVerb::Drop => drop_stack(world, actor, &stack, request.quantity, &name),
        ItemVerb::Give => give(world, actor, &stack, request, &name),
        // `Utilize` travels as a Freeform Action Prompt, never as an item verb.
        ItemVerb::Utilize => Err(format!(
            "Describe how you use the {name} in the action prompt."
        )),
        // Retired; `available_verbs` never offers it, so the check above already refused it.
        ItemVerb::Craft => Err(format!("The {name} cannot be used that way.")),
    }
}

/// `Drop`: moves `quantity` items onto a new item entity on the actor's square.
fn drop_stack(
    world: &mut World,
    actor: EntityId,
    stack: &ItemStack,
    quantity: u32,
    name: &str,
) -> Result<VerbOutcome, String> {
    let position = live_position(world, actor).ok_or("Your character is not in the world.")?;
    let removed = change_inventory(world, actor, |inventory| {
        inventory.remove(stack.id(), quantity)
    })?;
    let mut ground = Inventory::default();
    let placed = ground
        .gain(removed.item().clone(), removed.quantity())
        .map_err(|error| inventory_refusal(&error))?;
    let visual = match removed.item() {
        Item::Custom(custom) => custom.description.clone(),
        Item::Catalog(_) => format!("{name} lying on the ground."),
    };
    let pile_name = if removed.quantity() > 1 {
        format!("{name} ({})", removed.quantity())
    } else {
        name.to_owned()
    };
    let id = world.resource_mut::<NextEntityId>().allocate();
    let spec = EntitySpec {
        name: pile_name,
        position,
        visual,
        tags: Vec::new(),
        passable: true,
    };
    let entity = world.spawn((spec.into_bundle(id), ground)).id();
    world.resource_mut::<EntityIndex>().0.insert(id, entity);
    let count = removed.quantity();
    Ok(VerbOutcome {
        line: format!("Drops {count} {name}."),
        spawned: vec![id],
        items: vec![lost(actor, removed), gained(id, placed)],
    })
}

/// `Give`: moves `quantity` items to an adjacent creature (a blocking entity) that carries
/// an inventory. Dropped item piles are passable, so they never receive.
fn give(
    world: &mut World,
    actor: EntityId,
    stack: &ItemStack,
    request: &ItemVerbRequest,
    name: &str,
) -> Result<VerbOutcome, String> {
    let recipient = request
        .recipient
        .filter(|&recipient| recipient != actor)
        .ok_or("Choose who to give it to.")?;
    let recipient_name = super::effects::live_name(world, recipient)
        .ok_or("Whoever you chose is no longer here.")?;
    let near = live_position(world, actor)
        .zip(live_position(world, recipient))
        .is_some_and(|(from, to)| from.grid_distance(to) <= 1);
    if !near {
        return Err(format!("Move next to {recipient_name} first."));
    }
    if read_inventory(world, recipient, |_| ()).is_none() || !blocks(world, recipient) {
        return Err(format!("{recipient_name} cannot carry items."));
    }
    if !may_change(world, recipient) {
        return Err(format!(
            "{recipient_name} is part of an action that is still resolving."
        ));
    }
    // Try the gain on a copy first, so a refused gain never removes the actor's items.
    read_inventory(world, recipient, |inventory| {
        inventory
            .clone()
            .gain(stack.item().clone(), request.quantity)
    })
    .ok_or(NOT_CARRIED)?
    .map_err(|error| inventory_refusal(&error))?;
    let removed = change_inventory(world, actor, |inventory| {
        inventory.remove(stack.id(), request.quantity)
    })?;
    let received = change_inventory(world, recipient, |inventory| {
        inventory.gain(removed.item().clone(), removed.quantity())
    })?;
    let count = removed.quantity();
    Ok(VerbOutcome {
        line: format!("Gives {count} {name} to {recipient_name}."),
        spawned: Vec::new(),
        items: vec![lost(actor, removed), gained(recipient, received)],
    })
}

/// Gives `holder` `quantity` of `item`, adding an inventory if it had none, and returns the
/// change. Nothing changes when the holder is gone or `quantity` is 0.
pub(crate) fn award(
    world: &mut World,
    holder: EntityId,
    item: Item,
    quantity: u32,
) -> Vec<ItemChange> {
    let Some(&entity) = world.resource::<EntityIndex>().0.get(&holder) else {
        return Vec::new();
    };
    if world.get::<Inventory>(entity).is_none()
        && let Ok(mut entity) = world.get_entity_mut(entity)
    {
        entity.insert(Inventory::default());
    }
    let Some(mut inventory) = world.get_mut::<Inventory>(entity) else {
        return Vec::new();
    };
    inventory
        .gain(item, quantity)
        .map_or_else(|_| Vec::new(), |stack| vec![gained(holder, stack)])
}

/// Removes up to `quantity` items matching `matches` from `holder` and returns the changes.
pub(crate) fn take(
    world: &mut World,
    holder: EntityId,
    quantity: u32,
    matches: impl Fn(&Item) -> bool,
) -> Vec<ItemChange> {
    change_inventory(world, holder, |inventory| {
        Ok(inventory.remove_matching(quantity, matches))
    })
    .unwrap_or_default()
    .into_iter()
    .map(|stack| lost(holder, stack))
    .collect()
}

/// An inventory holding the catalog items `items`, as (catalog id, quantity) pairs.
pub(crate) fn starting_inventory(items: &[(&str, u32)]) -> Inventory {
    let mut inventory = Inventory::default();
    for &(id, quantity) in items {
        // Starting items are fixed test content; a test checks every id and quantity.
        let id = CatalogId::new(id).expect("starting items use valid catalog ids");
        inventory
            .add(Item::Catalog(id), quantity)
            .expect("starting quantities are at least 1");
    }
    inventory
}

/// True when `holder` is a blocking entity, as creatures are and item piles are not.
fn blocks(world: &World, holder: EntityId) -> bool {
    world
        .resource::<EntityIndex>()
        .0
        .get(&holder)
        .and_then(|&entity| world.get::<Collider>(entity))
        .is_some_and(|collider| !collider.passable)
}

fn may_change(world: &World, character: EntityId) -> bool {
    world
        .resource::<ResolutionLocksResource>()
        .0
        .may_change(character, ChangeKind::Other)
}

/// Reads the inventory of `holder`; `None` when it is gone or carries none.
fn read_inventory<R>(
    world: &World,
    holder: EntityId,
    read: impl FnOnce(&Inventory) -> R,
) -> Option<R> {
    let entity = *world.resource::<EntityIndex>().0.get(&holder)?;
    world.get::<Inventory>(entity).map(read)
}

/// Changes the inventory of `holder`, turning a missing inventory or a failed change into
/// the refusal shown to the player.
fn change_inventory<R>(
    world: &mut World,
    holder: EntityId,
    change: impl FnOnce(&mut Inventory) -> Result<R, InventoryError>,
) -> Result<R, String> {
    let entity = *world
        .resource::<EntityIndex>()
        .0
        .get(&holder)
        .ok_or(NOT_CARRIED)?;
    let mut inventory = world.get_mut::<Inventory>(entity).ok_or(NOT_CARRIED)?;
    change(&mut inventory).map_err(|error| inventory_refusal(&error))
}

/// The fixed refusal sentence for an inventory change that failed.
fn inventory_refusal(error: &InventoryError) -> String {
    match error {
        InventoryError::NoSuchStack(_) => NOT_CARRIED.to_owned(),
        InventoryError::NotEnough { held, .. } => format!("You carry only {held}."),
        InventoryError::ZeroQuantity => "Choose at least 1.".to_owned(),
        InventoryError::TooMany => "That stack cannot hold any more.".to_owned(),
    }
}

fn gained(holder: EntityId, stack: ItemStack) -> ItemChange {
    ItemChange {
        holder,
        kind: ItemChangeKind::Gained,
        stack,
    }
}

fn lost(holder: EntityId, stack: ItemStack) -> ItemChange {
    ItemChange {
        holder,
        kind: ItemChangeKind::Lost,
        stack,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::scene::{GOBLIN_STARTING_ITEMS, PLAYER_STARTING_ITEMS};

    #[test]
    fn starting_items_are_valid_catalog_items() {
        for items in [PLAYER_STARTING_ITEMS, GOBLIN_STARTING_ITEMS] {
            for &(id, quantity) in items {
                let id = CatalogId::new(id).expect("valid id");
                assert!(Catalog::srd().contains(&id), "{id}");
                assert!(quantity > 0, "{id}");
            }
            assert_eq!(starting_inventory(items).stacks().len(), items.len());
        }
    }

    #[test]
    fn refusals_are_sentences() {
        assert_eq!(
            inventory_refusal(&InventoryError::NoSuchStack(zeta_core::protocol::StackId(
                3
            ))),
            NOT_CARRIED
        );
    }
}
