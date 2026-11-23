//! Carried items and the item verbs a player sends about them.
//!
//! Rules: `docs/design/game_design.md` Section 5. Protocol rules for every type here are in
//! the parent module docs.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::catalog::CatalogId;
use crate::entity::EntityId;

/// Identifies 1 stack inside 1 [`Inventory`]. Never reused within that inventory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StackId(pub u32);

/// A narrative-only item the AI DM created for a unique story object. It has no combat
/// stats and no crafting recipe (`docs/design/game_design.md` Section 5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomItem {
    /// Display name.
    pub name: String,
    /// Weight in thousandths of a pound, like catalog items.
    pub weight_thousandths_of_pound: u32,
    /// 1-sentence description.
    pub description: String,
    /// Free-form narrative tags such as `sealed`.
    pub tags: BTreeSet<String>,
}

/// What a stack holds: a catalog item or a custom narrative item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// An item from the item catalog.
    Catalog(CatalogId),
    /// A narrative-only item.
    Custom(CustomItem),
}

impl Item {
    /// The catalog id, when this is a catalog item.
    pub fn catalog_id(&self) -> Option<&CatalogId> {
        match self {
            Self::Catalog(id) => Some(id),
            Self::Custom(_) => None,
        }
    }
}

/// 1 ordered entry in an [`Inventory`]: an item, how many, and whether it is equipped.
///
/// Sent as a flat struct with 1 of `catalog_id` or `custom` set, because the networking
/// format cannot carry an enum with data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ItemStackFields", into = "ItemStackFields")]
pub struct ItemStack {
    id: StackId,
    item: Item,
    quantity: u32,
    equipped: bool,
}

impl ItemStack {
    /// Stack id, unique within its inventory.
    pub fn id(&self) -> StackId {
        self.id
    }

    /// What the stack holds.
    pub fn item(&self) -> &Item {
        &self.item
    }

    /// How many; always at least 1.
    pub fn quantity(&self) -> u32 {
        self.quantity
    }

    /// True when the whole stack is equipped.
    pub fn equipped(&self) -> bool {
        self.equipped
    }
}

/// Sent and stored form of [`ItemStack`].
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ItemStackFields {
    id: StackId,
    catalog_id: Option<CatalogId>,
    custom: Option<CustomItem>,
    quantity: u32,
    equipped: bool,
}

/// Why an [`ItemStack`] could not be read.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum InvalidItemStack {
    /// Neither or both of `catalog_id` and `custom` were set.
    #[error("an item stack needs exactly 1 of catalog_id or custom")]
    ItemKind,
    /// The quantity was 0.
    #[error("an item stack needs a quantity of at least 1")]
    ZeroQuantity,
}

impl TryFrom<ItemStackFields> for ItemStack {
    type Error = InvalidItemStack;

    fn try_from(fields: ItemStackFields) -> Result<Self, Self::Error> {
        let item = match (fields.catalog_id, fields.custom) {
            (Some(id), None) => Item::Catalog(id),
            (None, Some(custom)) => Item::Custom(custom),
            _ => return Err(InvalidItemStack::ItemKind),
        };
        if fields.quantity == 0 {
            return Err(InvalidItemStack::ZeroQuantity);
        }
        Ok(Self {
            id: fields.id,
            item,
            quantity: fields.quantity,
            equipped: fields.equipped,
        })
    }
}

impl From<ItemStack> for ItemStackFields {
    fn from(stack: ItemStack) -> Self {
        let (catalog_id, custom) = match stack.item {
            Item::Catalog(id) => (Some(id), None),
            Item::Custom(custom) => (None, Some(custom)),
        };
        Self {
            id: stack.id,
            catalog_id,
            custom,
            quantity: stack.quantity,
            equipped: stack.equipped,
        }
    }
}

/// Why an inventory change was refused.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum InventoryError {
    /// No stack has this id.
    #[error("no item stack {0:?}")]
    NoSuchStack(StackId),
    /// The stack holds fewer items than asked for.
    #[error("the stack holds {held}, not {wanted}")]
    NotEnough {
        /// Items in the stack.
        held: u32,
        /// Items asked for.
        wanted: u32,
    },
    /// A quantity of 0 was asked for.
    #[error("the quantity must be at least 1")]
    ZeroQuantity,
    /// The stack would hold more than `u32::MAX` items.
    #[error("the stack cannot hold that many items")]
    TooMany,
}

/// Items an entity carries, in display order. Replicated to every client.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct Inventory {
    stacks: Vec<ItemStack>,
    next_stack_id: u32,
}

impl Inventory {
    /// Every stack in display order.
    pub fn stacks(&self) -> &[ItemStack] {
        &self.stacks
    }

    /// The stack with this id.
    pub fn stack(&self, id: StackId) -> Option<&ItemStack> {
        self.stacks.iter().find(|stack| stack.id == id)
    }

    /// True when no stack is held.
    pub fn is_empty(&self) -> bool {
        self.stacks.is_empty()
    }

