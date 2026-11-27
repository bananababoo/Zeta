//! Terrain splat bake: control map, shade map, and ground facts of 1 streamed planet chunk.
//!
//! Contract: `terrain_contract.md` Section 4. The base ground of every tile comes from
//! [`ground_cover`]; the texture variants of 1 cover cross-fade by [`variant_value`] noise
//! ([`TerrainPalette::variant_mix`]), and each tile's ground fact is the variant at its center
//! ([`TerrainPalette::variant_at`]). Control
//! texels blend the 4 nearest tile covers with smoothstep weights, and the blend position is
//! displaced by value noise in face texel coordinates, so 5-foot squares never read as blocks.
//! Authored regions then composite only their painted layers on top, faded by the polygon's
//! `edge_feather`.
//!
//! Every value a texel or tile depends on is computed from face tile coordinates, never from
//! the chunk it belongs to, so adjacent chunks agree exactly on the texels they share.
//!
//! Every streamed planet chunk entity drawn by the splat path carries a [`ChunkGroundCover`].
//! It is the only input clutter placement reads, so its shape is a contract: change it only
//! through the coordinator.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::image::{Image, ImageSampler};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use zeta_core::world::{
    CHUNK_SIZE, ChunkCoord, CubeFace, GroundCover, GroundCoverInput, PlanetClimateMaps,
    PlanetHeightmaps, SHORE_DISTANCE_MAX_TILES, ground_cover, sample_climate, sample_tile,
    shore_distances, step_tile, tile_to_unit_vec,
};

use crate::planet_region::{RegionBounds, polygon_alpha};
use crate::planet_stream::spherical_hillshade;
use crate::terrain::{
    ResolvedLayer, SPLAT_PIXELS_PER_TILE, count_to_f32, layer_coverage, smoothstep,
};
use crate::terrain_palette::{
    MAX_PALETTE_ENTRIES, TerrainPalette, value_noise_unit, variant_value,
};

/// Tiles per chunk side as a `usize` index bound.
pub const CHUNK_TILES: usize = CHUNK_SIZE as usize;
/// Tiles per chunk.
pub const CHUNK_TILE_COUNT: usize = CHUNK_TILES * CHUNK_TILES;
/// Smallest on-screen tile size in pixels at which chunks draw through the splat material;
/// below it 1 control texel is smaller than a screen pixel and the color raster is used.
pub const SPLAT_MIN_TILE_PIXELS: f32 = 8.0;
/// Control texels per tile as an index.
const TEXELS_PER_TILE: usize = SPLAT_PIXELS_PER_TILE as usize;
/// Control texels per tile as a signed coordinate.
const TEXELS_PER_TILE_I64: i64 = SPLAT_PIXELS_PER_TILE as i64;
/// Control map side in texels: 8 texels per tile plus a 1-texel halo on each side.
pub const CONTROL_SIDE: usize = CHUNK_TILES * TEXELS_PER_TILE + 2;
/// Shade map side in texels: 1 texel per tile plus a 1-tile halo on each side.
pub const SHADE_SIDE: usize = CHUNK_TILES + 2;
/// Cover grid halo in tiles: edge noise moves a texel's 4 blend cells up to 2 tiles away
/// from its own tile, and halo texels sit 1 tile outside the chunk.
const COVER_HALO: usize = 3;
/// Cover grid side in tiles: chunk tiles plus [`COVER_HALO`] on each side.
const COVER_SIDE: usize = CHUNK_TILES + 2 * COVER_HALO;
/// Padding that makes [`shore_distances`] exact inside the cover grid.
const SHORE_PAD: usize = SHORE_DISTANCE_MAX_TILES as usize;
/// Side of the sampled tile window: cover grid plus shore padding on each side.
const WINDOW_SIDE: usize = COVER_SIDE + 2 * SHORE_PAD;
/// Tiles between the window's top-left tile and the chunk origin tile.
const WINDOW_OFFSET: i64 = COVER_HALO as i64 + SHORE_DISTANCE_MAX_TILES as i64;
/// Cover weights reach 0 and 1 this far from the midpoint between 2 tile centers.
const BLEND_EDGE: (f32, f32) = (0.1, 0.9);
/// Edge noise octaves: lattice spacing in control texels and amplitude in tiles. The sum
/// stays below 1 tile; it is large enough that cover borders meander across tile lines.
const WARP_OCTAVES: [(i64, f32); 3] = [(32, 0.55), (8, 0.3), (3, 0.1)];
/// [`hash_unit`] salt of the horizontal edge noise.
const WARP_SALT_X: u64 = 0x7370_6c61_7477_7801;
/// [`hash_unit`] salt of the vertical edge noise.
const WARP_SALT_Y: u64 = 0x7370_6c61_7477_7901;
/// Shade tint of dry land (moisture 0) and wet land (moisture 1).
const DRY_TINT: [f32; 3] = [1.04, 1.0, 0.92];
const WET_TINT: [f32; 3] = [0.95, 1.0, 1.0];
/// A tile counts as region painted when its painted coverage reaches this value.
const REGION_PAINTED_MIN: f32 = 0.5;

