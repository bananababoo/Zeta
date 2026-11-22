//! Freeform Action Prompt: a text box whose text is sent to the server, and the status of the
//! player's latest prompt.
//!
//! Rules: `docs/prototypes/realism_prototype.md` and `docs/architecture/technical_architecture.md`
//! Section 5. The text box is built from raw keyboard messages, so it depends on no engine
//! text-input widget.

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;
use zeta_core::protocol::{FreeformActionPrompt, PromptState, PromptStatusUpdate, StackId};

use crate::focus::{TextEntryActive, TextEntryUpdate, UiPanel};
use crate::settings::ClientSettings;

/// Shown in the text box while it is not focused.
const PROMPT_HINT: &str = "Press Enter to describe an action";
/// Font size of the text box.
const PROMPT_FONT_SIZE: f32 = 18.0;
/// Font size of the status line.
const STATUS_FONT_SIZE: f32 = 14.0;

/// An item attached to the prompt by `Utilize`: the stack the server attaches the catalog
/// checks for, and the line shown above the typed text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptAttachment {
    /// The carried stack being used.
    pub stack: StackId,
    /// Shown in the text box, such as `Thieves' Tools: Pick a lock (DC 15)`.
    pub label: String,
}

/// Client-only request to open the text box with an item attached and starting text typed.
/// Other plugins (the inventory) send it; the prompt plugin owns the text box.
#[derive(Message, Clone, Debug, PartialEq, Eq)]
pub struct OpenPromptWithItem {
    /// The attached item.
    pub attachment: PromptAttachment,
    /// Text typed in the box when it opens.
    pub text: String,
}

/// The text box: whether it has focus, what has been typed, and any attached item.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptEditor {
    focused: bool,
    text: String,
    attachment: Option<PromptAttachment>,
}

/// What 1 key press did to the text box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptKeyOutcome {
    /// The key was not for the text box.
    Ignored,
    /// Focus or text changed.
    Edited,
    /// The player submitted this prompt.
    Submitted(FreeformActionPrompt),
}

impl PromptEditor {
    /// Whether the text box takes keyboard input.
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// The typed text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The attached item, if any.
    pub fn attachment(&self) -> Option<&PromptAttachment> {
        self.attachment.as_ref()
    }

    /// Focuses the box with `attachment` attached and `text` typed, cut to `max_characters`.
    /// Replaces any text and attachment already there.
    pub fn open_with(&mut self, attachment: PromptAttachment, text: &str, max_characters: usize) {
        self.focused = true;
        self.text = text.chars().take(max_characters).collect();
        self.attachment = Some(attachment);
    }

    /// Applies 1 key press. Enter focuses an unfocused box; in a focused box Enter submits the
    /// trimmed text with the attached item, Escape cancels, Backspace deletes, and characters
    /// are typed up to `max_characters`. Submitting blank text only closes the box. Closing
    /// the box always drops the attachment.
    pub fn press(&mut self, key: &Key, max_characters: usize) -> PromptKeyOutcome {
        if !self.focused {
            if *key == Key::Enter {
                self.focused = true;
                return PromptKeyOutcome::Edited;
            }
            return PromptKeyOutcome::Ignored;
        }
        match key {
            Key::Enter => {
                self.focused = false;
                let submitted = self.text.trim().to_owned();
                self.text.clear();
                let attachment = self.attachment.take();
                if submitted.is_empty() {
                    PromptKeyOutcome::Edited
                } else {
                    PromptKeyOutcome::Submitted(FreeformActionPrompt {
                        text: submitted,
                        utilize: attachment.map(|attachment| attachment.stack),
                    })
                }
            }
            Key::Escape => {
                self.focused = false;
                self.text.clear();
                self.attachment = None;
                PromptKeyOutcome::Edited
            }
            Key::Backspace => {
                self.text.pop();
                PromptKeyOutcome::Edited
            }
            Key::Space => self.type_text(" ", max_characters),
            Key::Character(characters) => self.type_text(characters, max_characters),
            _ => PromptKeyOutcome::Ignored,
        }
    }

