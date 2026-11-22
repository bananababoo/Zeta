//! Inventory panel: the local character's replicated `Inventory` as item cards with verb
//! buttons, opened with a key or a button.
//!
//! Rules: `docs/design/game_design.md` Section 5 and `docs/architecture/technical_architecture.md`
//! Section 5. Buttons send `ItemVerbRequest`; the server decides every outcome and refuses
//! with a log line. `Utilize` sends nothing: it opens the Freeform Action Prompt with the item
//! attached, and the server attaches the catalog checks to that prompt.

use std::fmt::Debug;

use bevy::input::mouse::{MouseScrollUnit, MouseWheel};
use bevy::prelude::*;
use bevy::ui::UiGlobalTransform;
use bevy::window::PrimaryWindow;
use zeta_core::catalog::{Ability, Catalog, CatalogId, CatalogItem, Cost, Dice, UtilizeCheck};
use zeta_core::entity::EntityId;
use zeta_core::grid::GridPos;
use zeta_core::item_rules::{
    available_verbs, catalog_entry, craftable_targets, display_name, utilize_checks,
};
use zeta_core::protocol::{
    Collider, EntityName, Inventory, Item, ItemStack, ItemVerb, ItemVerbRequest,
};

use crate::focus::{TextEntryActive, TextEntryUpdate, UiPanel, cursor_is_over_any, key_label};
use crate::player::LocalPlayer;
use crate::prompt::{OpenPromptWithItem, PromptAttachment};
use crate::settings::ClientSettings;

/// Font size of the panel heading.
const HEADING_FONT_SIZE: f32 = 18.0;
/// Font size of the card text.
const CARD_FONT_SIZE: f32 = 14.0;
/// Font size of the verb buttons and the toggle button.
const BUTTON_FONT_SIZE: f32 = 14.0;
/// Width of the panel in logical pixels.
const PANEL_WIDTH_PIXELS: f32 = 360.0;
/// Shown for a recipient whose name has not replicated yet.
const UNNAMED_RECIPIENT: &str = "someone";
/// Card line for a catalog id the client's catalog lacks.
const UNKNOWN_CATALOG_ITEM: &str = "Not in this client's catalog";

/// Whether the inventory panel is open.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InventoryPanelOpen(pub bool);

/// Marks the inventory panel, the parent of the heading and every card.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InventoryPanel;

/// Marks the always-visible button that opens and closes the panel.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InventoryToggleButton;

/// A verb button on a card; clicking it performs the action it carries.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct CardActionButton(pub CardActionKind);

/// What a card button does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardActionKind {
    /// Sends this request to the server.
    Send(ItemVerbRequest),
    /// Opens the Freeform Action Prompt with the item attached.
    Utilize(OpenPromptWithItem),
}

/// 1 button on a card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardAction {
    /// Button text, such as `Give to Bram`.
    pub label: String,
    /// What the button does.
    pub kind: CardActionKind,
}

/// 1 item card: its text and its buttons.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardView {
    /// The lines of the card.
    pub text: String,
    /// The buttons, in display order.
    pub actions: Vec<CardAction>,
}

/// Everything the panel shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelView {
    /// The line above the cards.
    pub heading: String,
    /// 1 card per carried stack, in inventory order.
    pub cards: Vec<CardView>,
}

/// An entity next to the local character that can take a `Give`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Recipient {
    /// Display name.
    pub name: String,
    /// The entity.
    pub id: EntityId,
}

/// The inventory panel, its toggle button, and the verb buttons.
#[derive(Clone, Copy, Debug, Default)]
pub struct InventoryPlugin;

