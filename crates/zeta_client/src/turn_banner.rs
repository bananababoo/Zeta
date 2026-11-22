//! Turn Bubble banner: whose turn it is, the round, and what the current participant has used,
//! with a key and a button that end the local player's turn.
//!
//! Rules: `docs/prototypes/realism_prototype.md` and `docs/architecture/technical_architecture.md`
//! Section 5.

use bevy::prelude::*;
use bevy::ui::UiGlobalTransform;
use bevy::window::PrimaryWindow;
use zeta_core::entity::EntityId;
use zeta_core::protocol::{EndTurnRequest, EntityName, TurnBubble};

use crate::focus::{TextEntryActive, TextEntryUpdate, UiPanel, cursor_is_over};
use crate::player::LocalPlayer;
use crate::settings::ClientSettings;

/// Font size of the banner text.
const BANNER_FONT_SIZE: f32 = 20.0;
/// Font size of the end turn button.
const BUTTON_FONT_SIZE: f32 = 16.0;
/// Shown for a participant whose name has not replicated yet.
const UNKNOWN_PARTICIPANT: &str = "Someone";

/// Marks the root node of the banner.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TurnBanner;

/// Marks the banner text.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TurnBannerText;

/// Marks the button that ends the local player's turn.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EndTurnButton;

/// Shows the Turn Bubble banner and sends `EndTurnRequest`.
#[derive(Clone, Copy, Debug, Default)]
pub struct TurnBubbleBannerPlugin;

impl Plugin for TurnBubbleBannerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ClientSettings>()
            .init_resource::<LocalPlayer>()
            .init_resource::<TextEntryActive>()
            .add_systems(Startup, spawn_banner)
            .add_systems(
                Update,
                (
                    show_banner,
                    end_turn_by_key.after(TextEntryUpdate),
                    end_turn_by_click,
                ),
            );
    }
}

/// The participant whose turn it is, or `None` when `current` is past the end of `order`.
pub fn current_participant(bubble: &TurnBubble) -> Option<EntityId> {
    let index = usize::try_from(bubble.current).ok()?;
    bubble.order.get(index).copied()
}

/// The Turn Bubble to show: the one the local character is in, else the first.
pub fn relevant_bubble<'a>(
    bubbles: impl IntoIterator<Item = &'a TurnBubble>,
    local_player: LocalPlayer,
) -> Option<&'a TurnBubble> {
    let mut first = None;
    for bubble in bubbles {
        if local_player
            .character
            .is_some_and(|character| bubble.order.contains(&character))
        {
            return Some(bubble);
        }
        first = first.or(Some(bubble));
    }
    first
}

/// Whether it is the local character's turn in `bubble`.
pub fn is_local_turn(bubble: &TurnBubble, local_player: LocalPlayer) -> bool {
    current_participant(bubble).is_some_and(|participant| local_player.is(participant))
}