/// Ground facts of the 64×64 tiles of 1 streamed chunk.
///
/// Arrays are row-major: tile `(local_x, local_y)` is at `local_y * 64 + local_x`, the same
/// local coordinates as `PlanetTile::local_in_chunk` and `ChunkTerrain::tile`. Values are
/// taken at tile centers without the splat's edge noise.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct ChunkGroundCover {
    /// Chunk these facts describe.
    pub coord: ChunkCoord,
    /// Dominant palette index per tile (`TerrainPalette::entry`), including authored region
    /// paint.
    pub dominant: Box<[u8; CHUNK_TILE_COUNT]>,
    /// Whether an authored region's painted layer covers at least half of the tile center.
    pub region_painted: Box<[bool; CHUNK_TILE_COUNT]>,
}

impl ChunkGroundCover {
    /// Row-major array index of `(local_x, local_y)`, or `None` outside the chunk.
    #[must_use]
    pub fn index(local_x: u32, local_y: u32) -> Option<usize> {
        let (x, y) = (
            usize::try_from(local_x).ok()?,
            usize::try_from(local_y).ok()?,
        );
        (x < CHUNK_TILES && y < CHUNK_TILES).then_some(y * CHUNK_TILES + x)
    }

    /// Dominant palette index of `(local_x, local_y)`.
    #[must_use]
    pub fn dominant_at(&self, local_x: u32, local_y: u32) -> Option<u8> {
        Self::index(local_x, local_y).map(|index| self.dominant[index])
    }

    /// Whether region paint covers `(local_x, local_y)`.
    #[must_use]
    pub fn region_painted_at(&self, local_x: u32, local_y: u32) -> Option<bool> {
        Self::index(local_x, local_y).map(|index| self.region_painted[index])
    }
}

/// Painted layers of 1 authored region, resolved against the palette and live entities.
#[derive(Clone, Debug)]
pub struct RegionPaint {
    /// Cube face of the region.
    pub face: CubeFace,
    /// Polygon bounding box in face tile coordinates; layers are local to `(min_x, min_y)`.
    pub bounds: RegionBounds,
    /// Polygon ring in region-local tiles.
    pub ring: Vec<Vec2>,
    /// Inward fade width of the polygon edge in tiles.
    pub edge_feather: f32,
    /// Brush layers in paint order.
    pub layers: Vec<ResolvedLayer>,
}

impl RegionPaint {
    /// Whether the region's bounding box, grown by 1 tile, overlaps the chunk's tiles.
    #[must_use]
    pub fn touches(&self, coord: ChunkCoord) -> bool {
        if self.face != coord.face {
            return false;
        }
        let origin = coord.origin_tile();
        let (x0, y0) = (i64::from(origin.x), i64::from(origin.y));
        let size = i64::from(CHUNK_SIZE);
        self.bounds.min_x <= x0 + size
            && self.bounds.max_x >= x0
            && self.bounds.min_y <= y0 + size
            && self.bounds.max_y >= y0
    }
}

/// Every authored region's paint, shared with chunk bake tasks.
#[derive(Resource, Clone, Debug, Default)]
pub struct ChunkRegionPaint {
    /// Regions in load order.
    pub regions: Arc<Vec<RegionPaint>>,
    /// Incremented on every change, so chunks touching a region re-bake.
    pub generation: u64,
}

impl ChunkRegionPaint {
    /// Bake key of `coord`: `0` when no region touches it, otherwise a value that changes
    /// whenever [`Self::generation`] changes.
    #[must_use]
    pub fn key_for(&self, coord: ChunkCoord) -> u64 {
        let mut touching = self
            .regions
            .iter()
            .enumerate()
            .filter(|(_, region)| region.touches(coord))
            .peekable();
        if touching.peek().is_none() {
            return 0;
        }
        let mut hasher = DefaultHasher::new();
        self.generation.hash(&mut hasher);
        for (index, _) in touching {
            index.hash(&mut hasher);
        }
        hasher.finish().max(1)
    }
}