    fn type_text(&mut self, typed: &str, max_characters: usize) -> PromptKeyOutcome {
        let room = max_characters.saturating_sub(self.text.chars().count());
        self.text.extend(
            typed
                .chars()
                .filter(|character| !character.is_control())
                .take(room),
        );
        PromptKeyOutcome::Edited
    }
}

/// The server's latest status for this player's Freeform Action Prompt.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct LatestPromptStatus(pub Option<PromptStatusUpdate>);

/// Marks the text of the text box.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PromptInputText;

/// Marks the status line above the text box.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PromptStatusText;

/// The Freeform Action Prompt text box and status line.
#[derive(Clone, Copy, Debug, Default)]
pub struct FreeformActionPromptPlugin;

impl Plugin for FreeformActionPromptPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ClientSettings>()
            .init_resource::<TextEntryActive>()
            .init_resource::<PromptEditor>()
            .init_resource::<LatestPromptStatus>()
            .add_message::<OpenPromptWithItem>()
            .add_systems(Startup, spawn_prompt_panel)
            .add_systems(
                Update,
                (
                    (edit_prompt.in_set(TextEntryUpdate), record_prompt_status),
                    (show_prompt_input, show_prompt_status),
                )
                    .chain(),
            );
    }
}

/// The text box line: the hint when unfocused; when focused, the attached item on its own
/// line, then the typed text and a cursor.
pub fn prompt_input_text(editor: &PromptEditor) -> String {
    if !editor.focused {
        return PROMPT_HINT.to_owned();
    }
    match &editor.attachment {
        Some(attachment) => format!("[Utilize {}]\n> {}_", attachment.label, editor.text),
        None => format!("> {}_", editor.text),
    }
}

/// The status line for the latest Freeform Action Prompt status.
pub fn prompt_status_text(status: Option<&PromptStatusUpdate>) -> String {
    let Some(status) = status else {
        return String::new();
    };
    match (status.state, status.reason.as_deref()) {
        (PromptState::Resolving, _) => "The AI DM is resolving your action.".to_owned(),
        (PromptState::Queued, _) => "Your action waits for another action to resolve.".to_owned(),
        (PromptState::Applied, _) => "Your action was resolved.".to_owned(),
        (PromptState::Rejected, Some(reason)) => format!("Your action was rejected: {reason}"),
        (PromptState::Rejected, None) => "Your action was rejected.".to_owned(),
    }
}

fn spawn_prompt_panel(mut commands: Commands) {
    commands
        .spawn((
            UiPanel,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                bottom: Val::Px(0.0),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(8.0)),
                row_gap: Val::Px(4.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.75)),
        ))
        .with_children(|panel| {
            panel.spawn((
                PromptStatusText,
                Text::new(""),
                TextFont::from_font_size(STATUS_FONT_SIZE),
                TextColor(Color::srgb(0.8, 0.8, 0.6)),
            ));
            panel.spawn((
                PromptInputText,
                Text::new(PROMPT_HINT),
                TextFont::from_font_size(PROMPT_FONT_SIZE),
            ));
        });
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn edit_prompt(
    mut keyboard: MessageReader<KeyboardInput>,
    mut openings: MessageReader<OpenPromptWithItem>,
    settings: Res<ClientSettings>,
    mut editor: ResMut<PromptEditor>,
    mut text_entry: ResMut<TextEntryActive>,
    mut prompts: MessageWriter<FreeformActionPrompt>,
) {
    if let Some(opening) = openings.read().last() {
        editor.open_with(
            opening.attachment.clone(),
            &opening.text,
            settings.prompt_max_characters,
        );
    }
    for input in keyboard.read() {
        if input.state != ButtonState::Pressed {
            continue;
        }
        match editor.press(&input.logical_key, settings.prompt_max_characters) {
            PromptKeyOutcome::Ignored | PromptKeyOutcome::Edited => {}
            PromptKeyOutcome::Submitted(prompt) => {
                prompts.write(prompt);
            }
        }
    }
    let focused = editor.is_focused();
    if text_entry.0 != focused {
        text_entry.0 = focused;
    }
}

