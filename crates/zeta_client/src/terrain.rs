//! Generic external map configuration, GPU terrain splatmap material, and splatmap
//! rasterizer.
//!
//! No terrain category or entity name is hardcoded in Rust. Every material texture, brush
//! layer, and entity visual rule comes from an external JSON map config file.
//!
//! Coordinate contract: every position, width, radius, padding, feather, wobble, and sprite
//! size in [`MapConfig`] is in **grid squares** (`1.0` = 1 five-foot square). `width` on a
//! polyline is the **full** width (`half_width = width * 0.5`). `entity_cells` layers derive
//! terrain directly from replicated world entities so visual features stay synchronized with
//! server passability.

use std::path::{Path, PathBuf};

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Extent3d, TextureDimension, TextureFormat};
use bevy::shader::ShaderRef;
use bevy::sprite_render::Material2d;
use serde::Deserialize;
use zeta_core::grid::{GridPos, MapBounds};
use zeta_core::protocol::{EntityName, Tags};

/// Default map config path relative to the `assets/` directory.
pub const DEFAULT_MAP_CONFIG: &str = "maps/river_crossing.json";
/// Pixels of splatmap resolution per 5-foot map square.
const SPLAT_PIXELS_PER_TILE: u32 = 8;
/// Maximum terrain materials supported by the 4-layer GPU splatmap shader.
const MAX_MATERIALS: usize = 4;

/// External map visual definition loaded from JSON.
#[derive(Clone, Debug, Deserialize)]
pub struct MapConfig {
    /// How many grid squares 1 repeat of a seamless terrain texture spans.
    #[serde(default = "default_tiles_per_repeat")]
    pub tiles_per_repeat: f32,
    /// Opacity of the subtle 5-foot grid overlay (`0.0` hides the grid).
    #[serde(default = "default_grid_alpha")]
    pub grid_alpha: f32,
    /// Per-map ambient RGB tint applied to both terrain and entity sprites to unify mixed art packs.
    #[serde(default = "default_ambient_tint")]
    pub ambient_tint: [f32; 3],
    /// Ordered terrain materials: slot 0 is the base fill, slots 1..=3 map to splat channels.
    pub materials: Vec<TerrainMaterialDef>,
    /// Ordered brush layers painted into the terrain splatmap.
    #[serde(default)]
    pub layers: Vec<TerrainLayer>,
    /// Ordered visual rules applied to replicated entities; first match wins.
    #[serde(default)]
    pub entity_styles: Vec<EntityStyleRule>,
}

/// 1 configurable terrain material category (e.g. land, path, water, snow).
#[derive(Clone, Debug, Deserialize)]
pub struct TerrainMaterialDef {
    /// Category name referenced by `layers`.
    pub id: String,
    /// Asset path to the seamless texture, relative to `assets/`.
    pub texture: String,
    /// UV repeat scale multiplier for this texture.
    #[serde(default = "default_scale")]
    pub scale: f32,
    /// Optional future procedural detail configuration for this category.
    #[serde(default)]
    pub detail: Option<serde_json::Value>,
}

/// 1 painted terrain brush layer.
#[derive(Clone, Debug, Deserialize)]
pub struct TerrainLayer {
    /// Material `id` from `MapConfig::materials`.
    pub material: String,
    /// Region painted by this brush layer.
    pub shape: LayerShape,
    /// Maximum opacity of this layer in `[0.0, 1.0]`.
    #[serde(default = "default_blend")]
    pub blend: f32,
    /// Optional darkening rim painted just outside the layer edge.
    #[serde(default)]
    pub edge_darken: Option<EdgeDarken>,
}

/// Darkening band just outside a painted layer's edge.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct EdgeDarken {
    /// Width of the rim in grid squares.
    pub width: f32,
    /// Peak darkening strength in `[0.0, 1.0]`.
    pub strength: f32,
}

