//! Generic external map configuration, the GPU terrain splat material, and brush layer
//! coverage.
//!
//! No terrain category or entity name is hardcoded in Rust. Every brush layer and entity
//! visual rule comes from an external JSON map config file, and every material names a
//! palette entry of `assets/terrain/palette.json` (`terrain_palette`), never a texture path.
//!
//! Coordinate contract: every position, width, radius, padding, feather, wobble, and sprite
//! size in [`MapConfig`] is in **grid squares** (`1.0` = 1 five-foot square). `width` on a
//! polyline is the **full** width (`half_width = width * 0.5`). `entity_cells` layers derive
//! terrain directly from replicated world entities so visual features stay synchronized with
//! server passability.

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d};
use serde::Deserialize;
use zeta_core::grid::GridPos;
use zeta_core::protocol::{EntityName, Tags};

use crate::terrain_palette::{MAX_PALETTE_ENTRIES, TerrainPalette};

/// Default map config path relative to the `assets/` directory.
pub const DEFAULT_MAP_CONFIG: &str = "maps/river_crossing.json";
/// Control map texels per 5-foot square (`terrain_contract.md` Section 4).
pub(crate) const SPLAT_PIXELS_PER_TILE: u32 = 8;

/// External map visual definition loaded from JSON.
#[derive(Clone, Debug, Deserialize)]
pub struct MapConfig {
    /// Opacity of the subtle 5-foot grid overlay (`0.0` hides the grid).
    #[serde(default = "default_grid_alpha")]
    pub grid_alpha: f32,
    /// Per-map ambient RGB tint applied to entity sprites to unify mixed art packs.
    #[serde(default = "default_ambient_tint")]
    pub ambient_tint: [f32; 3],
    /// Terrain materials referenced by `layers`. The first material fills the ground of a
    /// standalone map; a planet region draws only its `layers` over the planet ground.
    pub materials: Vec<TerrainMaterialDef>,
    /// Ordered brush layers painted into the terrain control map.
    #[serde(default)]
    pub layers: Vec<TerrainLayer>,
    /// Ordered visual rules applied to replicated entities; first match wins.
    #[serde(default)]
    pub entity_styles: Vec<EntityStyleRule>,
}

/// 1 configurable terrain material category (e.g. land, path, water, snow).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerrainMaterialDef {
    /// Category name referenced by `layers`.
    pub id: String,
    /// Palette entry id (`assets/terrain/palette.json`) drawn for this category.
    pub palette: String,
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

/// Loaded map config and preloaded entity sprite handles, kept as a client resource.
#[derive(Resource, Clone, Debug)]
pub struct LoadedMapConfig {
    /// Parsed external map configuration.
    pub config: MapConfig,
    /// Preloaded entity style rules paired with their optional sprite handles.
    pub entity_styles: Vec<(EntityStyleRule, Option<Handle<Image>>)>,
}

impl LoadedMapConfig {
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

/// GPU material that blends palette texture-array layers through a per-chunk control map.
///
/// Bindings match `assets/shaders/terrain_splat.wgsl` (`terrain_contract.md` Section 4).
// `Material2d` + `AsBindGroup`: bevy_sprite_render-0.20.1/src/mesh2d/material.rs:148.
// `texture(N, dimension = "2d_array")`, texture without sampler:
// bevy_render-0.20.1/src/render_resource/bind_group.rs:174-189.
#[derive(AsBindGroup, Debug, Clone, Asset, TypePath)]
pub struct TerrainSplatMaterial {
    /// `(width_tiles, height_tiles, grid_alpha, control_texels_per_tile)`.
    #[uniform(0)]
    pub params: Vec4,
    /// Per palette layer: `xy` = quad origin tile modulo the layer repeat, `z` = 1 / repeat.
    #[uniform(0)]
    pub layer_params: [Vec4; MAX_PALETTE_ENTRIES],
    /// Control map (`RGBA8Unorm`, 1-texel halo): `R`/`G` palette indices of layers A/B,
    /// `B` weight of layer B, `A` brightness. Read with `textureLoad`, so no sampler.
    #[texture(1)]
    pub control: Handle<Image>,
    /// Per-tile hillshade x climate tint / 2 with a 1-tile halo.
    #[texture(2)]
    #[sampler(3)]
    pub shade: Handle<Image>,
    /// Palette texture array (`TerrainPaletteGpu`).
    #[texture(4, dimension = "2d_array")]
    #[sampler(5)]
    pub layers: Handle<Image>,
}

impl Material2d for TerrainSplatMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/terrain_splat.wgsl".into()
    }

    // `Material2d::alpha_mode`: bevy_sprite_render-0.20.1/src/mesh2d/material.rs:167.
    // `AlphaMode2d::Opaque`: bevy_sprite_render-0.20.1/src/mesh2d/material.rs:263.
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Opaque
    }
}

