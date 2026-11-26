//! Generic authored map region loader, non-overlap validator, and soft polygon mask
//! rasterizer.
//!
//! Rules: `docs/architecture/world_geography.md` Section 4. Every authored region file in
//! `assets/world/regions/*.json` declares a simple polygon footprint in face 5-foot tile
//! coordinates. No 2 authored region polygons on the same face may overlap; loading fails
//! immediately if [`validate_non_overlapping`] detects any collision.
//!
//! Coordinate contract:
//! - Polygon vertices use face 5-foot tile coordinates (`[x, y]` in `[0, FACE_TILES]`).
//! - Brush layers inside a region's [`MapConfig`] are expressed in region-local grid squares
//!   relative to the polygon bounding box's top-left corner `(bounds.min_x, bounds.min_y)`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use serde::Deserialize;
use zeta_core::grid::{GridPos, MapBounds};
use zeta_core::protocol::{EntityName, Tags};
use zeta_core::world::{
    CubeFace, FACE_TILES, RegionFootprint, point_in_polygon, validate_non_overlapping,
};

use crate::layout::FloatingOrigin;
use crate::terrain::{
    EntityStyleRule, MapConfig, SPLAT_PIXELS_PER_TILE, assets_root, load_repeating_texture,
};

/// Directory containing authored region JSON files, relative to `assets/`.
pub const WORLD_REGIONS_DIR: &str = "world/regions";
/// Default inward feather distance (in grid squares) along an authored polygon boundary.
const DEFAULT_EDGE_FEATHER: f32 = 1.25;
/// Maximum terrain materials supported by `TerrainSplatMaterial`.
const MAX_MATERIALS: usize = 4;

fn default_edge_feather() -> f32 {
    DEFAULT_EDGE_FEATHER
}

/// Resolves the workspace `assets/world/regions` directory.
#[must_use]
pub fn world_regions_dir() -> PathBuf {
    assets_root().join(WORLD_REGIONS_DIR)
}

/// Raw JSON schema for 1 authored map region file in `assets/world/regions/*.json`.
#[derive(Clone, Debug, Deserialize)]
pub struct AuthoredRegionConfig {
    /// Unique identifier of this authored region.
    pub id: String,
    /// Cube face containing this region's footprint.
    pub face: CubeFace,
    /// Simple polygon vertices in face 5-foot tile coordinates (`[x, y]`).
    pub polygon: Vec<[i64; 2]>,
    /// Inward boundary feather width in grid squares (`0.0` produces a crisp polygon edge).
    #[serde(default = "default_edge_feather")]
    pub edge_feather: f32,
    /// Region-local terrain materials, brush layers, and entity visual rules.
    #[serde(flatten)]
    pub map: MapConfig,
}

/// Integer bounding box of an authored region polygon in face 5-foot tile coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegionBounds {
    /// Minimum column (`+x`) in face tile coordinates.
    pub min_x: i64,
    /// Minimum row (`+y`) in face tile coordinates.
    pub min_y: i64,
    /// Exclusive maximum column (`+x`) in face tile coordinates.
    pub max_x: i64,
    /// Exclusive maximum row (`+y`) in face tile coordinates.
    pub max_y: i64,
}

impl RegionBounds {
    /// Width of the bounding box in 5-foot tiles.
    #[must_use]
    pub fn width_tiles(self) -> u32 {
        u32::try_from(self.max_x.saturating_sub(self.min_x)).unwrap_or(0)
    }

    /// Height of the bounding box in 5-foot tiles.
    #[must_use]
    pub fn height_tiles(self) -> u32 {
        u32::try_from(self.max_y.saturating_sub(self.min_y)).unwrap_or(0)
    }

    /// Region-local [`MapBounds`] matching this bounding box's width and height.
    #[must_use]
    pub fn map_bounds(self) -> MapBounds {
        MapBounds {
            width: self.width_tiles(),
            height: self.height_tiles(),
        }
    }