/// Geometric shape painted by a terrain layer, strictly in grid-square coordinates.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayerShape {
    /// Ribbon painted along connected grid-square points (`[x, y]`).
    Polyline {
        /// Vertices of the polyline in grid squares.
        points: Vec<[f32; 2]>,
        /// Full width of the solid interior in grid squares.
        width: f32,
        /// Soft transition width in grid squares.
        #[serde(default)]
        feather: f32,
        /// Organic edge displacement in grid squares.
        #[serde(default)]
        wobble: f32,
    },
    /// Axis-aligned rectangle in grid squares.
    Rect {
        /// Top-left corner `[x, y]` in grid squares.
        min: [f32; 2],
        /// Bottom-right corner `[x, y]` in grid squares.
        max: [f32; 2],
        /// Soft transition width in grid squares.
        #[serde(default)]
        feather: f32,
        /// Organic edge displacement in grid squares.
        #[serde(default)]
        wobble: f32,
    },
    /// Circular region in grid squares.
    Circle {
        /// Center `[x, y]` in grid squares.
        center: [f32; 2],
        /// Radius in grid squares.
        radius: f32,
        /// Soft transition width in grid squares.
        #[serde(default)]
        feather: f32,
        /// Organic edge displacement in grid squares.
        #[serde(default)]
        wobble: f32,
    },
    /// Union of live grid squares occupied by replicated entities matching any listed tag or name.
    EntityCells {
        /// Case-insensitive tags whose entities contribute their occupied square.
        #[serde(default)]
        match_tags: Vec<String>,
        /// Case-insensitive entity names whose entities also contribute their occupied square.
        #[serde(default)]
        match_names: Vec<String>,
        /// Outward expansion (positive) or inward inset (negative) from each cell boundary, in squares.
        #[serde(default)]
        padding: f32,
        /// Soft transition width in grid squares.
        #[serde(default)]
        feather: f32,
        /// Organic edge displacement in grid squares.
        #[serde(default)]
        wobble: f32,
    },
}

/// Snapshot of 1 replicated entity used to resolve `LayerShape::EntityCells`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerrainEntityCell {
    /// Grid square occupied by the entity.
    pub position: GridPos,
    /// Optional display name.
    pub name: Option<String>,
    /// Narrative tags on the entity.
    pub tags: Vec<String>,
}

/// Generic visual override rule for replicated entities.
#[derive(Clone, Debug, Deserialize)]
pub struct EntityStyleRule {
    /// Optional exact case-insensitive entity name filter.
    #[serde(default)]
    pub match_name: Option<String>,
    /// Optional case-insensitive tag filter.
    #[serde(default)]
    pub match_tag: Option<String>,
    /// Optional sprite asset path relative to `assets/`.
    #[serde(default)]
    pub sprite: Option<String>,
    /// Optional sprite size `[width, height]` in grid squares.
    #[serde(default)]
    pub size: Option<[f32; 2]>,
    /// Optional draw depth (`z`).
    #[serde(default)]
    pub depth: Option<f32>,
    /// When true, hides the entity's sprite (useful for entities already painted into terrain).
    #[serde(default)]
    pub hide_sprite: bool,
    /// When true, hides the entity's floating text label.
    #[serde(default)]
    pub hide_label: bool,
}

impl EntityStyleRule {
    /// Whether this rule matches an entity with `name` and `tags`.
    pub fn matches(&self, name: Option<&EntityName>, tags: Option<&Tags>) -> bool {
        if let Some(expected_name) = &self.match_name
            && !name.is_some_and(|n| n.0.eq_ignore_ascii_case(expected_name))
        {
            return false;
        }
        if let Some(expected_tag) = &self.match_tag
            && !tags.is_some_and(|t| t.0.iter().any(|tag| tag.eq_ignore_ascii_case(expected_tag)))
        {
            return false;
        }
        self.match_name.is_some() || self.match_tag.is_some()
    }
}

/// Loaded map config and preloaded Bevy texture handles, kept as a client resource.
#[derive(Resource, Clone, Debug)]
pub struct LoadedMapConfig {
    /// Parsed external map configuration.
    pub config: MapConfig,
    /// Preloaded repeating terrain texture handles for slots 0..4.
    pub material_textures: [Handle<Image>; MAX_MATERIALS],
    /// Per-slot UV scale multipliers for slots 0..4.
    pub material_scales: Vec4,
    /// Preloaded entity style rules paired with their optional sprite handles.
    pub entity_styles: Vec<(EntityStyleRule, Option<Handle<Image>>)>,
}

impl LoadedMapConfig {
    /// Per-map ambient lighting tint as a `Vec4` uniform for `TerrainSplatMaterial`.
    #[must_use]
    pub fn ambient_vec4(&self) -> Vec4 {
        let [r, g, b] = self.config.ambient_tint;
        Vec4::new(r, g, b, 1.0)
    }