/// CPU output of [`bake_chunk_splat`], uploaded as the chunk's material textures.
#[derive(Clone, Debug)]
pub struct ChunkSplatBake {
    /// [`CONTROL_SIDE`]² `Rgba8Unorm` texels: layer A index, layer B index, B weight,
    /// brightness.
    pub control: Vec<u8>,
    /// [`SHADE_SIDE`]² `Rgba8Unorm` texels: hillshade × climate tint / 2.
    pub shade: Vec<u8>,
    /// Ground facts of the chunk's tiles.
    pub ground: ChunkGroundCover,
}

/// Control and shade maps of a standalone (non-planet) map rectangle.
#[derive(Clone, Debug)]
pub struct RectSplat {
    /// `Rgba8Unorm` control texels, `control_size` wide and high.
    pub control: Vec<u8>,
    /// Control map size in texels.
    pub control_size: UVec2,
    /// `Rgba8Unorm` shade texels (neutral), `shade_size` wide and high.
    pub shade: Vec<u8>,
    /// Shade map size in texels.
    pub shade_size: UVec2,
}

/// Wraps baked `Rgba8Unorm` splat texels of `size` in a linear-data [`Image`] for the render
/// world.
#[must_use]
pub fn splat_texture(data: Vec<u8>, size: UVec2, sampler: ImageSampler) -> Image {
    // `Image::new`: bevy_image-0.20.1/src/image.rs:1123.
    // `TextureFormat::Rgba8Unorm`: wgpu-types-30.0.2/src/texture/format.rs:185.
    // `RenderAssetUsages::RENDER_WORLD`: bevy_asset-0.20.1/src/render_asset.rs:34.
    let mut image = Image::new(
        Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = sampler;
    image
}

/// Bakes the splat inputs of the chunk at `coord`.
#[must_use]
pub fn bake_chunk_splat(
    maps: &PlanetHeightmaps,
    climate: &PlanetClimateMaps,
    palette: &TerrainPalette,
    coord: ChunkCoord,
    regions: &[RegionPaint],
) -> ChunkSplatBake {
    let (cover, shade) = bake_cover_and_shade(maps, climate, palette, coord);
    let origin = coord.origin_tile();
    let touching: Vec<RegionInChunk<'_>> = regions
        .iter()
        .filter(|region| region.touches(coord))
        .map(|paint| RegionInChunk {
            paint,
            offset: Vec2::new(
                i64_to_f32(i64::from(origin.x) - paint.bounds.min_x),
                i64_to_f32(i64::from(origin.y) - paint.bounds.min_y),
            ),
            size: Vec2::new(
                count_to_f32(paint.bounds.width_tiles()),
                count_to_f32(paint.bounds.height_tiles()),
            ),
        })
        .collect();

    let face_salt = u64::try_from(coord.face.index()).unwrap_or(0) << 56;
    let texel_x0 = i64::from(origin.x) * TEXELS_PER_TILE_I64 - 1;
    let texel_y0 = i64::from(origin.y) * TEXELS_PER_TILE_I64 - 1;
    let mut control = vec![0_u8; CONTROL_SIDE * CONTROL_SIDE * 4];
    for py in 0..CONTROL_SIDE {
        let rel_y = index_to_i64(py) - 1;
        for px in 0..CONTROL_SIDE {
            let rel_x = index_to_i64(px) - 1;
            let global = (texel_x0 + index_to_i64(px), texel_y0 + index_to_i64(py));
            let warp = Vec2::new(
                warp_noise(face_salt ^ WARP_SALT_X, global.0, global.1),
                warp_noise(face_salt ^ WARP_SALT_Y, global.0, global.1),
            );
            let mut weights = [0.0_f32; MAX_PALETTE_ENTRIES];
            let variant_noise = variant_value(
                coord.face,
                global.0 * 2 + 1,
                global.1 * 2 + 1,
                TEXELS_PER_TILE_I64 * 2,
            );
            add_cover_weights(
                palette,
                &cover,
                (rel_x, rel_y),
                warp,
                variant_noise,
                &mut weights,
            );
            let mut brightness = 1.0;
            paint_regions(
                &touching,
                texel_tile_position(rel_x, rel_y),
                &mut weights,
                &mut brightness,
            );
            let offset = (py * CONTROL_SIDE + px) * 4;
            control[offset..offset + 4].copy_from_slice(&encode_texel(&weights, brightness));
        }
    }

    let mut dominant = Box::new([0_u8; CHUNK_TILE_COUNT]);
    let mut region_painted = Box::new([false; CHUNK_TILE_COUNT]);
    for local_y in 0..CHUNK_TILES {
        for local_x in 0..CHUNK_TILES {
            let mut weights = [0.0_f32; MAX_PALETTE_ENTRIES];
            weights[palette_slot(
                cover[(local_y + COVER_HALO) * COVER_SIDE + local_x + COVER_HALO].tile_variant,
            )] = 1.0;
            let mut brightness = 1.0;
            let center = Vec2::new(
                i64_to_f32(index_to_i64(local_x)) + 0.5,
                i64_to_f32(index_to_i64(local_y)) + 0.5,
            );
            let painted = paint_regions(&touching, center, &mut weights, &mut brightness);
            let index = local_y * CHUNK_TILES + local_x;
            dominant[index] = top_two(&weights).0;
            region_painted[index] = painted >= REGION_PAINTED_MIN;
        }
    }

    ChunkSplatBake {
        control,
        shade,
        ground: ChunkGroundCover {
            coord,
            dominant,
            region_painted,
        },
    }
}

/// Bakes a standalone map of `width` × `height` tiles: `base` fills the ground and `layers`
/// paint over it. The shade map is neutral (no planet relief).
#[must_use]
pub fn bake_rect_splat(width: u32, height: u32, base: u8, layers: &[ResolvedLayer]) -> RectSplat {
    let control_size = UVec2::new(
        width * SPLAT_PIXELS_PER_TILE + 2,
        height * SPLAT_PIXELS_PER_TILE + 2,
    );
    let (cw, ch) = (
        usize::try_from(control_size.x).unwrap_or(0),
        usize::try_from(control_size.y).unwrap_or(0),
    );
    let mut control = vec![0_u8; cw * ch * 4];
    for py in 0..ch {
        for px in 0..cw {
            let position = texel_tile_position(index_to_i64(px) - 1, index_to_i64(py) - 1);
            let mut weights = [0.0_f32; MAX_PALETTE_ENTRIES];
            weights[palette_slot(base)] = 1.0;
            let mut brightness = 1.0;
            paint_layers(layers, position, 1.0, &mut weights, &mut brightness);
            let offset = (py * cw + px) * 4;
            control[offset..offset + 4].copy_from_slice(&encode_texel(&weights, brightness));
        }
    }
    let shade_size = UVec2::new(width + 2, height + 2);
    let shade_count =
        usize::try_from(shade_size.x).unwrap_or(0) * usize::try_from(shade_size.y).unwrap_or(0);
    RectSplat {
        control,
        control_size,
        shade: [128_u8, 128, 128, 255].repeat(shade_count),
        shade_size,
    }
}

/// Ground cover of 1 tile of the cover grid.
#[derive(Clone, Copy)]
struct CoverCell {
    /// Cover from [`ground_cover`]; texels blend its variants with [`TerrainPalette::variant_mix`].
    kind: GroundCover,
    /// Variant at the tile center ([`TerrainPalette::variant_at`]), the tile's ground fact.
    tile_variant: u8,
}

/// Cover cells over the [`COVER_SIDE`]² grid (cover cell `(cx, cy)` is chunk tile
/// `(cx - COVER_HALO, cy - COVER_HALO)`) and shade texels over the [`SHADE_SIDE`]² grid
/// (chunk tiles plus a 1-tile halo).
fn bake_cover_and_shade(
    maps: &PlanetHeightmaps,
    climate: &PlanetClimateMaps,
    palette: &TerrainPalette,
    coord: ChunkCoord,
) -> (Vec<CoverCell>, Vec<u8>) {
    let origin = coord.origin_tile();
    let mut tiles = Vec::with_capacity(WINDOW_SIDE * WINDOW_SIDE);
    let mut samples = Vec::with_capacity(WINDOW_SIDE * WINDOW_SIDE);
    for wy in 0..WINDOW_SIDE {
        for wx in 0..WINDOW_SIDE {
            let (tile, _) = step_tile(
                origin,
                index_to_i64(wx) - WINDOW_OFFSET,
                index_to_i64(wy) - WINDOW_OFFSET,
            );
            tiles.push(tile);
            samples.push(sample_tile(maps, tile));
        }
    }
    let water: Vec<bool> = samples.iter().map(|sample| sample.is_water).collect();
    let shore = shore_distances(&water, WINDOW_SIDE, WINDOW_SIDE);

    let basis = coord.face.basis();
    let face_right = basis.right_u.map(f64::from);
    let face_down = basis.down_v.map(f64::from);
    let mut cover = vec![
        CoverCell {
            kind: GroundCover::ALL[0],
            tile_variant: 0,
        };
        COVER_SIDE * COVER_SIDE
    ];
    let mut shade = vec![0_u8; SHADE_SIDE * SHADE_SIDE * 4];
    let shade_start = COVER_HALO - 1;
    for cy in 0..COVER_SIDE {
        for cx in 0..COVER_SIDE {
            let w = (cy + SHORE_PAD) * WINDOW_SIDE + cx + SHORE_PAD;
            let sample = samples[w];
            let neighbors = [w - 1, w + 1, w - WINDOW_SIDE, w + WINDOW_SIDE];
            let slope_step_ft = neighbors
                .iter()
                .map(|&n| (samples[n].elevation_step_ft - sample.elevation_step_ft).abs())
                .max()
                .unwrap_or(0);
            let tile_climate = sample_climate(climate, tiles[w]);
            let kind = ground_cover(&GroundCoverInput {
                tile: tiles[w],
                sample,
                climate: tile_climate,
                slope_step_ft,
                shore_distance_tiles: shore[w],
            });
            cover[cy * COVER_SIDE + cx] = CoverCell {
                kind,
                tile_variant: palette.variant_at(kind, tiles[w]),
            };

            let (Some(sx), Some(sy)) = (cx.checked_sub(shade_start), cy.checked_sub(shade_start))
            else {
                continue;
            };
            if sx >= SHADE_SIDE || sy >= SHADE_SIDE {
                continue;
            }
            let slope_east =
                f64::from(samples[w + 1].elevation_ft - samples[w - 1].elevation_ft) * 0.5;
            let slope_south = f64::from(
                samples[w + WINDOW_SIDE].elevation_ft - samples[w - WINDOW_SIDE].elevation_ft,
            ) * 0.5;
            let hillshade = spherical_hillshade(
                tile_to_unit_vec(tiles[w]),
                face_right,
                face_down,
                slope_east,
                slope_south,
            );
            let (factor, tint) = if sample.is_water {
                (0.88 + 0.24 * hillshade, [1.0; 3])
            } else {
                let wet = tile_climate.moisture.clamp(0.0, 1.0);
                (
                    0.70 + 0.52 * hillshade,
                    [0_usize, 1, 2].map(|c| DRY_TINT[c] + (WET_TINT[c] - DRY_TINT[c]) * wet),
                )
            };
            let offset = (sy * SHADE_SIDE + sx) * 4;
            for channel in 0..3 {
                shade[offset + channel] = unit_to_u8(f64_to_f32(factor) * tint[channel] * 0.5);
            }
            shade[offset + 3] = 255;
        }
    }
    (cover, shade)
}

/// 1 region touching the chunk being baked, with its offset into region-local tiles.
struct RegionInChunk<'a> {
    paint: &'a RegionPaint,
    /// Region-local position of the chunk origin.
    offset: Vec2,
    /// Region bounding box size in tiles.
    size: Vec2,
}