    /// Center of the bounding box in continuous face tile coordinates.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "face tile coordinates <= 2^21 are exact in f64"
    )]
    pub fn center_tile(self) -> (f64, f64) {
        (
            (self.min_x + self.max_x) as f64 * 0.5,
            (self.min_y + self.max_y) as f64 * 0.5,
        )
    }

    /// Whether `position` lies inside this bounding box expanded by `margin` tiles on each side.
    #[must_use]
    pub fn contains_grid_pos(self, position: GridPos, margin: i64) -> bool {
        let px = i64::from(position.x);
        let py = i64::from(position.y);
        px >= self.min_x.saturating_sub(margin)
            && px < self.max_x.saturating_add(margin)
            && py >= self.min_y.saturating_sub(margin)
            && py < self.max_y.saturating_add(margin)
    }

    /// Translates a face-space [`GridPos`] inside this bounding box into region-local coordinates.
    #[must_use]
    pub fn to_local_pos(self, position: GridPos) -> Option<GridPos> {
        let px = i64::from(position.x);
        let py = i64::from(position.y);
        if px < self.min_x || px >= self.max_x || py < self.min_y || py >= self.max_y {
            return None;
        }
        let lx = u32::try_from(px - self.min_x).ok()?;
        let ly = u32::try_from(py - self.min_y).ok()?;
        Some(GridPos::new(lx, ly))
    }
}

/// Validated authored map region definition parsed from JSON (CPU-only, no GPU assets).
#[derive(Clone, Debug)]
pub struct AuthoredRegion {
    /// Unique identifier of this authored region.
    pub id: String,
    /// Cube face containing this region's footprint.
    pub face: CubeFace,
    /// Simple polygon vertices in face 5-foot tile coordinates.
    pub polygon: Vec<[i64; 2]>,
    /// Axis-aligned bounding box of `polygon` in face tile coordinates.
    pub bounds: RegionBounds,
    /// Inward boundary feather distance in grid squares.
    pub edge_feather: f32,
    /// Region-local terrain materials, brush layers, and entity visual rules.
    pub config: MapConfig,
}

impl AuthoredRegion {
    /// Validates a parsed [`AuthoredRegionConfig`] and computes its bounding box.
    ///
    /// # Errors
    /// Returns a descriptive error if the id is empty, the polygon has fewer than 3 vertices,
    /// any vertex leaves `[0, FACE_TILES]`, the bounding box is degenerate, `edge_feather` is
    /// invalid, or `map.materials` is empty.
    pub fn from_config(config: AuthoredRegionConfig) -> Result<Self, String> {
        if config.id.trim().is_empty() {
            return Err("authored region id must not be empty".to_owned());
        }
        if config.polygon.len() < 3 {
            return Err(format!(
                "region '{}' polygon must have at least 3 vertices",
                config.id
            ));
        }
        if !config.edge_feather.is_finite() || config.edge_feather < 0.0 {
            return Err(format!(
                "region '{}' edge_feather must be finite and non-negative",
                config.id
            ));
        }
        if config.map.materials.is_empty() {
            return Err(format!("region '{}' defines no materials", config.id));
        }

        let max_coord = i64::from(FACE_TILES);
        let mut min_x = i64::MAX;
        let mut min_y = i64::MAX;
        let mut max_x = i64::MIN;
        let mut max_y = i64::MIN;
        for &[vx, vy] in &config.polygon {
            if vx < 0 || vx > max_coord || vy < 0 || vy > max_coord {
                return Err(format!(
                    "region '{}' vertex [{vx}, {vy}] is outside [0, {max_coord}]",
                    config.id
                ));
            }
            min_x = min_x.min(vx);
            min_y = min_y.min(vy);
            max_x = max_x.max(vx);
            max_y = max_y.max(vy);
        }
        if max_x <= min_x || max_y <= min_y {
            return Err(format!(
                "region '{}' polygon has zero-area bounding box",
                config.id
            ));
        }

        Ok(Self {
            id: config.id,
            face: config.face,
            polygon: config.polygon,
            bounds: RegionBounds {
                min_x,
                min_y,
                max_x,
                max_y,
            },
            edge_feather: config.edge_feather,
            config: config.map,
        })
    }