    /// Per-map ambient lighting tint as a Bevy `Color` for map entity sprites.
    #[must_use]
    pub fn ambient_color(&self) -> Color {
        let [r, g, b] = self.config.ambient_tint;
        Color::srgb(r, g, b)
    }

    /// Returns the first matching entity style rule and its preloaded sprite handle.
    pub fn style_for(
        &self,
        name: Option<&EntityName>,
        tags: Option<&Tags>,
    ) -> Option<(&EntityStyleRule, Option<&Handle<Image>>)> {
        self.entity_styles
            .iter()
            .find(|(rule, _)| rule.matches(name, tags))
            .map(|(rule, handle)| (rule, handle.as_ref()))
    }
}

/// GPU material that blends up to 4 configurable terrain textures through an RGBA splatmap.
// `Material2d` + `AsBindGroup`: bevy_sprite_render-0.20.1/src/mesh2d/material.rs:148.
#[derive(AsBindGroup, Debug, Clone, Asset, TypePath)]
pub struct TerrainSplatMaterial {
    /// `(grid_width, grid_height, tiles_per_repeat, grid_alpha)`.
    #[uniform(0)]
    pub params: Vec4,
    /// Per-material UV scale multipliers for `layer0`..`layer3`.
    #[uniform(0)]
    pub scales: Vec4,
    /// Per-map ambient lighting tint (`rgb`, `1.0`).
    #[uniform(0)]
    pub ambient_tint: Vec4,
    /// RGBA splat weights: `R = layer1`, `G = layer2`, `B = layer3`, `A = edge darkening`.
    #[texture(1)]
    #[sampler(2)]
    pub splat: Handle<Image>,
    /// Base terrain texture (slot 0).
    #[texture(3)]
    #[sampler(4)]
    pub layer0: Handle<Image>,
    /// Terrain texture for splat channel R (slot 1).
    #[texture(5)]
    #[sampler(6)]
    pub layer1: Handle<Image>,
    /// Terrain texture for splat channel G (slot 2).
    #[texture(7)]
    #[sampler(8)]
    pub layer2: Handle<Image>,
    /// Terrain texture for splat channel B (slot 3).
    #[texture(9)]
    #[sampler(10)]
    pub layer3: Handle<Image>,
}

impl Material2d for TerrainSplatMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/terrain_splat.wgsl".into()
    }
}

fn default_tiles_per_repeat() -> f32 {
    4.5
}

fn default_grid_alpha() -> f32 {
    0.12
}

fn default_ambient_tint() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}

fn default_scale() -> f32 {
    1.0
}

fn default_blend() -> f32 {
    1.0
}

/// Resolves the workspace `assets/` directory whether launched via `cargo run` or standalone.
pub fn assets_root() -> PathBuf {
    let local = Path::new("assets");
    if local.is_dir() {
        return std::fs::canonicalize(local).unwrap_or_else(|_| local.to_path_buf());
    }
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(workspace_root) = manifest_dir.parent().and_then(Path::parent) {
        let candidate = workspace_root.join("assets");
        if candidate.is_dir() {
            return std::fs::canonicalize(&candidate).unwrap_or(candidate);
        }
    }
    PathBuf::from("assets")
}

/// Configures `AssetPlugin` to load from [`assets_root`].
pub fn client_asset_plugin() -> AssetPlugin {
    AssetPlugin {
        file_path: assets_root().to_string_lossy().into_owned(),
        ..default()
    }
}

/// Parses a [`MapConfig`] JSON file from `assets/` without loading GPU assets.
///
/// # Errors
///
/// Returns an error if the file cannot be read, contains invalid JSON, or defines no
/// materials.
pub fn read_map_config(relative_path: &str) -> Result<MapConfig, String> {
    let full_path = assets_root().join(relative_path);
    let raw = std::fs::read_to_string(&full_path)
        .map_err(|error| format!("could not read {}: {error}", full_path.display()))?;
    let config: MapConfig = serde_json::from_str(&raw)
        .map_err(|error| format!("invalid map config {}: {error}", full_path.display()))?;
    if config.materials.is_empty() {
        return Err(format!("{} defines no materials", full_path.display()));
    }
    Ok(config)
}

