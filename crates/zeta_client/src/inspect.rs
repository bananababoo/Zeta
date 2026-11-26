//! Right-click entity inspection popup: shows the name, description, and attributes (kind,
//! passability, and narrative tags) of any entity on the clicked map square.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 5 (independent plugin over
//! replicated components) and `docs/design/visual_style_guide.md`.

use bevy::prelude::*;
use bevy::ui::UiGlobalTransform;
use bevy::window::{PrimaryWindow, RequestRedraw};
use zeta_core::grid::GridPos;
use zeta_core::protocol::{Collider, Creature, EntityName, MapArea, PlayerCharacter, Tags, Visual};

use crate::focus::{
    TextEntryActive, TextEntryUpdate, UiClickConsumed, UiClickUpdate, UiPanel,
    clear_ui_click_consumed, cursor_is_over_any,
};
use crate::inventory::keep_on_screen;
use crate::layout::square_at;
use crate::map::is_alive_creature;
use crate::settings::ClientSettings;
use crate::theme::{TEXT_ON_PARCHMENT, UiTheme};

/// Font size of an inspected entity's name.
const TITLE_FONT_SIZE: f32 = 18.0;
/// Font size of the description and attribute lines.
const BODY_FONT_SIZE: f32 = 17.0;
/// Maximum text width inside the inspect card before wrapping, in logical pixels.
const CARD_TEXT_MAX_WIDTH_PIXELS: f32 = 280.0;
/// Space between the cursor and the inspect popup's top-left corner, in logical pixels.
const CURSOR_OFFSET_PIXELS: f32 = 8.0;
/// Draw order of the inspect popup above map panels.
const INSPECT_Z_INDEX: i32 = 10;
/// Fallback name for an entity without an `EntityName`.
const UNNAMED_ENTITY: &str = "Unknown";

/// 1 inspected entity's formatted name and attribute lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectedEntry {
    /// Display name.
    pub name: String,
    /// Description and attribute lines shown below the name.
    pub details: String,
}

/// The currently open inspect popup, or `None` when closed.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct InspectPopupState {
    /// Entries shown in the popup.
    pub entries: Vec<InspectedEntry>,
    /// Anchor position in logical window pixels before on-screen clamping.
    pub anchor: Vec2,
}

/// Marks the root node of the right-click inspect popup.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InspectMenu;

/// Stores the popup's desired top-left corner in logical pixels before clamping.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
struct InspectAnchor(Vec2);

/// Right-click entity inspection popup plugin.
#[derive(Clone, Copy, Debug, Default)]
pub struct EntityInspectPlugin;

impl Plugin for EntityInspectPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ClientSettings>()
            .init_resource::<TextEntryActive>()
            .init_resource::<UiClickConsumed>()
            .init_resource::<InspectPopupState>()
            .init_resource::<UiTheme>()
            .add_message::<RequestRedraw>()
            .add_systems(First, clear_ui_click_consumed)
            .add_systems(
                Update,
                (
                    close_by_key.after(TextEntryUpdate),
                    dismiss_on_left_click.in_set(UiClickUpdate),
                    inspect_on_right_click.in_set(UiClickUpdate),
                    sync_inspect_popup,
                    keep_inspect_on_screen,
                )
                    .chain(),
            );
    }
}