    /// True when an unequipped or equipped stack holds this catalog item.
    pub fn contains_catalog(&self, id: &CatalogId) -> bool {
        self.stacks
            .iter()
            .any(|stack| stack.item.catalog_id() == Some(id))
    }

    /// Adds items and returns the stack that holds them. A catalog item joins an unequipped
    /// stack of the same item; a custom item always gets its own stack.
    ///
    /// # Errors
    /// [`InventoryError::ZeroQuantity`] when `quantity` is 0, and
    /// [`InventoryError::TooMany`] when the joined stack would pass `u32::MAX`.
    pub fn add(&mut self, item: Item, quantity: u32) -> Result<StackId, InventoryError> {
        if quantity == 0 {
            return Err(InventoryError::ZeroQuantity);
        }
        if let Item::Catalog(id) = &item
            && let Some(stack) = self
                .stacks
                .iter_mut()
                .find(|stack| !stack.equipped && stack.item.catalog_id() == Some(id))
        {
            stack.quantity = stack
                .quantity
                .checked_add(quantity)
                .ok_or(InventoryError::TooMany)?;
            return Ok(stack.id);
        }
        let id = StackId(self.next_stack_id);
        self.next_stack_id = self.next_stack_id.saturating_add(1);
        self.stacks.push(ItemStack {
            id,
            item,
            quantity,
            equipped: false,
        });
        Ok(id)
    }

    /// Adds items like [`Inventory::add`] and returns only the added part: the holding
    /// stack's id and equipped state with `quantity`, for change records.
    ///
    /// # Errors
    /// As [`Inventory::add`].
    pub fn gain(&mut self, item: Item, quantity: u32) -> Result<ItemStack, InventoryError> {
        let id = self.add(item, quantity)?;
        let stack = self.stack(id).ok_or(InventoryError::NoSuchStack(id))?;
        Ok(ItemStack {
            quantity,
            ..stack.clone()
        })
    }

    /// Removes `quantity` items from a stack and returns them as a stack with the same id
    /// and equipped state. The stack disappears when emptied.
    ///
    /// # Errors
    /// [`InventoryError`] when the stack is missing, too small, or `quantity` is 0.
    pub fn remove(&mut self, id: StackId, quantity: u32) -> Result<ItemStack, InventoryError> {
        if quantity == 0 {
            return Err(InventoryError::ZeroQuantity);
        }
        let index = self
            .stacks
            .iter()
            .position(|stack| stack.id == id)
            .ok_or(InventoryError::NoSuchStack(id))?;
        let held = self.stacks[index].quantity;
        if held < quantity {
            return Err(InventoryError::NotEnough {
                held,
                wanted: quantity,
            });
        }
        if held == quantity {
            return Ok(self.stacks.remove(index));
        }
        let stack = &mut self.stacks[index];
        stack.quantity = held - quantity;
        Ok(ItemStack {
            quantity,
            ..stack.clone()
        })
    }

    /// Removes up to `quantity` items matching `matches`, oldest stacks first, and returns
    /// what was removed. Removes nothing when nothing matches.
    pub fn remove_matching(
        &mut self,
        quantity: u32,
        matches: impl Fn(&Item) -> bool,
    ) -> Vec<ItemStack> {
        let mut removed = Vec::new();
        let mut remaining = quantity;
        while remaining > 0 {
            let Some(stack) = self.stacks.iter().find(|stack| matches(&stack.item)) else {
                break;
            };
            let take = remaining.min(stack.quantity);
            let id = stack.id;
            match self.remove(id, take) {
                Ok(stack) => removed.push(stack),
                Err(_) => break,
            }
            remaining -= take;
        }
        removed
    }

    /// Sets whether a whole stack is equipped, and returns the changed stack.
    ///
    /// # Errors
    /// [`InventoryError::NoSuchStack`] when the stack is missing.
    pub fn set_equipped(
        &mut self,
        id: StackId,
        equipped: bool,
    ) -> Result<ItemStack, InventoryError> {
        let stack = self
            .stacks
            .iter_mut()
            .find(|stack| stack.id == id)
            .ok_or(InventoryError::NoSuchStack(id))?;
        stack.equipped = equipped;
        Ok(stack.clone())
    }
}

/// A deterministic action on 1 carried item (`docs/design/game_design.md` Section 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemVerb {
    /// Wield or wear the stack.
    Equip,
    /// Stop wielding or wearing the stack.
    Unequip,
    /// Put items on the ground at the character's square.
    Drop,
    /// Hand items to an adjacent creature.
    Give,
    /// Eat or drink 1 item.
    Consume,
    /// Retired: crafting was removed until its material rules are designed. Kept because
    /// network variants are never removed (`protocol` module docs) and old Event Log records
    /// may hold it; no stack offers it and the server refuses it.
    Craft,
    /// Use a tool through a Freeform Action Prompt. The client sends a
    /// `FreeformActionPrompt` with `utilize` set instead of this verb.
    Utilize,
}

