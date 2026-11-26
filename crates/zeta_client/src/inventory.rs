//! Inventory panel: the local character's replicated `Inventory` as a grid of small colored
//! slots, opened with a key or a button.
//!
//! Rules: `docs/design/game_design.md` Section 5 and `docs/architecture/technical_architecture.md`
//! Section 5. Each slot is 1 stack: its color and letter name the item category, a corner
//! number shows a quantity above 1, and a border marks an equipped item. Hovering a slot shows
//! a tooltip with the full details; clicking it opens a menu of the item's verbs. Menu entries
//! send `ItemVerbRequest`; the server decides every outcome and refuses with a log line.
//! `Utilize` sends nothing: it opens the Freeform Action Prompt with the item attached, and
//! the server attaches the catalog checks to that prompt. At most 1 popup (tooltip or menu)
//! exists at a time, a popup moves back inside the window when it would cross an edge, and
//! every popup change requests a redraw for reactive frame pacing.

use std::fmt::Debug;

use bevy::input::mouse::{MouseScrollUnit, MouseWheel};
use bevy::prelude::*;
use bevy::ui::UiGlobalTransform;
use bevy::window::{PrimaryWindow, RequestRedraw};
use zeta_core::catalog::{
    Ability, Catalog, CatalogId, CatalogItem, Cost, Dice, ItemCategory, UtilizeCheck,
};
use zeta_core::entity::EntityId;
use zeta_core::grid::GridPos;
use zeta_core::item_rules::{available_verbs, catalog_entry, display_name, utilize_checks};
use zeta_core::protocol::{
    Collider, EntityName, Inventory, Item, ItemStack, ItemVerb, ItemVerbRequest, StackId,
};

use crate::focus::{
    TextEntryActive, TextEntryUpdate, UiClickConsumed, UiClickUpdate, UiPanel,
    clear_ui_click_consumed, cursor_is_over, cursor_is_over_any, key_label,
};
use crate::player::LocalPlayer;
use crate::prompt::{OpenPromptWithItem, PromptAttachment};
use crate::settings::{ClientSettings, InventorySlotSettings};
use crate::theme::{SLOT_CORNER_RADIUS_PIXELS, TEXT_ON_PARCHMENT, TEXT_ON_WOOD, UiTheme};

/// Font size of the panel heading.
const HEADING_FONT_SIZE: f32 = 18.0;
/// Font size of the category letter inside a slot.
const SLOT_LETTER_FONT_SIZE: f32 = 18.0;
/// Font size of the quantity in a slot's corner.
const SLOT_QUANTITY_FONT_SIZE: f32 = 11.0;
/// Font size of the tooltip text.
const TOOLTIP_FONT_SIZE: f32 = 14.0;
/// Font size of the menu entries and the toggle button.
const BUTTON_FONT_SIZE: f32 = 14.0;
/// Width of the panel in logical pixels.
const PANEL_WIDTH_PIXELS: f32 = 360.0;
/// Widest a tooltip's text grows, in logical pixels, before it wraps. The limit sits on the
/// text node, not on the tooltip: layout measures the text's height before it knows a
/// parent's width limit, so a limit on the parent wrapped the text into more lines than the
/// background was sized for.
const TOOLTIP_TEXT_MAX_WIDTH_PIXELS: f32 = 288.0;
/// Space between a popup's edge and its content, in logical pixels.
const POPUP_PADDING_PIXELS: f32 = 6.0;
/// Draw order of popups above every other UI node.
const POPUP_Z_INDEX: i32 = 10;
/// Shown for a recipient whose name has not replicated yet.
const UNNAMED_RECIPIENT: &str = "someone";
/// Tooltip line for a catalog id the client's catalog lacks.
const UNKNOWN_CATALOG_ITEM: &str = "Not in this client's catalog";

/// Whether the inventory panel is open.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InventoryPanelOpen(pub bool);

/// Which slot is hovered and which slot's menu is open. The menu wins over the tooltip, so
/// at most 1 popup shows.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InventoryPopups {
    /// The stack under the cursor, which gets the tooltip.
    pub hovered: Option<StackId>,
    /// The stack whose action menu is open.
    pub menu: Option<StackId>,
}

/// Marks the inventory panel, the parent of the heading and the slot grid.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InventoryPanel;

/// Marks the always-visible button that opens and closes the panel.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InventoryToggleButton;

/// 1 slot in the grid, showing this stack.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct InventorySlot(pub StackId);

/// Marks the hover tooltip.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InventoryTooltip;

/// Marks the action menu of a clicked slot.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InventoryMenu;

/// Where a popup's top-left corner goes, in logical pixels, before it is kept on screen.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
struct PopupAnchor(Vec2);

/// An entry in the action menu; clicking it performs the action it carries.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct MenuEntry(pub MenuActionKind);

/// What a menu entry does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuActionKind {
    /// Sends this request to the server.
    Send(ItemVerbRequest),
    /// Opens the Freeform Action Prompt with the item attached.
    Utilize(OpenPromptWithItem),
}

/// 1 entry in a slot's action menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuAction {
    /// Entry text, such as `Give to Bram`.
    pub label: String,
    /// What the entry does.
    pub kind: MenuActionKind,
}

/// The category a slot shows through its color and letter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotCategory {
    /// A melee or ranged weapon.
    Weapon,
    /// Armor or a shield.
    Armor,
    /// A tool.
    Tool,
    /// Everything else, including custom items.
    Gear,
}

/// 1 slot: its look, its tooltip and its menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotView {
    /// The stack the slot shows.
    pub stack: StackId,
    /// Decides the color and letter.
    pub category: SlotCategory,
    /// The corner number, present above 1.
    pub quantity: Option<String>,
    /// Whether the slot gets the equipped border.
    pub equipped: bool,
    /// The hover tooltip text.
    pub tooltip: String,
    /// The click menu entries, in display order.
    pub actions: Vec<MenuAction>,
}

/// Everything the panel shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelView {
    /// The line above the slots.
    pub heading: String,
    /// 1 slot per carried stack, in inventory order.
    pub slots: Vec<SlotView>,
}

/// An entity next to the local character that can take a `Give`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Recipient {
    /// Display name.
    pub name: String,
    /// The entity.
    pub id: EntityId,
}

/// The panel view built last, which the popups read their content from.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
struct ShownInventory(Option<PanelView>);

/// The inventory panel, its toggle button, the slot tooltip, and the slot action menu.
#[derive(Clone, Copy, Debug, Default)]
pub struct InventoryPlugin;

impl Plugin for InventoryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ClientSettings>()
            .init_resource::<LocalPlayer>()
            .init_resource::<TextEntryActive>()
            .init_resource::<UiClickConsumed>()
            .init_resource::<InventoryPanelOpen>()
            .init_resource::<InventoryPopups>()
            .init_resource::<ShownInventory>()
            .init_resource::<UiTheme>()
            .add_message::<OpenPromptWithItem>()
            .add_message::<RequestRedraw>()
            .add_systems(Startup, spawn_inventory_ui)
            .add_systems(First, clear_ui_click_consumed)
            .add_systems(
                Update,
                (
                    (
                        toggle_by_key.after(TextEntryUpdate),
                        toggle_by_click,
                        close_menu_by_key.after(TextEntryUpdate),
                    ),
                    click_menu.in_set(UiClickUpdate),
                    click_slot.in_set(UiClickUpdate),
                    refresh_panel,
                    track_hover,
                    refresh_popups,
                    keep_popups_on_screen,
                    scroll_panel,
                )
                    .chain(),
            );
    }
}

