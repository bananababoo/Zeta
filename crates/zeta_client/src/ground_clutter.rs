//! Client-only multi-tier ground clutter: decals, understory plants, mid-scale props, and
//! tree canopies scattered deterministically over streamed planet chunks at gameplay zoom.
//!
//! Rules: `docs/architecture/world_geography.md` Section 6. Clutter is purely visual: the server
//! never sees it, it never blocks movement, and every placement is a pure function of canonical
//! planetary coordinates (`tile_hash` and 3D unit-sphere noise), so every client draws the same
//! battlemap ecology with zero network traffic.
//!
//! Architecture & invariants:
//! - `assets/terrain/clutter.json` maps a palette id (`assets/terrain/palette.json`) either to a
//!   legacy single-layer sprite list or to `layers`, an ordered list of [`ClutterLayerRule`]s
//!   across 4 Z-ordered tiers ([`ClutterTier::Decal`], [`ClutterTier::Understory`],
//!   [`ClutterTier::Prop`], [`ClutterTier::Canopy`]). Rust never names a sprite texture.
//! - Every clutter tier renders strictly above the streamed splat chunk (`z = -1.0` in
//!   `planet_stream.rs`, `0.0` in chunk-local space) and strictly below authored low-depth props
//!   (`0.5`) and gameplay entities, dropped items, creatures, player tokens (`1.0` in `map.rs`),
//!   and floating labels (`2.0`). Player tokens and items never disappear beneath tree canopies.
//! - Layers with `cell_step_tiles > 1` evaluate jittered candidate centers in canonical face-tile
//!   cells (`tile.x / step`, `tile.y / step`) and spawn each sprite only in the chunk that owns
//!   its center tile. Because Bevy 2D tests each child [`Sprite`]'s own [`Aabb`] against the
//!   camera frustum (`bevy_sprite-0.20.1/src/lib.rs:166-190`,
//!   `bevy_camera-0.20.1/src/visibility/mod.rs:650-658,804,823-836`), multi-tile trees and boulders
//!   near a chunk edge overhang into neighboring chunks seamlessly with zero duplicates and zero
//!   premature frustum popping.
//! - Multi-tile footprints check clearance against `region_painted` tiles (roads, bridges, town
//!   floors) and `clutter: false` covers (water, mud, ice), including across chunk borders via
//!   [`PlanetWorld`] and [`ChunkRegionPaint`].
//! - Loaded clutter PNGs pack at startup into 1 runtime [`TextureAtlasLayout`] + [`Image`] via
//!   [`TextureAtlasBuilder`] (falling back to standalone images if packing fails) so dense chunks
//!   with hundreds of casters and directional drop shadows batch into 1-2 GPU draw calls.

use std::collections::BTreeMap;
use std::f32::consts::TAU;
use std::f64::consts::FRAC_PI_2;

use bevy::asset::RenderAssetUsages;
use bevy::image::{
    CompressedImageFormats, ImageFormat, ImageSampler, ImageType, TextureAtlas,
    TextureAtlasBuilder, TextureAtlasLayout, TextureAtlasPlugin,
};
use bevy::prelude::*;
use serde::Deserialize;
use zeta_core::world::{
    CHUNK_SIZE, ClimateSample, FACE_TILES, GroundCoverInput, PlanetClimateMaps, PlanetHeightmaps,
    PlanetTile, SHORE_DISTANCE_MAX_TILES, ground_cover, hash_unit, sample_climate, sample_tile,
    step_tile, tile_hash, tile_to_unit_vec,
};

use crate::chunk_splat::{CHUNK_TILE_COUNT, ChunkGroundCover, ChunkRegionPaint, RegionPaint};
use crate::planet_region::polygon_alpha;
use crate::planet_stream::{PlanetFocusSync, PlanetStreamSyncMode, PlanetWorld};
use crate::terrain::{count_to_f32, layer_coverage, smoothstep};
use crate::terrain_palette::TerrainPalette;

/// Path of the clutter table, relative to `assets/`.
pub const CLUTTER_CONFIG_PATH: &str = "terrain/clutter.json";

/// Largest allowed drawn size of a clutter sprite's longest side, in 5-foot tiles.
pub const MAX_SPRITE_SIZE_TILES: f32 = 8.0;

/// Largest allowed canonical cell step for spaced multi-tile clutter layers, in tiles.
pub const MAX_CELL_STEP_TILES: u32 = 16;

/// Largest allowed per-instance warm/cool and brightness tint jitter (`[0.0, 0.3]`).
pub const MAX_TINT_VARIATION: f32 = 0.3;

/// Largest allowed opacity for a companion directional drop shadow (`[0.0, 0.85]`).
pub const MAX_SHADOW_ALPHA: f32 = 0.85;

/// Largest allowed companion drop shadow scale multiplier.
pub const MAX_SHADOW_SCALE: f32 = 2.0;

/// Default coherent grove/clearing feature diameter in 5-foot tiles when `cluster_scale_tiles` is omitted.
pub const DEFAULT_CLUSTER_SCALE_TILES: f32 = 18.0;

/// Default directional drop shadow offset `[east_tiles, south_tiles]` (SE shadow from a NW sun).
pub const DEFAULT_SHADOW_OFFSET_TILES: [f32; 2] = [0.18, 0.22];

/// Default companion drop shadow scale relative to the caster's `size_tiles`.
pub const DEFAULT_SHADOW_SCALE: f32 = 1.04;

/// Height of [`GroundClutterLayer`] above its parent splat chunk (`z = -1.0` in world space).
pub const CLUTTER_LAYER_Z: f32 = 0.08;

/// Most chunks whose clutter is rebuilt in 1 frame during asynchronous streaming.
pub const MAX_CLUTTER_CHUNKS_PER_FRAME: usize = 6;

/// Cool deep forest-green-black RGB tint of directional clutter drop shadows.
const SHADOW_TINT_RGB: [f32; 3] = [0.02, 0.04, 0.03];

/// Initial pixel side of the packed clutter texture atlas before doubling on demand.
const ATLAS_INITIAL_SIZE: u32 = 512;

/// Maximum pixel side of the packed clutter texture atlas (`wgpu` default 2D limit is `8192`:
/// `wgpu-types-30.0.2/src/limits.rs:424`).
const ATLAS_MAX_SIZE: u32 = 8_192;

/// Transparent pixel gutter between packed atlas rects so linear sampling never bleeds neighbor edges.
const ATLAS_PADDING_PIXELS: u32 = 2;

/// Tile period over which global Y-sorting wraps (`64` chunks = `4,096` tiles), keeping Y-order
/// seamless across `64x64` chunk borders while leaving ample `f32` mantissa precision per tile.
const Y_SORT_PERIOD_TILES: u32 = 4_096;

/// Sub-tile Y margin added at both ends of [`Y_SORT_PERIOD_TILES`] before normalizing to `[0, 1]`.
const Y_SORT_MARGIN_TILES: f32 = 8.0;

/// Base Z offset of a tier's shadow sub-band above [`ClutterTier::base_z`].
const TIER_SHADOW_BASE_Z: f32 = 0.005;

/// Y-sort span of a tier's shadow sub-band.
const TIER_SHADOW_Y_SPAN_Z: f32 = 0.035;

/// Deterministic hash tie-breaker span inside a tier's shadow sub-band.
const TIER_SHADOW_HASH_SPAN_Z: f32 = 0.000_002;

/// Base Z offset of a tier's caster sub-band above [`ClutterTier::base_z`] (strictly above shadows).
const TIER_CASTER_BASE_Z: f32 = 0.050;

/// Y-sort span of a tier's caster sub-band.
const TIER_CASTER_Y_SPAN_Z: f32 = 0.095;

/// Deterministic hash tie-breaker span inside a tier's caster sub-band (`< 0.18` tiles of Y-sort).
const TIER_CASTER_HASH_SPAN_Z: f32 = 0.000_004;

/// Fraction of a multi-tile `cell_step_tiles` cell kept clear at each border so jittered centers
/// in adjacent cells maintain natural minimum spacing.
const CELL_BORDER_MARGIN_FRACTION: f32 = 0.15;

/// Maximum sub-tile center offset (in tiles) when `cell_step_tiles == 1` and `size_tiles > 1.0`.
const OVERSIZED_SINGLE_TILE_SLACK: f32 = 0.35;

/// Painted region coverage threshold matching `chunk_splat.rs` `REGION_PAINTED_MIN`.
const REGION_PAINTED_THRESHOLD: f32 = 0.5;

/// Tiles per radian of arc at a cube-face center (`FACE_TILES / (pi / 2)`).
const TILES_PER_RADIAN: f64 = FACE_TILES as f64 / FRAC_PI_2;

/// Base seed of the 3D unit-sphere grove/clearing cluster noise.
const CLUSTER_NOISE_SEED: u64 = 0x636c_7573_7465_7201;

// Independent salts for deterministic per-tile and per-cell decisions.
const SALT_SPAWN: u64 = 0x636c_7574_7465_7201;
const SALT_SPRITE: u64 = 0x636c_7574_7465_7202;
const SALT_SIZE: u64 = 0x636c_7574_7465_7203;
const SALT_OFFSET_X: u64 = 0x636c_7574_7465_7204;
const SALT_OFFSET_Y: u64 = 0x636c_7574_7465_7205;
const SALT_FLIP_X: u64 = 0x636c_7574_7465_7206;
const SALT_FLIP_Y: u64 = 0x636c_7574_7465_7207;
const SALT_ROTATION: u64 = 0x636c_7574_7465_7208;
const SALT_TINT_BRIGHTNESS: u64 = 0x636c_7574_7465_7209;
const SALT_TINT_WARMTH: u64 = 0x636c_7574_7465_720a;
const SALT_Z_TIEBREAK: u64 = 0x636c_7574_7465_720b;
const SALT_CELL_JITTER_X: u64 = 0x636c_7574_7465_720c;
const SALT_CELL_JITTER_Y: u64 = 0x636c_7574_7465_720d;
const SALT_LAYER_MIX: u64 = 0x636c_7574_7465_720e;

const fn default_cell_step() -> u32 {
    1
}

const fn default_cluster_scale() -> f32 {
    DEFAULT_CLUSTER_SCALE_TILES
}

const fn default_true() -> bool {
    true
}

const fn default_shadow_offset() -> [f32; 2] {
    DEFAULT_SHADOW_OFFSET_TILES
}

const fn default_shadow_scale() -> f32 {
    DEFAULT_SHADOW_SCALE
}

/// Ecological vertical draw tier of a ground clutter layer.
///
/// Every tier occupies a disjoint Z-band strictly above the terrain chunk (`0.0` chunk-local,
/// `-1.0` world) and strictly below gameplay entities (`1.0` world in `map.rs`).
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ClutterTier {
    /// Flat ground-hugging details: moss patches, leaf litter, clover, tiny pebbles, wildflowers.
    #[default]
    Decal,
    /// Low foliage above decals: fern fronds, bracken, bushes, reeds, scrub shrubs.
    Understory,
    /// Solid mid-scale natural features: fallen mossy logs, tree stumps, layered boulders.
    Prop,
    /// Overhead tree crowns and spreading root-trunks, still drawn beneath tactical tokens.
    Canopy,
}

impl ClutterTier {
    /// All 4 clutter tiers in bottom-to-top draw order.
    pub const ALL: [Self; 4] = [Self::Decal, Self::Understory, Self::Prop, Self::Canopy];

    /// Disjoint base Z offset of this tier inside [`GroundClutterLayer`].
    #[must_use]
    pub const fn base_z(self) -> f32 {
        match self {
            Self::Decal => 0.00,
            Self::Understory => 0.20,
            Self::Prop => 0.40,
            Self::Canopy => 0.60,
        }
    }

    const fn salt_bits(self) -> u64 {
        match self {
            Self::Decal => 0,
            Self::Understory => 1,
            Self::Prop => 2,
            Self::Canopy => 3,
        }
    }
}

/// External clutter table loaded from [`CLUTTER_CONFIG_PATH`].
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ClutterConfig {
    /// Palette id (`assets/terrain/palette.json` `id`) to the clutter rule drawn on that cover.
    pub palette: BTreeMap<String, ClutterRule>,
}

/// Clutter rule for 1 terrain palette entry.
///
/// Supports both the single-layer legacy format (`density`, `follows_foliage`, `foliage_floor`,
/// `sprites`) and multi-tier battlemap ecology via `layers`. When `layers` is empty, the
/// top-level fields form 1 [`ClutterTier::Decal`] layer; when `sprites` is empty, `layers`
/// supplies every layer; if both are non-empty, the top-level `sprites` run first as a default
/// decal layer followed by `layers`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ClutterRule {
    /// Legacy single-layer spawn probability in `[0, 1]`.
    #[serde(default)]
    pub density: f32,
    /// Whether the legacy single-layer probability scales with climate foliage.
    #[serde(default)]
    pub follows_foliage: bool,
    /// Bare-ground share in `[0, 1]` retained when `follows_foliage` is `true` at foliage `0`.
    #[serde(default)]
    pub foliage_floor: f32,
    /// Weighted sprite choices for the legacy single-layer format.
    #[serde(default)]
    pub sprites: Vec<ClutterSprite>,
    /// Multi-tier ecological clutter layers evaluated independently on this palette cover.
    #[serde(default)]
    pub layers: Vec<ClutterLayerRule>,
}

/// 1 ecological clutter layer (decal, understory, prop, or canopy) within a [`ClutterRule`].
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "external JSON layer schema exposes independent boolean switches"
)]
pub struct ClutterLayerRule {
    /// Vertical Z-order tier (`decal`, `understory`, `prop`, `canopy`).
    #[serde(default)]
    pub tier: ClutterTier,
    /// Base spawn probability in `[0, 1]` per candidate cell (`1x1` tile or `cell_step_tiles^2` cell).
    pub density: f32,
    /// Whether `density` scales linearly from `foliage_floor` to `1.0` with `ClimateSample::foliage`.
    #[serde(default)]
    pub follows_foliage: bool,
    /// Share of `density` in `[0, 1]` kept when `follows_foliage` is `true` and foliage is `0`.
    #[serde(default)]
    pub foliage_floor: f32,
    /// Canonical face-tile cell size in `1..=16` (`1` = per-tile, `4` = 1 candidate per `4x4` tiles).
    #[serde(default = "default_cell_step")]
    pub cell_step_tiles: u32,
    /// Optional explicit clearance radius in tiles kept off roads, regions, and `clutter: false` covers.
    #[serde(default)]
    pub clearance_tiles: Option<f32>,
    /// Minimum mean annual air temperature in degrees Celsius (`ClimateSample::temperature`).
    #[serde(default)]
    pub min_temperature: Option<f32>,
    /// Maximum mean annual air temperature in degrees Celsius (`ClimateSample::temperature`).
    #[serde(default)]
    pub max_temperature: Option<f32>,
    /// Minimum relative moisture index in `[0, 1]` (`ClimateSample::moisture`).
    #[serde(default)]
    pub min_moisture: Option<f32>,
    /// Maximum relative moisture index in `[0, 1]` (`ClimateSample::moisture`).
    #[serde(default)]
    pub max_moisture: Option<f32>,
    /// Minimum foliage density in `[0, 1]` (`ClimateSample::foliage`).
    #[serde(default)]
    pub min_foliage: Option<f32>,
    /// Maximum foliage density in `[0, 1]` (`ClimateSample::foliage`).
    #[serde(default)]
    pub max_foliage: Option<f32>,
    /// Minimum 4-neighbor tactical elevation step in feet (`>= 0`).
    #[serde(default)]
    pub min_slope_step_ft: Option<i32>,
    /// Maximum 4-neighbor tactical elevation step in feet (`>= 0`).
    #[serde(default)]
    pub max_slope_step_ft: Option<i32>,
    /// Minimum surface elevation in feet relative to sea level.
    #[serde(default)]
    pub min_elevation_ft: Option<i32>,
    /// Maximum surface elevation in feet relative to sea level.
    #[serde(default)]
    pub max_elevation_ft: Option<i32>,
    /// Strength in `[0, 1]` of 3D sphere-space grove/clearing probability modulation (`0` = uniform).
    #[serde(default)]
    pub cluster_strength: f32,
    /// Characteristic grove/clearing diameter in 5-foot tiles (`> 0`).
    #[serde(default = "default_cluster_scale")]
    pub cluster_scale_tiles: f32,
    /// Seed offset for sphere-space cluster noise; layers with matching scale and offset co-locate.
    #[serde(default)]
    pub cluster_seed_offset: u64,
    /// Whether sprites in this layer rotate freely over `0..TAU` around Z (default `false`).
    #[serde(default)]
    pub rotatable: bool,
    /// Whether sprites in this layer randomly mirror horizontally (default `true`).
    #[serde(default = "default_true")]
    pub allow_flip_x: bool,
    /// Whether sprites in this layer randomly mirror vertically (default `false`).
    #[serde(default)]
    pub allow_flip_y: bool,
    /// Per-instance warm/cool and brightness tint variation in `[0.0, 0.3]` (default `0.0`).
    #[serde(default)]
    pub tint_variation: f32,
    /// Companion directional drop shadow alpha in `[0.0, 0.85]` (`0.0` disables shadows).
    #[serde(default)]
    pub shadow_alpha: f32,
    /// Shadow offset `[east_tiles, south_tiles]` in chunk world axes, never rotated by sprite Z angle.
    #[serde(default = "default_shadow_offset")]
    pub shadow_offset_tiles: [f32; 2],
    /// Companion shadow size multiplier relative to `size_tiles` (default `1.04`).
    #[serde(default = "default_shadow_scale")]
    pub shadow_scale: f32,
    /// Weighted sprite choices for this layer.
    pub sprites: Vec<ClutterSprite>,
}

