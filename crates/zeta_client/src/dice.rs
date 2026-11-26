//! Visible d20 roll: an animated die that tumbles, then settles on the server's natural roll
//! and shows the Difficulty Class, the reason, and whether the check succeeded.
//!
//! Rules: `docs/prototypes/realism_prototype.md` and `docs/architecture/technical_architecture.md`
//! Section 5. The server rolls; the tumbling faces are decoration only.

use std::collections::VecDeque;

use bevy::prelude::*;
use bevy::window::RequestRedraw;
use zeta_core::entity::EntityId;
use zeta_core::protocol::{DiceRollShown, EntityName};

use crate::focus::UiPanel;
use crate::settings::ClientSettings;
use crate::theme::{FAILURE_COLOR, SUCCESS_COLOR, TEXT_ON_WOOD, UiTheme};

/// Font size of the die face.
const FACE_FONT_SIZE: f32 = 64.0;
/// Font size of the roll details.
const DETAIL_FONT_SIZE: f32 = 16.0;
/// Number of faces on the die.
const D20_FACES: u64 = 20;
/// Step between tumbling faces, coprime with 20 so every face appears.
const TUMBLE_STEP: u64 = 7;
/// Face color while tumbling.
const TUMBLING_COLOR: Color = TEXT_ON_WOOD;

/// How long each part of the dice animation lasts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiceTiming {
    /// Seconds the die tumbles.
    pub animation_seconds: f32,
    /// Seconds between tumbling faces.
    pub face_seconds: f32,
    /// Seconds the settled result stays.
    pub result_seconds: f32,
}

impl DiceTiming {
    fn from_settings(settings: &ClientSettings) -> Self {
        Self {
            animation_seconds: settings.dice_animation_seconds,
            face_seconds: settings.dice_face_seconds,
            result_seconds: settings.dice_result_seconds,
        }
    }
}

/// The roll on screen and the rolls waiting, in log sequence order.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct DiceDisplay {
    waiting: VecDeque<DiceRollShown>,
    active: Option<ActiveRoll>,
}

#[derive(Clone, Debug, PartialEq)]
struct ActiveRoll {
    roll: DiceRollShown,
    elapsed_seconds: f32,
    face_timer_seconds: f32,
    faces_shown: u64,
}

impl ActiveRoll {
    fn new(roll: DiceRollShown) -> Self {
        Self {
            roll,
            elapsed_seconds: 0.0,
            face_timer_seconds: 0.0,
            faces_shown: 0,
        }
    }
}

/// What the die shows right now.
#[derive(Clone, Debug, PartialEq)]
pub struct DiceView {
    /// The face shown.
    pub face: u8,
    /// Whether the die has settled on the natural roll.
    pub settled: bool,
    /// The roll being shown.
    pub roll: DiceRollShown,
}

impl DiceDisplay {
    /// Queues a roll behind earlier rolls, ordered by log sequence. A roll already shown or
    /// queued is ignored.
    pub fn push(&mut self, roll: DiceRollShown) {
        let known = self
            .active
            .iter()
            .map(|active| &active.roll)
            .chain(self.waiting.iter())
            .any(|existing| existing.sequence == roll.sequence);
        if known {
            return;
        }
        let index = self
            .waiting
            .iter()
            .position(|waiting| waiting.sequence > roll.sequence)
            .unwrap_or(self.waiting.len());
        self.waiting.insert(index, roll);
        if self.active.is_none() {
            self.active = self.waiting.pop_front().map(ActiveRoll::new);
        }
    }

    /// Moves the animation forward by `delta_seconds`, starting the next roll once the
    /// current result has been shown long enough.
    pub fn advance(&mut self, delta_seconds: f32, timing: DiceTiming) {
        let Some(active) = &mut self.active else {
            return;
        };
        active.elapsed_seconds += delta_seconds;
        active.face_timer_seconds += delta_seconds;
        if timing.face_seconds > 0.0 {
            while active.face_timer_seconds >= timing.face_seconds {
                active.face_timer_seconds -= timing.face_seconds;
                active.faces_shown = active.faces_shown.wrapping_add(1);
            }
        }
        if active.elapsed_seconds >= timing.animation_seconds + timing.result_seconds {
            self.active = self.waiting.pop_front().map(ActiveRoll::new);
        }
    }

    /// What the die shows, or `None` when no roll is on screen.
    pub fn view(&self, timing: DiceTiming) -> Option<DiceView> {
        let active = self.active.as_ref()?;
        let settled = active.elapsed_seconds >= timing.animation_seconds;
        let face = if settled {
            active.roll.natural.face()
        } else {
            tumbling_face(active.roll.sequence, active.faces_shown)
        };
        Some(DiceView {
            face,
            settled,
            roll: active.roll.clone(),
        })
    }
}

/// A decorative face for the `faces_shown`-th step of the tumble of roll `sequence`.
fn tumbling_face(sequence: u64, faces_shown: u64) -> u8 {
    let index = sequence
        .wrapping_add(faces_shown.wrapping_mul(TUMBLE_STEP))
        .rem_euclid(D20_FACES);
    u8::try_from(index + 1).expect("an index below 20 plus 1 fits in u8")
}