impl Plugin for InventoryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ClientSettings>()
            .init_resource::<LocalPlayer>()
            .init_resource::<TextEntryActive>()
            .init_resource::<InventoryPanelOpen>()
            .add_message::<OpenPromptWithItem>()
            .add_systems(Startup, spawn_inventory_ui)
            .add_systems(
                Update,
                (
                    (
                        toggle_by_key.after(TextEntryUpdate),
                        toggle_by_click,
                        act_by_click,
                    ),
                    refresh_panel,
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

fn names(ids: &[CatalogId], catalog: &Catalog) -> String {
    ids.iter()
        .map(|id| catalog_name(id, catalog))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The card lines below the heading for a catalog entry.
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
    if !entry.crafts.is_empty() {
        lines.push(format!("Crafts: {}", names(&entry.crafts, catalog)));
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

/// The text of 1 item card: the name, quantity and equipped state, then the catalog data or
/// the custom item's weight, description and tags.
pub fn card_text(stack: &ItemStack, catalog: &Catalog) -> String {
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

fn request(
    verb: ItemVerb,
    stack: &ItemStack,
    recipient: Option<EntityId>,
    craft_target: Option<CatalogId>,
) -> CardActionKind {
    CardActionKind::Send(ItemVerbRequest {
        verb,
        stack: stack.id(),
        quantity: 1,
        recipient,
        craft_target,
    })
}

/// The buttons of 1 card, from the verbs the item offers. Each request moves 1 item. `Give`
/// becomes 1 button per adjacent recipient and none without one; `Craft` becomes 1 button
/// per target the item's catalog entry crafts.
pub fn card_actions(
    stack: &ItemStack,
    catalog: &Catalog,
    recipients: &[Recipient],
) -> Vec<CardAction> {
    let mut actions = Vec::new();
    for verb in available_verbs(stack, catalog) {
        match verb {
            ItemVerb::Give => {
                actions.extend(recipients.iter().map(|recipient| CardAction {
                    label: format!("Give to {}", recipient.name),
                    kind: request(verb, stack, Some(recipient.id), None),
                }));
            }
            ItemVerb::Craft => {
                actions.extend(craftable_targets(stack, catalog).into_iter().map(|target| {
                    CardAction {
                        label: format!("Craft {}", catalog_name(&target, catalog)),
                        kind: request(verb, stack, None, Some(target)),
                    }
                }));
            }
            ItemVerb::Utilize => actions.push(CardAction {
                label: words(verb),
                kind: CardActionKind::Utilize(utilize_opening(stack, catalog)),
            }),
            _ => actions.push(CardAction {
                label: words(verb),
                kind: request(verb, stack, None, None),
            }),
        }
    }
    actions
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
            cards: Vec::new(),
        };
    };
    if inventory.stacks().is_empty() {
        return PanelView {
            heading: "Inventory: nothing carried".to_owned(),
            cards: Vec::new(),
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
        cards: inventory
            .stacks()
            .iter()
            .map(|stack| CardView {
                text: card_text(stack, catalog),
                actions: card_actions(stack, catalog, recipients),
            })
            .collect(),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn spawn_inventory_ui(mut commands: Commands, settings: Res<ClientSettings>) {
    commands
        .spawn((
            InventoryToggleButton,
            UiPanel,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(8.0),
                left: Val::Px(8.0),
                padding: UiRect::axes(Val::Px(12.0), Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(Color::srgb(0.3, 0.3, 0.5)),
        ))
        .with_child((
            Text::new(format!("Inventory ({})", key_label(settings.inventory_key))),
            TextFont::from_font_size(BUTTON_FONT_SIZE),
        ));
    commands.spawn((
        InventoryPanel,
        UiPanel,
        Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            top: Val::Px(44.0),
            left: Val::Px(8.0),
            width: Val::Px(PANEL_WIDTH_PIXELS),
            max_height: Val::Percent(70.0),
            flex_direction: FlexDirection::Column,
            padding: UiRect::all(Val::Px(8.0)),
            row_gap: Val::Px(6.0),
            overflow: Overflow::scroll_y(),
            ..default()
        },
        ScrollPosition::default(),
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.8)),
    ));
}

/// Spawns the heading and cards of `view` under the panel.
fn spawn_view(panel: &mut ChildSpawnerCommands, view: &PanelView) {
    panel.spawn((
        Text::new(view.heading.clone()),
        TextFont::from_font_size(HEADING_FONT_SIZE),
    ));
    for card in &view.cards {
        panel
            .spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    padding: UiRect::all(Val::Px(6.0)),
                    row_gap: Val::Px(4.0),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.15, 0.15, 0.2, 0.9)),
            ))
            .with_children(|card_node| {
                card_node.spawn((
                    Text::new(card.text.clone()),
                    TextFont::from_font_size(CARD_FONT_SIZE),
                ));
                card_node
                    .spawn(Node {
                        flex_wrap: FlexWrap::Wrap,
                        column_gap: Val::Px(4.0),
                        row_gap: Val::Px(4.0),
                        ..default()
                    })
                    .with_children(|row| {
                        for action in &card.actions {
                            row.spawn((
                                CardActionButton(action.kind.clone()),
                                Node {
                                    padding: UiRect::axes(Val::Px(8.0), Val::Px(3.0)),
                                    ..default()
                                },
                                BackgroundColor(Color::srgb(0.3, 0.3, 0.5)),
                            ))
                            .with_child((
                                Text::new(action.label.clone()),
                                TextFont::from_font_size(BUTTON_FONT_SIZE),
                            ));
                        }
                    });
            });
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

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn act_by_click(
    buttons: Res<ButtonInput<MouseButton>>,
    window: Single<&Window, With<PrimaryWindow>>,
    open: Res<InventoryPanelOpen>,
    panels: Query<(&ComputedNode, &UiGlobalTransform), With<InventoryPanel>>,
    actions: Query<(&ComputedNode, &UiGlobalTransform, &CardActionButton)>,
    mut requests: MessageWriter<ItemVerbRequest>,
    mut openings: MessageWriter<OpenPromptWithItem>,
) {
    // A button scrolled out of the panel is clipped but keeps its layout, so the cursor must
    // also be over the panel itself.
    if !open.0
        || !buttons.just_pressed(MouseButton::Left)
        || !cursor_is_over_any(&window, panels.iter())
    {
        return;
    }
    let clicked = actions
        .iter()
        .find(|(node, transform, _)| cursor_is_over_any(&window, [(*node, *transform)]));
    match clicked.map(|(_, _, button)| &button.0) {
        Some(CardActionKind::Send(request)) => {
            requests.write(request.clone());
        }
        Some(CardActionKind::Utilize(opening)) => {
            openings.write(opening.clone());
        }
        None => {}
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
    reason = "Bevy system parameters are passed by value"
)]
fn refresh_panel(
    mut commands: Commands,
    open: Res<InventoryPanelOpen>,
    local_player: Res<LocalPlayer>,
    holders: Holders,
    mut panels: Query<(Entity, &mut Node), With<InventoryPanel>>,
    mut shown: Local<Option<PanelView>>,
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
    if shown.as_ref() == Some(&view) {
        return;
    }
    for (panel, _) in &panels {
        commands
            .entity(panel)
            .despawn_children()
            .with_children(|children| spawn_view(children, &view));
    }
    *shown = Some(view);
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

    /// An inventory holding `items` and the stack ids they got, in order.
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

    fn bram() -> Recipient {
        Recipient {
            name: "Bram".to_owned(),
            id: EntityId(9),
        }
    }

    fn labels(actions: &[CardAction]) -> Vec<&str> {
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
    fn a_weapon_card_shows_damage_properties_and_mastery() {
        assert_eq!(
            card_text(&catalog_stack("longsword"), catalog()),
            "Longsword\nWeapon - 15 gp - 3 lb\nMartial Melee weapon: 1d8 Slashing, versatile \
             1d10\nProperties: Versatile\nMastery: Sap"
        );
    }

    #[test]
    fn armor_cards_show_armor_class_and_drawbacks() {
        assert_eq!(
            card_text(&catalog_stack("chain_mail"), catalog()),
            "Chain Mail\nArmor - 75 gp - 55 lb\nHeavy armor: AC 16\nNeeds Strength 13\n\
             Disadvantage on Dexterity (Stealth) checks"
        );
        assert!(card_text(&catalog_stack("shield"), catalog()).contains("Shield: +2 AC"));
    }

    #[test]
    fn a_tool_card_shows_its_ability_check_and_craft_targets() {
        let text = card_text(&catalog_stack("painters_supplies"), catalog());
        assert!(text.contains("Tool ability: Wisdom"), "{text}");
        assert!(
            text.contains(
                "Utilize: Paint a recognizable image of something you've seen (DC 10, Wisdom)"
            ),
            "{text}"
        );
        assert!(
            text.contains("Crafts: Druidic Focus, Holy Symbol"),
            "{text}"
        );
    }

    #[test]
    fn a_card_heading_shows_quantity_and_equipped_state() {
        let rations = single(Item::Catalog(id("rations")), 2);
        assert!(card_text(&rations, catalog()).starts_with("Rations x2\n"));
        let mut inventory = Inventory::default();
        let sword = inventory
            .add(Item::Catalog(id("longsword")), 1)
            .expect("added");
        inventory.set_equipped(sword, true).expect("equipped");
        let stack = inventory.stack(sword).expect("stack");
        assert!(card_text(stack, catalog()).starts_with("Longsword (equipped)\n"));
    }

    #[test]
    fn a_custom_card_shows_its_description_and_tags() {
        let letter = single(
            Item::Custom(CustomItem {
                name: "Sealed Letter".to_owned(),
                weight_thousandths_of_pound: 0,
                description: "Wax seal of House Vell.".to_owned(),
                tags: BTreeSet::from(["quest".to_owned()]),
            }),
            1,
        );
        assert_eq!(
            card_text(&letter, catalog()),
            "Sealed Letter\nCustom item - 0 lb\nWax seal of House Vell.\nTags: quest"
        );
        assert_eq!(
            labels(&card_actions(&letter, catalog(), &[bram()])),
            ["Drop", "Give to Bram"]
        );
    }

    #[test]
    fn verbs_follow_the_item_and_send_1_item() {
        let sword = catalog_stack("longsword");
        let actions = card_actions(&sword, catalog(), &[]);
        assert_eq!(labels(&actions), ["Equip", "Drop"]);
        assert_eq!(
            actions[0].kind,
            CardActionKind::Send(ItemVerbRequest {
                verb: ItemVerb::Equip,
                stack: sword.id(),
                quantity: 1,
                recipient: None,
                craft_target: None,
            })
        );
        let rations = catalog_stack("rations");
        assert_eq!(
            labels(&card_actions(&rations, catalog(), &[])),
            ["Consume", "Drop"]
        );
    }

    #[test]
    fn give_names_each_adjacent_recipient() {
        let rope = catalog_stack("rope");
        let actions = card_actions(&rope, catalog(), &[bram()]);
        let give = actions.last().expect("give");
        assert_eq!(give.label, "Give to Bram");
        assert_eq!(
            give.kind,
            CardActionKind::Send(ItemVerbRequest {
                verb: ItemVerb::Give,
                stack: rope.id(),
                quantity: 1,
                recipient: Some(EntityId(9)),
                craft_target: None,
            })
        );
    }

    #[test]
    fn craft_offers_only_targets_the_catalog_lists() {
        let supplies = catalog_stack("painters_supplies");
        let targets = craftable_targets(&supplies, catalog());
        assert_ne!(targets, Vec::<CatalogId>::new());
        let crafts: Vec<CardActionKind> = card_actions(&supplies, catalog(), &[])
            .into_iter()
            .filter(|action| action.label.starts_with("Craft "))
            .map(|action| action.kind)
            .collect();
        let expected: Vec<CardActionKind> = targets
            .into_iter()
            .map(|target| request(ItemVerb::Craft, &supplies, None, Some(target)))
            .collect();
        assert_eq!(crafts, expected);
        assert!(
            !labels(&card_actions(&catalog_stack("rope"), catalog(), &[]))
                .iter()
                .any(|label| label.starts_with("Craft"))
        );
    }

    #[test]
    fn utilize_opens_the_prompt_with_the_check_attached() {
        let rope = catalog_stack("rope");
        let utilize = card_actions(&rope, catalog(), &[])
            .into_iter()
            .find(|action| action.label == "Utilize")
            .expect("rope offers Utilize");
        assert_eq!(
            utilize.kind,
            CardActionKind::Utilize(OpenPromptWithItem {
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
        assert_eq!(view.cards.len(), 2);
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

    fn tap(app: &mut App, key: KeyCode) {
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.reset_all();
        keys.press(key);
        app.update();
    }

    fn panel_display(app: &mut App) -> Display {
        let mut panels = app
            .world_mut()
            .query_filtered::<&Node, With<InventoryPanel>>();
        panels.single(app.world()).expect("1 panel").display
    }

    #[test]
    fn the_key_toggles_a_panel_of_the_local_characters_items() {
        let mut app = test_app();
        let (carried, _) = inventory(vec![(Item::Catalog(id("longsword")), 1)]);
        app.world_mut()
            .spawn((EntityId(1), GridPos::new(2, 2), carried));
        app.world_mut().resource_mut::<LocalPlayer>().character = Some(EntityId(1));
        app.update();
        assert_eq!(panel_display(&mut app), Display::None);

        // Without the input plugin `just_pressed` is never cleared, so update only once.
        tap(&mut app, crate::settings::INVENTORY_KEY);
        assert_eq!(panel_display(&mut app), Display::Flex);
        let mut texts = app.world_mut().query::<&Text>();
        assert!(
            texts
                .iter(app.world())
                .any(|text| text.0.starts_with("Longsword\n"))
        );
        let mut buttons = app.world_mut().query::<&CardActionButton>();
        assert_eq!(buttons.iter(app.world()).count(), 2);

        tap(&mut app, crate::settings::INVENTORY_KEY);
        assert_eq!(panel_display(&mut app), Display::None);
    }

    #[test]
    fn the_key_is_ignored_while_typing() {
        let mut app = test_app();
        app.world_mut().resource_mut::<TextEntryActive>().0 = true;
        tap(&mut app, crate::settings::INVENTORY_KEY);
        assert!(!app.world().resource::<InventoryPanelOpen>().0);
    }
}
