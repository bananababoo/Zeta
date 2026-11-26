//! Shared narration and speech log, ordered by the server's log sequence, with speech bubbles
//! above speakers.
//!
//! Rules: `docs/prototypes/realism_prototype.md` and `docs/architecture/technical_architecture.md`
//! Section 5. Dice rolls share the log sequence, so they appear as log lines in order too.

use bevy::input::mouse::{MouseScrollUnit, MouseWheel};
use bevy::prelude::*;
use bevy::sprite::Anchor;
use bevy::text::TextBounds;
use bevy::ui::UiGlobalTransform;
use bevy::window::PrimaryWindow;
use zeta_core::entity::EntityId;
use zeta_core::grid::GridPos;
use zeta_core::protocol::{DiceRollShown, EntityName, LogEntryKind, NarrationLogEntry};
use zeta_core::resolution::Volume;

use crate::focus::{UiPanel, cursor_is_over_any};
use crate::layout::{FloatingOrigin, square_center_at};
use crate::planet_stream::PlanetFocusSync;
use crate::settings::ClientSettings;
use crate::theme::{SPEECH_BUBBLE_COLOR, TEXT_ON_PARCHMENT, UiTheme};

/// Font size of log lines.
const LOG_FONT_SIZE: f32 = 19.0;
/// Font size of speech bubbles relative to the square size.
const BUBBLE_FONT_SCALE: f32 = 0.45;
/// Width of a speech bubble in squares, before it wraps.
const BUBBLE_WIDTH_SQUARES: f32 = 6.0;
/// Height of a speech bubble's bottom above its speaker's center, in squares.
const BUBBLE_HEIGHT_SQUARES: f32 = 0.6;
/// Draw order of speech bubbles, above every map entity.
const BUBBLE_DEPTH: f32 = 10.0;
/// Shown for a speaker or actor without a name.
const UNKNOWN_SPEAKER: &str = "Someone";

/// 1 line of the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLine {
    /// The server's log sequence, shared by narration lines and dice rolls.
    pub sequence: u64,
    /// The text shown.
    pub text: String,
}

/// The log lines received so far, sorted by sequence, at most the configured number.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct NarrationLog {
    lines: Vec<LogLine>,
}

impl NarrationLog {
    /// The lines in sequence order.
    pub fn lines(&self) -> &[LogLine] {
        &self.lines
    }

    /// Inserts a line at its sequence position, ignoring a sequence already present, then
    /// drops the oldest lines beyond `max_lines`. Returns whether the log changed.
    pub fn insert(&mut self, line: LogLine, max_lines: usize) -> bool {
        let Err(index) = self
            .lines
            .binary_search_by_key(&line.sequence, |existing| existing.sequence)
        else {
            return false;
        };
        self.lines.insert(index, line);
        let excess = self.lines.len().saturating_sub(max_lines);
        self.lines.drain(..excess);
        true
    }

    /// The whole log as 1 text, 1 line per entry.
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The log text of a narration or speech entry.
pub fn entry_text(entry: &NarrationLogEntry) -> String {
    let speaker = entry.speaker_name.as_deref().unwrap_or(UNKNOWN_SPEAKER);
    match entry.kind {
        LogEntryKind::Action => format!("{speaker}: {}", entry.text),
        LogEntryKind::Narration => entry.text.clone(),
        LogEntryKind::Speech => {
            let verb = match entry.vol {
                Some(Volume::Whisper) => "whispers",
                Some(Volume::Shout) => "shouts",
                Some(Volume::Normal) | None => "says",
            };
            format!("{speaker} {verb}: \"{}\"", entry.text)
        }
        LogEntryKind::Notice => format!("Notice: {}", entry.text),
    }
}

/// The log text of a dice roll.
pub fn roll_text(roll: &DiceRollShown, actor_name: Option<&str>) -> String {
    let actor = actor_name.unwrap_or(UNKNOWN_SPEAKER);
    let natural = i16::from(roll.natural.face());
    let modifier = i16::from(roll.modifier);
    let sign = if modifier < 0 { '-' } else { '+' };
    let outcome = if roll.success { "success" } else { "failure" };
    format!(
        "{actor} rolls {natural} {sign} {} = {} against DC {} ({}): {outcome}",
        modifier.abs(),
        natural + modifier,
        roll.dc,
        roll.reason
    )
}

/// Marks the scrolling log panel.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NarrationLogPanel;

/// Marks the log text.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NarrationLogText;

/// A speech bubble above a speaker, removed after its time runs out.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct SpeechBubble {
    /// The speaking entity.
    pub speaker: EntityId,
    /// Seconds until the bubble disappears.
    pub remaining_seconds: f32,
}