    /// Converts this region into a pure `zeta_core` [`RegionFootprint`] for overlap checks.
    #[must_use]
    pub fn footprint(&self) -> RegionFootprint {
        RegionFootprint {
            id: self.id.clone(),
            face: self.face,
            polygon: self.polygon.clone(),
        }
    }

    /// Whether the center of `position` lies inside or on the boundary of this region's polygon.
    #[must_use]
    pub fn contains_tile(&self, position: GridPos) -> bool {
        if !self.bounds.contains_grid_pos(position, 0) {
            return false;
        }
        point_in_polygon(
            [i64::from(position.x), i64::from(position.y)],
            &self.polygon,
        )
    }

    /// Rasterizes the soft polygon alpha mask image for this region.
    #[must_use]
    pub fn build_mask_image(&self) -> Image {
        build_polygon_mask(&self.polygon, self.bounds, self.edge_feather)
    }
}

/// GPU-ready authored region with preloaded repeating terrain textures, entity sprites, and
/// soft polygon alpha mask.
#[derive(Clone, Debug)]
pub struct LoadedAuthoredRegion {
    /// Validated region geometry and visual configuration.
    pub region: AuthoredRegion,
    /// Preloaded repeating terrain texture handles for slots 0..4.
    pub material_textures: [Handle<Image>; MAX_MATERIALS],
    /// Per-slot UV scale multipliers for slots 0..4.
    pub material_scales: Vec4,
    /// Preloaded entity style rules paired with their optional sprite handles.
    pub entity_styles: Vec<(EntityStyleRule, Option<Handle<Image>>)>,
    /// Pre-rasterized soft polygon mask image handle.
    pub mask_handle: Handle<Image>,
}

impl LoadedAuthoredRegion {
    /// Per-region ambient lighting tint as a `Vec4` uniform for `TerrainSplatMaterial`.
    #[must_use]
    pub fn ambient_vec4(&self) -> Vec4 {
        let [r, g, b] = self.region.config.ambient_tint;
        Vec4::new(r, g, b, 1.0)
    }

    /// Per-region ambient lighting tint as a Bevy `Color` for entity sprites.
    #[must_use]
    pub fn ambient_color(&self) -> Color {
        let [r, g, b] = self.region.config.ambient_tint;
        Color::srgb(r, g, b)
    }

    /// Returns the first matching entity style rule and its preloaded sprite handle.
    #[must_use]
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

/// Validated non-overlapping collection of loaded authored regions stored as a Bevy resource.
#[derive(Resource, Clone, Debug, Default)]
pub struct LoadedAuthoredRegions {
    /// All loaded authored regions, sorted deterministically by region filename.
    pub regions: Vec<LoadedAuthoredRegion>,
}

impl LoadedAuthoredRegions {
    /// Pure footprints for syncing into `PlanetRegions`.
    #[must_use]
    pub fn footprints(&self) -> Vec<RegionFootprint> {
        self.regions
            .iter()
            .map(|loaded| loaded.region.footprint())
            .collect()
    }

    /// Iterates over regions on `face` whose bounding box lies within `radius_tiles` of `origin`.
    pub fn regions_near(
        &self,
        face: CubeFace,
        origin: FloatingOrigin,
        radius_tiles: f64,
    ) -> impl Iterator<Item = &LoadedAuthoredRegion> {
        self.regions.iter().filter(move |loaded| {
            if loaded.region.face != face {
                return false;
            }
            let b = loaded.region.bounds;
            #[expect(
                clippy::cast_precision_loss,
                reason = "face tile coordinates <= 2^21 are exact in f64"
            )]
            let (min_x, max_x, min_y, max_y) = (
                b.min_x as f64,
                b.max_x as f64,
                b.min_y as f64,
                b.max_y as f64,
            );
            origin.tile_x >= min_x - radius_tiles
                && origin.tile_x <= max_x + radius_tiles
                && origin.tile_y >= min_y - radius_tiles
                && origin.tile_y <= max_y + radius_tiles
        })
    }

    /// Finds the matching entity style rule and ambient tint for an entity at `(face, position)`.
    ///
    /// Checks regions containing `position` first, then falls back to any region's style rule so
    /// entities pushed slightly past a region's border keep their visual appearance.
    #[must_use]
    pub fn style_for_at(
        &self,
        face: CubeFace,
        position: GridPos,
        name: Option<&EntityName>,
        tags: Option<&Tags>,
    ) -> Option<(&EntityStyleRule, Option<&Handle<Image>>, Color)> {
        for loaded in &self.regions {
            if loaded.region.face == face
                && loaded.region.contains_tile(position)
                && let Some((rule, handle)) = loaded.style_for(name, tags)
            {
                return Some((rule, handle, loaded.ambient_color()));
            }
        }
        for loaded in &self.regions {
            if loaded.region.face == face
                && let Some((rule, handle)) = loaded.style_for(name, tags)
            {
                return Some((rule, handle, loaded.ambient_color()));
            }
        }
        None
    }
}