/// The detail lines under the die: who rolls, the Difficulty Class and reason, and once
/// settled the total and the outcome.
pub fn dice_detail_text(view: &DiceView, actor_name: Option<&str>) -> String {
    let roll = &view.roll;
    let actor = actor_name.unwrap_or("Someone");
    let header = format!("{actor} rolls a d20\nDC {}: {}", roll.dc, roll.reason);
    if !view.settled {
        return header;
    }
    let natural = i16::from(roll.natural.face());
    let modifier = i16::from(roll.modifier);
    let total = natural + modifier;
    let sign = if modifier < 0 { '-' } else { '+' };
    let outcome = if roll.success { "Success" } else { "Failure" };
    format!(
        "{header}\n{natural} {sign} {} = {total}: {outcome}",
        modifier.abs()
    )
}

/// Marks the root node of the dice panel.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DicePanel;

/// Marks the die face text.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiceFaceText;

/// Marks the roll details text.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiceDetailText;

/// Shows every `DiceRollShown` as an animated d20.
#[derive(Clone, Copy, Debug, Default)]
pub struct DiceRollPlugin;

impl Plugin for DiceRollPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ClientSettings>()
            .init_resource::<DiceDisplay>()
            .init_resource::<UiTheme>()
            // Registers only if no window plugin did (bevy_app-0.20.1/src/sub_app.rs:395).
            .add_message::<RequestRedraw>()
            .add_systems(Startup, spawn_dice_panel)
            .add_systems(
                Update,
                (queue_rolls, animate_dice, show_dice, redraw_while_tumbling).chain(),
            );
    }
}