/// Formats 1 entity's name, optional visual description, kind/passability, and tags.
#[must_use]
pub fn format_entry(
    name: Option<&EntityName>,
    visual: Option<&Visual>,
    tags: Option<&Tags>,
    collider: Option<&Collider>,
    is_creature: bool,
    is_player: bool,
) -> InspectedEntry {
    let display_name = name
        .map(|n| n.0.trim())
        .filter(|n| !n.is_empty())
        .unwrap_or(UNNAMED_ENTITY)
        .to_owned();

    let mut lines = Vec::new();
    if let Some(look) = visual.map(|v| v.0.trim()).filter(|v| !v.is_empty()) {
        lines.push(look.to_owned());
    }

    let kind = if is_alive_creature(is_creature, is_player, tags) {
        "Creature"
    } else if is_creature || is_player {
        "Creature (dead)"
    } else {
        "Object"
    };
    let movement = match collider {
        Some(c) if !c.passable => "Blocking",
        _ => "Passable",
    };
    lines.push(format!("{kind} - {movement}"));

    if let Some(tags) = tags
        && !tags.0.is_empty()
    {
        let joined: Vec<&str> = tags.0.iter().map(String::as_str).collect();
        lines.push(format!("Attributes: {}", joined.join(", ")));
    }

    InspectedEntry {
        name: display_name,
        details: lines.join("\n"),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn close_by_key(
    keys: Res<ButtonInput<KeyCode>>,
    text_entry: Res<TextEntryActive>,
    mut state: ResMut<InspectPopupState>,
) {
    if !text_entry.0 && keys.just_pressed(KeyCode::Escape) && !state.entries.is_empty() {
        state.entries.clear();
    }
}

/// Left-clicking outside an open inspect popup closes it and consumes the click so the map
/// does not also move the character.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn dismiss_on_left_click(
    buttons: Res<ButtonInput<MouseButton>>,
    window: Single<&Window, With<PrimaryWindow>>,
    menus: Query<(&ComputedNode, &UiGlobalTransform), With<InspectMenu>>,
    mut state: ResMut<InspectPopupState>,
    mut click_consumed: ResMut<UiClickConsumed>,
) {
    if state.entries.is_empty() || !buttons.just_pressed(MouseButton::Left) {
        return;
    }
    if cursor_is_over_any(&window, menus.iter()) {
        click_consumed.0 = true;
        return;
    }
    state.entries.clear();
    click_consumed.0 = true;
}

type OtherUiPanels<'w, 's> = Query<
    'w,
    's,
    (&'static ComputedNode, &'static UiGlobalTransform),
    (With<UiPanel>, Without<InspectMenu>),
>;

type InspectableEntities<'w, 's> = Query<
    'w,
    's,
    (
        &'static GridPos,
        Option<&'static EntityName>,
        Option<&'static Visual>,
        Option<&'static Tags>,
        Option<&'static Collider>,
        Has<Creature>,
        Has<PlayerCharacter>,
    ),
>;

#[expect(
    clippy::too_many_arguments,
    reason = "a Bevy system lists every resource and query it reads as a parameter"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn inspect_on_right_click(
    buttons: Res<ButtonInput<MouseButton>>,
    settings: Res<ClientSettings>,
    text_entry: Res<TextEntryActive>,
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<(&Camera, &GlobalTransform), With<Camera2d>>,
    panels: OtherUiPanels,
    areas: Query<&MapArea>,
    entities: InspectableEntities,
    mut state: ResMut<InspectPopupState>,
) {
    if text_entry.0 || !buttons.just_pressed(MouseButton::Right) {
        return;
    }
    if cursor_is_over_any(&window, panels.iter()) {
        return;
    }
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let (camera, camera_transform) = *camera;
    let Ok(world) = camera.viewport_to_world_2d(camera_transform, cursor) else {
        return;
    };
    let Some(area) = areas.iter().next() else {
        return;
    };
    let Some(square) = square_at(world, area.0, settings.tile_size) else {
        state.entries.clear();
        return;
    };

    let mut entries: Vec<InspectedEntry> = entities
        .iter()
        .filter(|(pos, ..)| **pos == square)
        .map(
            |(_, name, visual, tags, collider, is_creature, is_player)| {
                format_entry(name, visual, tags, collider, is_creature, is_player)
            },
        )
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    state.entries = entries;
    state.anchor = cursor + Vec2::splat(CURSOR_OFFSET_PIXELS);
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn sync_inspect_popup(
    mut commands: Commands,
    state: Res<InspectPopupState>,
    theme: Res<UiTheme>,
    existing: Query<Entity, With<InspectMenu>>,
    mut redraws: MessageWriter<RequestRedraw>,
) {
    if !state.is_changed() {
        return;
    }
    for entity in &existing {
        commands.entity(entity).despawn();
    }
    redraws.write(RequestRedraw);
    if state.entries.is_empty() {
        return;
    }
    commands
        .spawn((
            InspectMenu,
            UiPanel,
            InspectAnchor(state.anchor),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(state.anchor.x),
                top: Val::Px(state.anchor.y),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(14.0)),
                row_gap: Val::Px(8.0),
                ..default()
            },
            theme.parchment_panel(),
            GlobalZIndex(INSPECT_Z_INDEX),
        ))
        .with_children(|card| {
            for entry in &state.entries {
                card.spawn(Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(3.0),
                    ..default()
                })
                .with_children(|section| {
                    section.spawn((
                        Node {
                            max_width: Val::Px(CARD_TEXT_MAX_WIDTH_PIXELS),
                            ..default()
                        },
                        Text::new(entry.name.clone()),
                        theme.heading_font(TITLE_FONT_SIZE),
                        TextColor(TEXT_ON_PARCHMENT),
                    ));
                    section.spawn((
                        Node {
                            max_width: Val::Px(CARD_TEXT_MAX_WIDTH_PIXELS),
                            ..default()
                        },
                        Text::new(entry.details.clone()),
                        theme.narration_font(BODY_FONT_SIZE),
                        TextColor(TEXT_ON_PARCHMENT),
                    ));
                });
            }
        });
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn keep_inspect_on_screen(
    window: Single<&Window, With<PrimaryWindow>>,
    mut menus: Query<(&ComputedNode, &InspectAnchor, &mut Node), With<InspectMenu>>,
) {
    let screen = Vec2::new(window.width(), window.height());
    for (computed, anchor, mut node) in &mut menus {
        if computed.size() == Vec2::ZERO {
            continue;
        }
        let size = computed.size() * computed.inverse_scale_factor();
        let clamped = keep_on_screen(anchor.0, size, screen);
        let (left, top) = (Val::Px(clamped.x), Val::Px(clamped.y));
        if node.left != left || node.top != top {
            node.left = left;
            node.top = top;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn format_entry_shows_name_description_and_attributes_for_objects() {
        let entry = format_entry(
            Some(&EntityName("Wooden Bridge".to_owned())),
            Some(&Visual("A weathered timber plank bridge.".to_owned())),
            Some(&Tags(BTreeSet::from([
                "flammable".to_owned(),
                "wooden".to_owned(),
            ]))),
            Some(&Collider { passable: true }),
            false,
            false,
        );
        assert_eq!(entry.name, "Wooden Bridge");
        assert_eq!(
            entry.details,
            "A weathered timber plank bridge.\nObject - Passable\nAttributes: flammable, wooden"
        );
    }

    #[test]
    fn format_entry_distinguishes_alive_and_dead_creatures() {
        let alive = format_entry(
            Some(&EntityName("Goblin".to_owned())),
            None,
            None,
            Some(&Collider { passable: false }),
            true,
            false,
        );
        assert_eq!(alive.details, "Creature - Blocking");

        let dead = format_entry(
            Some(&EntityName("Goblin".to_owned())),
            None,
            Some(&Tags(BTreeSet::from(["dead".to_owned()]))),
            Some(&Collider { passable: true }),
            true,
            false,
        );
        assert_eq!(dead.details, "Creature (dead) - Passable\nAttributes: dead");
    }

    #[test]
    fn escape_closes_an_open_inspect_popup() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .add_plugins(EntityInspectPlugin);
        app.world_mut().resource_mut::<InspectPopupState>().entries = vec![InspectedEntry {
            name: "Wooden Bridge".to_owned(),
            details: "Object - Passable".to_owned(),
        }];
        app.update();
        let mut menus = app.world_mut().query::<&InspectMenu>();
        assert_eq!(menus.iter(app.world()).count(), 1);

        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Escape);
        app.update();
        assert_eq!(menus.iter(app.world()).count(), 0);
    }
}
