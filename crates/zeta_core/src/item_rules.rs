//! Deterministic item rules shared by the server and the client: which verbs a stack offers,
//! what a tool can craft, and how carried items are described to the AI DM.
//!
//! Rules: `docs/design/game_design.md` Section 5 and `docs/architecture/ai_orchestration.md`
//! Section 1.

use serde_json::{Value, json};
use thiserror::Error;

use crate::catalog::{Catalog, CatalogId, CatalogItem, ItemCategory, UtilizeCheck};
use crate::protocol::inventory::{Inventory, Item, ItemStack, ItemVerb, StackId};

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
/// and spellcasting focuses, `Consume` for consumables, `Craft` and `Utilize` when the
/// catalog lists them. Custom items offer only `Drop` and `Give`.
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
        if !entry.crafts.is_empty() {
            verbs.push(ItemVerb::Craft);
        }
        if !entry.utilize.is_empty() {
            verbs.push(ItemVerb::Utilize);
        }
    }
    verbs.push(ItemVerb::Drop);
    verbs.push(ItemVerb::Give);
    verbs
}

/// Catalog items the stack's tool can craft, with item groups replaced by their members,
/// sorted by identifier without duplicates.
pub fn craftable_targets(stack: &ItemStack, catalog: &Catalog) -> Vec<CatalogId> {
    let Some(entry) = catalog_entry(stack, catalog) else {
        return Vec::new();
    };
    let mut targets: Vec<CatalogId> = entry
        .crafts
        .iter()
        .flat_map(|crafted| match catalog.get(crafted) {
            Some(group) if group.is_group() => group.group_members.clone(),
            _ => vec![crafted.clone()],
        })
        .filter(|target| catalog.get(target).is_some_and(|item| !item.is_group()))
        .collect();
    targets.sort();
    targets.dedup();
    targets
}

/// The checks `Utilize` attaches to a Freeform Action Prompt for this stack.
pub fn utilize_checks<'catalog>(
    stack: &ItemStack,
    catalog: &'catalog Catalog,
) -> &'catalog [UtilizeCheck] {
    catalog_entry(stack, catalog).map_or(&[], |entry| entry.utilize.as_slice())
}

/// Why a `Craft` verb was refused.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum CraftError {
    /// The character does not carry the tool stack.
    #[error("you do not carry that tool")]
    NoTool,
    /// The request named no target.
    #[error("choose what to craft")]
    NoTarget,
    /// The tool cannot make the target.
    #[error("{tool} cannot craft {target}")]
    NotCraftable {
        /// Tool name.
        tool: String,
        /// Requested catalog id.
        target: CatalogId,
    },
}

/// Checks a `Craft` request: the character carries `tool`, and that tool's catalog entry
/// crafts `target` directly or through an item group. Returns the target on success.
///
/// # Errors
/// [`CraftError`] naming the first rule that fails.
pub fn check_craft(
    inventory: &Inventory,
    tool: StackId,
    target: Option<&CatalogId>,
    catalog: &Catalog,
) -> Result<CatalogId, CraftError> {
    let stack = inventory.stack(tool).ok_or(CraftError::NoTool)?;
    let target = target.ok_or(CraftError::NoTarget)?;
    if craftable_targets(stack, catalog).contains(target) {
        return Ok(target.clone());
    }
    Err(CraftError::NotCraftable {
        tool: display_name(stack.item(), catalog),
        target: target.clone(),
    })
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

/// Carried items for the AI request character summary: each stack with its quantity,
/// equipped state, and full catalog entry, or the custom item fields.
pub fn items_summary(inventory: &Inventory, catalog: &Catalog) -> Value {
    Value::Array(
        inventory
            .stacks()
            .iter()
            .map(|stack| {
                let item = match stack.item() {
                    Item::Catalog(id) => json!({
                        "catalog_id": id,
                        "catalog_entry": catalog.get(id),
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
    use crate::protocol::inventory::CustomItem;

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
        assert_eq!(verbs("painters_supplies"), [Craft, Utilize, Drop, Give]);
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
        assert_eq!(craftable_targets(stack, Catalog::srd()).len(), 0);
    }

    #[test]
    fn craft_targets_expand_item_groups() {
        let (inventory, stack) = holding("painters_supplies");
        let targets = craftable_targets(inventory.stack(stack).expect("held"), Catalog::srd());
        assert!(targets.contains(&id("wooden_staff")), "{targets:?}");
        for group in ["druidic_focus", "holy_symbol"] {
            assert!(!targets.contains(&id(group)), "groups are not targets");
        }
        assert!(targets.len() > 1, "{targets:?}");
    }

    #[test]
    fn craft_needs_the_tool_and_a_listed_target() {
        let (inventory, stack) = holding("painters_supplies");
        let catalog = Catalog::srd();
        assert_eq!(
            check_craft(&inventory, stack, Some(&id("wooden_staff")), catalog),
            Ok(id("wooden_staff"))
        );
        assert_eq!(
            check_craft(&inventory, StackId(99), Some(&id("wooden_staff")), catalog),
            Err(CraftError::NoTool)
        );
        assert_eq!(
            check_craft(&inventory, stack, None, catalog),
            Err(CraftError::NoTarget)
        );
        assert!(matches!(
            check_craft(&inventory, stack, Some(&id("longsword")), catalog),
            Err(CraftError::NotCraftable { .. })
        ));
    }

    #[test]
    fn summary_includes_catalog_entries() {
        let (inventory, _) = holding("painters_supplies");
        let summary = items_summary(&inventory, Catalog::srd());
        assert_eq!(summary[0]["name"], "Painter's Supplies");
        assert_eq!(summary[0]["item"]["catalog_entry"]["category"], "tool");
        let (inventory, stack) = holding("painters_supplies");
        let checks = utilize_checks(inventory.stack(stack).expect("held"), Catalog::srd());
        assert_eq!(checks.len(), 1);
    }
}
