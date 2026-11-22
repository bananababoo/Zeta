//! Frame pacing: the client updates when input arrives or a wait runs out, instead of
//! redrawing as fast as it can, so an idle client uses little CPU.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 5. Plugins that animate
//! (the d20 tumble) send `RequestRedraw` while they move, which wakes the client at once.

use bevy::prelude::*;
use bevy::winit::{UpdateMode, WinitSettings};

use crate::settings::ClientSettings;

/// The winit update modes for the waits in `settings`. While focused, any input wakes the
/// client; while unfocused, only input to the window does.
pub fn winit_settings(settings: &ClientSettings) -> WinitSettings {
    // `WinitSettings`, `UpdateMode::reactive`, and `UpdateMode::reactive_low_power`:
    // bevy_winit-0.20.1/src/winit_config.rs:6, :112, and :127.
    WinitSettings {
        focused_mode: UpdateMode::reactive(settings.focused_update_wait),
        unfocused_mode: UpdateMode::reactive_low_power(settings.unfocused_update_wait),
    }
}

/// Replaces Bevy's continuous game loop with reactive updates. Add it after
/// `DefaultPlugins`, so its `WinitSettings` replaces the default one.
#[derive(Clone, Copy, Debug, Default)]
pub struct FramePacingPlugin;

impl Plugin for FramePacingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ClientSettings>();
        let settings = winit_settings(app.world().resource::<ClientSettings>());
        app.insert_resource(settings);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn the_waits_come_from_the_client_settings() {
        let settings = ClientSettings {
            focused_update_wait: Duration::from_millis(50),
            unfocused_update_wait: Duration::from_millis(500),
            ..ClientSettings::default()
        };
        let winit = winit_settings(&settings);
        assert_eq!(
            winit.focused_mode,
            UpdateMode::reactive(Duration::from_millis(50))
        );
        assert_eq!(
            winit.unfocused_mode,
            UpdateMode::reactive_low_power(Duration::from_millis(500))
        );
    }

    #[test]
    fn the_plugin_replaces_the_continuous_default() {
        let mut app = App::new();
        app.insert_resource(WinitSettings::game())
            .add_plugins(FramePacingPlugin);
        let winit = app.world().resource::<WinitSettings>();
        assert_ne!(winit.focused_mode, UpdateMode::Continuous);
    }
}