/// Chunk tile-space position of the center of control texel `(rel_x, rel_y)` (halo removed).
fn texel_tile_position(rel_x: i64, rel_y: i64) -> Vec2 {
    let per_tile = i64_to_f32(TEXELS_PER_TILE_I64);
    Vec2::new(
        (i64_to_f32(rel_x) + 0.5) / per_tile,
        (i64_to_f32(rel_y) + 0.5) / per_tile,
    )
}

/// Adds the smoothstep weights of the 4 cover cells around texel `(rel_x, rel_y)`, displaced
/// by `warp` tiles. Each cell's weight is shared among its cover's variants by
/// [`TerrainPalette::variant_mix`] at the texel's `variant_noise`.
///
/// The tile and sub-tile parts are kept apart, so the fraction depends only on the texel's
/// position inside its tile and the shared noise: neighboring chunks compute identical weights
/// for the texels they share.
fn add_cover_weights(
    palette: &TerrainPalette,
    cover: &[CoverCell],
    (rel_x, rel_y): (i64, i64),
    warp: Vec2,
    variant_noise: f32,
    weights: &mut [f32; MAX_PALETTE_ENTRIES],
) {
    let per_tile = i64_to_f32(TEXELS_PER_TILE_I64);
    let fraction = Vec2::new(
        (i64_to_f32(rel_x.rem_euclid(TEXELS_PER_TILE_I64)) + 0.5) / per_tile,
        (i64_to_f32(rel_y.rem_euclid(TEXELS_PER_TILE_I64)) + 0.5) / per_tile,
    );
    // Offset from the center of the lower tile: 0 at its center, 1 at the next one.
    let shifted = fraction + warp - Vec2::splat(0.5);
    let floor = shifted.floor();
    let blend = shifted - floor;
    let last = index_to_i64(COVER_SIDE - 2);
    let cell = |tile: i64, step: f32| {
        // Cover cell = chunk tile + COVER_HALO.
        let index = (tile + f32_floor_to_i64(step) + index_to_i64(COVER_HALO)).clamp(0, last);
        usize::try_from(index).unwrap_or(0)
    };
    let cx = cell(rel_x.div_euclid(TEXELS_PER_TILE_I64), floor.x);
    let cy = cell(rel_y.div_euclid(TEXELS_PER_TILE_I64), floor.y);
    let fx = smoothstep(BLEND_EDGE.0, BLEND_EDGE.1, blend.x);
    let fy = smoothstep(BLEND_EDGE.0, BLEND_EDGE.1, blend.y);
    let corners = [
        (cx, cy, (1.0 - fx) * (1.0 - fy)),
        (cx + 1, cy, fx * (1.0 - fy)),
        (cx, cy + 1, (1.0 - fx) * fy),
        (cx + 1, cy + 1, fx * fy),
    ];
    for (x, y, weight) in corners {
        for (index, share) in palette.variant_mix(cover[y * COVER_SIDE + x].kind, variant_noise) {
            weights[palette_slot(index)] += weight * share;
        }
    }
}

