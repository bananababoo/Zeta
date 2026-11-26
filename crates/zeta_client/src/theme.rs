//! Shared client UI theme: Warm Wood & Linen.
//!
//! Every UI plugin reads colors, fonts, monochrome icons, and 9-sliced panel/button sprites
//! from [`UiTheme`] so all panels share 1 cohesive look (warm wood, cream linen, walnut ink,
//! muted earthy accents; no gold or metal; zero pixel-art HUD clash). Item icon mappings come
//! from `assets/ui/icon_map.json`, never hardcoded item lists in Rust.
//!
//! Headless unit tests using `MinimalPlugins` have no `AssetServer`; [`ThemePlugin`] falls back
//! to `Default` handles when no asset server is present.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy::sprite::{BorderRect, SliceScaleMode, TextureSlicer};
use bevy::ui::VisualBox;
use bevy::ui::widget::NodeImageMode;
use serde::Deserialize;

use crate::terrain::assets_root;

/// Warm cream text and icon tint on dark/wood surfaces (`#FBF5EA`).
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

/// Tracked UI font and panel sprite paths relative to `assets/`.
const HEADING_FONT_PATH: &str = "fonts/fraunces.ttf";
const NARRATION_FONT_PATH: &str = "fonts/patrick_hand.ttf";
const UI_FONT_PATH: &str = "fonts/nunito.ttf";
const PANEL_WOOD_PATH: &str = "ui/panel_wood.png";
const PANEL_PARCHMENT_PATH: &str = "ui/panel_parchment.png";
const BUTTON_WOOD_PATH: &str = "ui/button_wood.png";
const BUTTON_PARCHMENT_PATH: &str = "ui/button_parchment.png";
const ICON_MAP_PATH: &str = "ui/icon_map.json";

/// External JSON icon mapping (`assets/ui/icon_map.json`).
#[derive(Clone, Debug, Deserialize)]
struct IconMapConfig {
    defaults: DefaultIconPaths,
    #[serde(default)]
    items: HashMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
struct DefaultIconPaths {
    weapon: String,
    armor: String,
    tool: String,
    gear: String,
    d20: String,
}

fn load_icon_map_config() -> Result<IconMapConfig, String> {
    let full_path = assets_root().join(ICON_MAP_PATH);
    let raw = std::fs::read_to_string(&full_path)
        .map_err(|error| format!("could not read {}: {error}", full_path.display()))?;
    serde_json::from_str(&raw)
        .map_err(|error| format!("invalid icon map {}: {error}", full_path.display()))
}

/// Monochrome white-on-transparent RPG icons tinted at runtime by [`UiTheme`].
#[derive(Clone, Debug, Default)]
pub struct UiIcons {
    /// Fallback icon for weapon slots.
    pub weapon: Handle<Image>,
    /// Fallback icon for armor and shield slots.
    pub armor: Handle<Image>,
    /// Fallback icon for tool slots.
    pub tool: Handle<Image>,
    /// Fallback icon for general gear slots.
    pub gear: Handle<Image>,
    /// d20 die icon for the dice panel.
    pub d20: Handle<Image>,
    /// Specific SRD catalog id -> monochrome icon handle.
    pub by_catalog_id: HashMap<String, Handle<Image>>,
}

/// Loaded font, monochrome icon, and 9-slice sprite handles for the client UI.
#[derive(Resource, Clone, Debug, Default)]
pub struct UiTheme {
    /// Display/heading font (`Fraunces` — warm bold woodblock serif).
    pub heading_font: Handle<Font>,
    /// Narrative/journal font (`Patrick Hand` — warm hand-inked tabletop journal).
    pub narration_font: Handle<Font>,
    /// Compact HUD/control font (`Nunito` — warm rounded sans).
    pub ui_font: Handle<Font>,
    /// 9-sliced notched wood panel (`assets/ui/panel_wood.png`).
    pub panel_wood: Handle<Image>,
    /// 9-sliced notched parchment panel (`assets/ui/panel_parchment.png`).
    pub panel_parchment: Handle<Image>,
    /// 9-sliced wood button (`assets/ui/button_wood.png`).
    pub button_wood: Handle<Image>,
    /// 9-sliced parchment button (`assets/ui/button_parchment.png`).
    pub button_parchment: Handle<Image>,
    /// Preloaded monochrome RPG icons (`assets/ui/icons/`).
    pub icons: UiIcons,
}

impl UiTheme {
    /// Heading `TextFont` (`Fraunces` bold).
    /// `TextFont::with_font`: `bevy_text-0.20.1/src/text.rs:719`.
    /// `TextFont::with_font_weight`: `bevy_text-0.20.1/src/text.rs:743`.
    #[must_use]
    pub fn heading_font(&self, size: f32) -> TextFont {
        TextFont::from_font_size(size)
            .with_font(self.heading_font.clone())
            .with_font_weight(FontWeight::BOLD)
    }

