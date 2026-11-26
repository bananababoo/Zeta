//! Shared client UI theme: Warm Wood & Linen.
//!
//! Every UI plugin reads colors, fonts, and 9-sliced panel/button sprites from [`UiTheme`] so
//! all panels share 1 cohesive look (warm wood, cream linen, walnut ink, muted earthy accents;
//! no gold or metal).
//!
//! Headless unit tests using `MinimalPlugins` have no `AssetServer`; [`ThemePlugin`] falls back
//! to `Default` handles when no asset server is present.

use bevy::prelude::*;
use bevy::sprite::{BorderRect, SliceScaleMode, TextureSlicer};
use bevy::ui::VisualBox;
use bevy::ui::widget::NodeImageMode;

/// Warm cream text on dark/wood surfaces (`#FBF5EA`).
pub const TEXT_ON_WOOD: Color = Color::srgb(0.984, 0.961, 0.918);
/// Soft oatmeal secondary text on wood surfaces (`#DDD0BE`).
pub const TEXT_MUTED_ON_WOOD: Color = Color::srgb(0.867, 0.816, 0.745);
/// Deep walnut ink text on parchment/linen surfaces (`#2F2219`).
pub const TEXT_ON_PARCHMENT: Color = Color::srgb(0.184, 0.133, 0.098);
/// Warm honey-ochre highlight for status lines on wood (`#F5D5A8`).
pub const STATUS_TEXT_COLOR: Color = Color::srgb(0.961, 0.835, 0.659);
/// Warm sage green for successful checks (`#7CCF7C`).
pub const SUCCESS_COLOR: Color = Color::srgb(0.486, 0.812, 0.486);
/// Warm terracotta red for failed checks (`#E8715D`).
pub const FAILURE_COLOR: Color = Color::srgb(0.910, 0.443, 0.365);
/// Warm cream speech bubble text above map speakers (`#FBF3DE`).
pub const SPEECH_BUBBLE_COLOR: Color = Color::srgb(0.984, 0.953, 0.871);

/// Subtle warm tint applied to wood panels so the map peeks softly behind edges.
const WOOD_PANEL_TINT: Color = Color::srgba(0.95, 0.90, 0.85, 0.95);
/// Subtle warm tint applied to parchment panels (narration log, tooltips).
const PARCHMENT_PANEL_TINT: Color = Color::srgba(0.98, 0.95, 0.90, 0.96);
/// Tint for primary wood buttons.
const WOOD_BUTTON_TINT: Color = Color::srgba(0.96, 0.91, 0.86, 0.98);
/// Tint for secondary parchment buttons.
const PARCHMENT_BUTTON_TINT: Color = Color::srgba(0.98, 0.95, 0.90, 0.98);

/// 9-slice border inset in pixels for `panel_wood.png` and `panel_parchment.png` (100x100).
const PANEL_SLICE_INSET: f32 = 14.0;
/// 9-slice border inset in pixels for `button_wood.png` and `button_parchment.png` (190x49).
const BUTTON_SLICE_INSET: f32 = 10.0;

/// Corner radius for rounded earthy slots (inventory squares).
pub const SLOT_CORNER_RADIUS_PIXELS: f32 = 6.0;

/// Loaded font and 9-slice sprite handles for the client UI.
#[derive(Resource, Clone, Debug, Default)]
pub struct UiTheme {
    /// Display/heading font (`Cinzel`).
    pub heading_font: Handle<Font>,
    /// Narrative/journal font (`EB Garamond`).
    pub narration_font: Handle<Font>,
    /// Compact HUD/control font (`Inter`).
    pub ui_font: Handle<Font>,
    /// 9-sliced notched wood panel (`assets/ui/panel_wood.png`).
    pub panel_wood: Handle<Image>,
    /// 9-sliced notched parchment panel (`assets/ui/panel_parchment.png`).
    pub panel_parchment: Handle<Image>,
    /// 9-sliced wood button (`assets/ui/button_wood.png`).
    pub button_wood: Handle<Image>,
    /// 9-sliced parchment button (`assets/ui/button_parchment.png`).
    pub button_parchment: Handle<Image>,
}