fn record_prompt_status(
    mut updates: MessageReader<PromptStatusUpdate>,
    mut status: ResMut<LatestPromptStatus>,
) {
    if let Some(update) = updates.read().last() {
        status.0 = Some(update.clone());
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn show_prompt_input(
    editor: Res<PromptEditor>,
    mut texts: Query<&mut Text, With<PromptInputText>>,
) {
    if !editor.is_changed() {
        return;
    }
    for mut text in &mut texts {
        text.0 = prompt_input_text(&editor);
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn show_prompt_status(
    status: Res<LatestPromptStatus>,
    mut texts: Query<&mut Text, With<PromptStatusText>>,
) {
    if !status.is_changed() {
        return;
    }
    for mut text in &mut texts {
        text.0 = prompt_status_text(status.0.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use bevy::input::keyboard::NativeKey;

    use super::*;

    fn character(text: &str) -> Key {
        Key::Character(text.into())
    }

    fn rope_attachment() -> PromptAttachment {
        PromptAttachment {
            stack: StackId(4),
            label: "Rope: Tie a knot (DC 10)".to_owned(),
        }
    }

    #[test]
    fn an_attached_item_is_shown_and_sent_with_the_prompt() {
        let mut editor = PromptEditor::default();
        editor.open_with(rope_attachment(), "I use my Rope to ", 100);
        assert!(editor.is_focused());
        assert_eq!(
            prompt_input_text(&editor),
            "[Utilize Rope: Tie a knot (DC 10)]\n> I use my Rope to _"
        );
        editor.press(&character("x"), 100);
        assert_eq!(
            editor.press(&Key::Enter, 100),
            PromptKeyOutcome::Submitted(FreeformActionPrompt {
                text: "I use my Rope to x".to_owned(),
                utilize: Some(StackId(4)),
            })
        );
        assert_eq!(editor.attachment(), None);
    }

    #[test]
    fn escape_drops_the_attached_item() {
        let mut editor = PromptEditor::default();
        editor.open_with(rope_attachment(), "abc", 2);
        assert_eq!(editor.text(), "ab");
        editor.press(&Key::Escape, 100);
        assert_eq!(editor.attachment(), None);
        editor.press(&Key::Enter, 100);
        editor.press(&character("go"), 100);
        assert_eq!(
            editor.press(&Key::Enter, 100),
            PromptKeyOutcome::Submitted(FreeformActionPrompt {
                text: "go".to_owned(),
                utilize: None,
            })
        );
    }

    #[test]
    fn the_plugin_opens_the_box_for_an_item_and_takes_the_keyboard() {
        let mut app = test_app();
        app.world_mut().write_message(OpenPromptWithItem {
            attachment: rope_attachment(),
            text: "I use my Rope".to_owned(),
        });
        app.update();
        assert!(app.world().resource::<TextEntryActive>().0);
        assert_eq!(
            app.world().resource::<PromptEditor>().attachment(),
            Some(&rope_attachment())
        );
    }

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<KeyboardInput>()
            .add_message::<FreeformActionPrompt>()
            .add_message::<PromptStatusUpdate>()
            .add_plugins(FreeformActionPromptPlugin);
        app
    }

    fn key_message(key: Key) -> KeyboardInput {
        KeyboardInput {
            key_code: KeyCode::Unidentified(bevy::input::keyboard::NativeKeyCode::Unidentified),
            logical_key: key,
            state: ButtonState::Pressed,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        }
    }

    fn type_keys(app: &mut App, keys: Vec<Key>) {
        for key in keys {
            app.world_mut().write_message(key_message(key));
        }
        app.update();
    }

    fn sent(app: &App) -> Vec<FreeformActionPrompt> {
        app.world()
            .resource::<Messages<FreeformActionPrompt>>()
            .iter_current_update_messages()
            .cloned()
            .collect()
    }

    fn single_text<M: Component>(app: &mut App) -> String {
        let mut texts = app.world_mut().query_filtered::<&Text, With<M>>();
        texts.single(app.world()).expect("1 text").0.clone()
    }

    #[test]
    fn keys_are_ignored_until_enter_focuses_the_box() {
        let mut editor = PromptEditor::default();
        assert_eq!(editor.press(&character("w"), 10), PromptKeyOutcome::Ignored);
        assert_eq!(editor.press(&Key::Enter, 10), PromptKeyOutcome::Edited);
        assert!(editor.is_focused());
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn typing_then_enter_submits_the_trimmed_text_and_closes_the_box() {
        let mut editor = PromptEditor::default();
        editor.press(&Key::Enter, 100);
        for key in [
            character("h"),
            character("i"),
            Key::Space,
            character("!"),
            Key::Space,
        ] {
            editor.press(&key, 100);
        }
        assert_eq!(editor.text(), "hi ! ");
        assert_eq!(
            editor.press(&Key::Enter, 100),
            PromptKeyOutcome::Submitted(FreeformActionPrompt {
                text: "hi !".to_owned(),
                utilize: None,
            })
        );
        assert!(!editor.is_focused());
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn escape_cancels_and_clears_the_text() {
        let mut editor = PromptEditor::default();
        editor.press(&Key::Enter, 100);
        editor.press(&character("x"), 100);
        assert_eq!(editor.press(&Key::Escape, 100), PromptKeyOutcome::Edited);
        assert!(!editor.is_focused());
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn backspace_deletes_the_last_character() {
        let mut editor = PromptEditor::default();
        editor.press(&Key::Enter, 100);
        editor.press(&character("ab"), 100);
        editor.press(&Key::Backspace, 100);
        assert_eq!(editor.text(), "a");
    }

    #[test]
    fn blank_text_is_never_submitted() {
        let mut editor = PromptEditor::default();
        editor.press(&Key::Enter, 100);
        editor.press(&Key::Space, 100);
        assert_eq!(editor.press(&Key::Enter, 100), PromptKeyOutcome::Edited);
    }

    #[test]
    fn typing_stops_at_the_character_limit_and_drops_control_characters() {
        let mut editor = PromptEditor::default();
        editor.press(&Key::Enter, 3);
        editor.press(&character("a\u{7}b"), 3);
        editor.press(&character("cd"), 3);
        assert_eq!(editor.text(), "abc");
    }

    #[test]
    fn unknown_keys_are_ignored_while_focused() {
        let mut editor = PromptEditor::default();
        editor.press(&Key::Enter, 100);
        assert_eq!(
            editor.press(&Key::Unidentified(NativeKey::Unidentified), 100),
            PromptKeyOutcome::Ignored
        );
    }

    #[test]
    fn the_plugin_sends_the_prompt_and_tracks_text_entry_focus() {
        let mut app = test_app();
        type_keys(&mut app, vec![Key::Enter, character("l"), character("o")]);
        assert!(app.world().resource::<TextEntryActive>().0);
        assert_eq!(single_text::<PromptInputText>(&mut app), "> lo_");

        type_keys(&mut app, vec![character("o"), character("k"), Key::Enter]);
        assert_eq!(
            sent(&app),
            vec![FreeformActionPrompt {
                text: "look".to_owned(),
                utilize: None,
            }]
        );
        assert!(!app.world().resource::<TextEntryActive>().0);
        assert_eq!(single_text::<PromptInputText>(&mut app), PROMPT_HINT);
    }

    #[test]
    fn released_keys_are_not_typed() {
        let mut app = test_app();
        let mut release = key_message(Key::Enter);
        release.state = ButtonState::Released;
        app.world_mut().write_message(release);
        app.update();
        assert!(!app.world().resource::<PromptEditor>().is_focused());
    }

    #[test]
    fn the_status_line_shows_the_latest_prompt_status() {
        let mut app = test_app();
        app.update();
        assert_eq!(single_text::<PromptStatusText>(&mut app), "");

        app.world_mut().write_message(PromptStatusUpdate {
            state: PromptState::Rejected,
            reason: Some("Please rephrase.".to_owned()),
        });
        app.update();
        assert_eq!(
            single_text::<PromptStatusText>(&mut app),
            "Your action was rejected: Please rephrase."
        );
    }

    #[test]
    fn every_prompt_state_has_a_status_line() {
        for state in [
            PromptState::Resolving,
            PromptState::Queued,
            PromptState::Applied,
            PromptState::Rejected,
        ] {
            let update = PromptStatusUpdate {
                state,
                reason: None,
            };
            assert_ne!(prompt_status_text(Some(&update)), "");
        }
    }
}