/// The banner text for `bubble`. `name_of` looks up a participant's display name.
pub fn banner_text(
    bubble: &TurnBubble,
    local_player: LocalPlayer,
    name_of: impl Fn(EntityId) -> Option<String>,
) -> String {
    let round = bubble.completed_rounds.saturating_add(1);
    if is_local_turn(bubble, local_player) {
        let movement = if bubble.movement_used {
            "used"
        } else {
            "available"
        };
        let prompt = if bubble.prompt_used {
            "used"
        } else {
            "available"
        };
        format!("Your turn - Round {round}\nMovement: {movement}   Action: {prompt}")
    } else {
        let name = current_participant(bubble)
            .and_then(name_of)
            .unwrap_or_else(|| UNKNOWN_PARTICIPANT.to_owned());
        format!("{name}'s turn - Round {round}")
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn spawn_banner(mut commands: Commands, settings: Res<ClientSettings>) {
    let key_name = format!("End turn ({:?})", settings.end_turn_key);
    commands
        .spawn((
            TurnBanner,
            UiPanel,
            Node {
                display: Display::None,
                position_type: PositionType::Absolute,
                top: Val::Px(8.0),
                left: Val::Percent(30.0),
                right: Val::Percent(30.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                padding: UiRect::all(Val::Px(8.0)),
                row_gap: Val::Px(6.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.1, 0.1, 0.2, 0.85)),
        ))
        .with_children(|banner| {
            banner.spawn((
                TurnBannerText,
                Text::new(""),
                TextFont::from_font_size(BANNER_FONT_SIZE),
                TextLayout::justify(Justify::Center),
            ));
            banner
                .spawn((
                    EndTurnButton,
                    Node {
                        display: Display::None,
                        padding: UiRect::axes(Val::Px(12.0), Val::Px(4.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.3, 0.3, 0.5)),
                ))
                .with_child((
                    Text::new(key_name),
                    TextFont::from_font_size(BUTTON_FONT_SIZE),
                ));
        });
}

type BannerNode<'w, 's> =
    Query<'w, 's, &'static mut Node, (With<TurnBanner>, Without<EndTurnButton>)>;
type ButtonNode<'w, 's> =
    Query<'w, 's, &'static mut Node, (With<EndTurnButton>, Without<TurnBanner>)>;

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn show_banner(
    local_player: Res<LocalPlayer>,
    bubbles: Query<&TurnBubble>,
    names: Query<(&EntityId, &EntityName)>,
    mut banners: BannerNode,
    mut buttons: ButtonNode,
    mut texts: Query<&mut Text, With<TurnBannerText>>,
) {
    let bubble = relevant_bubble(bubbles.iter(), *local_player);
    let banner_display = if bubble.is_some() {
        Display::Flex
    } else {
        Display::None
    };
    let local_turn = bubble.is_some_and(|bubble| is_local_turn(bubble, *local_player));
    let button_display = if local_turn {
        Display::Flex
    } else {
        Display::None
    };
    for mut node in &mut banners {
        if node.display != banner_display {
            node.display = banner_display;
        }
    }
    for mut node in &mut buttons {
        if node.display != button_display {
            node.display = button_display;
        }
    }
    let Some(bubble) = bubble else {
        return;
    };
    let wanted = banner_text(bubble, *local_player, |participant| {
        names
            .iter()
            .find(|(id, _)| **id == participant)
            .map(|(_, name)| name.0.clone())
    });
    for mut text in &mut texts {
        if text.0 != wanted {
            text.0.clone_from(&wanted);
        }
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn end_turn_by_key(
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<ClientSettings>,
    text_entry: Res<TextEntryActive>,
    local_player: Res<LocalPlayer>,
    bubbles: Query<&TurnBubble>,
    mut requests: MessageWriter<EndTurnRequest>,
) {
    if text_entry.0 || !keys.just_pressed(settings.end_turn_key) {
        return;
    }
    if relevant_bubble(bubbles.iter(), *local_player)
        .is_some_and(|bubble| is_local_turn(bubble, *local_player))
    {
        requests.write(EndTurnRequest);
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn end_turn_by_click(
    buttons: Res<ButtonInput<MouseButton>>,
    local_player: Res<LocalPlayer>,
    window: Single<&Window, With<PrimaryWindow>>,
    end_turn_buttons: Query<(&ComputedNode, &UiGlobalTransform), With<EndTurnButton>>,
    bubbles: Query<&TurnBubble>,
    mut requests: MessageWriter<EndTurnRequest>,
) {
    if !buttons.just_pressed(MouseButton::Left) {
        return;
    }
    let clicked = end_turn_buttons
        .iter()
        .any(|(node, transform)| cursor_is_over(&window, node, *transform));
    if clicked
        && relevant_bubble(bubbles.iter(), *local_player)
            .is_some_and(|bubble| is_local_turn(bubble, *local_player))
    {
        requests.write(EndTurnRequest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bubble(order: Vec<u64>, current: u32) -> TurnBubble {
        TurnBubble {
            order: order.into_iter().map(EntityId).collect(),
            current,
            completed_rounds: 1,
            movement_used: true,
            prompt_used: false,
        }
    }

    fn local(id: u64) -> LocalPlayer {
        LocalPlayer {
            character: Some(EntityId(id)),
        }
    }

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<EndTurnRequest>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .add_plugins(TurnBubbleBannerPlugin);
        app.world_mut().insert_resource(local(1));
        app
    }

    fn banner_display(app: &mut App) -> Display {
        let mut nodes = app.world_mut().query_filtered::<&Node, With<TurnBanner>>();
        nodes.single(app.world()).expect("1 banner").display
    }

    fn banner_string(app: &mut App) -> String {
        let mut texts = app
            .world_mut()
            .query_filtered::<&Text, With<TurnBannerText>>();
        texts.single(app.world()).expect("1 banner text").0.clone()
    }

    fn press_end_turn(app: &mut App) -> usize {
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(crate::settings::END_TURN_KEY);
        app.update();
        app.world()
            .resource::<Messages<EndTurnRequest>>()
            .iter_current_update_messages()
            .count()
    }

    #[test]
    fn the_banner_says_your_turn_with_what_is_left_for_the_local_player() {
        let text = banner_text(&bubble(vec![1, 2], 0), local(1), |_| None);
        assert_eq!(
            text,
            "Your turn - Round 2\nMovement: used   Action: available"
        );
    }

    #[test]
    fn the_banner_names_another_participant_whose_turn_it_is() {
        let text = banner_text(&bubble(vec![1, 2], 1), local(1), |id| {
            (id == EntityId(2)).then(|| "Goblin".to_owned())
        });
        assert_eq!(text, "Goblin's turn - Round 2");
        let unnamed = banner_text(&bubble(vec![1, 2], 1), local(1), |_| None);
        assert_eq!(unnamed, "Someone's turn - Round 2");
    }

    #[test]
    fn a_current_index_past_the_order_has_no_participant() {
        assert_eq!(current_participant(&bubble(vec![1], 5)), None);
        assert!(!is_local_turn(&bubble(vec![1], 5), local(1)));
    }

    #[test]
    fn the_bubble_with_the_local_character_is_preferred() {
        let others = bubble(vec![5, 6], 0);
        let mine = bubble(vec![7, 1], 0);
        let bubbles = [others.clone(), mine.clone()];
        assert_eq!(relevant_bubble(&bubbles, local(1)), Some(&mine));
        assert_eq!(relevant_bubble(&bubbles, local(9)), Some(&others));
    }

    #[test]
    fn the_banner_is_hidden_without_a_turn_bubble_and_shown_with_one() {
        let mut app = test_app();
        app.update();
        assert_eq!(banner_display(&mut app), Display::None);

        app.world_mut().spawn(bubble(vec![1, 2], 0));
        app.update();
        assert_eq!(banner_display(&mut app), Display::Flex);
        assert!(banner_string(&mut app).starts_with("Your turn"));
    }

    #[test]
    fn the_end_turn_key_sends_a_request_only_on_the_local_turn() {
        let mut app = test_app();
        app.world_mut().spawn(bubble(vec![1, 2], 0));
        assert_eq!(press_end_turn(&mut app), 1);

        let mut app = test_app();
        app.world_mut().spawn(bubble(vec![1, 2], 1));
        assert_eq!(press_end_turn(&mut app), 0);
    }

    #[test]
    fn the_end_turn_key_does_nothing_while_typing() {
        let mut app = test_app();
        app.world_mut().spawn(bubble(vec![1, 2], 0));
        app.world_mut().resource_mut::<TextEntryActive>().0 = true;
        assert_eq!(press_end_turn(&mut app), 0);
    }
}
