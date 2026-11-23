//! Deterministic item rules shared by the server and the client: which verbs a stack offers,
//! and how carried items are described to the AI DM.
//!
//! Rules: `docs/design/game_design.md` Section 5 and `docs/architecture/ai_orchestration.md`
//! Section 1.

use serde_json::{Value, json};

use crate::catalog::{Catalog, CatalogItem, ItemCategory, UtilizeCheck};
use crate::protocol::inventory::{Inventory, Item, ItemStack, ItemVerb};

/// True for catalog categories a character can wield or wear.
fn equippable(category: ItemCategory) -> bool {
    matches!(
        category,
        ItemCategory::Weapon | ItemCategory::Armor | ItemCategory::SpellcastingFocus
    )
}

/// The catalog entry of a catalog stack; `None` for custom items and unknown ids.
pub fn catalog_entry<'catalog>(
    stack: &ItemStack,
    catalog: &'catalog Catalog,
) -> Option<&'catalog CatalogItem> {
    stack.item().catalog_id().and_then(|id| catalog.get(id))
}

/// Verbs the stack offers now, in display order: `Equip` or `Unequip` for weapons, armor
/// and spellcasting focuses, `Consume` for consumables, `Utilize` when the catalog lists
/// checks. The retired `Craft` is never offered. Custom items offer only `Drop` and `Give`.
pub fn available_verbs(stack: &ItemStack, catalog: &Catalog) -> Vec<ItemVerb> {
    let mut verbs = Vec::new();
    if let Some(entry) = catalog_entry(stack, catalog) {
        if equippable(entry.category) {
            verbs.push(if stack.equipped() {
                ItemVerb::Unequip
            } else {
                ItemVerb::Equip
            });
        }
        if entry.consumable {
            verbs.push(ItemVerb::Consume);
        }
        if !entry.utilize.is_empty() {
            verbs.push(ItemVerb::Utilize);
        }
    }
    verbs.push(ItemVerb::Drop);
    verbs.push(ItemVerb::Give);
    verbs
}

/// The checks `Utilize` attaches to a Freeform Action Prompt for this stack.
pub fn utilize_checks<'catalog>(
    stack: &ItemStack,
    catalog: &'catalog Catalog,
) -> &'catalog [UtilizeCheck] {
    catalog_entry(stack, catalog).map_or(&[], |entry| entry.utilize.as_slice())
}

/// Name shown to players and the AI DM: the catalog name, the custom name, or the raw id
/// when the catalog lacks it.
pub fn display_name(item: &Item, catalog: &Catalog) -> String {
    match item {
        Item::Catalog(id) => catalog
            .get(id)
            .map_or_else(|| id.to_string(), |entry| entry.name.clone()),
        Item::Custom(custom) => custom.name.clone(),
    }
}

/// A catalog entry as the AI DM sees it: every field except the SRD `crafts` list, which no
/// verb uses until crafting is designed.
pub fn entry_for_ai_dm(entry: &CatalogItem) -> Value {
    let mut value = json!(entry);
    if let Value::Object(fields) = &mut value {
        fields.remove("crafts");
    }
    value
}

/// Carried items for the AI request character summary: each stack with its quantity,
/// equipped state, and catalog entry from [`entry_for_ai_dm`], or the custom item fields.
pub fn items_summary(inventory: &Inventory, catalog: &Catalog) -> Value {
    Value::Array(
        inventory
            .stacks()
            .iter()
            .map(|stack| {
                let item = match stack.item() {
                    Item::Catalog(id) => json!({
                        "catalog_id": id,
                        "catalog_entry": catalog.get(id).map(entry_for_ai_dm),
                    }),
                    Item::Custom(custom) => json!({ "custom": custom }),
                };
                json!({
                    "name": display_name(stack.item(), catalog),
                    "quantity": stack.quantity(),
                    "equipped": stack.equipped(),
                    "item": item,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::catalog::CatalogId;
    use crate::protocol::inventory::{CustomItem, StackId};

    fn id(text: &str) -> CatalogId {
        CatalogId::new(text).expect("valid id")
    }

    fn holding(text: &str) -> (Inventory, StackId) {
        let mut inventory = Inventory::default();
        let stack = inventory.add(Item::Catalog(id(text)), 1).expect("adds");
        (inventory, stack)
    }

    fn verbs(text: &str) -> Vec<ItemVerb> {
        let (inventory, stack) = holding(text);
        let stack = inventory.stack(stack).expect("held");
        available_verbs(stack, Catalog::srd())
    }

    #[test]
    fn verbs_follow_the_catalog_entry() {
        use ItemVerb::*;
        assert_eq!(verbs("longsword"), [Equip, Drop, Give]);
        assert_eq!(verbs("rations"), [Consume, Drop, Give]);
        assert_eq!(verbs("painters_supplies"), [Utilize, Drop, Give]);
        assert_eq!(verbs("rope"), [Utilize, Drop, Give]);
    }

    #[test]
    fn equipped_stacks_offer_unequip() {
        let (mut inventory, stack) = holding("longsword");
        let equipped = inventory.set_equipped(stack, true).expect("equips");
        assert_eq!(
            available_verbs(&equipped, Catalog::srd()),
            [ItemVerb::Unequip, ItemVerb::Drop, ItemVerb::Give]
        );
    }

    #[test]
    fn custom_items_offer_only_drop_and_give() {
        let mut inventory = Inventory::default();
        let stack = inventory
            .add(
                Item::Custom(CustomItem {
                    name: "Brass Key".to_owned(),
                    weight_thousandths_of_pound: 50,
                    description: "A small brass key.".to_owned(),
                    tags: BTreeSet::new(),
                }),
                1,
            )
            .expect("adds");
        let stack = inventory.stack(stack).expect("held");
        assert_eq!(
            available_verbs(stack, Catalog::srd()),
            [ItemVerb::Drop, ItemVerb::Give]
        );
    }

    #[test]
    fn summary_includes_catalog_entries() {
        let (inventory, _) = holding("painters_supplies");
        let summary = items_summary(&inventory, Catalog::srd());
        assert_eq!(summary[0]["name"], "Painter's Supplies");
        assert_eq!(summary[0]["item"]["catalog_entry"]["category"], "tool");
        assert!(summary[0]["item"]["catalog_entry"].get("crafts").is_none());
        let (inventory, stack) = holding("painters_supplies");
        let checks = utilize_checks(inventory.stack(stack).expect("held"), Catalog::srd());
        assert_eq!(checks.len(), 1);
    }
}