/// Splits a unit enum variant's name into words: `SleightOfHand` becomes `Sleight Of Hand`.
/// Catalog enums are unit variants, so their `Debug` text is the variant name.
fn words(value: impl Debug) -> String {
    let name = format!("{value:?}");
    let mut spaced = String::with_capacity(name.len() + 4);
    for (index, character) in name.chars().enumerate() {
        if index > 0 && character.is_uppercase() {
            spaced.push(' ');
        }
        spaced.push(character);
    }
    spaced
}

/// A price in the largest coin that divides it: `1500` copper is `15 gp`, plus `per N` for
/// prices that buy several units.
pub fn cost_text(cost: Cost) -> String {
    let copper = cost.copper_pieces;
    let price = if copper > 0 && copper.is_multiple_of(100) {
        format!("{} gp", copper / 100)
    } else if copper > 0 && copper.is_multiple_of(10) {
        format!("{} sp", copper / 10)
    } else {
        format!("{copper} cp")
    };
    if cost.per_quantity > 1 {
        format!("{price} per {}", cost.per_quantity)
    } else {
        price
    }
}

/// A weight in pounds without trailing zeros: `3000` thousandths is `3 lb`, `250` is
/// `0.25 lb`.
pub fn weight_text(thousandths_of_pound: u64) -> String {
    let whole = thousandths_of_pound / 1000;
    let fraction = thousandths_of_pound % 1000;
    if fraction == 0 {
        return format!("{whole} lb");
    }
    let digits = format!("{fraction:03}");
    format!("{whole}.{} lb", digits.trim_end_matches('0'))
}

/// Dice as `1d8`, or a flat number for 1-sided dice such as the Blowgun's `1`.
pub fn dice_text(dice: Dice) -> String {
    if dice.sides == 1 {
        dice.count.to_string()
    } else {
        format!("{}d{}", dice.count, dice.sides)
    }
}

/// A Utilize check as `Pick a lock (DC 15, Dexterity)`. `tool_ability` is used when the
/// check names no ability of its own.
pub fn check_text(check: &UtilizeCheck, tool_ability: Option<Ability>) -> String {
    let mut text = format!("{} (DC {}", check.description, check.dc);
    if let Some(ability) = check.ability.or(tool_ability) {
        text.push_str(", ");
        text.push_str(&words(ability));
    }
    if let Some(skill) = check.skill {
        text.push_str(" (");
        text.push_str(&words(skill));
        text.push(')');
    }
    text.push(')');
    text
}

fn catalog_name(id: &CatalogId, catalog: &Catalog) -> String {
    display_name(&Item::Catalog(id.clone()), catalog)
}

/// The tooltip lines below the heading for a catalog entry.
fn catalog_lines(entry: &CatalogItem, catalog: &Catalog) -> Vec<String> {
    let mut summary = vec![words(entry.category)];
    summary.extend(entry.cost.map(cost_text));
    summary.push(weight_text(u64::from(entry.weight_thousandths_of_pound)));
    let mut lines = vec![summary.join(" - ")];
    if let Some(weapon) = &entry.weapon {
        let mut damage = format!(
            "{} {} weapon: {} {}",
            words(weapon.category),
            words(weapon.kind),
            dice_text(weapon.damage_dice),
            words(weapon.damage_type)
        );
        if let Some(versatile) = weapon.versatile_dice {
            damage.push_str(", versatile ");
            damage.push_str(&dice_text(versatile));
        }
        lines.push(damage);
        if !weapon.properties.is_empty() {
            let properties: Vec<String> = weapon.properties.iter().map(words).collect();
            lines.push(format!("Properties: {}", properties.join(", ")));
        }
        if let Some(range) = weapon.range {
            lines.push(format!(
                "Range: {}/{} ft",
                range.normal_feet, range.long_feet
            ));
        }
        lines.push(format!("Mastery: {}", words(weapon.mastery)));
        if let Some(ammunition) = &weapon.ammunition {
            lines.push(format!("Ammunition: {}", catalog_name(ammunition, catalog)));
        }
    }
    if let Some(armor) = entry.armor {
        if armor.category == zeta_core::catalog::ArmorCategory::Shield {
            lines.push(format!("Shield: +{} AC", armor.armor_class));
        } else {
            lines.push(format!(
                "{} armor: AC {}",
                words(armor.category),
                armor.armor_class
            ));
        }
        if let Some(strength) = armor.strength_requirement {
            lines.push(format!("Needs Strength {strength}"));
        }
        if armor.stealth_disadvantage {
            lines.push("Disadvantage on Dexterity (Stealth) checks".to_owned());
        }
    }
    if let Some(tool) = entry.tool {
        lines.push(format!("Tool ability: {}", words(tool.ability)));
    }
    let tool_ability = entry.tool.map(|tool| tool.ability);
    for check in &entry.utilize {
        lines.push(format!("Utilize: {}", check_text(check, tool_ability)));
    }
    if !entry.contents.is_empty() {
        let contents: Vec<String> = entry
            .contents
            .iter()
            .map(|content| {
                format!(
                    "{} x{}",
                    catalog_name(&content.item, catalog),
                    content.quantity
                )
            })
            .collect();
        lines.push(format!("Contains: {}", contents.join(", ")));
    }
    lines
}

/// The tooltip of 1 slot: the name, quantity and equipped state, then the catalog data or
/// the custom item's weight, description and tags.
pub fn tooltip_text(stack: &ItemStack, catalog: &Catalog) -> String {
    let mut heading = display_name(stack.item(), catalog);
    if stack.quantity() > 1 {
        heading.push_str(" x");
        heading.push_str(&stack.quantity().to_string());
    }
    if stack.equipped() {
        heading.push_str(" (equipped)");
    }
    let mut lines = vec![heading];
    match stack.item() {
        Item::Custom(custom) => {
            lines.push(format!(
                "Custom item - {}",
                weight_text(u64::from(custom.weight_thousandths_of_pound))
            ));
            if !custom.description.is_empty() {
                lines.push(custom.description.clone());
            }
            if !custom.tags.is_empty() {
                let tags: Vec<&str> = custom.tags.iter().map(String::as_str).collect();
                lines.push(format!("Tags: {}", tags.join(", ")));
            }
        }
        Item::Catalog(_) => match catalog_entry(stack, catalog) {
            Some(entry) => lines.extend(catalog_lines(entry, catalog)),
            None => lines.push(UNKNOWN_CATALOG_ITEM.to_owned()),
        },
    }
    lines.join("\n")
}

/// The slot category of a stack. Shields are armor in the catalog, so they count as armor.
/// Custom items and ids the client's catalog lacks are gear.
pub fn slot_category(stack: &ItemStack, catalog: &Catalog) -> SlotCategory {
    match catalog_entry(stack, catalog).map(|entry| entry.category) {
        Some(ItemCategory::Weapon) => SlotCategory::Weapon,
        Some(ItemCategory::Armor) => SlotCategory::Armor,
        Some(ItemCategory::Tool) => SlotCategory::Tool,
        _ => SlotCategory::Gear,
    }
}

/// The letter inside a slot: `W`, `A`, `T` or `G`.
pub fn slot_letter(category: SlotCategory) -> char {
    match category {
        SlotCategory::Weapon => 'W',
        SlotCategory::Armor => 'A',
        SlotCategory::Tool => 'T',
        SlotCategory::Gear => 'G',
    }
}