    /// Narration/tooltip `TextFont` (`Patrick Hand`).
    #[must_use]
    pub fn narration_font(&self, size: f32) -> TextFont {
        TextFont::from_font_size(size).with_font(self.narration_font.clone())
    }

    /// Compact HUD/button `TextFont` (`Nunito` semibold).
    #[must_use]
    pub fn ui_font(&self, size: f32) -> TextFont {
        TextFont::from_font_size(size)
            .with_font(self.ui_font.clone())
            .with_font_weight(FontWeight::SEMIBOLD)
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

    /// Tinted monochrome icon node (`ImageNode::new`: `bevy_ui-0.20.1/src/widget/image.rs:74`).
    #[must_use]
    pub fn tinted_icon(&self, handle: Handle<Image>, tint: Color) -> ImageNode {
        let mut node = ImageNode::new(handle);
        node.color = tint;
        node
    }

    /// Resolves the monochrome icon for an inventory slot, falling back to its category icon.
    #[must_use]
    pub fn slot_icon(
        &self,
        catalog_id: Option<&str>,
        category: crate::inventory::SlotCategory,
    ) -> Handle<Image> {
        if let Some(id) = catalog_id
            && let Some(handle) = self.icons.by_catalog_id.get(id)
        {
            return handle.clone();
        }
        match category {
            crate::inventory::SlotCategory::Weapon => self.icons.weapon.clone(),
            crate::inventory::SlotCategory::Armor => self.icons.armor.clone(),
            crate::inventory::SlotCategory::Tool => self.icons.tool.clone(),
            crate::inventory::SlotCategory::Gear => self.icons.gear.clone(),
        }
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
    theme.heading_font = assets.load(HEADING_FONT_PATH);
    theme.narration_font = assets.load(NARRATION_FONT_PATH);
    theme.ui_font = assets.load(UI_FONT_PATH);
    theme.panel_wood = assets.load(PANEL_WOOD_PATH);
    theme.panel_parchment = assets.load(PANEL_PARCHMENT_PATH);
    theme.button_wood = assets.load(BUTTON_WOOD_PATH);
    theme.button_parchment = assets.load(BUTTON_PARCHMENT_PATH);

    match load_icon_map_config() {
        Ok(icon_map) => {
            theme.icons.weapon = assets.load(icon_map.defaults.weapon);
            theme.icons.armor = assets.load(icon_map.defaults.armor);
            theme.icons.tool = assets.load(icon_map.defaults.tool);
            theme.icons.gear = assets.load(icon_map.defaults.gear);
            theme.icons.d20 = assets.load(icon_map.defaults.d20);
            for (catalog_id, path) in icon_map.items {
                theme
                    .icons
                    .by_catalog_id
                    .insert(catalog_id, assets.load(path));
            }
        }
        Err(error) => warn!("UI icon map not loaded ({error}); icons will use defaults"),
    }
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

    #[test]
    fn every_theme_font_panel_and_icon_asset_exists_on_disk() {
        let root = assets_root();
        for relative in [
            HEADING_FONT_PATH,
            NARRATION_FONT_PATH,
            UI_FONT_PATH,
            PANEL_WOOD_PATH,
            PANEL_PARCHMENT_PATH,
            BUTTON_WOOD_PATH,
            BUTTON_PARCHMENT_PATH,
            ICON_MAP_PATH,
        ] {
            assert!(
                root.join(relative).is_file(),
                "missing theme asset: {relative}"
            );
        }

        let icon_map = load_icon_map_config().expect("valid assets/ui/icon_map.json");
        for relative in [
            &icon_map.defaults.weapon,
            &icon_map.defaults.armor,
            &icon_map.defaults.tool,
            &icon_map.defaults.gear,
            &icon_map.defaults.d20,
        ] {
            assert!(
                root.join(relative).is_file(),
                "missing default icon asset: {relative}"
            );
        }
        for (catalog_id, relative) in &icon_map.items {
            assert!(
                root.join(relative).is_file(),
                "missing icon asset for {catalog_id}: {relative}"
            );
        }
    }
}