/// Reads, parses, and validates every `*.json` authored region file in `dir`.
///
/// Validates unique region ids and checks every pair of polygons with
/// [`validate_non_overlapping`], returning an error listing all collisions on any overlap.
///
/// # Errors
/// Returns an error if `dir` cannot be read, any JSON file is malformed or invalid, duplicate
/// region ids exist, or any 2 region polygons overlap.
pub fn read_authored_regions_from_dir(dir: &Path) -> Result<Vec<AuthoredRegion>, String> {
    let entries = std::fs::read_dir(dir).map_err(|error| {
        format!(
            "could not read regions directory {}: {error}",
            dir.display()
        )
    })?;
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    paths.sort();

    let mut regions = Vec::with_capacity(paths.len());
    let mut seen_ids = HashSet::with_capacity(paths.len());
    for path in paths {
        let raw = std::fs::read_to_string(&path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        let parsed: AuthoredRegionConfig = serde_json::from_str(&raw)
            .map_err(|error| format!("invalid region config {}: {error}", path.display()))?;
        let region = AuthoredRegion::from_config(parsed)?;
        if !seen_ids.insert(region.id.clone()) {
            return Err(format!(
                "duplicate authored region id '{}' in {}",
                region.id,
                path.display()
            ));
        }
        regions.push(region);
    }

    let footprints: Vec<RegionFootprint> = regions.iter().map(AuthoredRegion::footprint).collect();
    if let Err(overlaps) = validate_non_overlapping(&footprints) {
        let details: Vec<String> = overlaps.iter().map(ToString::to_string).collect();
        return Err(format!("authored regions overlap: {}", details.join("; ")));
    }

    Ok(regions)
}

/// Loads all validated authored regions from `dir` and preloads their GPU textures and masks.
///
/// # Errors
/// Returns any parsing or non-overlap validation error from [`read_authored_regions_from_dir`].
pub fn load_authored_regions(
    asset_server: &AssetServer,
    images: &mut Assets<Image>,
    dir: &Path,
) -> Result<LoadedAuthoredRegions, String> {
    let regions = read_authored_regions_from_dir(dir)?;
    let mut loaded = Vec::with_capacity(regions.len());

    for region in regions {
        let first_tex =
            load_repeating_texture(asset_server, region.config.materials[0].texture.clone());
        let mut material_textures = [
            first_tex.clone(),
            first_tex.clone(),
            first_tex.clone(),
            first_tex,
        ];
        let mut scales = [1.0_f32; MAX_MATERIALS];
        for (slot, material) in region
            .config
            .materials
            .iter()
            .take(MAX_MATERIALS)
            .enumerate()
        {
            material_textures[slot] =
                load_repeating_texture(asset_server, material.texture.clone());
            scales[slot] = material.scale;
        }
        let entity_styles = region
            .config
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
        // `Assets::add`: bevy_asset-0.20.1/src/assets.rs:401.
        let mask_handle = images.add(region.build_mask_image());

        loaded.push(LoadedAuthoredRegion {
            region,
            material_textures,
            material_scales: Vec4::from_array(scales),
            entity_styles,
            mask_handle,
        });
    }

    Ok(LoadedAuthoredRegions { regions: loaded })
}

#[expect(
    clippy::cast_precision_loss,
    reason = "region dimensions and mask pixel counts stay far below 2^23"
)]
fn count_to_f32(value: u32) -> f32 {
    value as f32
}