/// 1 clutter sprite choice, its drawn tile-size range, and optional per-sprite visual overrides.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ClutterSprite {
    /// Image path relative to `assets/`.
    pub texture: String,
    /// Relative pick weight, at least `1`.
    pub weight: u32,
    /// Smallest drawn size of the sprite's longest side, in 5-foot tiles (`> 0`).
    pub min_size_tiles: f32,
    /// Largest drawn size of the sprite's longest side, in 5-foot tiles (`<= 8.0`).
    pub max_size_tiles: f32,
    /// Optional per-sprite override for [`ClutterLayerRule::clearance_tiles`].
    #[serde(default)]
    pub clearance_tiles: Option<f32>,
    /// Optional per-sprite override for [`ClutterLayerRule::rotatable`].
    #[serde(default)]
    pub rotatable: Option<bool>,
    /// Optional per-sprite override for [`ClutterLayerRule::allow_flip_x`].
    #[serde(default)]
    pub allow_flip_x: Option<bool>,
    /// Optional per-sprite override for [`ClutterLayerRule::allow_flip_y`].
    #[serde(default)]
    pub allow_flip_y: Option<bool>,
    /// Optional per-sprite override for [`ClutterLayerRule::tint_variation`].
    #[serde(default)]
    pub tint_variation: Option<f32>,
    /// Optional per-sprite override for [`ClutterLayerRule::shadow_alpha`].
    #[serde(default)]
    pub shadow_alpha: Option<f32>,
    /// Optional per-sprite override for [`ClutterLayerRule::shadow_offset_tiles`].
    #[serde(default)]
    pub shadow_offset_tiles: Option<[f32; 2]>,
    /// Optional per-sprite override for [`ClutterLayerRule::shadow_scale`].
    #[serde(default)]
    pub shadow_scale: Option<f32>,
}

/// 1 entry of the terrain palette as the clutter table needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaletteClutterFlag<'a> {
    /// Palette id.
    pub id: &'a str,
    /// Whether clutter may grow on or overhang onto this cover (`false` for water, roads, floors).
    pub clutter: bool,
}

/// [`ClutterConfig`] resolved against the terrain palette in palette layer order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClutterTable {
    rules: Vec<Option<ClutterRule>>,
    layers: Vec<Vec<ClutterLayerRule>>,
    palette_allows_clutter: Vec<bool>,
}

impl ClutterConfig {
    /// Parses and validates a clutter table from JSON text.
    ///
    /// # Errors
    /// Returns a descriptive message naming the palette id and invalid field when JSON parsing
    /// fails or any rule violates size, probability, cell-step, filter, or shadow bounds.
    pub fn from_json(raw: &str) -> Result<Self, String> {
        let config: Self =
            serde_json::from_str(raw).map_err(|error| format!("invalid clutter table: {error}"))?;
        for (id, rule) in &config.palette {
            rule.validate()
                .map_err(|error| format!("palette id {id}: {error}"))?;
        }
        Ok(config)
    }

    /// Reads and validates [`CLUTTER_CONFIG_PATH`] from the workspace `assets/` folder.
    ///
    /// # Errors
    /// Returns a message if the file cannot be read or fails [`Self::from_json`].
    pub fn load_default() -> Result<Self, String> {
        let full_path = crate::terrain::assets_root().join(CLUTTER_CONFIG_PATH);
        let raw = std::fs::read_to_string(&full_path)
            .map_err(|error| format!("could not read {}: {error}", full_path.display()))?;
        Self::from_json(&raw).map_err(|error| format!("{}: {error}", full_path.display()))
    }

    /// Every distinct sprite texture path referenced across legacy rules and multi-tier `layers`,
    /// sorted in a deterministic order.
    #[must_use]
    pub fn textures(&self) -> Vec<&str> {
        let mut textures: Vec<&str> = self
            .palette
            .values()
            .flat_map(|rule| {
                rule.sprites
                    .iter()
                    .chain(rule.layers.iter().flat_map(|layer| layer.sprites.iter()))
            })
            .map(|sprite| sprite.texture.as_str())
            .collect();
        textures.sort_unstable();
        textures.dedup();
        textures
    }

    /// Resolves this configuration against `palette`, in palette index order.
    ///
    /// # Errors
    /// Returns an error if the table names an unknown palette id or attaches clutter to a cover
    /// whose palette entry sets `clutter: false`.
    pub fn resolve(&self, palette: &[PaletteClutterFlag<'_>]) -> Result<ClutterTable, String> {
        for id in self.palette.keys() {
            match palette.iter().find(|entry| entry.id == id) {
                None => return Err(format!("clutter table names unknown palette id {id}")),
                Some(entry) if !entry.clutter => {
                    return Err(format!(
                        "clutter table gives clutter to palette id {id}, which has clutter: false"
                    ));
                }
                Some(_) => {}
            }
        }
        let palette_allows_clutter = palette.iter().map(|entry| entry.clutter).collect();
        let rules: Vec<Option<ClutterRule>> = palette
            .iter()
            .map(|entry| {
                entry
                    .clutter
                    .then(|| self.palette.get(entry.id).cloned())
                    .flatten()
            })
            .collect();
        let layers = rules
            .iter()
            .map(|rule| {
                rule.as_ref()
                    .map_or_else(Vec::new, ClutterRule::effective_layers)
            })
            .collect();
        Ok(ClutterTable {
            rules,
            layers,
            palette_allows_clutter,
        })
    }
}

impl ClutterRule {
    /// Expands this rule into its ordered list of [`ClutterLayerRule`]s, normalizing legacy
    /// top-level `sprites` into a default [`ClutterTier::Decal`] layer.
    #[must_use]
    pub fn effective_layers(&self) -> Vec<ClutterLayerRule> {
        let mut out = Vec::with_capacity(usize::from(!self.sprites.is_empty()) + self.layers.len());
        if !self.sprites.is_empty() {
            out.push(ClutterLayerRule {
                tier: ClutterTier::Decal,
                density: self.density,
                follows_foliage: self.follows_foliage,
                foliage_floor: self.foliage_floor,
                cell_step_tiles: 1,
                clearance_tiles: None,
                min_temperature: None,
                max_temperature: None,
                min_moisture: None,
                max_moisture: None,
                min_foliage: None,
                max_foliage: None,
                min_slope_step_ft: None,
                max_slope_step_ft: None,
                min_elevation_ft: None,
                max_elevation_ft: None,
                cluster_strength: 0.0,
                cluster_scale_tiles: DEFAULT_CLUSTER_SCALE_TILES,
                cluster_seed_offset: 0,
                rotatable: false,
                allow_flip_x: true,
                allow_flip_y: false,
                tint_variation: 0.0,
                shadow_alpha: 0.0,
                shadow_offset_tiles: DEFAULT_SHADOW_OFFSET_TILES,
                shadow_scale: DEFAULT_SHADOW_SCALE,
                sprites: self.sprites.clone(),
            });
        }
        out.extend(self.layers.iter().cloned());
        out
    }

    fn validate(&self) -> Result<(), String> {
        validate_unit_interval("density", self.density)?;
        validate_unit_interval("foliage_floor", self.foliage_floor)?;
        let effective = self.effective_layers();
        if effective.is_empty() {
            return Err("sprites and layers are both empty".to_owned());
        }
        for (index, layer) in effective.iter().enumerate() {
            layer
                .validate()
                .map_err(|error| format!("layer {index}: {error}"))?;
        }
        Ok(())
    }
}

impl ClutterLayerRule {
    fn validate(&self) -> Result<(), String> {
        validate_unit_interval("density", self.density)?;
        validate_unit_interval("foliage_floor", self.foliage_floor)?;
        if !(1..=MAX_CELL_STEP_TILES).contains(&self.cell_step_tiles) {
            return Err(format!(
                "cell_step_tiles {} is outside 1..={MAX_CELL_STEP_TILES}",
                self.cell_step_tiles
            ));
        }
        if let Some(clearance) = self.clearance_tiles {
            validate_clearance(clearance)?;
        }
        validate_f32_range(
            "temperature",
            self.min_temperature,
            self.max_temperature,
            None,
        )?;
        validate_f32_range(
            "moisture",
            self.min_moisture,
            self.max_moisture,
            Some((0.0, 1.0)),
        )?;
        validate_f32_range(
            "foliage",
            self.min_foliage,
            self.max_foliage,
            Some((0.0, 1.0)),
        )?;
        validate_i32_range(
            "slope_step_ft",
            self.min_slope_step_ft,
            self.max_slope_step_ft,
            Some(0),
        )?;
        validate_i32_range(
            "elevation_ft",
            self.min_elevation_ft,
            self.max_elevation_ft,
            None,
        )?;
        validate_unit_interval("cluster_strength", self.cluster_strength)?;
        if !self.cluster_scale_tiles.is_finite() || self.cluster_scale_tiles <= 0.0 {
            return Err(format!(
                "cluster_scale_tiles {} must be finite and > 0",
                self.cluster_scale_tiles
            ));
        }
        validate_tint_variation(self.tint_variation)?;
        validate_shadow_params(
            self.shadow_alpha,
            self.shadow_offset_tiles,
            self.shadow_scale,
        )?;
        if self.sprites.is_empty() {
            return Err("sprites is empty".to_owned());
        }
        for sprite in &self.sprites {
            sprite.validate()?;
        }
        Ok(())
    }

    const fn has_climate_filter(&self) -> bool {
        self.min_temperature.is_some()
            || self.max_temperature.is_some()
            || self.min_moisture.is_some()
            || self.max_moisture.is_some()
            || self.min_foliage.is_some()
            || self.max_foliage.is_some()
    }

    const fn has_terrain_filter(&self) -> bool {
        self.min_slope_step_ft.is_some()
            || self.max_slope_step_ft.is_some()
            || self.min_elevation_ft.is_some()
            || self.max_elevation_ft.is_some()
    }

    fn matches_environment(&self, input: &ClutterTileInput) -> bool {
        if self.has_climate_filter() {
            let (Some(temp), Some(moisture)) = (input.temperature, input.moisture) else {
                return false;
            };
            if self.min_temperature.is_some_and(|min| temp < min)
                || self.max_temperature.is_some_and(|max| temp > max)
                || self.min_moisture.is_some_and(|min| moisture < min)
                || self.max_moisture.is_some_and(|max| moisture > max)
                || self.min_foliage.is_some_and(|min| input.foliage < min)
                || self.max_foliage.is_some_and(|max| input.foliage > max)
            {
                return false;
            }
        }
        if self.has_terrain_filter() {
            let (Some(slope), Some(elev)) = (input.slope_step_ft, input.elevation_ft) else {
                return false;
            };
            if self.min_slope_step_ft.is_some_and(|min| slope < min)
                || self.max_slope_step_ft.is_some_and(|max| slope > max)
                || self.min_elevation_ft.is_some_and(|min| elev < min)
                || self.max_elevation_ft.is_some_and(|max| elev > max)
            {
                return false;
            }
        }
        true
    }
}

impl ClutterSprite {
    fn validate(&self) -> Result<(), String> {
        if self.texture.trim().is_empty() {
            return Err("sprite texture path is empty".to_owned());
        }
        if self.weight == 0 {
            return Err(format!("{} has weight 0", self.texture));
        }
        let sizes_valid = self.min_size_tiles.is_finite()
            && self.max_size_tiles.is_finite()
            && self.min_size_tiles > 0.0
            && self.min_size_tiles <= self.max_size_tiles
            && self.max_size_tiles <= MAX_SPRITE_SIZE_TILES;
        if !sizes_valid {
            return Err(format!(
                "{} needs 0 < min_size_tiles <= max_size_tiles <= {MAX_SPRITE_SIZE_TILES}",
                self.texture
            ));
        }
        if let Some(clearance) = self.clearance_tiles {
            validate_clearance(clearance).map_err(|e| format!("{}: {e}", self.texture))?;
        }
        if let Some(tint) = self.tint_variation {
            validate_tint_variation(tint).map_err(|e| format!("{}: {e}", self.texture))?;
        }
        let shadow_alpha = self.shadow_alpha.unwrap_or(0.0);
        let shadow_offset = self
            .shadow_offset_tiles
            .unwrap_or(DEFAULT_SHADOW_OFFSET_TILES);
        let shadow_scale = self.shadow_scale.unwrap_or(DEFAULT_SHADOW_SCALE);
        validate_shadow_params(shadow_alpha, shadow_offset, shadow_scale)
            .map_err(|e| format!("{}: {e}", self.texture))?;
        Ok(())
    }
}

fn validate_unit_interval(name: &str, value: f32) -> Result<(), String> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(format!("{name} {value} is outside [0, 1]"));
    }
    Ok(())
}

fn validate_clearance(clearance: f32) -> Result<(), String> {
    if !clearance.is_finite() || !(0.0..=MAX_SPRITE_SIZE_TILES).contains(&clearance) {
        return Err(format!(
            "clearance_tiles {clearance} is outside [0, {MAX_SPRITE_SIZE_TILES}]"
        ));
    }
    Ok(())
}

fn validate_tint_variation(tint: f32) -> Result<(), String> {
    if !tint.is_finite() || !(0.0..=MAX_TINT_VARIATION).contains(&tint) {
        return Err(format!(
            "tint_variation {tint} is outside [0, {MAX_TINT_VARIATION}]"
        ));
    }
    Ok(())
}

fn validate_shadow_params(alpha: f32, offset: [f32; 2], scale: f32) -> Result<(), String> {
    if !alpha.is_finite() || !(0.0..=MAX_SHADOW_ALPHA).contains(&alpha) {
        return Err(format!(
            "shadow_alpha {alpha} is outside [0, {MAX_SHADOW_ALPHA}]"
        ));
    }
    if !offset[0].is_finite()
        || !offset[1].is_finite()
        || offset[0].abs() > MAX_SPRITE_SIZE_TILES
        || offset[1].abs() > MAX_SPRITE_SIZE_TILES
    {
        return Err(format!(
            "shadow_offset_tiles {offset:?} exceeds +/-{MAX_SPRITE_SIZE_TILES}"
        ));
    }
    if !scale.is_finite() || scale <= 0.0 || scale > MAX_SHADOW_SCALE {
        return Err(format!(
            "shadow_scale {scale} is outside (0, {MAX_SHADOW_SCALE}]"
        ));
    }
    Ok(())
}