/// Loads and parses a [`MapConfig`] JSON file from `assets/` and preloads its textures.
///
/// # Errors
///
/// Returns an error if the file cannot be read, contains invalid JSON, or defines no
/// materials.
pub fn load_map_config(
    asset_server: &AssetServer,
    relative_path: &str,
) -> Result<LoadedMapConfig, String> {
    let config = read_map_config(relative_path)?;

    let first_tex = load_repeating_texture(asset_server, config.materials[0].texture.clone());
    let mut material_textures = [
        first_tex.clone(),
        first_tex.clone(),
        first_tex.clone(),
        first_tex,
    ];
    let mut scales = [1.0_f32; MAX_MATERIALS];
    for (slot, material) in config.materials.iter().take(MAX_MATERIALS).enumerate() {
        material_textures[slot] = load_repeating_texture(asset_server, material.texture.clone());
        scales[slot] = material.scale;
    }

    let entity_styles = config
        .entity_styles
        .iter()
        .cloned()
        .map(|rule| {
            let handle = rule
                .sprite
                .as_ref()
                .map(|path| asset_server.load(path.clone()));
            (rule, handle)
        })
        .collect();

    Ok(LoadedMapConfig {
        config,
        material_textures,
        material_scales: Vec4::from_array(scales),
        entity_styles,
    })
}

/// Loads a seamless terrain texture configured with repeating UV addressing.
pub fn load_repeating_texture(asset_server: &AssetServer, path: String) -> Handle<Image> {
    // `AssetServer::load_builder`: bevy_asset-0.20.1/src/server/mod.rs:371.
    // `LoadBuilder::with_settings`: bevy_asset-0.20.1/src/server/mod.rs:1737.
    // `ImageLoaderSettings::sampler`: bevy_image-0.20.1/src/image_loader.rs:151.
    // `ImageAddressMode::Repeat`: bevy_image-0.20.1/src/image.rs:747.
    asset_server
        .load_builder()
        .with_settings(|settings: &mut ImageLoaderSettings| {
            settings.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                address_mode_u: ImageAddressMode::Repeat,
                address_mode_v: ImageAddressMode::Repeat,
                ..ImageSamplerDescriptor::linear()
            });
        })
        .load(path)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "map dimensions and splatmap pixel counts stay far below 2^23"
)]
fn count_to_f32(value: u32) -> f32 {
    value as f32
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is clamped to [0.0, 1.0] before scaling to 0..=255"
)]
fn weight_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0) as u8
}

/// Uniform parameters for a `MapBounds` terrain quad from `config`.
pub fn terrain_params(bounds: MapBounds, config: &MapConfig) -> Vec4 {
    Vec4::new(
        count_to_f32(bounds.width),
        count_to_f32(bounds.height),
        config.tiles_per_repeat,
        config.grid_alpha,
    )
}