/// Client to server: apply 1 item verb to 1 stack of the player's character.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Message))]
pub struct ItemVerbRequest {
    /// The verb.
    pub verb: ItemVerb,
    /// The stack it acts on.
    pub stack: StackId,
    /// How many items, for `Drop` and `Give`; other verbs ignore it.
    pub quantity: u32,
    /// Who receives the items, for `Give`.
    pub recipient: Option<EntityId>,
    /// Retired with `Craft`; kept because network fields are never removed. Clients send `None`.
    pub craft_target: Option<CatalogId>,
}

#[cfg(test)]
mod tests {
    use super::super::assert_json_round_trip;
    use super::*;

    fn catalog(id: &str) -> Item {
        Item::Catalog(CatalogId::new(id).expect("valid id"))
    }

    fn letter() -> Item {
        Item::Custom(CustomItem {
            name: "Sealed Letter".to_owned(),
            weight_thousandths_of_pound: 10,
            description: "A letter sealed with the baron's crest.".to_owned(),
            tags: BTreeSet::from(["sealed".to_owned()]),
        })
    }

    #[test]
    fn inventory_and_verbs_round_trip_through_json() {
        let mut inventory = Inventory::default();
        inventory.add(catalog("rope"), 2).expect("adds");
        inventory.add(letter(), 1).expect("adds");
        assert_json_round_trip(&inventory);
        assert_json_round_trip(&ItemVerbRequest {
            verb: ItemVerb::Give,
            stack: StackId(1),
            quantity: 1,
            recipient: Some(EntityId(4)),
            craft_target: None,
        });
        // The retired `Craft` stays readable, because old Event Log records may hold it.
        assert_json_round_trip(&ItemVerbRequest {
            verb: ItemVerb::Craft,
            stack: StackId(0),
            quantity: 1,
            recipient: None,
            craft_target: Some(CatalogId::new("druidic_focus").expect("valid id")),
        });
    }

    #[test]
    fn stacks_need_exactly_1_item_kind_and_a_quantity() {
        let both = r#"{"id":0,"catalog_id":"rope","custom":{"name":"a","weight_thousandths_of_pound":0,"description":"b","tags":[]},"quantity":1,"equipped":false}"#;
        let neither = r#"{"id":0,"catalog_id":null,"custom":null,"quantity":1,"equipped":false}"#;
        let zero = r#"{"id":0,"catalog_id":"rope","custom":null,"quantity":0,"equipped":false}"#;
        for text in [both, neither, zero] {
            assert!(serde_json::from_str::<ItemStack>(text).is_err(), "{text}");
        }
    }

    #[test]
    fn catalog_items_merge_and_custom_items_do_not() {
        let mut inventory = Inventory::default();
        let rope = inventory.add(catalog("rope"), 1).expect("adds");
        assert_eq!(inventory.add(catalog("rope"), 2), Ok(rope));
        let first = inventory.add(letter(), 1).expect("adds");
        let second = inventory.add(letter(), 1).expect("adds");
        assert_ne!(first, second);
        assert_eq!(inventory.stack(rope).map(ItemStack::quantity), Some(3));
        assert_eq!(
            inventory.add(catalog("rope"), 0),
            Err(InventoryError::ZeroQuantity)
        );
        assert_eq!(
            inventory.add(catalog("rope"), u32::MAX),
            Err(InventoryError::TooMany)
        );
        assert_eq!(inventory.stack(rope).map(ItemStack::quantity), Some(3));
    }

    #[test]
    fn equipped_stacks_do_not_absorb_new_items() {
        let mut inventory = Inventory::default();
        let sword = inventory.add(catalog("longsword"), 1).expect("adds");
        inventory.set_equipped(sword, true).expect("equips");
        let spare = inventory.add(catalog("longsword"), 1).expect("adds");
        assert_ne!(sword, spare);
    }

    #[test]
    fn remove_splits_and_empties_stacks() {
        let mut inventory = Inventory::default();
        let rope = inventory.add(catalog("rope"), 3).expect("adds");
        let part = inventory.remove(rope, 2).expect("removes");
        assert_eq!(part.quantity(), 2);
        assert_eq!(
            inventory.remove(rope, 5),
            Err(InventoryError::NotEnough { held: 1, wanted: 5 })
        );
        inventory.remove(rope, 1).expect("removes");
        assert!(inventory.is_empty());
        assert_eq!(
            inventory.remove(rope, 1),
            Err(InventoryError::NoSuchStack(rope))
        );
    }

    #[test]
    fn remove_matching_takes_up_to_the_quantity() {
        let mut inventory = Inventory::default();
        inventory.add(catalog("rope"), 1).expect("adds");
        inventory.add(letter(), 1).expect("adds");
        let rope = CatalogId::new("rope").expect("valid id");
        let removed = inventory.remove_matching(5, |item| item.catalog_id() == Some(&rope));
        assert_eq!(removed.len(), 1);
        assert_eq!(inventory.stacks().len(), 1);
        assert_eq!(inventory.remove_matching(1, |_| false).len(), 0);
    }
}