fn default_grid_alpha() -> f32 {
    0.12
}

fn default_ambient_tint() -> [f32; 3] {
    [1.0, 1.0, 1.0]
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

/// Loads and parses a [`MapConfig`] JSON file from `assets/` and preloads its entity sprites.
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
        entity_styles,
    })
}

/// Palette index of every material of `config`, in `materials` order.
///
/// # Errors
///
/// Returns an error naming the material when its `palette` id is not in `palette`, or naming
/// the layer when its `material` is not in `materials`.
pub fn resolve_material_palette(
    config: &MapConfig,
    palette: &TerrainPalette,
) -> Result<Vec<u8>, String> {
    let indices = config
        .materials
        .iter()
        .map(|material| {
            palette.index_of(&material.palette).ok_or_else(|| {
                format!(
                    "material {:?} names unknown palette entry {:?}",
                    material.id, material.palette
                )
            })
        })
        .collect::<Result<Vec<u8>, String>>()?;
    if let Some(layer) = config
        .layers
        .iter()
        .find(|layer| !config.materials.iter().any(|m| m.id == layer.material))
    {
        return Err(format!("layer names unknown material {:?}", layer.material));
    }
    Ok(indices)
}

/// 1 brush layer with its palette index and its resolved `entity_cells` squares.
#[derive(Clone, Debug)]
pub struct ResolvedLayer {
    /// Palette index drawn by the layer.
    pub palette_index: u8,
    /// Brush layer definition.
    pub layer: TerrainLayer,
    /// Squares matched by an `entity_cells` shape (empty for other shapes).
    pub cells: Vec<GridPos>,
}

/// Resolves every layer of `config` against `palette` and live `entities`.
///
/// # Errors
///
/// Returns the [`resolve_material_palette`] error.
pub fn resolve_layers(
    config: &MapConfig,
    palette: &TerrainPalette,
    entities: &[TerrainEntityCell],
) -> Result<Vec<ResolvedLayer>, String> {
    let indices = resolve_material_palette(config, palette)?;
    Ok(config
        .layers
        .iter()
        .filter_map(|layer| {
            let slot = config
                .materials
                .iter()
                .position(|m| m.id == layer.material)?;
            Some(ResolvedLayer {
                palette_index: indices[slot],
                layer: layer.clone(),
                cells: resolve_layer_cells(&layer.shape, entities),
            })
        })
        .collect())
}

/// Coverage of `layer` at `tile_pos` (grid squares, map-local): `(weight, rim)`.
///
/// `weight` is the layer opacity in `[0.0, blend]`; `rim` is the edge darkening strength just
/// outside the edge (`0.0` without `edge_darken`). Organic wobble displaces the edge.
pub fn layer_coverage(layer: &ResolvedLayer, tile_pos: Vec2) -> (f32, f32) {
    let (signed_dist, feather, wobble) =
        shape_signed_distance(&layer.layer.shape, tile_pos, &layer.cells);
    if !signed_dist.is_finite() {
        return (0.0, 0.0);
    }
    let displaced = signed_dist + wobble * organic_wave(tile_pos.x, tile_pos.y);
    let half_feather = feather.max(0.01) * 0.5;
    let outside = smoothstep(-half_feather, half_feather, displaced);
    let weight = (1.0 - outside) * layer.layer.blend.clamp(0.0, 1.0);
    let rim = layer.layer.edge_darken.map_or(0.0, |edge| {
        outside
            * (1.0 - smoothstep(half_feather, half_feather + edge.width, displaced))
            * edge.strength.clamp(0.0, 1.0)
    });
    (weight, rim)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "map dimensions and control map pixel counts stay far below 2^23"
)]
pub(crate) fn count_to_f32(value: u32) -> f32 {
    value as f32
}

/// Returns whether an entity with `name` and `tags` contributes to any `EntityCells` layer of
/// `config`. Used to detect when replicated entities require a control map rebuild.
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