impl UiTheme {
    /// Heading `TextFont` (`Cinzel`).
    /// `TextFont::with_font`: `bevy_text-0.20.1/src/text.rs:719`.
    #[must_use]
    pub fn heading_font(&self, size: f32) -> TextFont {
        TextFont::from_font_size(size).with_font(self.heading_font.clone())
    }

    /// Narration/tooltip `TextFont` (`EB Garamond`).
    #[must_use]
    pub fn narration_font(&self, size: f32) -> TextFont {
        TextFont::from_font_size(size).with_font(self.narration_font.clone())
    }

    /// Compact HUD/button `TextFont` (`Inter`).
    #[must_use]
    pub fn ui_font(&self, size: f32) -> TextFont {
        TextFont::from_font_size(size).with_font(self.ui_font.clone())
    }

    /// 9-sliced warm wood panel background.
    /// `ImageNode::with_mode`: `bevy_ui-0.20.1/src/widget/image.rs:185`.
    /// `TextureSlicer`: `bevy_sprite-0.20.1/src/texture_slice/slicer.rs:14`.
    #[must_use]
    pub fn wood_panel(&self) -> ImageNode {
        sliced_image(self.panel_wood.clone(), PANEL_SLICE_INSET, WOOD_PANEL_TINT)
    }

    /// 9-sliced warm parchment/linen panel background.
    #[must_use]
    pub fn parchment_panel(&self) -> ImageNode {
        sliced_image(
            self.panel_parchment.clone(),
            PANEL_SLICE_INSET,
            PARCHMENT_PANEL_TINT,
        )
    }

    /// 9-sliced warm wood button background.
    #[must_use]
    pub fn wood_button(&self) -> ImageNode {
        sliced_image(
            self.button_wood.clone(),
            BUTTON_SLICE_INSET,
            WOOD_BUTTON_TINT,
        )
    }

    /// 9-sliced warm parchment button background.
    #[must_use]
    pub fn parchment_button(&self) -> ImageNode {
        sliced_image(
            self.button_parchment.clone(),
            BUTTON_SLICE_INSET,
            PARCHMENT_BUTTON_TINT,
        )
    }
}

fn sliced_image(handle: Handle<Image>, inset: f32, color: Color) -> ImageNode {
    ImageNode {
        color,
        image: handle,
        image_mode: NodeImageMode::Sliced(TextureSlicer {
            border: BorderRect::all(inset),
            center_scale_mode: SliceScaleMode::Stretch,
            sides_scale_mode: SliceScaleMode::Stretch,
            max_corner_scale: 1.0,
        }),
        // Draw 9-sliced panel/button backgrounds across the full border box so `Node::padding`
        // keeps child text inside the drawn frame rim (`bevy_ui-0.20.1/src/ui_node.rs:1752`).
        visual_box: VisualBox::BorderBox,
        ..default()
    }
}

/// Initializes [`UiTheme`] in `PreStartup` so every plugin's `Startup` UI builder can use it.
#[derive(Clone, Copy, Debug, Default)]
pub struct ThemePlugin;

impl Plugin for ThemePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UiTheme>()
            .add_systems(PreStartup, load_theme_assets);
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn load_theme_assets(assets: Option<Res<AssetServer>>, mut theme: ResMut<UiTheme>) {
    let Some(assets) = assets else {
        return;
    };
    theme.heading_font = assets.load("fonts/cinzel.ttf");
    theme.narration_font = assets.load("fonts/eb_garamond.ttf");
    theme.ui_font = assets.load("fonts/inter.ttf");
    theme.panel_wood = assets.load("ui/panel_wood.png");
    theme.panel_parchment = assets.load("ui/panel_parchment.png");
    theme.button_wood = assets.load("ui/button_wood.png");
    theme.button_parchment = assets.load("ui/button_parchment.png");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_plugin_initializes_cleanly_without_an_asset_server() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, ThemePlugin));
        app.update();
        let theme = app.world().resource::<UiTheme>();
        assert_eq!(theme.wood_panel().color, WOOD_PANEL_TINT);
        assert_eq!(theme.parchment_panel().color, PARCHMENT_PANEL_TINT);
        assert_eq!(theme.wood_button().color, WOOD_BUTTON_TINT);
    }
}