/// Keeps the reactive update loop (`crate::frame_pacing`) drawing every frame while the die
/// tumbles, so the faces change smoothly. `RequestRedraw`: bevy_window-0.20.1/src/event.rs:61.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn redraw_while_tumbling(
    display: Res<DiceDisplay>,
    settings: Res<ClientSettings>,
    mut redraws: MessageWriter<RequestRedraw>,
) {
    let tumbling = display
        .view(DiceTiming::from_settings(&settings))
        .is_some_and(|view| !view.settled);
    if tumbling {
        redraws.write(RequestRedraw);
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn spawn_dice_panel(mut commands: Commands, theme: Res<UiTheme>) {
    commands
        .spawn((
            DicePanel,
            UiPanel,
            Node {
                display: Display::None,
                position_type: PositionType::Absolute,
                top: Val::Percent(25.0),
                left: Val::Percent(35.0),
                right: Val::Percent(35.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                padding: UiRect::axes(Val::Px(20.0), Val::Px(16.0)),
                row_gap: Val::Px(6.0),
                ..default()
            },
            theme.wood_panel(),
        ))
        .with_children(|panel| {
            panel
                .spawn(Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(12.0),
                    ..default()
                })
                .with_children(|header| {
                    header.spawn((
                        Node {
                            width: Val::Px(36.0),
                            height: Val::Px(36.0),
                            ..default()
                        },
                        theme.tinted_icon(theme.icons.d20.clone(), TEXT_ON_WOOD),
                    ));
                    header.spawn((
                        DiceFaceText,
                        Text::new(""),
                        theme.heading_font(FACE_FONT_SIZE),
                        TextColor(TUMBLING_COLOR),
                    ));
                });
            panel.spawn((
                DiceDetailText,
                Text::new(""),
                theme.ui_font(DETAIL_FONT_SIZE),
                TextColor(TEXT_ON_WOOD),
                TextLayout::justify(Justify::Center),
            ));
        });
}

fn queue_rolls(mut rolls: MessageReader<DiceRollShown>, mut display: ResMut<DiceDisplay>) {
    for roll in rolls.read() {
        display.push(roll.clone());
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn animate_dice(time: Res<Time>, settings: Res<ClientSettings>, mut display: ResMut<DiceDisplay>) {
    display.advance(time.delta_secs(), DiceTiming::from_settings(&settings));
}

type FaceText<'w, 's> = Query<
    'w,
    's,
    (&'static mut Text, &'static mut TextColor),
    (With<DiceFaceText>, Without<DiceDetailText>),
>;
type DetailText<'w, 's> =
    Query<'w, 's, &'static mut Text, (With<DiceDetailText>, Without<DiceFaceText>)>;

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn show_dice(
    display: Res<DiceDisplay>,
    settings: Res<ClientSettings>,
    names: Query<(&EntityId, &EntityName)>,
    mut panels: Query<&mut Node, With<DicePanel>>,
    mut faces: FaceText,
    mut details: DetailText,
) {
    let view = display.view(DiceTiming::from_settings(&settings));
    let wanted_display = if view.is_some() {
        Display::Flex
    } else {
        Display::None
    };
    for mut node in &mut panels {
        if node.display != wanted_display {
            node.display = wanted_display;
        }
    }
    let Some(view) = view else {
        return;
    };
    let face = view.face.to_string();
    let color = match (view.settled, view.roll.success) {
        (false, _) => TUMBLING_COLOR,
        (true, true) => SUCCESS_COLOR,
        (true, false) => FAILURE_COLOR,
    };
    for (mut text, mut text_color) in &mut faces {
        if text.0 != face {
            text.0.clone_from(&face);
        }
        if text_color.0 != color {
            text_color.0 = color;
        }
    }
    let actor_name = names
        .iter()
        .find(|(id, _)| **id == view.roll.actor)
        .map(|(_, name)| name.0.as_str());
    let detail = dice_detail_text(&view, actor_name);
    for mut text in &mut details {
        if text.0 != detail {
            text.0.clone_from(&detail);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::time::TimeUpdateStrategy;
    use zeta_core::resolution::D20;

    use super::*;

    const TIMING: DiceTiming = DiceTiming {
        animation_seconds: 1.0,
        face_seconds: 0.1,
        result_seconds: 2.0,
    };

    fn roll(sequence: u64, natural: u8, modifier: i8, success: bool) -> DiceRollShown {
        DiceRollShown {
            sequence,
            actor: EntityId(1),
            natural: D20::new(natural).expect("a valid face"),
            modifier,
            dc: 12,
            reason: "Leap the gap".to_owned(),
            success,
        }
    }

    #[test]
    fn the_die_tumbles_then_settles_on_the_natural_roll() {
        let mut display = DiceDisplay::default();
        display.push(roll(1, 17, 0, true));
        let tumbling = display.view(TIMING).expect("a roll is shown");
        assert!(!tumbling.settled);
        display.advance(1.0, TIMING);
        let settled = display.view(TIMING).expect("a roll is shown");
        assert!(settled.settled);
        assert_eq!(settled.face, 17);
    }

    #[test]
    fn tumbling_faces_are_always_valid_d20_faces_and_change() {
        let faces: Vec<u8> = (0..40).map(|step| tumbling_face(3, step)).collect();
        assert!(faces.iter().all(|face| (1..=20).contains(face)));
        assert!(faces.windows(2).all(|pair| pair[0] != pair[1]));
    }

    #[test]
    fn the_display_hides_after_the_result_time_and_then_shows_the_next_roll() {
        let mut display = DiceDisplay::default();
        display.push(roll(2, 5, 0, false));
        display.push(roll(3, 9, 0, false));
        display.advance(3.0, TIMING);
        assert_eq!(
            display.view(TIMING).expect("the next roll").roll.sequence,
            3
        );
        display.advance(3.0, TIMING);
        assert_eq!(display.view(TIMING), None);
    }

    #[test]
    fn waiting_rolls_are_shown_in_sequence_order_and_duplicates_are_dropped() {
        let mut display = DiceDisplay::default();
        display.push(roll(1, 5, 0, false));
        display.push(roll(9, 5, 0, false));
        display.push(roll(4, 5, 0, false));
        display.push(roll(4, 5, 0, false));
        let mut order = Vec::new();
        while let Some(view) = display.view(TIMING) {
            order.push(view.roll.sequence);
            display.advance(3.0, TIMING);
        }
        assert_eq!(order, vec![1, 4, 9]);
    }

    #[test]
    fn details_show_the_dc_reason_total_and_outcome_once_settled() {
        let mut view = DiceView {
            face: 3,
            settled: false,
            roll: roll(1, 14, -2, true),
        };
        assert_eq!(
            dice_detail_text(&view, Some("Ayla")),
            "Ayla rolls a d20\nDC 12: Leap the gap"
        );
        view.settled = true;
        assert_eq!(
            dice_detail_text(&view, None),
            "Someone rolls a d20\nDC 12: Leap the gap\n14 - 2 = 12: Success"
        );
        view.roll = roll(1, 8, 3, false);
        assert!(dice_detail_text(&view, None).ends_with("8 + 3 = 11: Failure"));
    }

    #[test]
    fn the_plugin_shows_a_received_roll_and_settles_it_over_time() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<DiceRollShown>()
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
                500,
            )))
            .add_plugins(DiceRollPlugin);
        app.update();
        let mut panels = app.world_mut().query_filtered::<&Node, With<DicePanel>>();
        assert_eq!(
            panels.single(app.world()).expect("1 panel").display,
            Display::None
        );

        app.world_mut().write_message(roll(1, 20, 0, true));
        app.update();
        assert!(
            !app.world().resource::<Messages<RequestRedraw>>().is_empty(),
            "a tumbling die keeps the client drawing"
        );
        for _ in 0..4 {
            app.update();
        }
        assert_eq!(
            panels.single(app.world()).expect("1 panel").display,
            Display::Flex
        );
        let mut faces = app
            .world_mut()
            .query_filtered::<&Text, With<DiceFaceText>>();
        assert_eq!(faces.single(app.world()).expect("1 face").0, "20");
    }
}