/// The fill color of a slot, from the settings.
pub fn slot_color(category: SlotCategory, settings: &InventorySlotSettings) -> Color {
    match category {
        SlotCategory::Weapon => settings.weapon_color,
        SlotCategory::Armor => settings.armor_color,
        SlotCategory::Tool => settings.tool_color,
        SlotCategory::Gear => settings.gear_color,
    }
}

/// The corner number of a slot: the quantity when it is above 1.
pub fn quantity_label(stack: &ItemStack) -> Option<String> {
    (stack.quantity() > 1).then(|| stack.quantity().to_string())
}

/// The prompt opening for `Utilize`: the item and its checks attached, and starting text.
pub fn utilize_opening(stack: &ItemStack, catalog: &Catalog) -> OpenPromptWithItem {
    let name = display_name(stack.item(), catalog);
    let tool_ability = catalog_entry(stack, catalog)
        .and_then(|entry| entry.tool)
        .map(|tool| tool.ability);
    let checks: Vec<String> = utilize_checks(stack, catalog)
        .iter()
        .map(|check| check_text(check, tool_ability))
        .collect();
    OpenPromptWithItem {
        attachment: PromptAttachment {
            stack: stack.id(),
            label: format!("{name}: {}", checks.join("; ")),
        },
        text: format!("I use my {name} to "),
    }
}

fn request(verb: ItemVerb, stack: &ItemStack, recipient: Option<EntityId>) -> MenuActionKind {
    MenuActionKind::Send(ItemVerbRequest {
        verb,
        stack: stack.id(),
        quantity: 1,
        recipient,
        craft_target: None,
    })
}

/// The action menu of 1 slot, from the verbs the item offers. Each request moves 1 item.
/// `Give` becomes 1 entry per adjacent recipient and none without one.
pub fn menu_actions(
    stack: &ItemStack,
    catalog: &Catalog,
    recipients: &[Recipient],
) -> Vec<MenuAction> {
    let mut actions = Vec::new();
    for verb in available_verbs(stack, catalog) {
        match verb {
            ItemVerb::Give => {
                actions.extend(recipients.iter().map(|recipient| MenuAction {
                    label: format!("Give to {}", recipient.name),
                    kind: request(verb, stack, Some(recipient.id)),
                }));
            }
            ItemVerb::Utilize => actions.push(MenuAction {
                label: words(verb),
                kind: MenuActionKind::Utilize(utilize_opening(stack, catalog)),
            }),
            _ => actions.push(MenuAction {
                label: words(verb),
                kind: request(verb, stack, None),
            }),
        }
    }
    actions
}

/// The top-left corner, in logical pixels, of a popup placed `gap` logical pixels right of
/// the inventory panel, aligned with the slot's top edge. `panel_center`, `panel_size`,
/// `slot_center`, and `slot_size` are in physical pixels, as layout computes them;
/// `inverse_scale` converts physical pixels to logical ones.
pub fn popup_position(
    panel_center: Vec2,
    panel_size: Vec2,
    slot_center: Vec2,
    slot_size: Vec2,
    inverse_scale: f32,
    gap: f32,
) -> Vec2 {
    Vec2::new(
        (panel_center.x + panel_size.x / 2.0) * inverse_scale + gap,
        (slot_center.y - slot_size.y / 2.0) * inverse_scale,
    )
}

/// The top-left corner, in logical pixels, that keeps a popup of `size` inside a screen of
/// `screen`: `position` moved left and up just enough, but never past the top-left corner.
pub fn keep_on_screen(position: Vec2, size: Vec2, screen: Vec2) -> Vec2 {
    position.min(screen - size).max(Vec2::ZERO)
}

/// Entities that can take a `Give` from the character `own` at `own_pos`: inventory holders
/// within 1 square, other than the character, with a `Collider` that blocks movement, as the
/// server requires. Passable holders are dropped item piles, not creatures. Sorted by name.
pub fn adjacent_recipients<'a>(
    own: EntityId,
    own_pos: GridPos,
    holders: impl IntoIterator<
        Item = (
            EntityId,
            GridPos,
            Option<&'a EntityName>,
            Option<&'a Collider>,
        ),
    >,
) -> Vec<Recipient> {
    let mut recipients: Vec<Recipient> = holders
        .into_iter()
        .filter(|(id, pos, _, collider)| {
            *id != own
                && own_pos.grid_distance(*pos) <= 1
                && collider.is_some_and(|collider| !collider.passable)
        })
        .map(|(id, _, name, _)| Recipient {
            name: name.map_or_else(|| UNNAMED_RECIPIENT.to_owned(), |name| name.0.clone()),
            id,
        })
        .collect();
    recipients.sort();
    recipients
}

/// The whole panel for the local character's inventory, or a waiting line before the
/// character has replicated.
pub fn panel_view(
    inventory: Option<&Inventory>,
    catalog: &Catalog,
    recipients: &[Recipient],
) -> PanelView {
    let Some(inventory) = inventory else {
        return PanelView {
            heading: "Inventory: waiting for your character".to_owned(),
            slots: Vec::new(),
        };
    };
    if inventory.stacks().is_empty() {
        return PanelView {
            heading: "Inventory: nothing carried".to_owned(),
            slots: Vec::new(),
        };
    }
    let total: u64 = inventory
        .stacks()
        .iter()
        .map(|stack| {
            let each = match stack.item() {
                Item::Custom(custom) => custom.weight_thousandths_of_pound,
                Item::Catalog(_) => catalog_entry(stack, catalog)
                    .map_or(0, |entry| entry.weight_thousandths_of_pound),
            };
            u64::from(each) * u64::from(stack.quantity())
        })
        .sum();
    PanelView {
        heading: format!("Inventory: {} carried", weight_text(total)),
        slots: inventory
            .stacks()
            .iter()
            .map(|stack| SlotView {
                stack: stack.id(),
                category: slot_category(stack, catalog),
                quantity: quantity_label(stack),
                equipped: stack.equipped(),
                tooltip: tooltip_text(stack, catalog),
                actions: menu_actions(stack, catalog, recipients),
            })
            .collect(),
    }
}

/// What a popup shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PopupContent {
    /// A hover tooltip with this text.
    Tooltip(String),
    /// An action menu with these entries.
    Menu(Vec<MenuAction>),
}

/// The popup `popups` asks for, if its slot is in `view`: the open menu, else the hovered
/// slot's tooltip.
fn wanted_popup(
    popups: InventoryPopups,
    view: Option<&PanelView>,
) -> Option<(StackId, PopupContent)> {
    let slot = |stack: StackId| view.and_then(|view| view.slots.iter().find(|s| s.stack == stack));
    if let Some(stack) = popups.menu {
        return slot(stack).map(|slot| (stack, PopupContent::Menu(slot.actions.clone())));
    }
    popups
        .hovered
        .and_then(slot)
        .map(|slot| (slot.stack, PopupContent::Tooltip(slot.tooltip.clone())))
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn spawn_inventory_ui(mut commands: Commands, settings: Res<ClientSettings>, theme: Res<UiTheme>) {
    commands
        .spawn((
            InventoryToggleButton,
            UiPanel,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(8.0),
                left: Val::Px(8.0),
                padding: UiRect::axes(Val::Px(16.0), Val::Px(8.0)),
                ..default()
            },
            theme.wood_button(),
        ))
        .with_child((
            Text::new(format!("Inventory ({})", key_label(settings.inventory_key))),
            theme.ui_font(BUTTON_FONT_SIZE),
            TextColor(TEXT_ON_WOOD),
        ));
    commands.spawn((
        InventoryPanel,
        UiPanel,
        Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            top: Val::Px(50.0),
            left: Val::Px(8.0),
            width: Val::Px(PANEL_WIDTH_PIXELS),
            max_height: Val::Percent(70.0),
            flex_direction: FlexDirection::Column,
            padding: UiRect::all(Val::Px(14.0)),
            row_gap: Val::Px(8.0),
            overflow: Overflow::scroll_y(),
            ..default()
        },
        ScrollPosition::default(),
        theme.wood_panel(),
    ));
}