fn validate_f32_range(
    name: &str,
    min: Option<f32>,
    max: Option<f32>,
    bounds: Option<(f32, f32)>,
) -> Result<(), String> {
    for (bound_label, val) in [("min", min), ("max", max)] {
        if let Some(v) = val {
            if !v.is_finite() {
                return Err(format!("{bound_label}_{name} must be finite"));
            }
            if let Some((lo, hi)) = bounds
                && !(lo..=hi).contains(&v)
            {
                return Err(format!("{bound_label}_{name} {v} is outside [{lo}, {hi}]"));
            }
        }
    }
    if let (Some(lo), Some(hi)) = (min, max)
        && lo > hi
    {
        return Err(format!("min_{name} ({lo}) exceeds max_{name} ({hi})"));
    }
    Ok(())
}

fn validate_i32_range(
    name: &str,
    min: Option<i32>,
    max: Option<i32>,
    lower_bound: Option<i32>,
) -> Result<(), String> {
    if let Some(lo_limit) = lower_bound {
        for (bound_label, val) in [("min", min), ("max", max)] {
            if let Some(v) = val
                && v < lo_limit
            {
                return Err(format!("{bound_label}_{name} ({v}) is below {lo_limit}"));
            }
        }
    }
    if let (Some(lo), Some(hi)) = (min, max)
        && lo > hi
    {
        return Err(format!("min_{name} ({lo}) exceeds max_{name} ({hi})"));
    }
    Ok(())
}

/// Environmental and per-tile inputs that clutter placement reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClutterTileInput {
    /// Dominant palette index of the tile.
    pub palette_index: usize,
    /// Whether an authored region painted this tile (roads, paths, bridges, town floors).
    pub region_painted: bool,
    /// Mean annual air temperature in degrees Celsius (`None` when climate maps are absent).
    pub temperature: Option<f32>,
    /// Relative moisture index in `[0, 1]` (`None` when climate maps are absent).
    pub moisture: Option<f32>,
    /// Climate foliage density in `[0, 1]` (`ClimateSample::foliage`).
    pub foliage: f32,
    /// Maximum 4-neighbor tactical elevation step in feet (`None` when heightmaps are absent).
    pub slope_step_ft: Option<i32>,
    /// Surface elevation in feet relative to sea level (`None` when heightmaps are absent).
    pub elevation_ft: Option<i32>,
    /// 3D unit-sphere direction of the candidate tile, used for seam-continuous grove clustering.
    pub unit_dir: [f64; 3],
    /// Stable per-tile or per-cell hash (`zeta_core::world::tile_hash`).
    pub hash: u64,
}

impl ClutterTileInput {
    /// Minimal tile input without optional climate/elevation maps, used by single-tile callers and tests.
    #[must_use]
    pub const fn basic(palette_index: usize, foliage: f32, hash: u64) -> Self {
        Self {
            palette_index,
            region_painted: false,
            temperature: None,
            moisture: None,
            foliage,
            slope_step_ft: None,
            elevation_ft: None,
            unit_dir: [1.0, 0.0, 0.0],
            hash,
        }
    }
}

/// Resolved visual placement of 1 clutter sprite relative to its owning tile center.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClutterPlacement {
    /// Index of the originating [`ClutterLayerRule`] in the palette entry's effective layers.
    pub layer: usize,
    /// Ecological vertical draw tier of this sprite.
    pub tier: ClutterTier,
    /// Index into that layer's `sprites` slice.
    pub sprite: usize,
    /// Offset of the sprite center from the owning tile center, in tiles (`+x` east, `+y` south).
    pub offset_tiles: Vec2,
    /// Drawn size of the sprite's longest side, in 5-foot tiles.
    pub size_tiles: f32,
    /// Footprint clearance radius around the sprite center kept off roads and water, in tiles.
    pub clearance_radius_tiles: f32,
    /// Counter-clockwise Z rotation in radians (`0.0` when `rotatable` is `false`).
    pub rotation_radians: f32,
    /// Whether the sprite is mirrored horizontally.
    pub flip_x: bool,
    /// Whether the sprite is mirrored vertically.
    pub flip_y: bool,
    /// Per-instance linear/sRGB tint multiplier `[r, g, b]`.
    pub tint_rgb: [f32; 3],
    /// Companion drop shadow opacity (`0.0` means no companion shadow is spawned).
    pub shadow_alpha: f32,
    /// Companion drop shadow offset `[east_tiles, south_tiles]` in chunk world axes.
    pub shadow_offset_tiles: Vec2,
    /// Companion drop shadow scale multiplier relative to `size_tiles`.
    pub shadow_scale: f32,
    /// Deterministic value in `[0, 1)` breaking Z-order ties between sprites on the same row.
    pub z_tiebreak: f32,
}

impl ClutterTable {
    /// Returns the raw [`ClutterRule`] for `palette_index`, if that cover carries clutter.
    #[must_use]
    pub fn rule(&self, palette_index: usize) -> Option<&ClutterRule> {
        self.rules.get(palette_index).and_then(Option::as_ref)
    }

    /// Returns the expanded [`ClutterLayerRule`] list for `palette_index` (empty if none).
    #[must_use]
    pub fn layers(&self, palette_index: usize) -> &[ClutterLayerRule] {
        self.layers.get(palette_index).map_or(&[], Vec::as_slice)
    }

    /// Returns whether palette entry `palette_index` allows clutter (`clutter: true` in `palette.json`).
    #[must_use]
    pub fn palette_allows_clutter(&self, palette_index: usize) -> bool {
        self.palette_allows_clutter
            .get(palette_index)
            .copied()
            .unwrap_or(false)
    }

    /// Evaluates the first per-tile (`cell_step_tiles == 1`) placement on `input`, if any.
    ///
    /// Pure and deterministic: the same input always yields the exact same [`ClutterPlacement`].
    #[must_use]
    pub fn place(&self, input: ClutterTileInput) -> Option<ClutterPlacement> {
        if input.region_painted {
            return None;
        }
        let layers = self.layers(input.palette_index);
        for (layer_index, layer) in layers.iter().enumerate() {
            if layer.cell_step_tiles == 1
                && let Some(placement) = evaluate_layer_placement(layer, layer_index, None, &input)
            {
                return Some(placement);
            }
        }
        None
    }

    /// Evaluates a specific layer `layer_index` for `input`, optionally supplying a pre-computed
    /// sub-tile offset (used by canonical `cell_step_tiles > 1` cell placement).
    #[must_use]
    pub fn place_layer(
        &self,
        layer_index: usize,
        sub_offset_override: Option<Vec2>,
        input: ClutterTileInput,
    ) -> Option<ClutterPlacement> {
        if input.region_painted {
            return None;
        }
        let layer = self.layers(input.palette_index).get(layer_index)?;
        evaluate_layer_placement(layer, layer_index, sub_offset_override, &input)
    }
}

/// Evaluates 1 [`ClutterLayerRule`] against `input` and resolves all visual attributes.
fn evaluate_layer_placement(
    layer: &ClutterLayerRule,
    layer_index: usize,
    sub_offset_override: Option<Vec2>,
    input: &ClutterTileInput,
) -> Option<ClutterPlacement> {
    if !layer.matches_environment(input) {
        return None;
    }
    let hash = mix_layer_hash(
        input.hash,
        layer_index,
        layer.tier,
        layer.cluster_seed_offset,
    );
    let base_chance = if layer.follows_foliage {
        let foliage = input.foliage.clamp(0.0, 1.0);
        layer.density * (layer.foliage_floor + (1.0 - layer.foliage_floor) * foliage)
    } else {
        layer.density
    };
    if base_chance <= 0.0 {
        return None;
    }

    let strength = layer.cluster_strength.clamp(0.0, 1.0);
    let max_chance = (base_chance * (1.0 + strength)).min(1.0);
    let spawn_roll = hash_unit(hash, SALT_SPAWN);
    if spawn_roll >= max_chance {
        return None;
    }

    let chance = if strength > 0.0 {
        let grove = sphere_cluster_unit(
            input.unit_dir,
            layer.cluster_scale_tiles,
            layer.cluster_seed_offset,
        );
        let factor = 1.0 + strength * (2.0 * grove - 1.0);
        (base_chance * factor).clamp(0.0, 1.0)
    } else {
        base_chance.clamp(0.0, 1.0)
    };
    if spawn_roll >= chance {
        return None;
    }

    let sprite_index = pick_weighted(&layer.sprites, hash_unit(hash, SALT_SPRITE));
    let sprite = &layer.sprites[sprite_index];
    let size_tiles = sprite.min_size_tiles
        + (sprite.max_size_tiles - sprite.min_size_tiles) * hash_unit(hash, SALT_SIZE);

    let offset_tiles = sub_offset_override.unwrap_or_else(|| {
        let slack = if size_tiles <= 1.0 {
            (1.0 - size_tiles) * 0.5
        } else {
            OVERSIZED_SINGLE_TILE_SLACK
        };
        Vec2::new(
            (hash_unit(hash, SALT_OFFSET_X) * 2.0 - 1.0) * slack,
            (hash_unit(hash, SALT_OFFSET_Y) * 2.0 - 1.0) * slack,
        )
    });

    let default_clearance = if size_tiles > 1.0 || layer.cell_step_tiles > 1 {
        size_tiles * 0.5
    } else {
        0.0
    };
    let clearance_radius_tiles = sprite
        .clearance_tiles
        .or(layer.clearance_tiles)
        .unwrap_or(default_clearance);

    let rotatable = sprite.rotatable.unwrap_or(layer.rotatable);
    let allow_flip_x = sprite.allow_flip_x.unwrap_or(layer.allow_flip_x);
    let allow_flip_y = sprite.allow_flip_y.unwrap_or(layer.allow_flip_y);
    let tint_variation = sprite.tint_variation.unwrap_or(layer.tint_variation);
    let shadow_alpha = sprite.shadow_alpha.unwrap_or(layer.shadow_alpha);
    let shadow_offset = sprite
        .shadow_offset_tiles
        .unwrap_or(layer.shadow_offset_tiles);
    let shadow_scale = sprite.shadow_scale.unwrap_or(layer.shadow_scale);

    let rotation_radians = if rotatable {
        hash_unit(hash, SALT_ROTATION) * TAU
    } else {
        0.0
    };
    let flip_x = allow_flip_x && hash_unit(hash, SALT_FLIP_X) < 0.5;
    let flip_y = allow_flip_y && hash_unit(hash, SALT_FLIP_Y) < 0.5;
    let tint_rgb = compute_painterly_tint(hash, tint_variation);

    Some(ClutterPlacement {
        layer: layer_index,
        tier: layer.tier,
        sprite: sprite_index,
        offset_tiles,
        size_tiles,
        clearance_radius_tiles,
        rotation_radians,
        flip_x,
        flip_y,
        tint_rgb,
        shadow_alpha,
        shadow_offset_tiles: Vec2::from_array(shadow_offset),
        shadow_scale,
        z_tiebreak: hash_unit(hash, SALT_Z_TIEBREAK),
    })
}

/// Preserves legacy bit-exact hashes for default layer 0 while decorrelating additional layers.
const fn mix_layer_hash(
    base_hash: u64,
    layer_index: usize,
    tier: ClutterTier,
    seed_offset: u64,
) -> u64 {
    if layer_index == 0 && matches!(tier, ClutterTier::Decal) && seed_offset == 0 {
        base_hash
    } else {
        let tag = (layer_index as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(tier.salt_bits() << 48)
            .wrapping_add(seed_offset.wrapping_mul(0xBF58_476D_1CE4_E5B9));
        splitmix64(base_hash ^ splitmix64(SALT_LAYER_MIX ^ tag))
    }
}

/// Computes a subtle warm-sunlit / cool-shaded RGB multiplier around `[1.0, 1.0, 1.0]`.
fn compute_painterly_tint(hash: u64, tint_variation: f32) -> [f32; 3] {
    if tint_variation <= 0.0 {
        return [1.0, 1.0, 1.0];
    }
    let brightness = (hash_unit(hash, SALT_TINT_BRIGHTNESS) * 2.0 - 1.0) * tint_variation;
    let warmth = (hash_unit(hash, SALT_TINT_WARMTH) * 2.0 - 1.0) * (tint_variation * 0.45);
    [
        (1.0 + brightness + warmth).clamp(0.4, 1.35),
        (1.0 + brightness + warmth * 0.2).clamp(0.4, 1.35),
        (1.0 + brightness - warmth).clamp(0.4, 1.35),
    ]
}

/// Evaluates coherent 2-octave 3D unit-sphere grove modulation in `[0.0, 1.0]`, continuous across
/// all 6 cube faces and 12 seams (`0.0` = open sunlit clearing, `1.0` = dense grove interior).
#[must_use]
pub fn sphere_cluster_unit(unit_dir: [f64; 3], scale_tiles: f32, seed_offset: u64) -> f32 {
    let feature_tiles = f64::from(scale_tiles.max(0.5));
    let frequency = TILES_PER_RADIAN / feature_tiles;
    let seed = splitmix64(CLUSTER_NOISE_SEED ^ seed_offset);
    let coarse = sphere_value_noise_3d(
        unit_dir[0] * frequency,
        unit_dir[1] * frequency,
        unit_dir[2] * frequency,
        seed,
    );
    let fine_freq = frequency * 2.35;
    let fine = sphere_value_noise_3d(
        unit_dir[0] * fine_freq,
        unit_dir[1] * fine_freq,
        unit_dir[2] * fine_freq,
        seed ^ 0xA5A5_5A5A_F0F0_0F0F,
    );
    let combined = coarse * 0.72 + fine * 0.28;
    // 3D trilinear lattice averages cluster tightly near 0; contrast-stretching before smoothstep
    // produces genuine open clearings (~0) and lush groves (~1) with symmetric mean 0.5.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "clamped noise value lies in [-1.0, 1.0], exact enough in f32"
    )]
    let contrasted = (combined * 1.85).clamp(-1.0, 1.0) as f32;
    smoothstep(-1.0, 1.0, contrasted)
}

fn sphere_value_noise_3d(x: f64, y: f64, z: f64, seed: u64) -> f64 {
    let (fx, fy, fz) = (x.floor(), y.floor(), z.floor());
    let tx = quintic_fade(x - fx);
    let ty = quintic_fade(y - fy);
    let tz = quintic_fade(z - fz);

    #[allow(
        clippy::cast_possible_truncation,
        reason = "unit-sphere coordinates scaled by tile frequency fit well within i64"
    )]
    let (ix, iy, iz) = (fx as i64, fy as i64, fz as i64);

    let c000 = lattice_signed_unit(ix, iy, iz, seed);
    let c100 = lattice_signed_unit(ix + 1, iy, iz, seed);
    let c010 = lattice_signed_unit(ix, iy + 1, iz, seed);
    let c110 = lattice_signed_unit(ix + 1, iy + 1, iz, seed);
    let c001 = lattice_signed_unit(ix, iy, iz + 1, seed);
    let c101 = lattice_signed_unit(ix + 1, iy, iz + 1, seed);
    let c011 = lattice_signed_unit(ix, iy + 1, iz + 1, seed);
    let c111 = lattice_signed_unit(ix + 1, iy + 1, iz + 1, seed);

    let x00 = c000 + (c100 - c000) * tx;
    let x10 = c010 + (c110 - c010) * tx;
    let x01 = c001 + (c101 - c001) * tx;
    let x11 = c011 + (c111 - c011) * tx;
    let y0 = x00 + (x10 - x00) * ty;
    let y1 = x01 + (x11 - x01) * ty;
    y0 + (y1 - y0) * tz
}