#[expect(
    clippy::cast_precision_loss,
    reason = "local polygon offsets inside an authored region stay far below 2^23"
)]
fn local_offset_to_f32(value: i64) -> f32 {
    value as f32
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is clamped to [0.0, 1.0] before scaling to 0..=255"
)]
fn alpha_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    if (edge1 - edge0).abs() <= f32::EPSILON {
        return if value < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
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

fn point_in_ring_f32(point: Vec2, ring: &[Vec2]) -> bool {
    let count = ring.len();
    if count < 3 {
        return false;
    }
    let mut inside = false;
    for idx in 0..count {
        let start = ring[idx];
        let end = ring[(idx + 1) % count];
        let crosses_y = (start.y > point.y) != (end.y > point.y);
        if crosses_y {
            let intersect_x = start.x + (point.y - start.y) * (end.x - start.x) / (end.y - start.y);
            if point.x < intersect_x {
                inside = !inside;
            }
        }
    }
    inside
}

/// Rasterizes an arbitrary simple polygon (convex or concave) into an RGBA8 mask image over
/// `bounds`, with `0` outside the polygon and a smooth inward feather of `edge_feather` tiles.
#[must_use]
pub fn build_polygon_mask(polygon: &[[i64; 2]], bounds: RegionBounds, edge_feather: f32) -> Image {
    let width = bounds.width_tiles().max(1) * SPLAT_PIXELS_PER_TILE;
    let height = bounds.height_tiles().max(1) * SPLAT_PIXELS_PER_TILE;
    let scale = count_to_f32(SPLAT_PIXELS_PER_TILE);

    let mut ring: Vec<Vec2> = polygon
        .iter()
        .map(|&[vx, vy]| {
            Vec2::new(
                local_offset_to_f32(vx - bounds.min_x),
                local_offset_to_f32(vy - bounds.min_y),
            )
        })
        .collect();
    if ring.len() > 1 && ring.first() == ring.last() {
        ring.pop();
    }

    let mut data = vec![0_u8; (width * height * 4) as usize];
    let edge_count = ring.len();

    for py in 0..height {
        let tile_y = (count_to_f32(py) + 0.5) / scale;
        for px in 0..width {
            let tile_x = (count_to_f32(px) + 0.5) / scale;
            let point = Vec2::new(tile_x, tile_y);

            let alpha = if point_in_ring_f32(point, &ring) {
                if edge_feather <= 0.0 || edge_count < 3 {
                    1.0
                } else {
                    let mut min_dist = f32::INFINITY;
                    for idx in 0..edge_count {
                        let d = point_to_segment_distance(
                            point,
                            ring[idx],
                            ring[(idx + 1) % edge_count],
                        );
                        min_dist = min_dist.min(d);
                    }
                    smoothstep(0.0, edge_feather, min_dist)
                }
            } else {
                0.0
            };

            let byte = alpha_to_u8(alpha);
            let index = ((py * width + px) * 4) as usize;
            data[index] = byte;
            data[index + 1] = byte;
            data[index + 2] = byte;
            data[index + 3] = byte;
        }
    }

    // `Image::new`: bevy_image-0.20.1/src/image.rs:1123.
    // `TextureFormat::Rgba8Unorm`: wgpu-types-30.0.2/src/texture/format.rs:185.
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

    fn sample_mask(image: &Image, bounds: RegionBounds, tile_x: u32, tile_y: u32) -> u8 {
        let data = image.data.as_ref().expect("mask bytes");
        let width = bounds.width_tiles() * SPLAT_PIXELS_PER_TILE;
        let px = tile_x * SPLAT_PIXELS_PER_TILE + SPLAT_PIXELS_PER_TILE / 2;
        let py = tile_y * SPLAT_PIXELS_PER_TILE + SPLAT_PIXELS_PER_TILE / 2;
        data[((py * width + px) * 4) as usize]
    }

    fn minimal_region_json(id: &str, polygon: &str) -> String {
        format!(
            r#"{{
                "id": "{id}",
                "face": "pos_x",
                "polygon": {polygon},
                "materials": [{{ "id": "land", "texture": "terrain/grass.png" }}]
            }}"#
        )
    }

    #[test]
    fn every_authored_region_in_assets_parses_is_non_overlapping_and_references_existing_files() {
        let root = assets_root();
        let regions = read_authored_regions_from_dir(&world_regions_dir())
            .expect("all assets/world/regions/*.json must parse and be non-overlapping");
        assert!(
            !regions.is_empty(),
            "at least 1 authored region exists in assets/world/regions"
        );

        let footprints: Vec<RegionFootprint> =
            regions.iter().map(AuthoredRegion::footprint).collect();
        assert_eq!(validate_non_overlapping(&footprints), Ok(()));

        for region in &regions {
            for material in &region.config.materials {
                assert!(
                    root.join(&material.texture).is_file(),
                    "region '{}': missing terrain texture {}",
                    region.id,
                    material.texture
                );
            }
            for rule in &region.config.entity_styles {
                if let Some(sprite) = &rule.sprite {
                    assert!(
                        root.join(sprite).is_file(),
                        "region '{}': missing entity sprite {sprite}",
                        region.id
                    );
                }
            }
        }
    }

    #[test]
    fn overlapping_regions_are_rejected_and_adjacent_regions_are_accepted() {
        let base =
            std::env::temp_dir().join(format!("zeta_planet_region_test_{}", std::process::id()));
        let ok_dir = base.join("adjacent");
        let bad_dir = base.join("overlap");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&ok_dir).expect("create ok dir");
        std::fs::create_dir_all(&bad_dir).expect("create bad dir");

        std::fs::write(
            ok_dir.join("west.json"),
            minimal_region_json("west", "[[100, 100], [120, 100], [120, 120], [100, 120]]"),
        )
        .expect("write west");
        std::fs::write(
            ok_dir.join("east.json"),
            minimal_region_json("east", "[[120, 100], [140, 100], [140, 120], [120, 120]]"),
        )
        .expect("write east");

        let adjacent =
            read_authored_regions_from_dir(&ok_dir).expect("edge-sharing regions are valid");
        assert_eq!(adjacent.len(), 2);

        std::fs::write(
            bad_dir.join("alpha.json"),
            minimal_region_json("alpha", "[[100, 100], [120, 100], [120, 120], [100, 120]]"),
        )
        .expect("write alpha");
        std::fs::write(
            bad_dir.join("beta.json"),
            minimal_region_json("beta", "[[110, 110], [130, 110], [130, 130], [110, 130]]"),
        )
        .expect("write beta");

        let err = read_authored_regions_from_dir(&bad_dir)
            .expect_err("overlapping regions must be rejected");
        assert!(err.contains("alpha") && err.contains("beta"), "{err}");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn polygon_mask_handles_concave_shapes_and_feathers_edges() {
        // Concave L-shape occupying [0..12, 0..12] minus the [6..12, 0..6] top-right bay.
        let l_poly = vec![
            [1000, 2000],
            [1006, 2000],
            [1006, 2006],
            [1012, 2006],
            [1012, 2012],
            [1000, 2012],
        ];
        let bounds = RegionBounds {
            min_x: 1000,
            min_y: 2000,
            max_x: 1012,
            max_y: 2012,
        };
        let mask = build_polygon_mask(&l_poly, bounds, 1.5);

        // Deep inside the vertical leg (local tile 3, 3 -> >= 2.5 tiles from all edges): solid.
        assert_eq!(sample_mask(&mask, bounds, 3, 3), 255);
        // Deep inside the bottom-right foot (local tile 9, 9): solid.
        assert_eq!(sample_mask(&mask, bounds, 9, 9), 255);
        // Near the left boundary (local tile 0, 3 -> 0.5 tiles from x=0 with feather 1.5): feathered.
        let edge_alpha = sample_mask(&mask, bounds, 0, 3);
        assert!(
            edge_alpha > 0 && edge_alpha < 255,
            "expected soft feathering near boundary, got {edge_alpha}"
        );
        // Inside the concave top-right cutout (local tile 9, 2): strictly transparent.
        assert_eq!(sample_mask(&mask, bounds, 9, 2), 0);
    }
}