/// Spawns 1 slot square of the grid.
fn spawn_slot(
    grid: &mut ChildSpawnerCommands,
    slot: &SlotView,
    look: &InventorySlotSettings,
    theme: &UiTheme,
) {
    let border = if slot.equipped {
        UiRect::all(Val::Px(look.equipped_border_pixels))
    } else {
        UiRect::default()
    };
    grid.spawn((
        InventorySlot(slot.stack),
        Node {
            width: Val::Px(look.size_pixels),
            height: Val::Px(look.size_pixels),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            border,
            border_radius: BorderRadius::all(Val::Px(SLOT_CORNER_RADIUS_PIXELS)),
            ..default()
        },
        BackgroundColor(slot_color(slot.category, look)),
        BorderColor::all(look.equipped_border_color),
    ))
    .with_children(|square| {
        square.spawn((
            Text::new(slot_letter(slot.category).to_string()),
            theme.ui_font(SLOT_LETTER_FONT_SIZE),
            TextColor(TEXT_ON_WOOD),
        ));
        if let Some(quantity) = &slot.quantity {
            square.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    right: Val::Px(3.0),
                    bottom: Val::Px(1.0),
                    ..default()
                },
                Text::new(quantity.clone()),
                theme.ui_font(SLOT_QUANTITY_FONT_SIZE),
                TextColor(TEXT_ON_WOOD),
            ));
        }
    });
}

/// Spawns the heading and the slot grid of `view` under the panel.
fn spawn_view(
    panel: &mut ChildSpawnerCommands,
    view: &PanelView,
    look: &InventorySlotSettings,
    theme: &UiTheme,
) {
    panel.spawn((
        Text::new(view.heading.clone()),
        theme.heading_font(HEADING_FONT_SIZE),
        TextColor(TEXT_ON_WOOD),
    ));
    panel
        .spawn(Node {
            flex_wrap: FlexWrap::Wrap,
            column_gap: Val::Px(look.gap_pixels),
            row_gap: Val::Px(look.gap_pixels),
            ..default()
        })
        .with_children(|grid| {
            for slot in &view.slots {
                spawn_slot(grid, slot, look, theme);
            }
        });
}

/// Spawns a popup with its top-left corner at `position` in logical pixels, above every
/// other node. The menu takes clicks, so it is a `UiPanel`; the tooltip only shows text.
/// `keep_popups_on_screen` moves it once layout knows its size.
fn spawn_popup(
    commands: &mut Commands,
    content: &PopupContent,
    position: Vec2,
    theme: &UiTheme,
) -> Entity {
    let anchor = PopupAnchor(position);
    match content {
        PopupContent::Tooltip(text) => commands
            .spawn((
                InventoryTooltip,
                anchor,
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(position.x),
                    top: Val::Px(position.y),
                    flex_direction: FlexDirection::Column,
                    padding: UiRect::all(Val::Px(POPUP_PADDING_PIXELS + 6.0)),
                    row_gap: Val::Px(4.0),
                    ..default()
                },
                theme.parchment_panel(),
                GlobalZIndex(POPUP_Z_INDEX),
            ))
            .with_child((
                Node {
                    max_width: Val::Px(TOOLTIP_TEXT_MAX_WIDTH_PIXELS),
                    ..default()
                },
                Text::new(text.clone()),
                theme.narration_font(TOOLTIP_FONT_SIZE + 1.0),
                TextColor(TEXT_ON_PARCHMENT),
            ))
            .id(),
        PopupContent::Menu(actions) => commands
            .spawn((
                InventoryMenu,
                UiPanel,
                anchor,
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(position.x),
                    top: Val::Px(position.y),
                    flex_direction: FlexDirection::Column,
                    padding: UiRect::all(Val::Px(POPUP_PADDING_PIXELS + 4.0)),
                    row_gap: Val::Px(6.0),
                    ..default()
                },
                theme.wood_panel(),
                GlobalZIndex(POPUP_Z_INDEX),
            ))
            .with_children(|menu| {
                for action in actions {
                    menu.spawn((
                        MenuEntry(action.kind.clone()),
                        Node {
                            padding: UiRect::axes(Val::Px(12.0), Val::Px(6.0)),
                            ..default()
                        },
                        theme.parchment_button(),
                    ))
                    .with_child((
                        Text::new(action.label.clone()),
                        theme.ui_font(BUTTON_FONT_SIZE),
                        TextColor(TEXT_ON_PARCHMENT),
                    ));
                }
            })
            .id(),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn toggle_by_key(
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<ClientSettings>,
    text_entry: Res<TextEntryActive>,
    mut open: ResMut<InventoryPanelOpen>,
) {
    if !text_entry.0 && keys.just_pressed(settings.inventory_key) {
        open.0 = !open.0;
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn toggle_by_click(
    buttons: Res<ButtonInput<MouseButton>>,
    window: Single<&Window, With<PrimaryWindow>>,
    toggles: Query<(&ComputedNode, &UiGlobalTransform), With<InventoryToggleButton>>,
    mut open: ResMut<InventoryPanelOpen>,
) {
    if buttons.just_pressed(MouseButton::Left) && cursor_is_over_any(&window, toggles.iter()) {
        open.0 = !open.0;
    }
}

/// Escape closes the action menu, unless the player is typing: the prompt uses Escape then.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn close_menu_by_key(
    keys: Res<ButtonInput<KeyCode>>,
    text_entry: Res<TextEntryActive>,
    mut popups: ResMut<InventoryPopups>,
) {
    if !text_entry.0 && keys.just_pressed(KeyCode::Escape) && popups.menu.is_some() {
        popups.menu = None;
    }
}

/// A click on a menu entry performs it and closes the menu.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn click_menu(
    buttons: Res<ButtonInput<MouseButton>>,
    window: Single<&Window, With<PrimaryWindow>>,
    entries: Query<(&ComputedNode, &UiGlobalTransform, &MenuEntry)>,
    mut popups: ResMut<InventoryPopups>,
    mut click_consumed: ResMut<UiClickConsumed>,
    mut requests: MessageWriter<ItemVerbRequest>,
    mut openings: MessageWriter<OpenPromptWithItem>,
) {
    if !buttons.just_pressed(MouseButton::Left) {
        return;
    }
    let clicked = entries
        .iter()
        .find(|(node, transform, _)| cursor_is_over(&window, node, **transform));
    let Some((_, _, entry)) = clicked else {
        return;
    };
    match &entry.0 {
        MenuActionKind::Send(request) => {
            requests.write(request.clone());
        }
        MenuActionKind::Utilize(opening) => {
            openings.write(opening.clone());
        }
    }
    popups.menu = None;
    click_consumed.0 = true;
}