fn quintic_fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn lattice_signed_unit(ix: i64, iy: i64, iz: i64, seed: u64) -> f64 {
    let mut state = splitmix64(seed ^ ix.cast_unsigned());
    state = splitmix64(state ^ iy.cast_unsigned());
    state = splitmix64(state ^ iz.cast_unsigned());
    let mantissa = state >> 11;
    #[allow(
        clippy::cast_precision_loss,
        reason = "53-bit integer fits losslessly in f64 mantissa"
    )]
    let unit = (mantissa as f64) * (1.0 / ((1_u64 << 53) as f64));
    unit * 2.0 - 1.0
}

const fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

/// Picks a sprite index with probability proportional to its weight; `roll` is in `[0, 1)`.
fn pick_weighted(sprites: &[ClutterSprite], roll: f32) -> usize {
    let total: f64 = sprites.iter().map(|sprite| f64::from(sprite.weight)).sum();
    let target = f64::from(roll) * total;
    let mut cumulative = 0.0;
    for (index, sprite) in sprites.iter().enumerate() {
        cumulative += f64::from(sprite.weight);
        if target < cumulative {
            return index;
        }
    }
    sprites.len().saturating_sub(1)
}

/// Optional planetary context passed to [`chunk_clutter`] for climate/slope filtering and
/// cross-chunk footprint clearance near rivers, coastlines, and authored roads.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClutterWorldContext<'a> {
    /// Layer 0 planetary heightmaps for elevation/slope filters and cross-chunk water checks.
    pub maps: Option<&'a PlanetHeightmaps>,
    /// Planetary climate maps for temperature, moisture, and foliage sampling.
    pub climate: Option<&'a PlanetClimateMaps>,
    /// Active authored region paint layers for cross-chunk road/bridge/town clearance.
    pub regions: &'a [RegionPaint],
    /// Terrain palette for classifying cross-chunk natural ground covers.
    pub palette: Option<&'a TerrainPalette>,
}

/// 1 resolved clutter sprite instance belonging to 1 streamed `64x64` chunk.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChunkClutterSprite {
    /// Owning tile column inside the chunk (`0..64`).
    pub local_x: u32,
    /// Owning tile row inside the chunk (`0..64`).
    pub local_y: u32,
    /// Canonical face row (`PlanetTile::y`) of the owning tile, used for cross-chunk Y-sorting.
    pub tile_y: u32,
    /// Dominant palette index of the owning tile.
    pub palette_index: u8,
    /// Resolved tier, sprite index, offset, size, rotation, tint, and shadow parameters.
    pub placement: ClutterPlacement,
}

#[derive(Clone, Copy)]
struct CachedTileEnv {
    climate: Option<ClimateSample>,
    terrain: Option<(i32, i32)>,
}

struct ChunkEnvSampler<'a> {
    world: ClutterWorldContext<'a>,
    cache: Vec<Option<CachedTileEnv>>,
}

impl<'a> ChunkEnvSampler<'a> {
    fn new(world: ClutterWorldContext<'a>, need_cache: bool) -> Self {
        let cache = if need_cache {
            vec![None; CHUNK_TILE_COUNT]
        } else {
            Vec::new()
        };
        Self { world, cache }
    }

    fn build_input(
        &mut self,
        local_x: u32,
        local_y: u32,
        tile: PlanetTile,
        palette_index: u8,
        layer: &ClutterLayerRule,
        hash: u64,
    ) -> ClutterTileInput {
        let need_climate = layer.follows_foliage || layer.has_climate_filter();
        let need_terrain = layer.has_terrain_filter();
        let need_unit_dir = layer.cluster_strength > 0.0;
        let idx = ChunkGroundCover::index(local_x, local_y);
        let mut cached = idx
            .and_then(|i| self.cache.get(i).copied().flatten())
            .unwrap_or(CachedTileEnv {
                climate: None,
                terrain: None,
            });
        let mut updated = false;

        if need_climate
            && cached.climate.is_none()
            && let Some(maps) = self.world.climate
        {
            cached.climate = Some(sample_climate(maps, tile));
            updated = true;
        }
        if need_terrain
            && cached.terrain.is_none()
            && let Some(maps) = self.world.maps
        {
            cached.terrain = Some(sample_slope_and_elevation(maps, tile));
            updated = true;
        }
        if updated
            && let Some(i) = idx
            && let Some(slot) = self.cache.get_mut(i)
        {
            *slot = Some(cached);
        }

        let foliage = match cached.climate {
            Some(clim) => clim.foliage,
            None if self.world.climate.is_some() => 1.0,
            None => 0.0,
        };
        let unit_dir = if need_unit_dir {
            tile_to_unit_vec(tile)
        } else {
            [1.0, 0.0, 0.0]
        };

        ClutterTileInput {
            palette_index: usize::from(palette_index),
            region_painted: false,
            temperature: cached.climate.map(|c| c.temperature),
            moisture: cached.climate.map(|c| c.moisture),
            foliage,
            slope_step_ft: cached.terrain.map(|(slope, _)| slope),
            elevation_ft: cached.terrain.map(|(_, elev)| elev),
            unit_dir,
            hash,
        }
    }
}

fn sample_slope_and_elevation(maps: &PlanetHeightmaps, tile: PlanetTile) -> (i32, i32) {
    let center = sample_tile(maps, tile);
    let mut max_step = 0_i32;
    for (dx, dy) in [(-1_i64, 0_i64), (1, 0), (0, -1), (0, 1)] {
        let (neighbor, _) = step_tile(tile, dx, dy);
        let step_diff =
            (sample_tile(maps, neighbor).elevation_step_ft - center.elevation_step_ft).abs();
        max_step = max_step.max(step_diff);
    }
    (max_step, center.elevation_ft)
}

/// Generates every clutter sprite belonging to `cover`, sorted deterministically by tier and tile.
///
/// Evaluates both `1x1` per-tile layers and canonical multi-tile `cell_step_tiles > 1` layers,
/// enforcing footprint clearance against `region_painted` tiles and `clutter: false` covers.
#[must_use]
pub fn chunk_clutter(
    table: &ClutterTable,
    cover: &ChunkGroundCover,
    world: ClutterWorldContext<'_>,
) -> Vec<ChunkClutterSprite> {
    let mut present_palettes = [false; 256];
    let mut any_rules = false;
    for (&palette_idx, &painted) in cover.dominant.iter().zip(cover.region_painted.iter()) {
        if !painted && !table.layers(usize::from(palette_idx)).is_empty() {
            present_palettes[usize::from(palette_idx)] = true;
            any_rules = true;
        }
    }
    if !any_rules {
        return Vec::new();
    }

    let mut sampler = ChunkEnvSampler::new(world, world.climate.is_some() || world.maps.is_some());
    let mut sprites = Vec::new();
    evaluate_per_tile_layers(table, cover, &world, &mut sampler, &mut sprites);

    // Pass 2: canonical face-cell (`cell_step_tiles > 1`) spaced multi-tile layers.
    let origin = cover.coord.origin_tile();
    for (palette_usize, &is_present) in present_palettes.iter().enumerate() {
        if !is_present {
            continue;
        }
        let Ok(palette_index) = u8::try_from(palette_usize) else {
            continue;
        };
        for (layer_index, layer) in table.layers(palette_usize).iter().enumerate() {
            if layer.cell_step_tiles <= 1 {
                continue;
            }
            evaluate_spaced_cell_layer(
                table,
                cover,
                &world,
                &mut sampler,
                origin,
                palette_index,
                layer_index,
                layer,
                &mut sprites,
            );
        }
    }

    sprites.sort_by_key(|s| {
        (
            s.placement.tier,
            s.local_y,
            s.local_x,
            s.palette_index,
            s.placement.layer,
        )
    });
    sprites
}

