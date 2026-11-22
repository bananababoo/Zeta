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
