//! Keyboard and mouse focus shared by the client plugins: whether a text box takes the
//! keyboard, and which screen panels take mouse clicks.
//!
//! Placeholder: `docs/architecture/technical_architecture.md` Section 5 leaves the client
//! layout undecided, so focus is a flag and a marker until a layout exists.

use bevy::prelude::*;
use bevy::ui::UiGlobalTransform;

/// True while a text box takes keyboard input, so movement and turn keys do not fire while
/// typing.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextEntryActive(pub bool);

/// Systems that update `TextEntryActive`. Keyboard shortcuts run after this set, so they
/// see the focus of the current frame.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextEntryUpdate;

/// True while a UI click on this frame (such as closing an open menu) has already been
/// handled, so map click handlers ignore the same click. Reset in `First` each frame.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UiClickConsumed(pub bool);

/// Systems that handle UI clicks and may set [`UiClickConsumed`]. Map click handlers run
/// after this set.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UiClickUpdate;

/// Clears [`UiClickConsumed`] at the start of each frame.
#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
pub fn clear_ui_click_consumed(mut consumed: ResMut<UiClickConsumed>) {
    consumed.0 = false;
}

/// Marks a screen panel that takes mouse clicks, so clicks on it never reach the map.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UiPanel;

/// Whether the mouse cursor of `window` is over the laid-out UI node.
pub fn cursor_is_over(window: &Window, node: &ComputedNode, transform: UiGlobalTransform) -> bool {
    window
        .physical_cursor_position()
        .is_some_and(|cursor| node.contains_point(transform, cursor))
}

/// Whether the mouse cursor of `window` is over any of the laid-out UI nodes. Hidden nodes
/// have a size of 0, so they never contain the cursor.
pub fn cursor_is_over_any<'a>(
    window: &Window,
    nodes: impl IntoIterator<Item = (&'a ComputedNode, &'a UiGlobalTransform)>,
) -> bool {
    nodes
        .into_iter()
        .any(|(node, transform)| cursor_is_over(window, node, *transform))
}

/// A key's name for players: `KeyI` is `I`, `Digit1` is `1`, `ArrowUp` is `Arrow Up`.
/// `KeyCode` variants (`bevy_input` keyboard.rs:275) are named after the physical key, so the
/// name comes from their `Debug` text.
pub fn key_label(key: KeyCode) -> String {
    let name = format!("{key:?}");
    if let Some(rest) = name
        .strip_prefix("Key")
        .or_else(|| name.strip_prefix("Digit"))
        .filter(|rest| !rest.is_empty())
    {
        return rest.to_owned();
    }
    let mut spaced = String::with_capacity(name.len() + 4);
    for (index, character) in name.chars().enumerate() {
        if index > 0 && character.is_uppercase() {
            spaced.push(' ');
        }
        spaced.push(character);
    }
    spaced
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_labels_drop_the_engine_prefixes() {
        assert_eq!(key_label(KeyCode::KeyI), "I");
        assert_eq!(key_label(KeyCode::Digit1), "1");
        assert_eq!(key_label(KeyCode::ArrowUp), "Arrow Up");
        assert_eq!(key_label(KeyCode::Enter), "Enter");
    }
}