/// The shared narration and speech log, and speech bubbles.
#[derive(Clone, Copy, Debug, Default)]
pub struct NarrationLogPlugin;

impl Plugin for NarrationLogPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ClientSettings>()
            .init_resource::<FloatingOrigin>()
            .init_resource::<NarrationLog>()
            .init_resource::<UiTheme>()
            .add_systems(Startup, spawn_log_panel)
            .add_systems(
                Update,
                (
                    (record_entries, record_rolls),
                    show_log,
                    scroll_log,
                    spawn_speech_bubbles,
                    update_speech_bubbles,
                )
                    .chain()
                    .after(PlanetFocusSync),
            );
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn spawn_log_panel(mut commands: Commands, theme: Res<UiTheme>) {
    commands
        .spawn((
            NarrationLogPanel,
            UiPanel,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(8.0),
                right: Val::Px(8.0),
                bottom: Val::Px(104.0),
                width: Val::Percent(28.0),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(14.0)),
                overflow: Overflow::scroll_y(),
                ..default()
            },
            ScrollPosition::default(),
            theme.parchment_panel(),
        ))
        .with_child((
            NarrationLogText,
            Text::new(""),
            theme.narration_font(LOG_FONT_SIZE),
            TextColor(TEXT_ON_PARCHMENT),
        ));
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn record_entries(
    mut entries: MessageReader<NarrationLogEntry>,
    settings: Res<ClientSettings>,
    mut log: ResMut<NarrationLog>,
) {
    for entry in entries.read() {
        let line = LogLine {
            sequence: entry.sequence,
            text: entry_text(entry),
        };
        log.insert(line, settings.narration_log_max_lines);
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn record_rolls(
    mut rolls: MessageReader<DiceRollShown>,
    settings: Res<ClientSettings>,
    names: Query<(&EntityId, &EntityName)>,
    mut log: ResMut<NarrationLog>,
) {
    for roll in rolls.read() {
        let actor_name = names
            .iter()
            .find(|(id, _)| **id == roll.actor)
            .map(|(_, name)| name.0.as_str());
        let line = LogLine {
            sequence: roll.sequence,
            text: roll_text(roll, actor_name),
        };
        log.insert(line, settings.narration_log_max_lines);
    }
}

/// Rewrites the log text and scrolls to the newest line when the log changes. Scrolling past
/// the end is clamped by the layout, so the largest value shows the bottom.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn show_log(
    log: Res<NarrationLog>,
    mut texts: Query<&mut Text, With<NarrationLogText>>,
    mut panels: Query<&mut ScrollPosition, With<NarrationLogPanel>>,
) {
    if !log.is_changed() {
        return;
    }
    let wanted = log.text();
    for mut text in &mut texts {
        text.0.clone_from(&wanted);
    }
    for mut scroll in &mut panels {
        scroll.y = f32::MAX;
    }
}

type LogPanel<'w, 's> = Query<
    'w,
    's,
    (
        &'static ComputedNode,
        &'static UiGlobalTransform,
        &'static mut ScrollPosition,
    ),
    With<NarrationLogPanel>,
>;

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn scroll_log(
    mut wheel: MessageReader<MouseWheel>,
    settings: Res<ClientSettings>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut panels: LogPanel,
) {
    let lines: f32 = wheel
        .read()
        .map(|event| match event.unit {
            MouseScrollUnit::Line => event.y * settings.narration_log_scroll_pixels_per_line,
            MouseScrollUnit::Pixel => event.y,
        })
        .sum();
    if lines == 0.0 {
        return;
    }
    for (node, transform, mut scroll) in &mut panels {
        if !cursor_is_over_any(&window, [(node, transform)]) {
            continue;
        }
        let max_scroll =
            (node.content_size().y - node.size().y).max(0.0) * node.inverse_scale_factor();
        scroll.y = (scroll.y.min(max_scroll) - lines).clamp(0.0, max_scroll);
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn spawn_speech_bubbles(
    mut commands: Commands,
    mut entries: MessageReader<NarrationLogEntry>,
    settings: Res<ClientSettings>,
    origin: Res<FloatingOrigin>,
    theme: Res<UiTheme>,
    speakers: Query<(&EntityId, &GridPos)>,
    bubbles: Query<(Entity, &SpeechBubble)>,
) {
    let tile_size = settings.tile_size;
    for entry in entries.read() {
        if entry.kind != LogEntryKind::Speech {
            continue;
        }
        let Some(speaker) = entry.speaker else {
            continue;
        };
        let Some((_, position)) = speakers.iter().find(|(id, _)| **id == speaker) else {
            continue;
        };
        for (bubble, existing) in &bubbles {
            if existing.speaker == speaker {
                commands.entity(bubble).despawn();
            }
        }
        commands.spawn((
            SpeechBubble {
                speaker,
                remaining_seconds: settings.speech_bubble_seconds,
            },
            Text2d::new(entry.text.clone()),
            theme.narration_font(tile_size * BUBBLE_FONT_SCALE),
            TextColor(SPEECH_BUBBLE_COLOR),
            TextLayout::justify(Justify::Center),
            TextBounds {
                width: Some(tile_size * BUBBLE_WIDTH_SQUARES),
                height: None,
            },
            Anchor::BOTTOM_CENTER,
            Transform::from_translation(bubble_position(*position, tile_size, *origin)),
        ));
    }
}

/// Where a speech bubble above a speaker on `position` sits.
fn bubble_position(position: GridPos, tile_size: f32, origin: FloatingOrigin) -> Vec3 {
    (square_center_at(position, tile_size, origin)
        + Vec2::new(0.0, tile_size * BUBBLE_HEIGHT_SQUARES))
    .extend(BUBBLE_DEPTH)
}

/// Keeps bubbles above moving speakers and removes them when their time runs out.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn update_speech_bubbles(
    mut commands: Commands,
    time: Res<Time>,
    settings: Res<ClientSettings>,
    origin: Res<FloatingOrigin>,
    speakers: Query<(&EntityId, &GridPos)>,
    mut bubbles: Query<(Entity, &mut SpeechBubble, &mut Transform)>,
) {
    for (entity, mut bubble, mut transform) in &mut bubbles {
        bubble.remaining_seconds -= time.delta_secs();
        if bubble.remaining_seconds <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        if let Some((_, position)) = speakers.iter().find(|(id, _)| **id == bubble.speaker) {
            let wanted = bubble_position(*position, settings.tile_size, *origin);
            if transform.translation != wanted {
                transform.translation = wanted;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::time::TimeUpdateStrategy;
    use zeta_core::resolution::D20;

    use super::*;

    fn entry(sequence: u64, kind: LogEntryKind, text: &str) -> NarrationLogEntry {
        NarrationLogEntry {
            sequence,
            kind,
            speaker: Some(EntityId(3)),
            speaker_name: Some("Goblin".to_owned()),
            text: text.to_owned(),
            vol: None,
        }
    }

    fn line(sequence: u64) -> LogLine {
        LogLine {
            sequence,
            text: sequence.to_string(),
        }
    }

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<NarrationLogEntry>()
            .add_message::<DiceRollShown>()
            .add_message::<MouseWheel>()
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
                200,
            )))
            .add_plugins(NarrationLogPlugin);
        app
    }

    fn log_text(app: &mut App) -> String {
        let mut texts = app
            .world_mut()
            .query_filtered::<&Text, With<NarrationLogText>>();
        texts.single(app.world()).expect("1 log text").0.clone()
    }

    fn bubble_count(app: &mut App) -> usize {
        let mut bubbles = app.world_mut().query::<&SpeechBubble>();
        bubbles.iter(app.world()).count()
    }

    #[test]
    fn lines_are_kept_in_sequence_order_whatever_order_they_arrive_in() {
        let mut log = NarrationLog::default();
        for sequence in [5, 1, 3] {
            assert!(log.insert(line(sequence), 10));
        }
        let order: Vec<u64> = log.lines().iter().map(|line| line.sequence).collect();
        assert_eq!(order, vec![1, 3, 5]);
        assert_eq!(log.text(), "1\n3\n5");
    }

    #[test]
    fn a_repeated_sequence_is_ignored() {
        let mut log = NarrationLog::default();
        assert!(log.insert(line(1), 10));
        assert!(!log.insert(line(1), 10));
        assert_eq!(log.lines().len(), 1);
    }

    #[test]
    fn the_oldest_lines_are_dropped_beyond_the_limit() {
        let mut log = NarrationLog::default();
        for sequence in 1..=5 {
            log.insert(line(sequence), 3);
        }
        assert_eq!(log.text(), "3\n4\n5");
    }

    #[test]
    fn each_entry_kind_has_its_own_line_format() {
        assert_eq!(
            entry_text(&entry(1, LogEntryKind::Action, "I kick the barrel")),
            "Goblin: I kick the barrel"
        );
        assert_eq!(
            entry_text(&entry(1, LogEntryKind::Narration, "It rolls.")),
            "It rolls."
        );
        assert_eq!(
            entry_text(&entry(1, LogEntryKind::Notice, "Please rephrase.")),
            "Notice: Please rephrase."
        );
        let mut shout = entry(1, LogEntryKind::Speech, "Stop!");
        shout.vol = Some(Volume::Shout);
        assert_eq!(entry_text(&shout), "Goblin shouts: \"Stop!\"");
        shout.vol = None;
        shout.speaker_name = None;
        assert_eq!(entry_text(&shout), "Someone says: \"Stop!\"");
    }

    #[test]
    fn a_roll_line_shows_the_total_dc_and_outcome() {
        let roll = DiceRollShown {
            sequence: 2,
            actor: EntityId(1),
            natural: D20::new(11).expect("a valid face"),
            modifier: -1,
            dc: 10,
            reason: "Climb".to_owned(),
            success: true,
        };
        assert_eq!(
            roll_text(&roll, Some("Ayla")),
            "Ayla rolls 11 - 1 = 10 against DC 10 (Climb): success"
        );
    }

    #[test]
    fn the_plugin_merges_narration_and_rolls_by_sequence() {
        let mut app = test_app();
        app.world_mut()
            .write_message(entry(3, LogEntryKind::Narration, "The barrel bursts."));
        app.world_mut().write_message(DiceRollShown {
            sequence: 2,
            actor: EntityId(3),
            natural: D20::new(15).expect("a valid face"),
            modifier: 0,
            dc: 12,
            reason: "Kick".to_owned(),
            success: true,
        });
        app.world_mut()
            .spawn((EntityId(3), EntityName("Goblin".to_owned())));
        app.update();
        assert_eq!(
            log_text(&mut app),
            "Goblin rolls 15 + 0 = 15 against DC 12 (Kick): success\nThe barrel bursts."
        );
    }

    #[test]
    fn speech_shows_a_bubble_above_the_speaker_that_expires() {
        let mut app = test_app();
        app.world_mut().spawn((EntityId(3), GridPos::new(1, 1)));
        app.world_mut()
            .write_message(entry(1, LogEntryKind::Speech, "Who goes there?"));
        app.update();
        assert_eq!(bubble_count(&mut app), 1);
        let mut bubbles = app.world_mut().query::<(&Text2d, &Transform)>();
        let (text, transform) = bubbles.single(app.world()).expect("1 bubble");
        assert_eq!(text.0, "Who goes there?");
        let tile_size = crate::settings::TILE_SIZE;
        assert!(
            transform.translation.y
                > square_center_at(GridPos::new(1, 1), tile_size, FloatingOrigin::default()).y
        );

        // Virtual time advances at most 250 milliseconds per update, so step in 200.
        for _ in 0..25 {
            app.update();
        }
        assert_eq!(bubble_count(&mut app), 0);
    }

    #[test]
    fn a_new_line_from_the_same_speaker_replaces_the_bubble() {
        let mut app = test_app();
        app.world_mut().spawn((EntityId(3), GridPos::new(1, 1)));
        app.world_mut()
            .write_message(entry(1, LogEntryKind::Speech, "Halt."));
        app.update();
        app.world_mut()
            .write_message(entry(2, LogEntryKind::Speech, "I said halt!"));
        app.update();
        assert_eq!(bubble_count(&mut app), 1);
    }

    #[test]
    fn narration_never_shows_a_bubble() {
        let mut app = test_app();
        app.world_mut().spawn((EntityId(3), GridPos::new(1, 1)));
        app.world_mut()
            .write_message(entry(1, LogEntryKind::Narration, "Wind howls."));
        app.update();
        assert_eq!(bubble_count(&mut app), 0);
    }
}