/// A click on a slot opens its menu, or closes it when it is already open. A click anywhere
/// else, other than on the menu, closes the menu and consumes the click so the map does not
/// also move the character.
#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "Bevy system parameters are passed by value, 1 per resource and query"
)]
fn click_slot(
    buttons: Res<ButtonInput<MouseButton>>,
    window: Single<&Window, With<PrimaryWindow>>,
    open: Res<InventoryPanelOpen>,
    panels: Query<(&ComputedNode, &UiGlobalTransform), With<InventoryPanel>>,
    slots: Query<(&ComputedNode, &UiGlobalTransform, &InventorySlot)>,
    menus: Query<(&ComputedNode, &UiGlobalTransform), With<InventoryMenu>>,
    mut popups: ResMut<InventoryPopups>,
    mut click_consumed: ResMut<UiClickConsumed>,
) {
    if !buttons.just_pressed(MouseButton::Left) || cursor_is_over_any(&window, menus.iter()) {
        return;
    }
    let had_menu = popups.menu.is_some();
    let clicked = slot_under_cursor(&window, open.0, &panels, &slots);
    popups.menu = match clicked {
        Some(stack) if popups.menu != Some(stack) => Some(stack),
        _ => None,
    };
    if had_menu && popups.menu.is_none() {
        click_consumed.0 = true;
    }
}

/// The slot under the cursor of an open panel. A slot scrolled out of the panel is clipped
/// but keeps its layout, so the cursor must also be over the panel itself.
fn slot_under_cursor(
    window: &Window,
    open: bool,
    panels: &Query<(&ComputedNode, &UiGlobalTransform), With<InventoryPanel>>,
    slots: &Query<(&ComputedNode, &UiGlobalTransform, &InventorySlot)>,
) -> Option<StackId> {
    if !open || !cursor_is_over_any(window, panels.iter()) {
        return None;
    }
    slots
        .iter()
        .find(|(node, transform, _)| cursor_is_over(window, node, **transform))
        .map(|(_, _, slot)| slot.0)
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn track_hover(
    window: Single<&Window, With<PrimaryWindow>>,
    open: Res<InventoryPanelOpen>,
    panels: Query<(&ComputedNode, &UiGlobalTransform), With<InventoryPanel>>,
    slots: Query<(&ComputedNode, &UiGlobalTransform, &InventorySlot)>,
    mut popups: ResMut<InventoryPopups>,
) {
    let hovered = slot_under_cursor(&window, open.0, &panels, &slots);
    if popups.hovered != hovered {
        popups.hovered = hovered;
    }
}

type Holders<'w, 's> = Query<
    'w,
    's,
    (
        &'static EntityId,
        &'static GridPos,
        &'static Inventory,
        Option<&'static EntityName>,
        Option<&'static Collider>,
    ),
>;

fn current_view(local_player: LocalPlayer, holders: &Holders) -> PanelView {
    let catalog = Catalog::srd();
    let own = local_player
        .character
        .and_then(|character| holders.iter().find(|(id, ..)| **id == character));
    let Some((own_id, own_pos, inventory, _, _)) = own else {
        return panel_view(None, catalog, &[]);
    };
    let recipients = adjacent_recipients(
        *own_id,
        *own_pos,
        holders
            .iter()
            .map(|(id, pos, _, name, collider)| (*id, *pos, name, collider)),
    );
    panel_view(Some(inventory), catalog, &recipients)
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "Bevy system parameters are passed by value, 1 per resource and query"
)]
fn refresh_panel(
    mut commands: Commands,
    open: Res<InventoryPanelOpen>,
    settings: Res<ClientSettings>,
    theme: Res<UiTheme>,
    local_player: Res<LocalPlayer>,
    holders: Holders,
    mut panels: Query<(Entity, &mut Node), With<InventoryPanel>>,
    mut shown: ResMut<ShownInventory>,
) {
    let display = if open.0 { Display::Flex } else { Display::None };
    for (_, mut node) in &mut panels {
        if node.display != display {
            node.display = display;
        }
    }
    if !open.0 {
        return;
    }
    let view = current_view(*local_player, &holders);
    if shown.0.as_ref() == Some(&view) {
        return;
    }
    for (panel, _) in &panels {
        commands
            .entity(panel)
            .despawn_children()
            .with_children(|children| {
                spawn_view(children, &view, &settings.inventory_slots, &theme);
            });
    }
    shown.0 = Some(view);
}

/// The popup on screen and where it was placed.
#[derive(Debug)]
struct ShownPopup {
    content: PopupContent,
    position: Vec2,
    entity: Entity,
}

/// Keeps the 1 popup in step with `InventoryPopups`: closes popups of a closed panel or a
/// stack that is gone, and respawns the popup only when its content or place changes. Each
/// change requests a redraw, so the reactive frame pacing shows it.
#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "Bevy system parameters are passed by value, 1 per resource and query"
)]
fn refresh_popups(
    mut commands: Commands,
    open: Res<InventoryPanelOpen>,
    settings: Res<ClientSettings>,
    theme: Res<UiTheme>,
    view: Res<ShownInventory>,
    mut popups: ResMut<InventoryPopups>,
    panels: Query<(&ComputedNode, &UiGlobalTransform), With<InventoryPanel>>,
    slots: Query<(&ComputedNode, &UiGlobalTransform, &InventorySlot)>,
    mut shown: Local<Option<ShownPopup>>,
    mut redraws: MessageWriter<RequestRedraw>,
) {
    let carried = |stack: StackId| {
        open.0
            && view
                .0
                .as_ref()
                .is_some_and(|view| view.slots.iter().any(|slot| slot.stack == stack))
    };
    if popups.menu.is_some_and(|stack| !carried(stack)) {
        popups.menu = None;
    }
    if popups.hovered.is_some_and(|stack| !carried(stack)) {
        popups.hovered = None;
    }
    let Some((stack, content)) = wanted_popup(*popups, view.0.as_ref()) else {
        if let Some(old) = shown.take() {
            commands.entity(old.entity).despawn();
            redraws.write(RequestRedraw);
        }
        return;
    };
    let laid_out_panel = panels.iter().find(|(node, _)| node.size() != Vec2::ZERO);
    let laid_out_slot = slots
        .iter()
        .find(|(node, _, slot)| slot.0 == stack && node.size() != Vec2::ZERO);
    let (Some((panel_node, panel_transform)), Some((node, transform, _))) =
        (laid_out_panel, laid_out_slot)
    else {
        // A rebuilt panel or slot has no size until layout runs; place the popup next frame.
        redraws.write(RequestRedraw);
        return;
    };
    let position = popup_position(
        panel_transform.translation,
        panel_node.size(),
        transform.translation,
        node.size(),
        node.inverse_scale_factor(),
        settings.inventory_slots.popup_gap_pixels,
    );
    if shown
        .as_ref()
        .is_some_and(|old| old.content == content && old.position == position)
    {
        return;
    }
    if let Some(old) = shown.take() {
        commands.entity(old.entity).despawn();
    }
    let entity = spawn_popup(&mut commands, &content, position, &theme);
    *shown = Some(ShownPopup {
        content,
        position,
        entity,
    });
    redraws.write(RequestRedraw);
}