/// Hermite smoothstep of `value` between `edge0` and `edge1`.
pub(crate) fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolves the first layer of `config_json`, pointing every material at palette entry 0
    /// so the coverage tests stay independent of the shipped palette ids.
    fn single_layer(config_json: &str, entities: &[TerrainEntityCell]) -> ResolvedLayer {
        let mut config: MapConfig = serde_json::from_str(config_json).expect("valid map config");
        let palette = TerrainPalette::load().expect("shipped palette");
        for material in &mut config.materials {
            material.palette = palette.entries()[0].id.clone();
        }
        resolve_layers(&config, &palette, entities)
            .expect("layers resolve")
            .remove(0)
    }

    #[test]
    fn polyline_coverage_uses_full_width_in_grid_squares() {
        let layer = single_layer(
            r#"{
                "materials": [{ "id": "band", "palette": "x" }],
                "layers": [{
                    "material": "band",
                    "shape": { "kind": "polyline", "points": [[0.0, 5.5], [10.0, 5.5]], "width": 1.0, "feather": 0.2 }
                }]
            }"#,
            &[],
        );
        assert!(layer_coverage(&layer, Vec2::new(5.5, 5.5)).0 > 0.95);
        assert!(layer_coverage(&layer, Vec2::new(5.5, 4.5)).0 < 0.01);
    }

    #[test]
    fn edge_darken_rim_sits_just_outside_the_edge() {
        let layer = single_layer(
            r#"{
                "materials": [{ "id": "zone", "palette": "x" }],
                "layers": [{
                    "material": "zone",
                    "shape": { "kind": "circle", "center": [5.0, 5.0], "radius": 2.0, "feather": 0.2 },
                    "edge_darken": { "width": 1.0, "strength": 0.5 }
                }]
            }"#,
            &[],
        );
        assert!(layer_coverage(&layer, Vec2::new(5.0, 5.0)).1 < 0.01);
        assert!(layer_coverage(&layer, Vec2::new(7.4, 5.0)).1 > 0.2);
        assert!(layer_coverage(&layer, Vec2::new(9.5, 5.0)).1 < 0.01);
    }

    #[test]
    fn entity_cells_layer_stays_aligned_with_matching_grid_squares() {
        let cells = vec![
            TerrainEntityCell {
                position: GridPos::new(3, 3),
                name: None,
                tags: vec!["feature_tag".to_owned()],
            },
            TerrainEntityCell {
                position: GridPos::new(3, 4),
                name: None,
                tags: vec!["feature_tag".to_owned()],
            },
        ];
        let layer = single_layer(
            r#"{
                "materials": [{ "id": "feature", "palette": "x" }],
                "layers": [{
                    "material": "feature",
                    "shape": { "kind": "entity_cells", "match_tags": ["feature_tag"], "feather": 0.2, "wobble": 0.05 }
                }]
            }"#,
            &cells,
        );
        assert!(layer_coverage(&layer, Vec2::new(3.5, 3.5)).0 > 0.85);
        assert!(layer_coverage(&layer, Vec2::new(3.5, 4.5)).0 > 0.85);
        assert!(layer_coverage(&layer, Vec2::new(2.5, 3.5)).0 < 0.01);
        assert!(layer_coverage(&layer, Vec2::new(4.5, 3.5)).0 < 0.01);
    }

    #[test]
    fn unknown_palette_ids_and_materials_are_rejected() {
        let palette = TerrainPalette::load().expect("shipped palette");
        let bad_palette: MapConfig =
            serde_json::from_str(r#"{ "materials": [{ "id": "a", "palette": "no_such_entry" }] }"#)
                .expect("parses");
        assert!(resolve_material_palette(&bad_palette, &palette).is_err());
        let first = &palette.entries()[0].id;
        let bad_layer: MapConfig = serde_json::from_str(&format!(
            r#"{{ "materials": [{{ "id": "a", "palette": "{first}" }}],
                 "layers": [{{ "material": "b", "shape": {{ "kind": "circle", "center": [0.0, 0.0], "radius": 1.0 }} }}] }}"#
        ))
        .expect("parses");
        assert!(resolve_material_palette(&bad_layer, &palette).is_err());
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
    fn every_map_config_in_assets_resolves_against_the_palette_and_sprites_exist() {
        let root = assets_root();
        let palette = TerrainPalette::load().expect("shipped palette");
        let entries: Vec<_> = std::fs::read_dir(root.join("maps"))
            .expect("assets/maps directory exists")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        assert!(!entries.is_empty(), "at least 1 map config exists");

        for entry in entries {
            let rel = format!("maps/{}", entry.file_name().to_string_lossy());
            let config = read_map_config(&rel).unwrap_or_else(|e| panic!("invalid {rel}: {e}"));
            resolve_material_palette(&config, &palette).unwrap_or_else(|e| panic!("{rel}: {e}"));
            for rule in &config.entity_styles {
                if let Some(sprite) = &rule.sprite {
                    assert!(
                        root.join(sprite).is_file(),
                        "{rel}: missing entity sprite {sprite}"
                    );
                }
            }
        }
    }
}