fn evaluate_per_tile_layers(
    table: &ClutterTable,
    cover: &ChunkGroundCover,
    world: &ClutterWorldContext<'_>,
    sampler: &mut ChunkEnvSampler<'_>,
    sprites: &mut Vec<ChunkClutterSprite>,
) {
    for local_y in 0..CHUNK_SIZE {
        for local_x in 0..CHUNK_SIZE {
            let (Some(tile), Some(palette_index), Some(false)) = (
                cover.coord.tile_at(local_x, local_y),
                cover.dominant_at(local_x, local_y),
                cover.region_painted_at(local_x, local_y),
            ) else {
                continue;
            };
            let layers = table.layers(usize::from(palette_index));
            if layers.is_empty() {
                continue;
            }
            let base_hash = tile_hash(tile);
            for (layer_index, layer) in layers.iter().enumerate() {
                if layer.cell_step_tiles != 1 {
                    continue;
                }
                let input =
                    sampler.build_input(local_x, local_y, tile, palette_index, layer, base_hash);
                if let Some(placement) = evaluate_layer_placement(layer, layer_index, None, &input)
                    && footprint_is_clear(
                        table,
                        cover,
                        world,
                        local_x,
                        local_y,
                        placement.offset_tiles,
                        placement.clearance_radius_tiles,
                    )
                {
                    sprites.push(ChunkClutterSprite {
                        local_x,
                        local_y,
                        tile_y: tile.y,
                        palette_index,
                        placement,
                    });
                }
            }
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "internal helper threads chunk cover, sampler, and layer metadata without wrapper churn"
)]
fn evaluate_spaced_cell_layer(
    table: &ClutterTable,
    cover: &ChunkGroundCover,
    world: &ClutterWorldContext<'_>,
    sampler: &mut ChunkEnvSampler<'_>,
    origin: PlanetTile,
    palette_index: u8,
    layer_index: usize,
    layer: &ClutterLayerRule,
    sprites: &mut Vec<ChunkClutterSprite>,
) {
    let step = layer.cell_step_tiles;
    let min_cell_x = origin.x / step;
    let max_cell_x = origin.x.saturating_add(CHUNK_SIZE - 1) / step;
    let min_cell_y = origin.y / step;
    let max_cell_y = origin.y.saturating_add(CHUNK_SIZE - 1) / step;

    for cell_y in min_cell_y..=max_cell_y {
        let cell_min_y = cell_y.saturating_mul(step);
        let cell_max_y = cell_min_y.saturating_add(step).min(FACE_TILES);
        let cell_h = cell_max_y.saturating_sub(cell_min_y);
        if cell_h == 0 {
            continue;
        }
        for cell_x in min_cell_x..=max_cell_x {
            let cell_min_x = cell_x.saturating_mul(step);
            let cell_max_x = cell_min_x.saturating_add(step).min(FACE_TILES);
            let cell_w = cell_max_x.saturating_sub(cell_min_x);
            if cell_w == 0 {
                continue;
            }

            let cell_anchor = PlanetTile {
                face: origin.face,
                x: cell_min_x,
                y: cell_min_y,
            };
            // Salted by `(layer_index, tier, cluster_seed_offset)` rather than `palette_index` so
            // blended variants of 1 cover never double-pack the same canonical cell.
            let cell_hash = mix_layer_hash(
                tile_hash(cell_anchor),
                layer_index,
                layer.tier,
                layer.cluster_seed_offset,
            );
            let span = 1.0 - 2.0 * CELL_BORDER_MARGIN_FRACTION;
            let jx = CELL_BORDER_MARGIN_FRACTION + span * hash_unit(cell_hash, SALT_CELL_JITTER_X);
            let jy = CELL_BORDER_MARGIN_FRACTION + span * hash_unit(cell_hash, SALT_CELL_JITTER_Y);

            let in_x = jx * count_to_f32(cell_w);
            let in_y = jy * count_to_f32(cell_h);
            let step_in_x = bounded_floor_u32(in_x, cell_w - 1);
            let step_in_y = bounded_floor_u32(in_y, cell_h - 1);
            let tile_x = cell_min_x + step_in_x;
            let tile_y = cell_min_y + step_in_y;

            // Spawn the sprite ONLY in the chunk that owns its jittered center `(tile_x, tile_y)`.
            if tile_x < origin.x
                || tile_x >= origin.x + CHUNK_SIZE
                || tile_y < origin.y
                || tile_y >= origin.y + CHUNK_SIZE
            {
                continue;
            }
            let local_x = tile_x - origin.x;
            let local_y = tile_y - origin.y;
            if cover.region_painted_at(local_x, local_y) != Some(false)
                || cover.dominant_at(local_x, local_y) != Some(palette_index)
            {
                continue;
            }
            let Some(tile) = cover.coord.tile_at(local_x, local_y) else {
                continue;
            };
            let sub_offset = Vec2::new(
                in_x - count_to_f32(step_in_x) - 0.5,
                in_y - count_to_f32(step_in_y) - 0.5,
            );

            let input = sampler.build_input(
                local_x,
                local_y,
                tile,
                palette_index,
                layer,
                tile_hash(tile) ^ cell_hash,
            );
            if let Some(placement) =
                evaluate_layer_placement(layer, layer_index, Some(sub_offset), &input)
                && footprint_is_clear(
                    table,
                    cover,
                    world,
                    local_x,
                    local_y,
                    placement.offset_tiles,
                    placement.clearance_radius_tiles,
                )
            {
                sprites.push(ChunkClutterSprite {
                    local_x,
                    local_y,
                    tile_y: tile.y,
                    palette_index,
                    placement,
                });
            }
        }
    }
}

/// Checks that no `region_painted` or `clutter: false` tile square intersects `clearance_radius`
/// around `(local_x + 0.5 + offset.x, local_y + 0.5 + offset.y)`.
fn footprint_is_clear(
    table: &ClutterTable,
    cover: &ChunkGroundCover,
    world: &ClutterWorldContext<'_>,
    local_x: u32,
    local_y: u32,
    offset_tiles: Vec2,
    clearance_radius: f32,
) -> bool {
    if clearance_radius <= 0.0 {
        return true;
    }
    let center_x = count_to_f32(local_x) + 0.5 + offset_tiles.x;
    let center_y = count_to_f32(local_y) + 0.5 + offset_tiles.y;
    let col_min = floor_to_i32(center_x - clearance_radius);
    let col_max = floor_to_i32(center_x + clearance_radius);
    let row_min = floor_to_i32(center_y - clearance_radius);
    let row_max = floor_to_i32(center_y + clearance_radius);
    let chunk_limit = i32::try_from(CHUNK_SIZE).unwrap_or(64);
    let origin = cover.coord.origin_tile();

    for row in row_min..=row_max {
        for col in col_min..=col_max {
            let tile_center_x = i32_to_f32(col) + 0.5;
            let tile_center_y = i32_to_f32(row) + 0.5;
            let dx = ((center_x - tile_center_x).abs() - 0.5).max(0.0);
            let dy = ((center_y - tile_center_y).abs() - 0.5).max(0.0);
            if dx * dx + dy * dy >= clearance_radius * clearance_radius {
                continue;
            }

            if col >= 0 && col < chunk_limit && row >= 0 && row < chunk_limit {
                let ux = u32::try_from(col).unwrap_or(0);
                let uy = u32::try_from(row).unwrap_or(0);
                if cover.region_painted_at(ux, uy) == Some(true) {
                    return false;
                }
                let Some(dom) = cover.dominant_at(ux, uy) else {
                    return false;
                };
                if !table.palette_allows_clutter(usize::from(dom)) {
                    return false;
                }
            } else if !outside_chunk_tile_allows_clutter(table, cover, world, origin, col, row) {
                return false;
            }
        }
    }
    true
}

/// Evaluates whether a tile `(tx, ty)` just outside `[0, CHUNK_SIZE)` permits overhanging clutter.
fn outside_chunk_tile_allows_clutter(
    table: &ClutterTable,
    cover: &ChunkGroundCover,
    world: &ClutterWorldContext<'_>,
    origin: PlanetTile,
    tx: i32,
    ty: i32,
) -> bool {
    let (neighbor_tile, _) = step_tile(origin, i64::from(tx), i64::from(ty));
    if !world.regions.is_empty() && tile_is_region_painted(world.regions, neighbor_tile) {
        return false;
    }
    if let Some(maps) = world.maps {
        let sample = sample_tile(maps, neighbor_tile);
        if sample.is_water {
            return false;
        }
        if let (Some(climate_maps), Some(palette)) = (world.climate, world.palette) {
            let (slope_step_ft, _) = sample_slope_and_elevation(maps, neighbor_tile);
            let kind = ground_cover(&GroundCoverInput {
                tile: neighbor_tile,
                sample,
                climate: sample_climate(climate_maps, neighbor_tile),
                slope_step_ft,
                shore_distance_tiles: SHORE_DISTANCE_MAX_TILES,
            });
            let variant = palette.variant_at(kind, neighbor_tile);
            return table.palette_allows_clutter(usize::from(variant));
        }
        return true;
    }

    // Fallback when no planetary maps exist (standalone unit tests): clamp to the nearest chunk
    // border tile so roads/rivers meeting a border still block intrusion while uniform wilderness
    // does not leave an artificial empty ring along chunk edges.
    let max_idx = i32::try_from(CHUNK_SIZE - 1).unwrap_or(63);
    let clamped_x = u32::try_from(tx.clamp(0, max_idx)).unwrap_or(0);
    let clamped_y = u32::try_from(ty.clamp(0, max_idx)).unwrap_or(0);
    cover.region_painted_at(clamped_x, clamped_y) == Some(false)
        && cover
            .dominant_at(clamped_x, clamped_y)
            .is_some_and(|dom| table.palette_allows_clutter(usize::from(dom)))
}

/// Checks whether any authored [`RegionPaint`] paints the center of `tile` at or above `0.5`.
fn tile_is_region_painted(regions: &[RegionPaint], tile: PlanetTile) -> bool {
    let tx = i64::from(tile.x);
    let ty = i64::from(tile.y);
    let mut uncovered = 1.0_f32;
    for paint in regions {
        if paint.face != tile.face
            || tx < paint.bounds.min_x
            || tx >= paint.bounds.max_x
            || ty < paint.bounds.min_y
            || ty >= paint.bounds.max_y
        {
            continue;
        }
        let local_x = u32::try_from(tx - paint.bounds.min_x).unwrap_or(0);
        let local_y = u32::try_from(ty - paint.bounds.min_y).unwrap_or(0);
        let pos = Vec2::new(count_to_f32(local_x) + 0.5, count_to_f32(local_y) + 0.5);
        let alpha = polygon_alpha(pos, &paint.ring, paint.edge_feather);
        if alpha <= 0.0 {
            continue;
        }
        for layer in &paint.layers {
            let (weight, _) = layer_coverage(layer, pos);
            let amount = (alpha * weight).clamp(0.0, 1.0);
            uncovered *= 1.0 - amount;
        }
        if 1.0 - uncovered >= REGION_PAINTED_THRESHOLD {
            return true;
        }
    }
    false
}

/// Local Z coordinate inside [`GroundClutterLayer`] for a caster sprite.
#[must_use]
pub fn caster_local_z(tier: ClutterTier, tile_y: u32, offset_y_tiles: f32, z_tiebreak: f32) -> f32 {
    let y_norm = normalized_sort_y(tile_y, offset_y_tiles);
    let tie = z_tiebreak.clamp(0.0, 1.0);
    tier.base_z()
        + TIER_CASTER_BASE_Z
        + y_norm * TIER_CASTER_Y_SPAN_Z
        + tie * TIER_CASTER_HASH_SPAN_Z
}

/// Local Z coordinate inside [`GroundClutterLayer`] for a companion drop shadow sprite.
#[must_use]
pub fn shadow_local_z(tier: ClutterTier, tile_y: u32, offset_y_tiles: f32, z_tiebreak: f32) -> f32 {
    let y_norm = normalized_sort_y(tile_y, offset_y_tiles);
    let tie = z_tiebreak.clamp(0.0, 1.0);
    tier.base_z()
        + TIER_SHADOW_BASE_Z
        + y_norm * TIER_SHADOW_Y_SPAN_Z
        + tie * TIER_SHADOW_HASH_SPAN_Z
}

fn normalized_sort_y(tile_y: u32, offset_y_tiles: f32) -> f32 {
    let wrapped = count_to_f32(tile_y % Y_SORT_PERIOD_TILES) + 0.5 + offset_y_tiles;
    let span = count_to_f32(Y_SORT_PERIOD_TILES) + 2.0 * Y_SORT_MARGIN_TILES;
    ((wrapped + Y_SORT_MARGIN_TILES) / span).clamp(0.0, 1.0)
}

/// Draws multi-tier ground clutter on every streamed chunk carrying [`ChunkGroundCover`].
#[derive(Clone, Copy, Debug, Default)]
pub struct GroundClutterPlugin;

impl Plugin for GroundClutterPlugin {
    fn build(&self, app: &mut App) {
        // `App::is_plugin_added`: bevy_app-0.20.1/src/app.rs:570.
        // `TextureAtlasPlugin`: bevy_image-0.20.1/src/texture_atlas.rs:16.
        if app.is_plugin_added::<AssetPlugin>() && !app.is_plugin_added::<TextureAtlasPlugin>() {
            app.add_plugins(TextureAtlasPlugin);
        }
        // `resource_exists`: bevy_ecs-0.20.1/src/schedule/condition.rs:688.
        // `not`: bevy_ecs-0.20.1/src/schedule/condition.rs:1251.
        app.add_systems(
            Update,
            (
                load_ground_clutter.run_if(not(resource_exists::<GroundClutterAssets>)),
                (
                    clear_removed_chunk_clutter,
                    queue_changed_chunks,
                    build_queued_chunk_clutter,
                )
                    .chain()
                    .run_if(resource_exists::<GroundClutterAssets>),
            )
                .chain()
                .after(PlanetFocusSync),
        );
    }
}

/// Resolved clutter table and packed runtime texture atlas (or standalone image fallback).
#[derive(Resource, Debug, Default)]
pub struct GroundClutterAssets {
    table: ClutterTable,
    /// `images[palette_index][layer_index][sprite_index]`.
    images: Vec<Vec<Vec<ClutterImage>>>,
    uses_atlas: bool,
}

#[derive(Clone, Debug)]
enum ClutterSpriteSource {
    Atlas {
        image: Handle<Image>,
        layout: Handle<TextureAtlasLayout>,
        index: usize,
    },
    Standalone(Handle<Image>),
}

#[derive(Clone, Debug)]
struct ClutterImage {
    source: ClutterSpriteSource,
    /// Image dimensions scaled so the longest side is `1.0`.
    unit_size: Vec2,
}

struct DecodedClutterTexture {
    image: Image,
    unit_size: Vec2,
}

/// Marks a streamed chunk entity whose clutter layer is queued for (re)building.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClutterQueued;

/// Tracks the spawned [`GroundClutterLayer`] child entity of a chunk.
#[derive(Component, Debug)]
struct ChunkClutterLayer(Entity);

/// Child of a streamed chunk holding its clutter and shadow sprites in 5-foot tile units.
#[derive(Component, Debug)]
pub struct GroundClutterLayer;

/// Marks 1 caster clutter sprite entity.
#[derive(Component, Debug)]
pub struct GroundClutter;

/// Marks 1 companion directional drop shadow sprite entity.
#[derive(Component, Debug)]
pub struct GroundClutterShadow;

/// Loads [`GroundClutterAssets`] once [`TerrainPalette`] and [`Assets<Image>`] exist.
fn load_ground_clutter(
    mut commands: Commands,
    palette: Option<Res<TerrainPalette>>,
    images: Option<ResMut<Assets<Image>>>,
    mut layouts: Option<ResMut<Assets<TextureAtlasLayout>>>,
) {
    let (Some(palette), Some(mut images)) = (palette, images) else {
        return;
    };
    match GroundClutterAssets::load(&palette, &mut images, layouts.as_deref_mut()) {
        Ok(assets) => commands.insert_resource(assets),
        Err(error) => {
            // `error!`: bevy_log re-export of `tracing::error`.
            error!("ground clutter is off: {error}");
            commands.insert_resource(GroundClutterAssets::default());
        }
    }
}

impl GroundClutterAssets {
    /// Returns the resolved [`ClutterTable`].
    #[must_use]
    pub const fn table(&self) -> &ClutterTable {
        &self.table
    }

    /// Returns `true` when all clutter sprites share 1 packed [`TextureAtlasLayout`] + [`Image`].
    #[must_use]
    pub const fn uses_atlas(&self) -> bool {
        self.uses_atlas
    }

    fn load(
        palette: &TerrainPalette,
        images: &mut Assets<Image>,
        layouts: Option<&mut Assets<TextureAtlasLayout>>,
    ) -> Result<Self, String> {
        let config = ClutterConfig::load_default()?;
        Self::from_config_with_max_atlas(
            &config,
            palette,
            images,
            layouts,
            UVec2::splat(ATLAS_MAX_SIZE),
        )
    }

    /// Builds [`GroundClutterAssets`] from an already-parsed [`ClutterConfig`].
    ///
    /// # Errors
    /// Returns an error if `config` fails to resolve against `palette` or any referenced PNG
    /// cannot be read or decoded.
    pub fn from_config(
        config: &ClutterConfig,
        palette: &TerrainPalette,
        images: &mut Assets<Image>,
        layouts: Option<&mut Assets<TextureAtlasLayout>>,
    ) -> Result<Self, String> {
        Self::from_config_with_max_atlas(
            config,
            palette,
            images,
            layouts,
            UVec2::splat(ATLAS_MAX_SIZE),
        )
    }

    fn from_config_with_max_atlas(
        config: &ClutterConfig,
        palette: &TerrainPalette,
        images: &mut Assets<Image>,
        layouts: Option<&mut Assets<TextureAtlasLayout>>,
        max_atlas_size: UVec2,
    ) -> Result<Self, String> {
        let flags: Vec<PaletteClutterFlag<'_>> = palette
            .entries()
            .iter()
            .map(|entry| PaletteClutterFlag {
                id: &entry.id,
                clutter: entry.clutter,
            })
            .collect();
        let table = config.resolve(&flags)?;
        let texture_paths = config.textures();
        let mut decoded = Vec::with_capacity(texture_paths.len());
        for texture in &texture_paths {
            decoded.push(((*texture).to_owned(), decode_clutter_image(texture)?));
        }

        let (loaded, uses_atlas) =
            pack_or_upload_textures(decoded, images, layouts, max_atlas_size);
        let mapped = (0..flags.len())
            .map(|palette_index| {
                table
                    .layers(palette_index)
                    .iter()
                    .map(|layer| {
                        layer
                            .sprites
                            .iter()
                            .filter_map(|sprite| loaded.get(&sprite.texture).cloned())
                            .collect()
                    })
                    .collect()
            })
            .collect();

        Ok(Self {
            table,
            images: mapped,
            uses_atlas,
        })
    }

    /// Builds the caster `(GroundClutter, Sprite, Transform)` bundle for `sprite` in tile units
    /// (`(0, 0)` at chunk center, `+x` east, `+y` screen north).
    #[must_use]
    pub fn caster_bundle(
        &self,
        sprite: &ChunkClutterSprite,
    ) -> Option<(GroundClutter, Sprite, Transform)> {
        let placement = sprite.placement;
        let image = self
            .images
            .get(usize::from(sprite.palette_index))?
            .get(placement.layer)?
            .get(placement.sprite)?;
        let (pos_x, pos_y) = chunk_local_xy(sprite.local_x, sprite.local_y, placement.offset_tiles);
        let pos_z = caster_local_z(
            placement.tier,
            sprite.tile_y,
            placement.offset_tiles.y,
            placement.z_tiebreak,
        );
        let mut drawn = image.make_sprite();
        // `Sprite::custom_size`: bevy_sprite-0.20.1/src/sprite.rs:31.
        // `Sprite::flip_x`: bevy_sprite-0.20.1/src/sprite.rs:26.
        // `Sprite::flip_y`: bevy_sprite-0.20.1/src/sprite.rs:28.
        // `Sprite::color`: bevy_sprite-0.20.1/src/sprite.rs:24.
        // `Color::srgba`: bevy_color-0.20.1/src/color.rs:107.
        drawn.custom_size = Some(image.unit_size * placement.size_tiles);
        drawn.flip_x = placement.flip_x;
        drawn.flip_y = placement.flip_y;
        let [red, green, blue] = placement.tint_rgb;
        drawn.color = Color::srgba(red, green, blue, 1.0);
        // `Transform::from_xyz`: bevy_transform-0.20.1/src/components/transform.rs:119.
        // `Transform::with_rotation`: bevy_transform-0.20.1/src/components/transform.rs:250.
        // `Quat::from_rotation_z`: glam-0.33.12/src/f32/coresimd/quat.rs:182.
        let transform = Transform::from_xyz(pos_x, pos_y, pos_z)
            .with_rotation(Quat::from_rotation_z(placement.rotation_radians));
        Some((GroundClutter, drawn, transform))
    }

    /// Builds the companion `(GroundClutterShadow, Sprite, Transform)` bundle when
    /// `placement.shadow_alpha > 0.0`.
    ///
    /// Shadow translation offset `(+east, -south)` is applied directly in chunk world axes, never
    /// rotated by `placement.rotation_radians`, so every shadow across the map falls in 1
    /// consistent sunlight direction while preserving the caster's silhouette orientation.
    #[must_use]
    pub fn shadow_bundle(
        &self,
        sprite: &ChunkClutterSprite,
    ) -> Option<(GroundClutterShadow, Sprite, Transform)> {
        let placement = sprite.placement;
        if placement.shadow_alpha <= 0.0 {
            return None;
        }
        let image = self
            .images
            .get(usize::from(sprite.palette_index))?
            .get(placement.layer)?
            .get(placement.sprite)?;
        let (caster_x, caster_y) =
            chunk_local_xy(sprite.local_x, sprite.local_y, placement.offset_tiles);
        let shadow_x = caster_x + placement.shadow_offset_tiles.x;
        let shadow_y = caster_y - placement.shadow_offset_tiles.y;
        let shadow_z = shadow_local_z(
            placement.tier,
            sprite.tile_y,
            placement.offset_tiles.y,
            placement.z_tiebreak,
        );
        let mut drawn = image.make_sprite();
        drawn.custom_size = Some(image.unit_size * (placement.size_tiles * placement.shadow_scale));
        drawn.flip_x = placement.flip_x;
        drawn.flip_y = placement.flip_y;
        drawn.color = Color::srgba(
            SHADOW_TINT_RGB[0],
            SHADOW_TINT_RGB[1],
            SHADOW_TINT_RGB[2],
            placement.shadow_alpha,
        );
        let transform = Transform::from_xyz(shadow_x, shadow_y, shadow_z)
            .with_rotation(Quat::from_rotation_z(placement.rotation_radians));
        Some((GroundClutterShadow, drawn, transform))
    }
}

impl ClutterImage {
    fn make_sprite(&self) -> Sprite {
        match &self.source {
            // `Sprite::from_atlas_image`: bevy_sprite-0.20.1/src/sprite.rs:64.
            // `TextureAtlas`: bevy_image-0.20.1/src/texture_atlas.rs:214.
            ClutterSpriteSource::Atlas {
                image,
                layout,
                index,
            } => Sprite::from_atlas_image(
                image.clone(),
                TextureAtlas {
                    layout: layout.clone(),
                    index: *index,
                },
            ),
            // `Sprite::from_image`: bevy_sprite-0.20.1/src/sprite.rs:56.
            ClutterSpriteSource::Standalone(handle) => Sprite::from_image(handle.clone()),
        }
    }
}

fn pack_or_upload_textures(
    decoded: Vec<(String, DecodedClutterTexture)>,
    images: &mut Assets<Image>,
    layouts: Option<&mut Assets<TextureAtlasLayout>>,
    max_atlas_size: UVec2,
) -> (BTreeMap<String, ClutterImage>, bool) {
    if !decoded.is_empty()
        && let Some(layouts) = layouts
    {
        // `TextureAtlasBuilder`: bevy_image-0.20.1/src/texture_atlas_builder.rs:39.
        // `initial_size`: :72, `max_size`: :78, `add_texture`: :99, `padding`: :111, `build`: :204.
        let mut builder = TextureAtlasBuilder::default();
        let initial = UVec2::splat(ATLAS_INITIAL_SIZE).min(max_atlas_size);
        builder
            .initial_size(initial)
            .max_size(max_atlas_size)
            .padding(UVec2::splat(ATLAS_PADDING_PIXELS));
        for (_, entry) in &decoded {
            builder.add_texture(None, &entry.image);
        }
        match builder.build() {
            Ok((atlas_layout, _sources, atlas_image)) => {
                // `Assets::add`: bevy_asset-0.20.1/src/assets.rs:401.
                let image_handle = images.add(atlas_image);
                let layout_handle = layouts.add(atlas_layout);
                let mut map = BTreeMap::new();
                for (index, (path, entry)) in decoded.into_iter().enumerate() {
                    map.insert(
                        path,
                        ClutterImage {
                            source: ClutterSpriteSource::Atlas {
                                image: image_handle.clone(),
                                layout: layout_handle.clone(),
                                index,
                            },
                            unit_size: entry.unit_size,
                        },
                    );
                }
                return (map, true);
            }
            Err(error) => {
                warn!("clutter atlas packing exceeded bounds ({error}); using standalone textures");
            }
        }
    }

    let mut map = BTreeMap::new();
    for (path, entry) in decoded {
        map.insert(
            path,
            ClutterImage {
                source: ClutterSpriteSource::Standalone(images.add(entry.image)),
                unit_size: entry.unit_size,
            },
        );
    }
    (map, false)
}

fn decode_clutter_image(texture: &str) -> Result<DecodedClutterTexture, String> {
    let path = crate::terrain::assets_root().join(texture);
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    // `Image::from_buffer`: bevy_image-0.20.1/src/image.rs:1589.
    // `ImageType::Format`: bevy_image-0.20.1/src/image.rs:2343.
    // `ImageFormat::Png`: bevy_image-0.20.1/src/image.rs:297.
    // `CompressedImageFormats::NONE`: bevy_image-0.20.1/src/image.rs:2392.
    // `ImageSampler::Default`: bevy_image-0.20.1/src/image.rs:700.
    // `RenderAssetUsages::default`: bevy_asset-0.20.1/src/render_asset.rs:40.
    let image = Image::from_buffer(
        &bytes,
        ImageType::Format(ImageFormat::Png),
        CompressedImageFormats::NONE,
        true,
        ImageSampler::Default,
        RenderAssetUsages::default(),
    )
    .map_err(|error| format!("could not decode {}: {error}", path.display()))?;
    // `Image::size`: bevy_image-0.20.1/src/image.rs:1330.
    let size = image.size().as_vec2();
    let longest = size.max_element();
    if longest <= 0.0 {
        return Err(format!("{} is empty", path.display()));
    }
    Ok(DecodedClutterTexture {
        image,
        unit_size: size / longest,
    })
}

/// Drops the clutter of chunks that lost their ground cover (zoomed out to the overview).
fn clear_removed_chunk_clutter(
    mut commands: Commands,
    // `RemovedComponents::read`: bevy_ecs-0.20.1/src/lifecycle.rs:651.
    mut removed: RemovedComponents<ChunkGroundCover>,
    layers: Query<&ChunkClutterLayer>,
) {
    for chunk in removed.read() {
        let Ok(layer) = layers.get(chunk) else {
            continue;
        };
        // `Commands::get_entity`: bevy_ecs-0.20.1/src/system/commands/mod.rs:360.
        // `EntityCommands::despawn` (children follow, `linked_spawn`): commands/mod.rs:1882,
        // bevy_ecs-0.20.1/src/hierarchy.rs:148.
        if let Ok(mut layer) = commands.get_entity(layer.0) {
            layer.despawn();
        }
        if let Ok(mut chunk) = commands.get_entity(chunk) {
            // `EntityCommands::try_remove`: bevy_ecs-0.20.1/src/system/commands/mod.rs:1803.
            chunk.try_remove::<(ChunkClutterLayer, ClutterQueued)>();
        }
    }
}

/// Queues every chunk whose ground cover was added or rebaked.
fn queue_changed_chunks(
    mut commands: Commands,
    // `Changed`: bevy_ecs-0.20.1/src/query/filter.rs:996.
    changed: Query<Entity, Changed<ChunkGroundCover>>,
) {
    for chunk in &changed {
        // `EntityCommands::try_insert`: bevy_ecs-0.20.1/src/system/commands/mod.rs:1602.
        commands.entity(chunk).try_insert(ClutterQueued);
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn build_queued_chunk_clutter(
    mut commands: Commands,
    assets: Res<GroundClutterAssets>,
    world: Option<Res<PlanetWorld>>,
    paint: Option<Res<ChunkRegionPaint>>,
    palette: Option<Res<TerrainPalette>>,
    sync_mode: Option<Res<PlanetStreamSyncMode>>,
    queued: Query<(Entity, &ChunkGroundCover, Option<&ChunkClutterLayer>), With<ClutterQueued>>,
) {
    if queued.is_empty() {
        return;
    }
    let climate = world.as_deref().and_then(PlanetWorld::climate);
    if climate.is_none() {
        // `warn_once!`: bevy_log-0.20.1/src/once.rs:35.
        warn_once!("no planet climate maps: plant clutter draws at its bare-ground floor");
    }
    let context = ClutterWorldContext {
        maps: world.as_deref().map(PlanetWorld::maps),
        climate,
        regions: paint.as_deref().map_or(&[], |p| p.regions.as_slice()),
        palette: palette.as_deref(),
    };
    let budget = match sync_mode.as_deref() {
        Some(PlanetStreamSyncMode::Blocking) => usize::MAX,
        _ => MAX_CLUTTER_CHUNKS_PER_FRAME,
    };

    let tiles_to_chunk = 0.5 / half_chunk_tiles();
    for (chunk, cover, old_layer) in queued.iter().take(budget) {
        if let Some(old_layer) = old_layer
            && let Ok(mut old_layer) = commands.get_entity(old_layer.0)
        {
            old_layer.despawn();
        }
        let sprites = chunk_clutter(&assets.table, cover, context);
        // The splat chunk is a unit mesh scaled to `chunk_world_size` (`planet_stream.rs`
        // `spawn_splat_chunk`), so scaling by `1 / CHUNK_SIZE` places children directly in
        // 5-ft tile units while inheriting zoom and cube-face view rotation.
        // `Transform::with_scale`: bevy_transform-0.20.1/src/components/transform.rs:258.
        // `Visibility`: bevy_camera-0.20.1/src/visibility/mod.rs:83.
        // `ChildOf`: bevy_ecs-0.20.1/src/hierarchy.rs:107.
        // `EntityCommands::with_children`: bevy_ecs-0.20.1/src/hierarchy.rs:366.
        let layer = commands
            .spawn((
                GroundClutterLayer,
                Transform::from_xyz(0.0, 0.0, CLUTTER_LAYER_Z).with_scale(Vec3::new(
                    tiles_to_chunk,
                    tiles_to_chunk,
                    1.0,
                )),
                Visibility::default(),
                ChildOf(chunk),
            ))
            .with_children(|layer| {
                for sprite in &sprites {
                    if let Some(shadow) = assets.shadow_bundle(sprite) {
                        layer.spawn(shadow);
                    }
                    if let Some(caster) = assets.caster_bundle(sprite) {
                        layer.spawn(caster);
                    }
                }
            })
            .id();
        commands
            .entity(chunk)
            .try_remove::<ClutterQueued>()
            .try_insert(ChunkClutterLayer(layer));
    }
}

/// Converts `(local_x, local_y)` + `offset_tiles` into [`GroundClutterLayer`] coordinates
/// (`(0, 0)` at chunk center, `+x` east, `+y` screen north).
fn chunk_local_xy(local_x: u32, local_y: u32, offset_tiles: Vec2) -> (f32, f32) {
    let half_chunk = half_chunk_tiles();
    let x = local_tile_center(local_x) + offset_tiles.x - half_chunk;
    let y = half_chunk - (local_tile_center(local_y) + offset_tiles.y);
    (x, y)
}

/// Center of local tile `index` in tiles from the chunk's top-left corner.
fn local_tile_center(index: u32) -> f32 {
    f32::from(u16::try_from(index).unwrap_or(u16::MAX)) + 0.5
}

/// Half a chunk side in tiles (`32.0`).
fn half_chunk_tiles() -> f32 {
    local_tile_center(CHUNK_SIZE / 2) - 0.5
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped into [0, max_inclusive] before casting to u32"
)]
fn bounded_floor_u32(value: f32, max_inclusive: u32) -> u32 {
    (value.max(0.0).floor() as u32).min(max_inclusive)
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "tile clearance offsets lie in [-72, +72], well inside i32"
)]
fn floor_to_i32(value: f32) -> i32 {
    value.floor() as i32
}

#[allow(
    clippy::cast_precision_loss,
    reason = "small signed local chunk tile indices [-16, 80] are exact in f32"
)]
fn i32_to_f32(value: i32) -> f32 {
    value as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planet_region::RegionBounds;
    use crate::planet_stream::CHUNK_Z_DEPTH;
    use crate::terrain::{MapConfig, resolve_layers};
    use zeta_core::world::{
        ChunkCoord, CubeFace, FACE_CHUNKS, FaceClimateMap, FaceHeightmap, SEA_LEVEL_U16,
        temperature_to_byte, unit_to_byte,
    };

    const TEST_TABLE: &str = r#"{
        "palette": {
            "meadow": {
                "density": 1.0,
                "follows_foliage": true,
                "sprites": [
                    { "texture": "a.png", "weight": 3, "min_size_tiles": 0.3, "max_size_tiles": 0.6 },
                    { "texture": "b.png", "weight": 1, "min_size_tiles": 0.2, "max_size_tiles": 0.2 }
                ]
            },
            "gravel": {
                "density": 0.5,
                "follows_foliage": false,
                "sprites": [
                    { "texture": "c.png", "weight": 1, "min_size_tiles": 0.2, "max_size_tiles": 0.4 }
                ]
            }
        }
    }"#;

    const PALETTE: [PaletteClutterFlag<'static>; 4] = [
        PaletteClutterFlag {
            id: "meadow",
            clutter: true,
        },
        PaletteClutterFlag {
            id: "road",
            clutter: false,
        },
        PaletteClutterFlag {
            id: "gravel",
            clutter: true,
        },
        PaletteClutterFlag {
            id: "snow",
            clutter: true,
        },
    ];

    fn test_table() -> ClutterTable {
        ClutterConfig::from_json(TEST_TABLE)
            .and_then(|config| config.resolve(&PALETTE))
            .expect("test table is valid")
    }

    fn input(palette_index: usize, hash: u64) -> ClutterTileInput {
        ClutterTileInput::basic(palette_index, 1.0, hash)
    }

    fn uniform_cover(coord: ChunkCoord, palette_index: u8) -> ChunkGroundCover {
        ChunkGroundCover {
            coord,
            dominant: Box::new([palette_index; CHUNK_TILE_COUNT]),
            region_painted: Box::new([false; CHUNK_TILE_COUNT]),
        }
    }

    fn uniform_climate(temperature_c: f32, moisture: f32, foliage: f32) -> PlanetClimateMaps {
        let texel = [
            temperature_to_byte(temperature_c),
            unit_to_byte(moisture),
            unit_to_byte(foliage),
            0,
        ];
        let faces = CubeFace::ALL
            .map(|_| FaceClimateMap::new(8, vec![texel; 64]).expect("valid climate face"));
        PlanetClimateMaps::new(faces).expect("valid climate planet")
    }

    #[test]
    fn placement_is_identical_for_the_same_tile_input() {
        let table = test_table();
        for hash in 0..2_000_u64 {
            let first = table.place(input(0, hash.wrapping_mul(0x9e37_79b9_7f4a_7c15)));
            let second = table.place(input(0, hash.wrapping_mul(0x9e37_79b9_7f4a_7c15)));
            assert_eq!(first, second);
        }
    }

    #[test]
    fn region_painted_tiles_never_carry_clutter() {
        let table = test_table();
        for hash in 0..2_000_u64 {
            for palette_index in 0..PALETTE.len() {
                let painted = ClutterTileInput {
                    region_painted: true,
                    ..input(palette_index, hash)
                };
                assert_eq!(table.place(painted), None);
            }
        }
    }

    #[test]
    fn covers_without_clutter_or_without_a_rule_never_carry_clutter() {
        let table = test_table();
        for hash in 0..2_000_u64 {
            assert_eq!(table.place(input(1, hash)), None);
            assert_eq!(table.place(input(3, hash)), None);
            assert_eq!(table.place(input(99, hash)), None);
        }
    }

    #[test]
    fn spawn_rate_follows_density_and_foliage() {
        let table = test_table();
        let count = |palette_index: usize, foliage: f32| {
            (0..20_000_u64)
                .filter(|&hash| {
                    table
                        .place(ClutterTileInput {
                            foliage,
                            ..input(palette_index, hash.wrapping_mul(0x9e37_79b9_7f4a_7c15))
                        })
                        .is_some()
                })
                .count()
        };
        assert_eq!(
            count(0, 1.0),
            20_000,
            "density 1 at full foliage fills every tile"
        );
        assert_eq!(count(0, 0.0), 0, "no foliage means no plants");
        let half = count(0, 0.5);
        assert!((9_000..11_000).contains(&half), "half foliage gave {half}");
        let gravel_bare = count(2, 0.0);
        assert!(
            (9_000..11_000).contains(&gravel_bare),
            "pebbles ignore foliage, gave {gravel_bare}"
        );
    }

    #[test]
    fn foliage_floor_keeps_a_share_of_plants_on_bare_ground() {
        let table = ClutterConfig::from_json(
            r#"{"palette":{"meadow":{"density":1.0,"follows_foliage":true,"foliage_floor":0.5,"sprites":[{"texture":"a","weight":1,"min_size_tiles":0.2,"max_size_tiles":0.3}]}}}"#,
        )
        .and_then(|config| config.resolve(&PALETTE))
        .expect("floor table is valid");
        let count = |foliage: f32| {
            (0..20_000_u64)
                .filter(|&hash| {
                    table
                        .place(ClutterTileInput {
                            foliage,
                            ..input(0, hash.wrapping_mul(0x9e37_79b9_7f4a_7c15))
                        })
                        .is_some()
                })
                .count()
        };
        let bare = count(0.0);
        assert!(
            (9_000..11_000).contains(&bare),
            "floor 0.5 on bare ground gave {bare}"
        );
        assert_eq!(count(1.0), 20_000, "full foliage reaches the full density");
    }

    #[test]
    fn sprites_stay_inside_their_tile_within_their_size_range() {
        let table = test_table();
        let mut picks = [0_usize; 2];
        for hash in 0..20_000_u64 {
            let placement = table.place(input(0, hash)).expect("density 1 places");
            let sprite = &table.rule(0).expect("meadow rule").sprites[placement.sprite];
            assert!(placement.size_tiles >= sprite.min_size_tiles);
            assert!(placement.size_tiles <= sprite.max_size_tiles);
            let reach = placement.size_tiles * 0.5;
            assert!(placement.offset_tiles.x.abs() + reach <= 0.5 + f32::EPSILON);
            assert!(placement.offset_tiles.y.abs() + reach <= 0.5 + f32::EPSILON);
            picks[placement.sprite] += 1;
        }
        assert!(
            (14_000..16_000).contains(&picks[0]),
            "weighted picks {picks:?}"
        );
    }

    #[test]
    fn resolve_rejects_unknown_ids_and_clutter_on_covers_that_forbid_it() {
        let config = ClutterConfig::from_json(TEST_TABLE).expect("valid");
        let missing = [PaletteClutterFlag {
            id: "meadow",
            clutter: true,
        }];
        assert!(config.resolve(&missing).is_err());
        let forbidden = [
            PaletteClutterFlag {
                id: "meadow",
                clutter: false,
            },
            PaletteClutterFlag {
                id: "gravel",
                clutter: true,
            },
        ];
        assert!(config.resolve(&forbidden).is_err());
    }

    #[test]
    fn legacy_and_multi_tier_json_configs_parse_and_produce_expected_layers() {
        let multi_tier_raw = r#"{
            "palette": {
                "meadow": {
                    "layers": [
                        {
                            "tier": "decal",
                            "density": 0.45,
                            "follows_foliage": true,
                            "foliage_floor": 0.5,
                            "sprites": [
                                { "texture": "moss.png", "weight": 2, "min_size_tiles": 0.4, "max_size_tiles": 0.9 }
                            ]
                        },
                        {
                            "tier": "understory",
                            "density": 0.35,
                            "cell_step_tiles": 2,
                            "cluster_strength": 0.7,
                            "cluster_scale_tiles": 20.0,
                            "cluster_seed_offset": 7,
                            "tint_variation": 0.12,
                            "sprites": [
                                { "texture": "fern.png", "weight": 1, "min_size_tiles": 1.1, "max_size_tiles": 2.1, "rotatable": true }
                            ]
                        },
                        {
                            "tier": "prop",
                            "density": 0.25,
                            "cell_step_tiles": 3,
                            "rotatable": true,
                            "shadow_alpha": 0.45,
                            "sprites": [
                                { "texture": "log.png", "weight": 1, "min_size_tiles": 1.8, "max_size_tiles": 3.6 }
                            ]
                        },
                        {
                            "tier": "canopy",
                            "density": 0.6,
                            "cell_step_tiles": 4,
                            "rotatable": true,
                            "allow_flip_y": true,
                            "tint_variation": 0.15,
                            "shadow_alpha": 0.55,
                            "shadow_offset_tiles": [0.22, 0.28],
                            "shadow_scale": 1.06,
                            "sprites": [
                                { "texture": "oak.png", "weight": 1, "min_size_tiles": 3.2, "max_size_tiles": 5.5 }
                            ]
                        }
                    ]
                }
            }
        }"#;

        let config = ClutterConfig::from_json(multi_tier_raw).expect("multi-tier table is valid");
        assert_eq!(
            config.textures(),
            vec!["fern.png", "log.png", "moss.png", "oak.png"]
        );
        let table = config.resolve(&PALETTE).expect("resolves against palette");
        let layers = table.layers(0);
        assert_eq!(layers.len(), 4);
        assert_eq!(layers[0].tier, ClutterTier::Decal);
        assert_eq!(layers[1].tier, ClutterTier::Understory);
        assert_eq!(layers[2].tier, ClutterTier::Prop);
        assert_eq!(layers[3].tier, ClutterTier::Canopy);
    }

    #[test]
    fn from_json_rejects_invalid_rules() {
        let bad = [
            r#"{"palette":{"x":{"density":1.5,"follows_foliage":false,"sprites":[{"texture":"a","weight":1,"min_size_tiles":0.2,"max_size_tiles":0.3}]}}}"#,
            r#"{"palette":{"x":{"density":0.5,"follows_foliage":true,"foliage_floor":1.5,"sprites":[{"texture":"a","weight":1,"min_size_tiles":0.2,"max_size_tiles":0.3}]}}}"#,
            r#"{"palette":{"x":{"density":0.5,"follows_foliage":false,"sprites":[]}}}"#,
            r#"{"palette":{"x":{"density":0.5,"follows_foliage":false,"sprites":[{"texture":"a","weight":0,"min_size_tiles":0.2,"max_size_tiles":0.3}]}}}"#,
            r#"{"palette":{"x":{"density":0.5,"follows_foliage":false,"sprites":[{"texture":"a","weight":1,"min_size_tiles":0.5,"max_size_tiles":0.3}]}}}"#,
            r#"{"palette":{"x":{"density":0.5,"follows_foliage":false,"sprites":[{"texture":"a","weight":1,"min_size_tiles":0.5,"max_size_tiles":8.5}]}}}"#,
            r#"{"palette":{"x":{"layers":[{"density":0.5,"cell_step_tiles":0,"sprites":[{"texture":"a","weight":1,"min_size_tiles":0.5,"max_size_tiles":1.0}]}]}}}"#,
            r#"{"palette":{"x":{"layers":[{"density":0.5,"cell_step_tiles":17,"sprites":[{"texture":"a","weight":1,"min_size_tiles":0.5,"max_size_tiles":1.0}]}]}}}"#,
            r#"{"palette":{"x":{"layers":[{"density":0.5,"min_temperature":20.0,"max_temperature":5.0,"sprites":[{"texture":"a","weight":1,"min_size_tiles":0.5,"max_size_tiles":1.0}]}]}}}"#,
            r#"{"palette":{"x":{"layers":[{"density":0.5,"min_slope_step_ft":15,"max_slope_step_ft":5,"sprites":[{"texture":"a","weight":1,"min_size_tiles":0.5,"max_size_tiles":1.0}]}]}}}"#,
            r#"{"palette":{"x":{"layers":[{"density":0.5,"tint_variation":0.4,"sprites":[{"texture":"a","weight":1,"min_size_tiles":0.5,"max_size_tiles":1.0}]}]}}}"#,
            r#"{"palette":{"x":{"layers":[{"density":0.5,"shadow_alpha":0.95,"sprites":[{"texture":"a","weight":1,"min_size_tiles":0.5,"max_size_tiles":1.0}]}]}}}"#,
        ];
        for raw in bad {
            assert!(ClutterConfig::from_json(raw).is_err(), "accepted {raw}");
        }
    }

    #[test]
    fn multi_tier_z_order_stays_strictly_between_chunk_ground_and_gameplay_tokens() {
        // `map.rs` constants: `SQUARE_DEPTH = 0.0`, `river_crossing.json` bridge `0.5`,
        // `ENTITY_DEPTH = 1.0` (tokens, creatures, dropped items), `LABEL_DEPTH = 2.0`.
        const MAP_SQUARE_Z: f32 = 0.0;
        const BRIDGE_ENTITY_Z: f32 = 0.5;
        const TOKEN_ENTITY_Z: f32 = 1.0;

        for (pair_idx, window) in ClutterTier::ALL.windows(2).enumerate() {
            let (lower, upper) = (window[0], window[1]);
            let lower_max_caster = caster_local_z(lower, Y_SORT_PERIOD_TILES - 1, 8.0, 1.0);
            let lower_min_caster = caster_local_z(lower, 0, -8.0, 0.0);
            let lower_max_shadow = shadow_local_z(lower, Y_SORT_PERIOD_TILES - 1, 8.0, 1.0);
            assert!(
                lower_max_shadow < lower_min_caster,
                "tier {pair_idx} shadow ({lower_max_shadow}) must stay below caster ({lower_min_caster})"
            );

            let upper_min_shadow = shadow_local_z(upper, 0, -8.0, 0.0);
            assert!(
                lower_max_caster < upper_min_shadow,
                "tier {lower:?} max ({lower_max_caster}) must stay below {upper:?} shadow ({upper_min_shadow})"
            );
        }

        let lowest_local = CLUTTER_LAYER_Z + shadow_local_z(ClutterTier::Decal, 0, -8.0, 0.0);
        let highest_local = CLUTTER_LAYER_Z
            + caster_local_z(ClutterTier::Canopy, Y_SORT_PERIOD_TILES - 1, 8.0, 1.0);
        assert!(
            lowest_local > 0.0,
            "must sit strictly above chunk mesh local z=0"
        );
        assert!(
            highest_local < TOKEN_ENTITY_Z,
            "even chunk-local z ({highest_local}) must stay below token z ({TOKEN_ENTITY_Z})"
        );

        let lowest_world = CHUNK_Z_DEPTH + lowest_local;
        let highest_world = CHUNK_Z_DEPTH + highest_local;
        assert!(lowest_world > CHUNK_Z_DEPTH);
        assert!(highest_world < MAP_SQUARE_Z);
        assert!(highest_world < BRIDGE_ENTITY_Z);
        assert!(highest_world < TOKEN_ENTITY_Z);

        // Cross-chunk Y-sort: row 63 of chunk cy=10 (`tile_y = 703`) sorts below row 0 of chunk
        // cy=11 (`tile_y = 704`).
        let north_border = caster_local_z(ClutterTier::Canopy, 703, 0.0, 0.99);
        let south_border = caster_local_z(ClutterTier::Canopy, 704, 0.0, 0.0);
        assert!(
            south_border > north_border,
            "southern tree across chunk seam ({south_border}) must draw over northern tree ({north_border})"
        );
    }

    #[test]
    fn canonical_cell_step_enforces_minimum_spacing_and_never_duplicates_at_chunk_borders() {
        for (step, min_dist) in [(3_u32, 0.85_f32), (4_u32, 1.15_f32)] {
            let raw = format!(
                r#"{{
                    "palette": {{
                        "meadow": {{
                            "layers": [{{
                                "tier": "canopy",
                                "density": 1.0,
                                "cell_step_tiles": {step},
                                "sprites": [{{
                                    "texture": "tree.png",
                                    "weight": 1,
                                    "min_size_tiles": 3.2,
                                    "max_size_tiles": 4.4
                                }}]
                            }}]
                        }}
                    }}
                }}"#
            );
            let table = ClutterConfig::from_json(&raw)
                .and_then(|c| c.resolve(&PALETTE))
                .expect("valid spaced table");

            let start_col = 100_u32;
            let start_row = 200_u32;
            let mut global_centers = Vec::new();
            let mut overhanging_border_count = 0_usize;

            for dcy in 0..2_u32 {
                for dcx in 0..2_u32 {
                    let coord = ChunkCoord::new(CubeFace::PosX, start_col + dcx, start_row + dcy)
                        .expect("ok");
                    let cover = uniform_cover(coord, 0);
                    let sprites = chunk_clutter(&table, &cover, ClutterWorldContext::default());
                    let origin = coord.origin_tile();
                    for s in sprites {
                        let center_col = count_to_f32(s.local_x) + 0.5 + s.placement.offset_tiles.x;
                        let center_row = count_to_f32(s.local_y) + 0.5 + s.placement.offset_tiles.y;
                        if center_col + s.placement.size_tiles * 0.5 > count_to_f32(CHUNK_SIZE) {
                            overhanging_border_count += 1;
                        }
                        let gx = f64::from(origin.x) + f64::from(center_col);
                        let gy = f64::from(origin.y) + f64::from(center_row);
                        global_centers.push((gx, gy));
                    }
                }
            }

            assert!(
                overhanging_border_count > 0,
                "step {step} trees near chunk borders must overhang seamlessly"
            );
            assert!(
                global_centers.len() > 400,
                "expected dense cell coverage across 2x2 chunks, got {}",
                global_centers.len()
            );

            for i in 0..global_centers.len() {
                for j in (i + 1)..global_centers.len() {
                    let dx = global_centers[i].0 - global_centers[j].0;
                    let dy = global_centers[i].1 - global_centers[j].1;
                    let dist = (dx * dx + dy * dy).sqrt();
                    assert!(
                        dist >= f64::from(min_dist),
                        "step {step} placed 2 sprites too close ({dist:.3} tiles < {min_dist}) at {:?} and {:?}",
                        global_centers[i],
                        global_centers[j]
                    );
                }
            }
        }
    }

    fn east_border_road_paint(coord: ChunkCoord) -> RegionPaint {
        let palette = TerrainPalette::load().expect("shipped palette");
        let map_cfg: MapConfig = serde_json::from_str(&format!(
            r#"{{"materials":[{{"id":"p","palette":"{}"}}],
                 "layers":[{{"material":"p","shape":{{"kind":"rect","min":[0,0],"max":[4,64]}}}}]}}"#,
            palette.entries()[0].id
        ))
        .expect("valid region config");
        let origin = coord.origin_tile();
        RegionPaint {
            face: coord.face,
            bounds: RegionBounds {
                min_x: i64::from(origin.x) + i64::from(CHUNK_SIZE),
                min_y: i64::from(origin.y),
                max_x: i64::from(origin.x) + i64::from(CHUNK_SIZE) + 4,
                max_y: i64::from(origin.y) + i64::from(CHUNK_SIZE),
            },
            ring: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(4.0, 0.0),
                Vec2::new(4.0, 64.0),
                Vec2::new(0.0, 64.0),
            ],
            edge_feather: 0.0,
            layers: resolve_layers(&map_cfg, &palette, &[]).expect("resolves"),
        }
    }

    fn ocean_north_planet(target_face: CubeFace) -> PlanetHeightmaps {
        let res = 64_u32;
        let faces = CubeFace::ALL.map(|face| {
            let mut samples = Vec::with_capacity((res * res) as usize);
            for py in 0..res {
                for _px in 0..res {
                    let elev = if face == target_face && py < res / 2 {
                        SEA_LEVEL_U16 - 500
                    } else {
                        SEA_LEVEL_U16 + 200
                    };
                    samples.push(elev);
                }
            }
            FaceHeightmap::new(res, samples).expect("valid heightmap")
        });
        PlanetHeightmaps::new(faces).expect("valid planet")
    }

    #[test]
    fn footprint_clearance_keeps_large_props_off_roads_water_and_cross_chunk_hazards() {
        let raw = r#"{
            "palette": {
                "meadow": {
                    "layers": [
                        {
                            "tier": "decal",
                            "density": 1.0,
                            "sprites": [{ "texture": "tuft.png", "weight": 1, "min_size_tiles": 0.4, "max_size_tiles": 0.6 }]
                        },
                        {
                            "tier": "canopy",
                            "density": 1.0,
                            "cell_step_tiles": 2,
                            "sprites": [{ "texture": "oak.png", "weight": 1, "min_size_tiles": 4.0, "max_size_tiles": 4.0 }]
                        }
                    ]
                }
            }
        }"#;
        let table = ClutterConfig::from_json(raw)
            .and_then(|c| c.resolve(&PALETTE))
            .expect("valid clearance table");

        let coord = ChunkCoord::new(CubeFace::PosZ, FACE_CHUNKS / 2, FACE_CHUNKS / 2).expect("ok");
        let mut cover = uniform_cover(coord, 0);
        // Column 20..=22 is a region-painted road; row 40..=42 is a `clutter: false` channel.
        for y in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                let idx = ChunkGroundCover::index(x, y).expect("in chunk");
                if (20..=22).contains(&x) {
                    cover.region_painted[idx] = true;
                }
                if (40..=42).contains(&y) {
                    cover.dominant[idx] = 1;
                }
            }
        }

        let border_road = east_border_road_paint(coord);
        let maps = ocean_north_planet(coord.face);
        let world = ClutterWorldContext {
            maps: Some(&maps),
            climate: None,
            regions: std::slice::from_ref(&border_road),
            palette: None,
        };

        let sprites = chunk_clutter(&table, &cover, world);
        let mut adjacent_tufts = 0_usize;
        let mut canopy_count = 0_usize;

        for s in &sprites {
            let cx = count_to_f32(s.local_x) + 0.5 + s.placement.offset_tiles.x;
            let cy = count_to_f32(s.local_y) + 0.5 + s.placement.offset_tiles.y;
            if s.placement.tier == ClutterTier::Decal && s.local_x == 19 && s.local_y < 38 {
                adjacent_tufts += 1;
            }
            if s.placement.tier == ClutterTier::Canopy {
                canopy_count += 1;
                let radius = s.placement.clearance_radius_tiles;
                assert!((radius - 2.0).abs() < f32::EPSILON);
                // Must stay >= 2.0 tiles outside road `x in [20.0, 23.0]` and channel `y in [40.0, 43.0]`.
                assert!(
                    cx <= 20.0 - radius + 1e-4 || cx >= 23.0 + radius - 1e-4,
                    "canopy center x={cx} intruded onto road [20, 23]"
                );
                assert!(
                    cy <= 40.0 - radius + 1e-4 || cy >= 43.0 + radius - 1e-4,
                    "canopy center y={cy} intruded onto channel [40, 43]"
                );
                // Must stay >= 2.0 tiles west of the cross-chunk road at `x = 64.0`.
                assert!(
                    cx <= 64.0 - radius + 1e-4,
                    "canopy center x={cx} intruded onto cross-chunk road at x=64"
                );
                // Must stay >= 2.0 tiles south of the cross-chunk ocean north of `y = 0.0`.
                assert!(
                    cy >= radius - 1e-4,
                    "canopy center y={cy} intruded onto cross-chunk ocean north of y=0"
                );
            }
        }
        assert!(
            adjacent_tufts > 20,
            "small decals must still frame road edges right up to x=19"
        );
        assert!(canopy_count > 50, "canopies must populate clear interior");
    }

    #[test]
    fn climate_and_slope_filters_partition_taiga_temperate_and_jungle_layers() {
        let raw = r#"{
            "palette": {
                "meadow": {
                    "layers": [
                        {
                            "tier": "canopy",
                            "density": 1.0,
                            "max_temperature": 2.0,
                            "sprites": [{ "texture": "pine.png", "weight": 1, "min_size_tiles": 1.0, "max_size_tiles": 1.0 }]
                        },
                        {
                            "tier": "canopy",
                            "density": 1.0,
                            "min_temperature": 2.0,
                            "max_temperature": 20.0,
                            "max_slope_step_ft": 5,
                            "sprites": [{ "texture": "oak.png", "weight": 1, "min_size_tiles": 1.0, "max_size_tiles": 1.0 }]
                        },
                        {
                            "tier": "canopy",
                            "density": 1.0,
                            "min_temperature": 20.0,
                            "min_moisture": 0.65,
                            "sprites": [{ "texture": "palm.png", "weight": 1, "min_size_tiles": 1.0, "max_size_tiles": 1.0 }]
                        }
                    ]
                }
            }
        }"#;
        let table = ClutterConfig::from_json(raw)
            .and_then(|c| c.resolve(&PALETTE))
            .expect("valid biome filter table");

        let make_env = |temp: f32, moisture: f32, slope: i32| ClutterTileInput {
            palette_index: 0,
            region_painted: false,
            temperature: Some(temp),
            moisture: Some(moisture),
            foliage: 0.9,
            slope_step_ft: Some(slope),
            elevation_ft: Some(250),
            unit_dir: [0.0, 0.0, 1.0],
            hash: 42,
        };

        assert_eq!(
            table.place(make_env(-6.0, 0.5, 0)).map(|p| p.layer),
            Some(0)
        );
        assert_eq!(
            table.place(make_env(12.0, 0.5, 0)).map(|p| p.layer),
            Some(1)
        );
        assert_eq!(table.place(make_env(12.0, 0.5, 10)), None);
        assert_eq!(
            table.place(make_env(26.0, 0.8, 0)).map(|p| p.layer),
            Some(2)
        );
        assert_eq!(table.place(make_env(26.0, 0.4, 0)), None);

        // End-to-end through `chunk_clutter` with real `PlanetClimateMaps`.
        let coord = ChunkCoord::new(CubeFace::PosZ, 10, 10).expect("valid");
        let cover = uniform_cover(coord, 0);
        let taiga_climate = uniform_climate(-8.0, 0.5, 0.9);
        let taiga_sprites = chunk_clutter(
            &table,
            &cover,
            ClutterWorldContext {
                climate: Some(&taiga_climate),
                ..ClutterWorldContext::default()
            },
        );
        assert!(
            !taiga_sprites.is_empty(),
            "taiga climate must spawn layer 0"
        );
        assert!(taiga_sprites.iter().all(|s| s.placement.layer == 0));
    }

    #[test]
    fn sphere_cluster_modulation_forms_coherent_groves_and_clearings_across_face_seams() {
        let scale = 24.0_f32;
        let base = PlanetTile {
            face: CubeFace::PosX,
            x: 800_000,
            y: 800_000,
        };
        let mut grove_tiles = 0_usize;
        let mut clearing_tiles = 0_usize;
        let mut neighbor_agreements = 0_usize;
        let samples = 2_500_u32;

        for idx in 0..samples {
            let tile = PlanetTile {
                face: base.face,
                x: base.x + idx * 5,
                y: base.y + (idx % 50) * 7,
            };
            let next = PlanetTile {
                x: tile.x + 1,
                ..tile
            };
            let v0 = sphere_cluster_unit(tile_to_unit_vec(tile), scale, 3);
            let v1 = sphere_cluster_unit(tile_to_unit_vec(next), scale, 3);
            if v0 > 0.75 {
                grove_tiles += 1;
            } else if v0 < 0.25 {
                clearing_tiles += 1;
            }
            if (v0 >= 0.5) == (v1 >= 0.5) {
                neighbor_agreements += 1;
            }
        }
        assert!(
            grove_tiles > 200 && clearing_tiles > 200,
            "expected distinct groves ({grove_tiles}) and clearings ({clearing_tiles})"
        );
        assert!(
            neighbor_agreements * 100 / (samples as usize) >= 92,
            "adjacent tiles should stay spatially coherent, got {neighbor_agreements}/{samples}"
        );

        // Continuity across all 6 faces and 4 edges (all 12 cube-sphere seams).
        let last = FACE_TILES - 1;
        for face in CubeFace::ALL {
            for offset in [0_u32, 512, FACE_TILES / 2, last] {
                for (edge_tile, dx, dy) in [
                    (
                        PlanetTile {
                            face,
                            x: last,
                            y: offset,
                        },
                        1_i64,
                        0_i64,
                    ),
                    (
                        PlanetTile {
                            face,
                            x: 0,
                            y: offset,
                        },
                        -1,
                        0,
                    ),
                    (
                        PlanetTile {
                            face,
                            x: offset,
                            y: last,
                        },
                        0,
                        1,
                    ),
                    (
                        PlanetTile {
                            face,
                            x: offset,
                            y: 0,
                        },
                        0,
                        -1,
                    ),
                ] {
                    let (crossed, _) = step_tile(edge_tile, dx, dy);
                    let a = sphere_cluster_unit(tile_to_unit_vec(edge_tile), scale, 3);
                    let b = sphere_cluster_unit(tile_to_unit_vec(crossed), scale, 3);
                    assert!(
                        (a - b).abs() < 0.08,
                        "cluster seam jump across {edge_tile:?} -> {crossed:?}: {a} vs {b}"
                    );
                }
            }
        }
    }

    #[test]
    fn drop_shadows_keep_consistent_world_direction_under_random_sprite_rotation() {
        let config = ClutterConfig::from_json(
            r#"{
                "palette": {
                    "grass_lush": {
                        "layers": [{
                            "tier": "canopy",
                            "density": 1.0,
                            "rotatable": true,
                            "allow_flip_x": true,
                            "allow_flip_y": true,
                            "tint_variation": 0.2,
                            "shadow_alpha": 0.5,
                            "shadow_offset_tiles": [0.18, 0.22],
                            "shadow_scale": 1.04,
                            "sprites": [{
                                "texture": "terrain/clutter/grassclump_a.png",
                                "weight": 1,
                                "min_size_tiles": 3.0,
                                "max_size_tiles": 4.0
                            }]
                        }]
                    }
                }
            }"#,
        )
        .expect("valid shadow config");
        let palette = TerrainPalette::load().expect("shipped palette");
        let mut images = Assets::<Image>::default();
        let mut layouts = Assets::<TextureAtlasLayout>::default();
        let assets =
            GroundClutterAssets::from_config(&config, &palette, &mut images, Some(&mut layouts))
                .expect("loads assets");

        let grass_idx = palette.index_of("grass_lush").expect("grass_lush exists");
        let mut distinct_angles = 0_usize;
        let mut distinct_tints = 0_usize;

        for step in 0..64_u32 {
            let placement = assets
                .table()
                .place(ClutterTileInput::basic(
                    usize::from(grass_idx),
                    1.0,
                    u64::from(step).wrapping_mul(0x9E37_79B9_7F4A_7C15),
                ))
                .expect("density 1 spawns");
            if placement.rotation_radians > 0.2 && placement.rotation_radians < TAU - 0.2 {
                distinct_angles += 1;
            }
            if (placement.tint_rgb[0] - 1.0).abs() > 0.01 {
                distinct_tints += 1;
            }
            let chunk_sprite = ChunkClutterSprite {
                local_x: step % CHUNK_SIZE,
                local_y: step / 2,
                tile_y: 1_000 + step / 2,
                palette_index: grass_idx,
                placement,
            };
            let (_, caster_sprite, caster_tf) =
                assets.caster_bundle(&chunk_sprite).expect("caster bundle");
            let (_, shadow_sprite, shadow_tf) =
                assets.shadow_bundle(&chunk_sprite).expect("shadow bundle");

            let delta = shadow_tf.translation.truncate() - caster_tf.translation.truncate();
            assert!(
                (delta.x - 0.18).abs() < 1e-5 && (delta.y - (-0.22)).abs() < 1e-5,
                "shadow offset must stay (+0.18 east, -0.22 screen south) regardless of rotation {}, got {delta:?}",
                placement.rotation_radians
            );
            assert_eq!(shadow_tf.rotation, caster_tf.rotation);
            assert_eq!(shadow_sprite.flip_x, caster_sprite.flip_x);
            assert_eq!(shadow_sprite.flip_y, caster_sprite.flip_y);
            assert_eq!(
                shadow_sprite.color,
                Color::srgba(
                    SHADOW_TINT_RGB[0],
                    SHADOW_TINT_RGB[1],
                    SHADOW_TINT_RGB[2],
                    0.5
                )
            );
            assert!(shadow_tf.translation.z < caster_tf.translation.z);
            let caster_size = caster_sprite.custom_size.expect("sized");
            let shadow_size = shadow_sprite.custom_size.expect("sized");
            assert!((shadow_size - caster_size * 1.04).length() < 1e-5);
        }
        assert!(distinct_angles > 40, "expected diverse rotations");
        assert!(distinct_tints > 40, "expected per-instance tint variation");
    }

    #[test]
    fn runtime_texture_atlas_packs_shipped_sprites_and_falls_back_cleanly_when_oversized() {
        let config = ClutterConfig::load_default().expect("shipped clutter.json");
        let palette = TerrainPalette::load().expect("shipped palette.json");

        let mut images = Assets::<Image>::default();
        let mut layouts = Assets::<TextureAtlasLayout>::default();
        let packed =
            GroundClutterAssets::from_config(&config, &palette, &mut images, Some(&mut layouts))
                .expect("packs into atlas");
        assert!(
            packed.uses_atlas(),
            "shipped clutter sprites must pack into 1 atlas"
        );
        assert_eq!(
            images.len(),
            1,
            "atlas path uploads exactly 1 combined Image"
        );
        assert_eq!(
            layouts.len(),
            1,
            "atlas path registers 1 TextureAtlasLayout"
        );

        // Tiny max atlas size forces `TextureAtlasBuilder::build` to return `NotEnoughSpace` and
        // verifies clean fallback to standalone `Image` handles.
        let mut fallback_images = Assets::<Image>::default();
        let mut fallback_layouts = Assets::<TextureAtlasLayout>::default();
        let fallback = GroundClutterAssets::from_config_with_max_atlas(
            &config,
            &palette,
            &mut fallback_images,
            Some(&mut fallback_layouts),
            UVec2::splat(4),
        )
        .expect("fallback succeeds");
        assert!(!fallback.uses_atlas());
        assert_eq!(fallback_images.len(), config.textures().len());
    }

    #[test]
    fn blocking_stream_mode_builds_all_queued_chunks_in_one_frame() {
        let build_app = |mode: PlanetStreamSyncMode| {
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .insert_resource(mode)
                .insert_resource(GroundClutterAssets::default())
                .add_plugins(GroundClutterPlugin);
            for cx in 0..10_u32 {
                let coord = ChunkCoord::new(CubeFace::PosX, cx, 0).expect("valid");
                app.world_mut().spawn(uniform_cover(coord, 0));
            }
            app.update();
            app.world_mut()
                .query_filtered::<Entity, With<ClutterQueued>>()
                .iter(app.world())
                .count()
        };

        assert_eq!(
            build_app(PlanetStreamSyncMode::Async),
            10 - MAX_CLUTTER_CHUNKS_PER_FRAME,
            "async mode respects MAX_CLUTTER_CHUNKS_PER_FRAME"
        );
        assert_eq!(
            build_app(PlanetStreamSyncMode::Blocking),
            0,
            "blocking mode builds all queued chunks in 1 frame for ui_shot"
        );
    }

    /// Chunk whose left half is `meadow` and right half `gravel`, with every 4th row painted
    /// by a region and column 0 a `road` tile.
    fn test_cover() -> ChunkGroundCover {
        let coord = ChunkCoord::new(CubeFace::PosZ, 5, 7).expect("valid chunk");
        let mut dominant = Box::new([0_u8; CHUNK_TILE_COUNT]);
        let mut region_painted = Box::new([false; CHUNK_TILE_COUNT]);
        for local_y in 0..CHUNK_SIZE {
            for local_x in 0..CHUNK_SIZE {
                let index = ChunkGroundCover::index(local_x, local_y).expect("inside chunk");
                dominant[index] = match local_x {
                    0 => 1,
                    x if x < CHUNK_SIZE / 2 => 0,
                    _ => 2,
                };
                region_painted[index] = local_y % 4 == 0;
            }
        }
        ChunkGroundCover {
            coord,
            dominant,
            region_painted,
        }
    }

    #[test]
    fn chunk_clutter_never_lands_on_painted_or_clutter_free_tiles() {
        let table = test_table();
        let cover = test_cover();
        let sprites = chunk_clutter(&table, &cover, ClutterWorldContext::default());
        assert!(!sprites.is_empty(), "gravel ignores foliage and must draw");
        for sprite in &sprites {
            assert_eq!(
                cover.region_painted_at(sprite.local_x, sprite.local_y),
                Some(false),
                "sprite on a region-painted tile"
            );
            assert_eq!(
                cover.dominant_at(sprite.local_x, sprite.local_y),
                Some(sprite.palette_index)
            );
            assert_eq!(
                sprite.palette_index, 2,
                "only gravel draws without climate maps"
            );
        }
        assert_eq!(
            chunk_clutter(&table, &cover, ClutterWorldContext::default()),
            sprites,
            "deterministic"
        );
    }

    #[test]
    fn shipped_clutter_table_resolves_against_the_palette_and_every_texture_exists() {
        let config = ClutterConfig::load_default().expect("assets/terrain/clutter.json is valid");
        let palette = TerrainPalette::load().expect("assets/terrain/palette.json is valid");
        let flags: Vec<PaletteClutterFlag<'_>> = palette
            .entries()
            .iter()
            .map(|entry| PaletteClutterFlag {
                id: &entry.id,
                clutter: entry.clutter,
            })
            .collect();
        config
            .resolve(&flags)
            .expect("clutter table matches the palette");
        let root = crate::terrain::assets_root();
        for texture in config.textures() {
            assert!(
                root.join(texture).is_file(),
                "missing clutter sprite {texture}"
            );
        }
    }
}