/// Moves each laid-out popup back inside the window when its size would push it past the
/// right or bottom edge. Each move requests a redraw, so the next frame shows it.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn keep_popups_on_screen(
    window: Single<&Window, With<PrimaryWindow>>,
    mut popups: Query<(&PopupAnchor, &ComputedNode, &mut Node)>,
    mut redraws: MessageWriter<RequestRedraw>,
) {
    // `Window::width` and `Window::height` are logical: bevy_window-0.20.1/src/window.rs:562
    // and :570.
    let screen = Vec2::new(window.width(), window.height());
    for (anchor, computed, mut node) in &mut popups {
        if computed.size() == Vec2::ZERO {
            continue;
        }
        let size = computed.size() * computed.inverse_scale_factor();
        let position = keep_on_screen(anchor.0, size, screen);
        let (left, top) = (Val::Px(position.x), Val::Px(position.y));
        if node.left != left || node.top != top {
            node.left = left;
            node.top = top;
            redraws.write(RequestRedraw);
        }
    }
}

type ScrollPanel<'w, 's> = Query<
    'w,
    's,
    (
        &'static ComputedNode,
        &'static UiGlobalTransform,
        &'static mut ScrollPosition,
    ),
    With<InventoryPanel>,
>;

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn scroll_panel(
    mut wheel: MessageReader<MouseWheel>,
    settings: Res<ClientSettings>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut panels: ScrollPanel,
) {
    let pixels: f32 = wheel
        .read()
        .map(|event| match event.unit {
            MouseScrollUnit::Line => event.y * settings.inventory_scroll_pixels_per_line,
            MouseScrollUnit::Pixel => event.y,
        })
        .sum();
    if pixels == 0.0 {
        return;
    }
    for (node, transform, mut scroll) in &mut panels {
        if !cursor_is_over_any(&window, [(node, transform)]) {
            continue;
        }
        let max_scroll =
            (node.content_size().y - node.size().y).max(0.0) * node.inverse_scale_factor();
        scroll.y = (scroll.y.min(max_scroll) - pixels).clamp(0.0, max_scroll);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use zeta_core::protocol::CustomItem;

    use super::*;

    fn catalog() -> &'static Catalog {
        Catalog::srd()
    }

    fn id(text: &str) -> CatalogId {
        CatalogId::new(text).expect("valid id")
    }

    /// An inventory holding `items` and the stacks they got, in order.
    fn inventory(items: Vec<(Item, u32)>) -> (Inventory, Vec<ItemStack>) {
        let mut inventory = Inventory::default();
        let ids: Vec<_> = items
            .into_iter()
            .map(|(item, quantity)| inventory.add(item, quantity).expect("added"))
            .collect();
        let stacks = ids
            .iter()
            .map(|stack| inventory.stack(*stack).expect("stack").clone())
            .collect();
        (inventory, stacks)
    }

    fn single(item: Item, quantity: u32) -> ItemStack {
        inventory(vec![(item, quantity)]).1.remove(0)
    }

    fn catalog_stack(text: &str) -> ItemStack {
        single(Item::Catalog(id(text)), 1)
    }

    fn sealed_letter() -> ItemStack {
        single(
            Item::Custom(CustomItem {
                name: "Sealed Letter".to_owned(),
                weight_thousandths_of_pound: 0,
                description: "Wax seal of House Vell.".to_owned(),
                tags: BTreeSet::from(["quest".to_owned()]),
            }),
            1,
        )
    }

    fn bram() -> Recipient {
        Recipient {
            name: "Bram".to_owned(),
            id: EntityId(9),
        }
    }

    fn labels(actions: &[MenuAction]) -> Vec<&str> {
        actions.iter().map(|action| action.label.as_str()).collect()
    }

    #[test]
    fn coins_weights_and_dice_read_like_the_rules() {
        let cost = |copper_pieces, per_quantity| Cost {
            copper_pieces,
            per_quantity,
        };
        assert_eq!(cost_text(cost(1500, 1)), "15 gp");
        assert_eq!(cost_text(cost(50, 1)), "5 sp");
        assert_eq!(cost_text(cost(1, 5)), "1 cp per 5");
        assert_eq!(weight_text(3000), "3 lb");
        assert_eq!(weight_text(250), "0.25 lb");
        assert_eq!(weight_text(0), "0 lb");
        assert_eq!(dice_text(Dice { count: 1, sides: 8 }), "1d8");
        assert_eq!(dice_text(Dice { count: 1, sides: 1 }), "1");
    }

    #[test]
    fn slots_show_the_category_by_letter_and_color() {
        let look = ClientSettings::default().inventory_slots;
        let cases = [
            ("longsword", SlotCategory::Weapon, 'W', look.weapon_color),
            ("chain_mail", SlotCategory::Armor, 'A', look.armor_color),
            ("shield", SlotCategory::Armor, 'A', look.armor_color),
            (
                "painters_supplies",
                SlotCategory::Tool,
                'T',
                look.tool_color,
            ),
            ("rope", SlotCategory::Gear, 'G', look.gear_color),
            ("rations", SlotCategory::Gear, 'G', look.gear_color),
        ];
        for (item, category, letter, color) in cases {
            let found = slot_category(&catalog_stack(item), catalog());
            assert_eq!(found, category, "{item}");
            assert_eq!(slot_letter(found), letter, "{item}");
            assert_eq!(slot_color(found, &look), color, "{item}");
        }
        assert_eq!(
            slot_category(&sealed_letter(), catalog()),
            SlotCategory::Gear
        );
    }

    #[test]
    fn the_corner_number_shows_only_above_1() {
        assert_eq!(quantity_label(&catalog_stack("rope")), None);
        assert_eq!(
            quantity_label(&single(Item::Catalog(id("rations")), 3)),
            Some("3".to_owned())
        );
    }

    #[test]
    fn a_weapon_tooltip_shows_damage_properties_and_mastery() {
        assert_eq!(
            tooltip_text(&catalog_stack("longsword"), catalog()),
            "Longsword\nWeapon - 15 gp - 3 lb\nMartial Melee weapon: 1d8 Slashing, versatile \
             1d10\nProperties: Versatile\nMastery: Sap"
        );
    }

    #[test]
    fn armor_tooltips_show_armor_class_and_drawbacks() {
        assert_eq!(
            tooltip_text(&catalog_stack("chain_mail"), catalog()),
            "Chain Mail\nArmor - 75 gp - 55 lb\nHeavy armor: AC 16\nNeeds Strength 13\n\
             Disadvantage on Dexterity (Stealth) checks"
        );
        assert!(tooltip_text(&catalog_stack("shield"), catalog()).contains("Shield: +2 AC"));
    }

    #[test]
    fn a_tool_tooltip_shows_its_ability_and_check_but_no_crafting() {
        let text = tooltip_text(&catalog_stack("painters_supplies"), catalog());
        assert!(text.contains("Tool ability: Wisdom"), "{text}");
        assert!(
            text.contains(
                "Utilize: Paint a recognizable image of something you've seen (DC 10, Wisdom)"
            ),
            "{text}"
        );
        assert!(!text.contains("Craft"), "{text}");
    }

    #[test]
    fn a_tooltip_heading_shows_quantity_and_equipped_state() {
        let rations = single(Item::Catalog(id("rations")), 2);
        assert!(tooltip_text(&rations, catalog()).starts_with("Rations x2\n"));
        let mut inventory = Inventory::default();
        let sword = inventory
            .add(Item::Catalog(id("longsword")), 1)
            .expect("added");
        inventory.set_equipped(sword, true).expect("equipped");
        let stack = inventory.stack(sword).expect("stack");
        assert!(tooltip_text(stack, catalog()).starts_with("Longsword (equipped)\n"));
    }

    #[test]
    fn a_custom_tooltip_shows_its_description_and_tags() {
        let letter = sealed_letter();
        assert_eq!(
            tooltip_text(&letter, catalog()),
            "Sealed Letter\nCustom item - 0 lb\nWax seal of House Vell.\nTags: quest"
        );
        assert_eq!(
            labels(&menu_actions(&letter, catalog(), &[bram()])),
            ["Drop", "Give to Bram"]
        );
    }

    #[test]
    fn menu_verbs_follow_the_item_and_send_1_item() {
        let sword = catalog_stack("longsword");
        let actions = menu_actions(&sword, catalog(), &[]);
        assert_eq!(labels(&actions), ["Equip", "Drop"]);
        assert_eq!(
            actions[0].kind,
            MenuActionKind::Send(ItemVerbRequest {
                verb: ItemVerb::Equip,
                stack: sword.id(),
                quantity: 1,
                recipient: None,
                craft_target: None,
            })
        );
        let rations = catalog_stack("rations");
        assert_eq!(
            labels(&menu_actions(&rations, catalog(), &[])),
            ["Consume", "Drop"]
        );
        let supplies = catalog_stack("painters_supplies");
        assert_eq!(
            labels(&menu_actions(&supplies, catalog(), &[])),
            ["Utilize", "Drop"]
        );
    }

    #[test]
    fn give_names_each_adjacent_recipient() {
        let rope = catalog_stack("rope");
        let actions = menu_actions(&rope, catalog(), &[bram()]);
        let give = actions.last().expect("give");
        assert_eq!(give.label, "Give to Bram");
        assert_eq!(
            give.kind,
            MenuActionKind::Send(ItemVerbRequest {
                verb: ItemVerb::Give,
                stack: rope.id(),
                quantity: 1,
                recipient: Some(EntityId(9)),
                craft_target: None,
            })
        );
    }

    #[test]
    fn utilize_opens_the_prompt_with_the_check_attached() {
        let rope = catalog_stack("rope");
        let utilize = menu_actions(&rope, catalog(), &[])
            .into_iter()
            .find(|action| action.label == "Utilize")
            .expect("rope offers Utilize");
        assert_eq!(
            utilize.kind,
            MenuActionKind::Utilize(OpenPromptWithItem {
                attachment: PromptAttachment {
                    stack: rope.id(),
                    label: "Rope: Tie a knot with Rope (DC 10, Dexterity (Sleight Of Hand))"
                        .to_owned(),
                },
                text: "I use my Rope to ".to_owned(),
            })
        );
    }

    #[test]
    fn popups_sit_right_of_the_panel_aligned_with_the_slot() {
        let position = popup_position(
            Vec2::new(200.0, 200.0),
            Vec2::new(300.0, 400.0),
            Vec2::new(100.0, 200.0),
            Vec2::new(80.0, 80.0),
            0.5,
            6.0,
        );
        assert_eq!(position, Vec2::new(181.0, 80.0));
    }

    #[test]
    fn the_menu_wins_over_the_tooltip() {
        let (carried, stacks) = inventory(vec![
            (Item::Catalog(id("longsword")), 1),
            (Item::Catalog(id("rope")), 1),
        ]);
        let view = panel_view(Some(&carried), catalog(), &[]);
        let both = InventoryPopups {
            hovered: Some(stacks[0].id()),
            menu: Some(stacks[1].id()),
        };
        assert!(matches!(
            wanted_popup(both, Some(&view)),
            Some((stack, PopupContent::Menu(_))) if stack == stacks[1].id()
        ));
        let hover_only = InventoryPopups { menu: None, ..both };
        assert!(matches!(
            wanted_popup(hover_only, Some(&view)),
            Some((_, PopupContent::Tooltip(text))) if text.starts_with("Longsword\n")
        ));
        assert_eq!(wanted_popup(hover_only, None), None);
    }

    #[test]
    fn recipients_are_adjacent_blocking_holders_other_than_self() {
        let names = [
            EntityName("Zed".to_owned()),
            EntityName("Ann".to_owned()),
            EntityName("Pile".to_owned()),
            EntityName("Far".to_owned()),
        ];
        let blocking = Collider { passable: false };
        let passable = Collider { passable: true };
        let holders = vec![
            (EntityId(1), GridPos::new(5, 5), None, Some(&blocking)),
            (
                EntityId(2),
                GridPos::new(6, 6),
                Some(&names[0]),
                Some(&blocking),
            ),
            (
                EntityId(3),
                GridPos::new(4, 5),
                Some(&names[1]),
                Some(&blocking),
            ),
            (EntityId(6), GridPos::new(5, 4), Some(&names[1]), None),
            (
                EntityId(4),
                GridPos::new(5, 5),
                Some(&names[2]),
                Some(&passable),
            ),
            (
                EntityId(5),
                GridPos::new(7, 5),
                Some(&names[3]),
                Some(&blocking),
            ),
        ];
        let recipients = adjacent_recipients(EntityId(1), GridPos::new(5, 5), holders);
        assert_eq!(
            recipients,
            [
                Recipient {
                    name: "Ann".to_owned(),
                    id: EntityId(3)
                },
                Recipient {
                    name: "Zed".to_owned(),
                    id: EntityId(2)
                },
            ]
        );
    }

    #[test]
    fn the_heading_waits_for_the_character_then_sums_the_weight() {
        assert_eq!(
            panel_view(None, catalog(), &[]).heading,
            "Inventory: waiting for your character"
        );
        assert_eq!(
            panel_view(Some(&Inventory::default()), catalog(), &[]).heading,
            "Inventory: nothing carried"
        );
        let (carried, _) = inventory(vec![
            (Item::Catalog(id("longsword")), 1),
            (Item::Catalog(id("rations")), 2),
        ]);
        let view = panel_view(Some(&carried), catalog(), &[]);
        assert_eq!(view.heading, "Inventory: 7 lb carried");
        assert_eq!(view.slots.len(), 2);
        assert_eq!(view.slots[1].quantity.as_deref(), Some("2"));
    }

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<ItemVerbRequest>()
            .add_message::<MouseWheel>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .add_plugins(InventoryPlugin);
        app
    }

    /// Updates once with only `key` just pressed. Without the input plugin `just_pressed` is
    /// never cleared, so every update resets the keys first.
    fn tap(app: &mut App, key: KeyCode) {
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.reset_all();
        keys.press(key);
        app.update();
    }

    /// Updates once with no key just pressed.
    fn step(app: &mut App) {
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .reset_all();
        app.update();
    }

    fn panel_display(app: &mut App) -> Display {
        let mut panels = app
            .world_mut()
            .query_filtered::<&Node, With<InventoryPanel>>();
        panels.single(app.world()).expect("1 panel").display
    }

    fn count<C: Component>(app: &mut App) -> usize {
        let mut query = app.world_mut().query_filtered::<(), With<C>>();
        query.iter(app.world()).count()
    }

    /// Gives the panel and every slot a size, as layout would.
    fn lay_out_slots(app: &mut App) {
        let mut panels = app
            .world_mut()
            .query_filtered::<&mut ComputedNode, With<InventoryPanel>>();
        for mut node in panels.iter_mut(app.world_mut()) {
            node.size = Vec2::new(360.0, 200.0);
        }
        let mut slots = app
            .world_mut()
            .query_filtered::<&mut ComputedNode, With<InventorySlot>>();
        for mut node in slots.iter_mut(app.world_mut()) {
            node.size = Vec2::splat(40.0);
        }
    }

    /// An app with the local character carrying a longsword and a rope.
    fn app_with_items() -> (App, Vec<ItemStack>) {
        let mut app = test_app();
        let (carried, stacks) = inventory(vec![
            (Item::Catalog(id("longsword")), 1),
            (Item::Catalog(id("rope")), 1),
        ]);
        app.world_mut()
            .spawn((EntityId(1), GridPos::new(2, 2), carried));
        app.world_mut().resource_mut::<LocalPlayer>().character = Some(EntityId(1));
        app.update();
        (app, stacks)
    }

    #[test]
    fn the_key_toggles_a_panel_of_slots() {
        let (mut app, _) = app_with_items();
        assert_eq!(panel_display(&mut app), Display::None);

        tap(&mut app, crate::settings::INVENTORY_KEY);
        assert_eq!(panel_display(&mut app), Display::Flex);
        assert_eq!(count::<InventorySlot>(&mut app), 2);
        let mut texts = app.world_mut().query::<&Text>();
        let letters: Vec<String> = texts.iter(app.world()).map(|text| text.0.clone()).collect();
        assert!(letters.contains(&"W".to_owned()), "{letters:?}");
        assert!(letters.contains(&"G".to_owned()), "{letters:?}");

        tap(&mut app, crate::settings::INVENTORY_KEY);
        assert_eq!(panel_display(&mut app), Display::None);
    }

    #[test]
    fn hover_shows_1_tooltip_and_requests_a_redraw() {
        let (mut app, stacks) = app_with_items();
        tap(&mut app, crate::settings::INVENTORY_KEY);
        lay_out_slots(&mut app);
        app.world_mut().resource_mut::<InventoryPopups>().hovered = Some(stacks[0].id());
        step(&mut app);
        assert_eq!(count::<InventoryTooltip>(&mut app), 1);
        let mut texts = app.world_mut().query::<&Text>();
        assert!(
            texts
                .iter(app.world())
                .any(|text| text.0.starts_with("Longsword\n"))
        );
        assert!(!app.world().resource::<Messages<RequestRedraw>>().is_empty());

        app.world_mut().resource_mut::<InventoryPopups>().hovered = None;
        step(&mut app);
        assert_eq!(count::<InventoryTooltip>(&mut app), 0);
    }

    /// The width limit sits on the text, so layout measures the wrapped text's height and
    /// the background covers every line. On the tooltip itself the text overflowed it.
    #[test]
    fn the_tooltip_limits_its_text_width_so_the_background_fits() {
        let (mut app, stacks) = app_with_items();
        tap(&mut app, crate::settings::INVENTORY_KEY);
        lay_out_slots(&mut app);
        app.world_mut().resource_mut::<InventoryPopups>().hovered = Some(stacks[0].id());
        step(&mut app);
        let mut tooltips = app
            .world_mut()
            .query_filtered::<(&Node, &Children), With<InventoryTooltip>>();
        let (tooltip, children) = tooltips.single(app.world()).expect("1 tooltip");
        assert_eq!(tooltip.max_width, Val::Auto);
        let text = children.first().copied().expect("the tooltip has its text");
        let text_node = app.world().get::<Node>(text).expect("the text is a node");
        assert_eq!(text_node.max_width, Val::Px(TOOLTIP_TEXT_MAX_WIDTH_PIXELS));
    }

    #[test]
    fn keep_on_screen_moves_an_overflowing_popup_back_inside() {
        let screen = Vec2::new(800.0, 600.0);
        let size = Vec2::new(200.0, 100.0);
        let inside = Vec2::new(100.0, 100.0);
        assert_eq!(keep_on_screen(inside, size, screen), inside);
        assert_eq!(
            keep_on_screen(Vec2::new(700.0, 550.0), size, screen),
            Vec2::new(600.0, 500.0)
        );
        assert_eq!(
            keep_on_screen(Vec2::new(-5.0, 10.0), size, screen),
            Vec2::new(0.0, 10.0)
        );
        assert_eq!(
            keep_on_screen(inside, Vec2::new(900.0, 700.0), screen),
            Vec2::ZERO
        );
    }

    #[test]
    fn a_popup_past_the_window_edge_moves_back_on_screen() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<RequestRedraw>()
            .add_systems(Update, keep_popups_on_screen);
        app.world_mut().spawn((
            Window {
                resolution: bevy::window::WindowResolution::new(400, 300),
                ..default()
            },
            PrimaryWindow,
        ));
        let popup = app
            .world_mut()
            .spawn((
                PopupAnchor(Vec2::new(350.0, 280.0)),
                ComputedNode {
                    size: Vec2::new(120.0, 90.0),
                    ..default()
                },
                Node::default(),
            ))
            .id();
        app.update();
        let node = app.world().get::<Node>(popup).expect("the popup is a node");
        assert_eq!((node.left, node.top), (Val::Px(280.0), Val::Px(210.0)));
        assert!(!app.world().resource::<Messages<RequestRedraw>>().is_empty());
    }

    #[test]
    fn the_menu_replaces_the_tooltip_and_escape_closes_it() {
        let (mut app, stacks) = app_with_items();
        tap(&mut app, crate::settings::INVENTORY_KEY);
        lay_out_slots(&mut app);
        let mut popups = app.world_mut().resource_mut::<InventoryPopups>();
        popups.hovered = Some(stacks[0].id());
        popups.menu = Some(stacks[1].id());
        step(&mut app);
        assert_eq!(count::<InventoryMenu>(&mut app), 1);
        assert_eq!(count::<InventoryTooltip>(&mut app), 0);
        assert_eq!(count::<MenuEntry>(&mut app), 2);

        tap(&mut app, KeyCode::Escape);
        assert_eq!(app.world().resource::<InventoryPopups>().menu, None);
        assert_eq!(count::<InventoryMenu>(&mut app), 0);
        assert_eq!(count::<InventoryTooltip>(&mut app), 1);

        tap(&mut app, crate::settings::INVENTORY_KEY);
        assert_eq!(count::<InventoryTooltip>(&mut app), 0);
        assert_eq!(
            *app.world().resource::<InventoryPopups>(),
            InventoryPopups::default()
        );
    }

    #[test]
    fn the_key_is_ignored_while_typing() {
        let mut app = test_app();
        app.world_mut().resource_mut::<TextEntryActive>().0 = true;
        tap(&mut app, crate::settings::INVENTORY_KEY);
        assert!(!app.world().resource::<InventoryPanelOpen>().0);
    }

    #[test]
    fn clicking_outside_an_open_menu_closes_it_and_consumes_the_click() {
        let (mut app, stacks) = app_with_items();
        app.world_mut().spawn((Window::default(), PrimaryWindow));
        tap(&mut app, crate::settings::INVENTORY_KEY);
        lay_out_slots(&mut app);
        app.world_mut().resource_mut::<InventoryPopups>().menu = Some(stacks[0].id());
        step(&mut app);
        assert_eq!(count::<InventoryMenu>(&mut app), 1);

        let mut buttons = app.world_mut().resource_mut::<ButtonInput<MouseButton>>();
        buttons.reset_all();
        buttons.press(MouseButton::Left);
        step(&mut app);
        assert_eq!(app.world().resource::<InventoryPopups>().menu, None);
        assert!(app.world().resource::<UiClickConsumed>().0);

        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .reset_all();
        step(&mut app);
        assert!(!app.world().resource::<UiClickConsumed>().0);
    }
}