/// Returns whether an entity with `name` and `tags` contributes to any `EntityCells` layer of
/// `config`. Used to detect when replicated entities require a splatmap rebuild.
pub fn entity_matches_any_terrain_layer(
    config: &MapConfig,
    name: Option<&EntityName>,
    tags: Option<&Tags>,
) -> bool {
    config.layers.iter().any(|layer| {
        let LayerShape::EntityCells {
            match_tags,
            match_names,
            ..
        } = &layer.shape
        else {
            return false;
        };
        let tag_hit = tags.is_some_and(|t| {
            match_tags
                .iter()
                .any(|wanted| t.0.iter().any(|tag| tag.eq_ignore_ascii_case(wanted)))
        });
        let name_hit = name.is_some_and(|n| {
            match_names
                .iter()
                .any(|wanted| n.0.eq_ignore_ascii_case(wanted))
        });
        tag_hit || name_hit
    })
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    if (edge1 - edge0).abs() <= f32::EPSILON {
        return if value < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn organic_wave(x: f32, y: f32) -> f32 {
    (x * 0.63 + (y * 0.41).sin() * 1.4).sin() * 0.55
        + (y * 0.87 - (x * 0.52).cos() * 1.1).cos() * 0.45
}

fn point_to_segment_distance(point: Vec2, start: Vec2, end: Vec2) -> f32 {
    let ab = end - start;
    let len_sq = ab.length_squared();
    if len_sq <= f32::EPSILON {
        return point.distance(start);
    }
    let t = ((point - start).dot(ab) / len_sq).clamp(0.0, 1.0);
    point.distance(start + ab * t)
}

fn box_signed_distance(point: Vec2, min_p: Vec2, max_p: Vec2) -> f32 {
    let center = (min_p + max_p) * 0.5;
    let half = (max_p - min_p).abs() * 0.5;
    let q = (point - center).abs() - half;
    let outside = q.max(Vec2::ZERO).length();
    let inside = q.x.max(q.y).min(0.0);
    outside + inside
}

fn resolve_layer_cells(shape: &LayerShape, entities: &[TerrainEntityCell]) -> Vec<GridPos> {
    let LayerShape::EntityCells {
        match_tags,
        match_names,
        ..
    } = shape
    else {
        return Vec::new();
    };
    let mut cells: Vec<GridPos> = entities
        .iter()
        .filter(|entity| {
            let tag_hit = match_tags
                .iter()
                .any(|wanted| entity.tags.iter().any(|t| t.eq_ignore_ascii_case(wanted)));
            let name_hit = entity.name.as_deref().is_some_and(|n| {
                match_names
                    .iter()
                    .any(|wanted| n.eq_ignore_ascii_case(wanted))
            });
            tag_hit || name_hit
        })
        .map(|entity| entity.position)
        .collect();
    cells.sort_by_key(|pos| (pos.y, pos.x));
    cells.dedup();
    cells
}

/// Signed distance in grid squares from `tile_pos` to the boundary of `shape` (negative
/// inside, positive outside), plus `(feather, wobble)`.
fn shape_signed_distance(
    shape: &LayerShape,
    tile_pos: Vec2,
    matched_cells: &[GridPos],
) -> (f32, f32, f32) {
    match shape {
        LayerShape::Polyline {
            points,
            width,
            feather,
            wobble,
        } => {
            if points.is_empty() {
                return (f32::INFINITY, *feather, *wobble);
            }
            let min_dist = if points.len() == 1 {
                tile_pos.distance(Vec2::from_array(points[0]))
            } else {
                points
                    .windows(2)
                    .map(|pair| {
                        point_to_segment_distance(
                            tile_pos,
                            Vec2::from_array(pair[0]),
                            Vec2::from_array(pair[1]),
                        )
                    })
                    .fold(f32::INFINITY, f32::min)
            };
            (min_dist - *width * 0.5, *feather, *wobble)
        }
        LayerShape::Rect {
            min,
            max,
            feather,
            wobble,
        } => (
            box_signed_distance(tile_pos, Vec2::from_array(*min), Vec2::from_array(*max)),
            *feather,
            *wobble,
        ),
        LayerShape::Circle {
            center,
            radius,
            feather,
            wobble,
        } => (
            tile_pos.distance(Vec2::from_array(*center)) - *radius,
            *feather,
            *wobble,
        ),
        LayerShape::EntityCells {
            padding,
            feather,
            wobble,
            ..
        } => {
            if matched_cells.is_empty() {
                return (f32::INFINITY, *feather, *wobble);
            }
            let min_sdf = matched_cells
                .iter()
                .map(|cell| {
                    let min_p = Vec2::new(count_to_f32(cell.x), count_to_f32(cell.y));
                    let max_p = min_p + Vec2::ONE;
                    box_signed_distance(tile_pos, min_p, max_p)
                })
                .fold(f32::INFINITY, f32::min);
            (min_sdf - *padding, *feather, *wobble)
        }
    }
}

/// Rasterizes the configurable `layers` of `config` into an RGBA8 splatmap image for `bounds`,
/// using `entities` to resolve any `entity_cells` layers directly from live simulation state.
pub fn build_splatmap_from_config(
    bounds: MapBounds,
    config: &MapConfig,
    entities: &[TerrainEntityCell],
) -> Image {
    let width = bounds.width.max(1) * SPLAT_PIXELS_PER_TILE;
    let height = bounds.height.max(1) * SPLAT_PIXELS_PER_TILE;
    let scale = count_to_f32(SPLAT_PIXELS_PER_TILE);
    let mut data = vec![0_u8; (width * height * 4) as usize];

    let resolved_layers: Vec<(usize, &TerrainLayer, Vec<GridPos>)> = config
        .layers
        .iter()
        .filter_map(|layer| {
            let slot = config
                .materials
                .iter()
                .take(MAX_MATERIALS)
                .position(|m| m.id == layer.material)?;
            let cells = resolve_layer_cells(&layer.shape, entities);
            Some((slot, layer, cells))
        })
        .collect();

    for py in 0..height {
        let tile_y = (count_to_f32(py) + 0.5) / scale;
        for px in 0..width {
            let tile_x = (count_to_f32(px) + 0.5) / scale;
            let tile_pos = Vec2::new(tile_x, tile_y);
            let wave = organic_wave(tile_x, tile_y);

            let mut channels = [0.0_f32; 4]; // R=slot1, G=slot2, B=slot3, A=darken
            for (slot, layer, cells) in &resolved_layers {
                let (signed_dist, feather, wobble) =
                    shape_signed_distance(&layer.shape, tile_pos, cells);
                let displaced = signed_dist + wobble * wave;
                let half_feather = feather.max(0.01) * 0.5;
                let weight = (1.0 - smoothstep(-half_feather, half_feather, displaced))
                    * layer.blend.clamp(0.0, 1.0);

                if (1..MAX_MATERIALS).contains(slot) {
                    channels[slot - 1] = channels[slot - 1].max(weight);
                }
                if let Some(edge) = layer.edge_darken {
                    let rim = smoothstep(-half_feather, half_feather, displaced)
                        * (1.0 - smoothstep(half_feather, half_feather + edge.width, displaced))
                        * edge.strength.clamp(0.0, 1.0);
                    channels[3] = channels[3].max(rim);
                }
            }

            let index = ((py * width + px) * 4) as usize;
            data[index] = weight_to_u8(channels[0]);
            data[index + 1] = weight_to_u8(channels[1]);
            data[index + 2] = weight_to_u8(channels[2]);
            data[index + 3] = weight_to_u8(channels[3]);
        }
    }

    // `Image::new`: bevy_image-0.20.1/src/image.rs:1123.
    let mut image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor::linear());
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tile(image: &Image, bounds: MapBounds, tile_x: u32, tile_y: u32) -> [u8; 4] {
        let data = image.data.as_ref().expect("splatmap bytes");
        let width = bounds.width * SPLAT_PIXELS_PER_TILE;
        let px = tile_x * SPLAT_PIXELS_PER_TILE + SPLAT_PIXELS_PER_TILE / 2;
        let py = tile_y * SPLAT_PIXELS_PER_TILE + SPLAT_PIXELS_PER_TILE / 2;
        let i = ((py * width + px) * 4) as usize;
        [data[i], data[i + 1], data[i + 2], data[i + 3]]
    }

    #[test]
    fn splatmap_interprets_grid_coordinate_layers_with_full_width() {
        let config: MapConfig = serde_json::from_str(
            r#"{
                "materials": [
                    { "id": "base", "texture": "a.png" },
                    { "id": "band_a", "texture": "b.png" },
                    { "id": "zone_b", "texture": "c.png" }
                ],
                "layers": [
                    {
                        "material": "band_a",
                        "shape": { "kind": "polyline", "points": [[0.0, 5.5], [10.0, 5.5]], "width": 1.0, "feather": 0.2 }
                    },
                    {
                        "material": "zone_b",
                        "shape": { "kind": "circle", "center": [2.5, 2.5], "radius": 1.0, "feather": 0.2 }
                    }
                ]
            }"#,
        )
        .expect("valid generic map config");

        let bounds = MapBounds {
            width: 10,
            height: 10,
        };
        let image = build_splatmap_from_config(bounds, &config, &[]);

        // Row 5 (y=5.5) hits `band_a` (channel R), while row 4 (y=4.5, 1 full tile away) stays clear.
        assert!(sample_tile(&image, bounds, 5, 5)[0] > 200);
        assert_eq!(sample_tile(&image, bounds, 5, 4)[0], 0);
        // Circle at (2.5, 2.5) hits `zone_b` (channel G).
        assert!(sample_tile(&image, bounds, 2, 2)[1] > 200);
        assert_eq!(sample_tile(&image, bounds, 9, 9)[0], 0);
        assert_eq!(sample_tile(&image, bounds, 9, 9)[1], 0);
    }

    #[test]
    fn entity_style_rules_match_by_name_and_tag() {
        let rule: EntityStyleRule = serde_json::from_str(
            r#"{ "match_name": "Crate", "match_tag": "charred", "sprite": "objects/x.png" }"#,
        )
        .expect("valid style rule");
        let crate_name = EntityName("crate".to_owned());
        let charred = Tags(["charred".to_owned()].into_iter().collect());
        let plain = Tags(["wooden".to_owned()].into_iter().collect());

        assert!(rule.matches(Some(&crate_name), Some(&charred)));
        assert!(!rule.matches(Some(&crate_name), Some(&plain)));
        assert!(!rule.matches(None, Some(&charred)));
    }

    #[test]
    fn river_crossing_config_assets_exist_and_spatial_invariants_hold() {
        let root = assets_root();
        let config = read_map_config(DEFAULT_MAP_CONFIG).expect("valid river_crossing.json");

        for material in &config.materials {
            assert!(
                root.join(&material.texture).is_file(),
                "missing terrain texture: {}",
                material.texture
            );
        }
        for rule in &config.entity_styles {
            if let Some(sprite) = &rule.sprite {
                assert!(
                    root.join(sprite).is_file(),
                    "missing entity sprite: {sprite}"
                );
            }
        }

        // Simulate the real 20x20 server river layout (river on column 10, bridge at (10, 10)).
        let bounds = MapBounds {
            width: 20,
            height: 20,
        };
        let mut cells: Vec<TerrainEntityCell> = (0..bounds.height)
            .filter(|&row| row != 10)
            .map(|row| TerrainEntityCell {
                position: GridPos::new(10, row),
                name: Some("River".to_owned()),
                tags: vec!["water".to_owned(), "deep".to_owned()],
            })
            .collect();
        cells.push(TerrainEntityCell {
            position: GridPos::new(10, 10),
            name: Some("Wooden Bridge".to_owned()),
            tags: vec!["wooden".to_owned()],
        });

        let image = build_splatmap_from_config(bounds, &config, &cells);

        // Invariant 1: every river cell (including under the bridge) is solid water (channel B).
        for row in 0..bounds.height {
            let water = sample_tile(&image, bounds, 10, row)[2];
            assert!(
                water >= 220,
                "river tile (10, {row}) should be solid water, got {water}"
            );
        }

        // Invariant 2: adjacent dry walkable squares on columns 9 and 11 stay dry (no submerged land).
        for row in 0..bounds.height {
            let west_water = sample_tile(&image, bounds, 9, row)[2];
            let east_water = sample_tile(&image, bounds, 11, row)[2];
            assert_eq!(
                west_water, 0,
                "dry tile (9, {row}) must not be submerged in water"
            );
            assert_eq!(
                east_water, 0,
                "dry tile (11, {row}) must not be submerged in water"
            );
        }

        // Invariant 3: Wooden Bridge sprite width spans cleanly past both water edges onto solid banks.
        let bridge_rule = config
            .entity_styles
            .iter()
            .find(|r| r.match_name.as_deref() == Some("Wooden Bridge"))
            .expect("Wooden Bridge style rule");
        let bridge_width = bridge_rule.size.expect("bridge size")[0];
        let bridge_half = bridge_width * 0.5;
        let data = image.data.as_ref().expect("splatmap bytes");
        let row_y = 10 * SPLAT_PIXELS_PER_TILE + SPLAT_PIXELS_PER_TILE / 2;
        let img_w = bounds.width * SPLAT_PIXELS_PER_TILE;
        let water_pixels: Vec<u32> = (0..img_w)
            .filter(|&px| data[((row_y * img_w + px) * 4 + 2) as usize] > 16)
            .collect();
        let water_left = count_to_f32(*water_pixels.first().expect("water pixels"))
            / count_to_f32(SPLAT_PIXELS_PER_TILE);
        let water_right = count_to_f32(*water_pixels.last().expect("water pixels") + 1)
            / count_to_f32(SPLAT_PIXELS_PER_TILE);
        let bridge_left = 10.5 - bridge_half;
        let bridge_right = 10.5 + bridge_half;
        assert!(
            bridge_left < water_left - 0.15 && bridge_right > water_right + 0.15,
            "bridge [{bridge_left:.2}, {bridge_right:.2}] must anchor past water [{water_left:.2}, {water_right:.2}]"
        );
    }
}