/// Composites every touching region at chunk tile-space `position`; returns the painted
/// coverage in `[0.0, 1.0]`.
fn paint_regions(
    regions: &[RegionInChunk<'_>],
    position: Vec2,
    weights: &mut [f32; MAX_PALETTE_ENTRIES],
    brightness: &mut f32,
) -> f32 {
    let mut uncovered = 1.0;
    for region in regions {
        let local = region.offset + position;
        if local.x < 0.0 || local.y < 0.0 || local.x > region.size.x || local.y > region.size.y {
            continue;
        }
        let alpha = polygon_alpha(local, &region.paint.ring, region.paint.edge_feather);
        if alpha <= 0.0 {
            continue;
        }
        uncovered *= paint_layers(&region.paint.layers, local, alpha, weights, brightness);
    }
    1.0 - uncovered
}

/// Paints `layers` at layer-local `position` scaled by `alpha`; returns the fraction left
/// uncovered.
fn paint_layers(
    layers: &[ResolvedLayer],
    position: Vec2,
    alpha: f32,
    weights: &mut [f32; MAX_PALETTE_ENTRIES],
    brightness: &mut f32,
) -> f32 {
    let mut uncovered = 1.0;
    for layer in layers {
        let (weight, rim) = layer_coverage(layer, position);
        let amount = (alpha * weight).clamp(0.0, 1.0);
        if amount > 0.0 {
            for value in weights.iter_mut() {
                *value *= 1.0 - amount;
            }
            weights[palette_slot(layer.palette_index)] += amount;
            uncovered *= 1.0 - amount;
        }
        *brightness *= 1.0 - (alpha * rim).clamp(0.0, 1.0);
    }
    uncovered
}

/// Palette indices of the 2 heaviest weights and their weights (B equals A when only 1
/// weight is positive).
fn top_two(weights: &[f32; MAX_PALETTE_ENTRIES]) -> (u8, u8, f32, f32) {
    let (mut first, mut second) = (0_usize, 0_usize);
    let (mut first_weight, mut second_weight) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for (index, &weight) in weights.iter().enumerate() {
        if weight > first_weight {
            (second, second_weight) = (first, first_weight);
            (first, first_weight) = (index, weight);
        } else if weight > second_weight {
            (second, second_weight) = (index, weight);
        }
    }
    if second_weight <= 0.0 {
        (second, second_weight) = (first, 0.0);
    }
    let to_u8 = |index: usize| u8::try_from(index).unwrap_or(0);
    (
        to_u8(first),
        to_u8(second),
        first_weight.max(0.0),
        second_weight,
    )
}

/// Control texel bytes for `weights` (see the shader header in `terrain_splat.wgsl`).
fn encode_texel(weights: &[f32; MAX_PALETTE_ENTRIES], brightness: f32) -> [u8; 4] {
    let (first, second, first_weight, second_weight) = top_two(weights);
    let total = first_weight + second_weight;
    let blend = if total > 0.0 {
        second_weight / total
    } else {
        0.0
    };
    [first, second, unit_to_u8(blend), unit_to_u8(brightness)]
}

/// Weight slot of a palette index (indices past the array clamp to the last slot).
fn palette_slot(index: u8) -> usize {
    usize::from(index).min(MAX_PALETTE_ENTRIES - 1)
}

/// Edge noise in tiles at face control texel `(x, y)`, in about `[-0.95, 0.95]`.
fn warp_noise(salt: u64, x: i64, y: i64) -> f32 {
    WARP_OCTAVES
        .iter()
        .zip(0_u64..)
        .map(|(&(spacing, amplitude), octave)| {
            value_noise(salt.wrapping_add(octave), x, y, spacing) * amplitude
        })
        .sum()
}

/// Smooth value noise in `[-1.0, 1.0]` over a lattice every `spacing` texels.
fn value_noise(salt: u64, x: i64, y: i64, spacing: i64) -> f32 {
    value_noise_unit(salt, x, y, spacing) * 2.0 - 1.0
}

fn index_to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "chunk and region offsets stay far below 2^23"
)]
fn i64_to_f32(value: i64) -> f32 {
    value as f32
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "hillshade factors lie in [0, 2], well inside f32 range"
)]
fn f64_to_f32(value: f64) -> f32 {
    value as f32
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "callers pass floors of small offsets in [-2, 2]"
)]
fn f32_floor_to_i64(value: f32) -> i64 {
    value.floor() as i64
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is clamped into [0, 255] before the cast"
)]
fn unit_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::{MapConfig, resolve_layers};
    use zeta_core::world::{FACE_CHUNKS, FaceClimateMap, FaceHeightmap, SEA_LEVEL_U16};

    const MAP_RES: u32 = 16;

    fn flat_planet(elevation_ft: i32) -> PlanetHeightmaps {
        let encoded = u16::try_from(i32::from(SEA_LEVEL_U16) + elevation_ft).expect("in range");
        let side = usize::try_from(MAP_RES * MAP_RES).expect("fits");
        let faces = CubeFace::ALL
            .map(|_| FaceHeightmap::new(MAP_RES, vec![encoded; side]).expect("valid face"));
        PlanetHeightmaps::new(faces).expect("valid planet")
    }

    fn temperate_climate() -> PlanetClimateMaps {
        let side = usize::try_from(MAP_RES * MAP_RES).expect("fits");
        let faces = CubeFace::ALL
            .map(|_| FaceClimateMap::new(MAP_RES, vec![[180, 130, 60, 0]; side]).expect("valid"));
        PlanetClimateMaps::new(faces).expect("valid climate")
    }

    fn palette() -> TerrainPalette {
        TerrainPalette::load().expect("palette loads")
    }

    fn mid_chunk(dx: u32, dy: u32) -> ChunkCoord {
        ChunkCoord {
            face: CubeFace::PosZ,
            cx: FACE_CHUNKS / 2 + dx,
            cy: FACE_CHUNKS / 2 + dy,
        }
    }

    fn texel(bake: &ChunkSplatBake, x: usize, y: usize) -> [u8; 4] {
        let offset = (y * CONTROL_SIDE + x) * 4;
        bake.control[offset..offset + 4]
            .try_into()
            .expect("4 bytes")
    }

    /// A square region covering 10×10 tiles inside `coord`, painted with 1 rect layer.
    fn square_region(palette: &TerrainPalette, coord: ChunkCoord, id: &str) -> RegionPaint {
        let raw = format!(
            r#"{{"materials": [{{"id": "paint", "palette": "{id}"}}],
                "layers": [{{"material": "paint",
                             "shape": {{"kind": "rect", "min": [0, 0], "max": [10, 10]}}}}]}}"#
        );
        let config: MapConfig = serde_json::from_str(&raw).expect("valid config");
        let origin = coord.origin_tile();
        let (min_x, min_y) = (i64::from(origin.x) + 20, i64::from(origin.y) + 20);
        RegionPaint {
            face: coord.face,
            bounds: RegionBounds {
                min_x,
                min_y,
                max_x: min_x + 10,
                max_y: min_y + 10,
            },
            ring: vec![
                Vec2::ZERO,
                Vec2::new(10.0, 0.0),
                Vec2::new(10.0, 10.0),
                Vec2::new(0.0, 10.0),
            ],
            edge_feather: 1.0,
            layers: resolve_layers(&config, palette, &[]).expect("layers resolve"),
        }
    }

    #[test]
    fn adjacent_chunks_share_identical_edge_texels() {
        let (maps, climate, palette) = (flat_planet(120), temperate_climate(), palette());
        let left = bake_chunk_splat(&maps, &climate, &palette, mid_chunk(0, 0), &[]);
        let right = bake_chunk_splat(&maps, &climate, &palette, mid_chunk(1, 0), &[]);
        let below = bake_chunk_splat(&maps, &climate, &palette, mid_chunk(0, 1), &[]);
        let last = CONTROL_SIDE - 2;
        for i in 0..CONTROL_SIDE {
            for halo in 0..2 {
                assert_eq!(texel(&left, last + halo, i), texel(&right, halo, i));
                assert_eq!(texel(&left, i, last + halo), texel(&below, i, halo));
            }
        }
        for i in 0..SHADE_SIDE {
            for halo in 0..2 {
                let lhs = ((i * SHADE_SIDE) + SHADE_SIDE - 2 + halo) * 4;
                let rhs = ((i * SHADE_SIDE) + halo) * 4;
                assert_eq!(left.shade[lhs..lhs + 4], right.shade[rhs..rhs + 4]);
            }
        }
    }

    #[test]
    fn dominant_indices_name_palette_entries() {
        let (maps, climate, palette) = (flat_planet(120), temperate_climate(), palette());
        let bake = bake_chunk_splat(&maps, &climate, &palette, mid_chunk(0, 0), &[]);
        let count = palette.entries().len();
        assert!(bake.ground.dominant.iter().all(|&i| usize::from(i) < count));
        assert!(bake.ground.region_painted.iter().all(|&painted| !painted));
        for chunk in bake.control.chunks_exact(4) {
            assert!(usize::from(chunk[0]) < count && usize::from(chunk[1]) < count);
        }
    }

    #[test]
    fn region_paint_marks_tiles_inside_the_polygon_only() {
        let (maps, climate, palette) = (flat_planet(120), temperate_climate(), palette());
        let coord = mid_chunk(0, 0);
        let id = palette.entries()[0].id.clone();
        let region = square_region(&palette, coord, &id);
        let bake = bake_chunk_splat(&maps, &climate, &palette, coord, &[region]);
        let painted = |x: u32, y: u32| bake.ground.region_painted_at(x, y).expect("in chunk");
        assert!(painted(25, 25));
        assert_eq!(bake.ground.dominant_at(25, 25), Some(0));
        assert!(!painted(19, 25) && !painted(25, 31) && !painted(5, 5));
    }

    #[test]
    fn region_key_changes_only_for_touched_chunks() {
        let palette = palette();
        let coord = mid_chunk(0, 0);
        let id = palette.entries()[0].id.clone();
        let mut paint = ChunkRegionPaint {
            regions: Arc::new(vec![square_region(&palette, coord, &id)]),
            generation: 1,
        };
        let far = mid_chunk(3, 3);
        assert_eq!(paint.key_for(far), 0);
        let before = paint.key_for(coord);
        assert_ne!(before, 0);
        paint.generation = 2;
        assert_ne!(paint.key_for(coord), before);
        assert_eq!(paint.key_for(far), 0);
    }

    #[test]
    fn rect_splat_paints_layers_over_the_base() {
        let palette = palette();
        let id = palette.entries()[1].id.clone();
        let region = square_region(&palette, mid_chunk(0, 0), &id);
        let rect = bake_rect_splat(20, 20, 0, &region.layers);
        assert_eq!(rect.control_size, UVec2::new(162, 162));
        let at = |tile_x: usize, tile_y: usize| {
            let (x, y) = (tile_x * TEXELS_PER_TILE + 5, tile_y * TEXELS_PER_TILE + 5);
            rect.control[(y * 162 + x) * 4]
        };
        assert_eq!(at(5, 5), 1);
        assert_eq!(at(15, 15), 0);
        assert_eq!(rect.shade.len(), 22 * 22 * 4);
    }
}
