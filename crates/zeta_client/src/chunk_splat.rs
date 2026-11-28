//! Terrain splat bake: control map, sub-tile shade map, and ground facts of 1 streamed planet
//! chunk.
//!
//! Contract: `docs/architecture/world_geography.md` Section 4. The base ground of every tile
//! comes from [`ground_cover`]; the texture variants of 1 cover cross-fade by [`variant_value`]
//! noise ([`TerrainPalette::variant_mix`]), and each tile's ground fact is the variant at its
//! center ([`TerrainPalette::variant_at`]). Control texels blend nearby tile covers with smooth
//! multi-tile weights displaced by 3D sphere-space noise, while keeping land-to-water coastlines
//! crisp on their authoritative passability boundary. Authored regions then composite only their
//! painted layers on top, faded by the polygon's `edge_feather`.
//!
//! Every value a texel or tile depends on is computed from canonical planetary coordinates, never
//! from the chunk it belongs to, so adjacent chunks agree byte-for-byte on the halo texels they
//! share and variant/border fields cross cube-face seams smoothly.
//!
//! Every streamed planet chunk entity drawn by the splat path carries a [`ChunkGroundCover`].
//! It is the only input clutter placement reads, so its shape is a contract: change it only
//! through the coordinator.

use std::f64::consts::FRAC_PI_2;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::image::{Image, ImageSampler};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use zeta_core::world::{
    CHUNK_SIZE, ChunkCoord, CubeFace, FACE_TILES, GroundCover, GroundCoverInput, PlanetClimateMaps,
    PlanetHeightmaps, PlanetTile, SHORE_DISTANCE_MAX_TILES, TileSample, face_uv_to_unit_vec,
    ground_cover, sample_climate, sample_fractal_detail_ft, sample_macro_elevation_ft, sample_tile,
    shore_distances, step_tile, tile_to_unit_vec,
};

use crate::planet_region::{RegionBounds, polygon_alpha};
use crate::planet_stream::spherical_hillshade;
use crate::terrain::{
    ResolvedLayer, SPLAT_PIXELS_PER_TILE, count_to_f32, layer_coverage, smoothstep,
};
use crate::terrain_palette::{
    MAX_PALETTE_ENTRIES, TerrainPalette, sphere_noise_signed, variant_value,
};

/// Tiles per chunk side as a `usize` index bound.
pub const CHUNK_TILES: usize = CHUNK_SIZE as usize;
/// Tiles per chunk.
pub const CHUNK_TILE_COUNT: usize = CHUNK_TILES * CHUNK_TILES;
/// Smallest on-screen tile size in pixels at which chunks draw through the splat material;
/// below it overview zoom uses the color raster path (`PlanetCameraFocus::sera_coast` uses `3.0`).
pub const SPLAT_MIN_TILE_PIXELS: f32 = 2.5;
/// Control texels per tile as an index.
const TEXELS_PER_TILE: usize = SPLAT_PIXELS_PER_TILE as usize;
/// Control texels per tile as a signed coordinate.
const TEXELS_PER_TILE_I64: i64 = SPLAT_PIXELS_PER_TILE as i64;
/// Control map side in texels: 8 texels per tile plus a 1-texel halo on each side.
pub const CONTROL_SIDE: usize = CHUNK_TILES * TEXELS_PER_TILE + 2;
/// Sub-tile shade texels per tile side (`2.5`-foot shade texels capture canopy sunbeams, cliff
/// crevices, and shoreline wetness smoothly).
const SHADE_PIXELS_PER_TILE: usize = 2;
/// Shade map side in texels: 2 texels per tile plus a 1-texel halo on each side.
pub const SHADE_SIDE: usize = CHUNK_TILES * SHADE_PIXELS_PER_TILE + 2;
/// Coarse control-field lattice step in control texels (`4` texels = `0.5` tiles), keeping 3D
/// sphere-space warp and variant evaluation fast while preserving exact chunk-border parity.
const COARSE_STEP_TEXELS: i64 = 4;
/// Side of the per-chunk coarse field grid (`64 * 2 + 3 = 131`, covering `-1..=512` control
/// texels with zero boundary clamping).
const COARSE_SIDE: usize = (CHUNK_TILES * TEXELS_PER_TILE) / 4 + 3;
/// Cover grid halo in tiles: wide enough for the `4x4` smooth cover kernel plus `±1.4`-tile
/// 3D border warp and the 1-texel control halo without clamping at the window edge.
const COVER_HALO_TILES: u32 = 5;
/// [`COVER_HALO_TILES`] as an index.
const COVER_HALO: usize = COVER_HALO_TILES as usize;
/// Cover grid side in tiles: chunk tiles plus [`COVER_HALO`] on each side.
const COVER_SIDE: usize = CHUNK_TILES + 2 * COVER_HALO;
/// Continuous elevation sub-window side (`COVER_SIDE` plus a 1-tile ring for 3x3 Sobel normals).
const ELEVATION_WINDOW_SIDE: usize = COVER_SIDE + 2;
/// Padding that makes [`shore_distances`] exact inside the cover grid.
const SHORE_PAD: usize = SHORE_DISTANCE_MAX_TILES as usize;
/// Side of the sampled tile window: cover grid plus shore padding on each side.
const WINDOW_SIDE: usize = COVER_SIDE + 2 * SHORE_PAD;
/// Tiles between the window's top-left tile and the chunk origin tile.
const WINDOW_OFFSET: i64 = COVER_HALO_TILES as i64 + SHORE_DISTANCE_MAX_TILES as i64;
/// Steep tactical step threshold (in feet) at which exposed rock ledges get sculptural crevice
/// ambient occlusion (`ground.rs` `ROCK_MIN_SLOPE_STEP_FT`); gentler slopes shade smoothly.
const ROCK_LEDGE_MIN_STEP_FT: i32 = 10;
/// Tiles per radian of arc at a cube-face center (`FACE_TILES` spans `pi / 2`).
const TILES_PER_RADIAN: f64 = FACE_TILES as f64 / FRAC_PI_2;
/// Dappled forest canopy octave feature diameters (in tiles) and weights: clearing vs grove,
/// tree-crown shadow pools, and fine branch-gap sunbeams.
const CANOPY_OCTAVES: [(f64, f32, u64); 3] = [
    (14.0, 0.50, 0x6361_6E6F_7079_0101),
    (5.5, 0.32, 0x6361_6E6F_7079_0202),
    (2.2, 0.18, 0x6361_6E6F_7079_0303),
];
/// 3D sphere-space border warp octaves: feature diameter in tiles, amplitude in tiles, and seed.
const WARP_SPHERE_OCTAVES: [(f64, f32, u64); 3] = [
    (7.5, 0.90, 0x7370_6c61_7477_0101),
    (2.4, 0.36, 0x7370_6c61_7477_0202),
    (0.9, 0.14, 0x7370_6c61_7477_0303),
];
/// 3D sphere-space per-cover edge score octaves: feature diameter in tiles and score amplitude.
const EDGE_SPHERE_OCTAVES: [(f64, f32); 2] = [(3.2, 0.22), (1.1, 0.10)];
/// Seed base of per-cover 3D edge noise.
const EDGE_NOISE_SALT: u64 = 0x7370_6c61_7465_6401;
/// Score gap below the leading cover over which a cover's weight falls from 1 to 0.
const LAND_EDGE_SOFTNESS: f32 = 0.38;
/// Crisper score gap right along the land-water shoreline so water stays aligned with passability.
const SHORE_EDGE_SOFTNESS: f32 = 0.20;
/// Seed for subtle shoreline wetness edge scalloping on the unit sphere.
const SHORE_WET_SEED: u64 = 0x7368_6F72_6577_6574;
/// Seed for subtle ocean shoal / swell variation on the unit sphere.
const WATER_SHOAL_SEED: u64 = 0x7761_7465_7273_686F;
/// Shade tint of dry land (moisture 0) and wet land (moisture 1).
const DRY_TINT: [f32; 3] = [1.03, 1.00, 0.93];
const WET_TINT: [f32; 3] = [0.95, 1.02, 0.99];
/// Deep mossy grove shade tint and warm golden clearing sunbeam tint under forest canopy.
const CANOPY_SHADOW_TINT: [f32; 3] = [0.42, 0.55, 0.48];
const CANOPY_SUN_TINT: [f32; 3] = [1.20, 1.15, 0.98];
/// Subtle open-sky cloud and sun-patch tint for meadows, savannas, and open terrain.
const OPEN_CLOUD_TINT: [f32; 3] = [0.91, 0.95, 0.99];
const OPEN_SUN_TINT: [f32; 3] = [1.09, 1.05, 0.98];
/// Glacial ice-blue drift shadow and warm sunlit snow crest tints so snowfields stay sculpted
/// and colorful instead of clipping to flat white.
const SNOW_SHADOW_TINT: [f32; 3] = [0.68, 0.79, 0.97];
const SNOW_SUN_TINT: [f32; 3] = [0.94, 0.93, 0.90];
/// Seeds for open-sky cloud patches and wind-sculpted snowdrift shading on the unit sphere.
const OPEN_SKY_SEED: u64 = 0x6F70_656E_736B_7901;
const SNOW_DRIFT_SEED: u64 = 0x736E_6F77_6472_6674;
/// Damp shoreline darkening multiplier right at the water's edge.
const WET_SHORE_TINT: [f32; 3] = [0.76, 0.78, 0.81];
/// Water bathymetry tint stops: littoral surf, coastal cerulean shelf, deep ocean sapphire.
const SHALLOW_SURF_TINT: [f32; 3] = [0.95, 1.02, 1.02];
const COASTAL_SHELF_TINT: [f32; 3] = [0.85, 0.93, 1.00];
const DEEP_OCEAN_TINT: [f32; 3] = [0.70, 0.80, 0.94];
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
    /// [`SHADE_SIDE`]² `Rgba8Unorm` texels: composite shade × climate tint / 2.
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

/// Precomputed 3D sphere-space fields on the half-tile [`COARSE_SIDE`]² lattice.
#[derive(Clone, Copy)]
struct CoarseControlSample {
    unit_vec: [f64; 3],
    warp: Vec2,
    variant: f32,
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

    let (cover, shade) = bake_cover_and_shade(maps, climate, palette, coord, &touching);
    let coarse = bake_coarse_control_grid(coord.face, origin);

    let first_texel_col = i64::from(origin.x) * TEXELS_PER_TILE_I64 - 1;
    let first_texel_row = i64::from(origin.y) * TEXELS_PER_TILE_I64 - 1;
    let base_cell_col = first_texel_col.div_euclid(COARSE_STEP_TEXELS);
    let base_cell_row = first_texel_row.div_euclid(COARSE_STEP_TEXELS);
    let mut control = vec![0_u8; CONTROL_SIDE * CONTROL_SIDE * 4];

    for py in 0..CONTROL_SIDE {
        let rel_y = index_to_i64(py) - 1;
        let global_y = first_texel_row + index_to_i64(py);
        let cell_y =
            usize::try_from(global_y.div_euclid(COARSE_STEP_TEXELS) - base_cell_row).unwrap_or(0);
        let frac_y = i64_to_f32(global_y.rem_euclid(COARSE_STEP_TEXELS)) * 0.25;

        for px in 0..CONTROL_SIDE {
            let rel_x = index_to_i64(px) - 1;
            let global_x = first_texel_col + index_to_i64(px);
            let cell_x = usize::try_from(global_x.div_euclid(COARSE_STEP_TEXELS) - base_cell_col)
                .unwrap_or(0);
            let frac_x = i64_to_f32(global_x.rem_euclid(COARSE_STEP_TEXELS)) * 0.25;

            let field = bilerp_coarse_control(&coarse, cell_x, cell_y, frac_x, frac_y);
            let mut weights = [0.0_f32; MAX_PALETTE_ENTRIES];
            add_cover_weights(palette, &cover, (rel_x, rel_y), &field, &mut weights);
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

/// Evaluates 3D sphere-space variant noise and tangent-projected border warp on the half-tile
/// global lattice covering the chunk and its 1-texel halo.
fn bake_coarse_control_grid(face: CubeFace, origin: PlanetTile) -> Vec<CoarseControlSample> {
    let basis = face.basis();
    let face_right = basis.right_u.map(f64::from);
    let face_down = basis.down_v.map(f64::from);
    let base_cell_col =
        (i64::from(origin.x) * TEXELS_PER_TILE_I64 - 1).div_euclid(COARSE_STEP_TEXELS);
    let base_cell_row =
        (i64::from(origin.y) * TEXELS_PER_TILE_I64 - 1).div_euclid(COARSE_STEP_TEXELS);
    let fine_per_tile = TEXELS_PER_TILE_I64 * 2;
    let denom = f64::from(FACE_TILES) * i64_to_f64(fine_per_tile);

    let mut grid = Vec::with_capacity(COARSE_SIDE * COARSE_SIDE);
    for gy in 0..COARSE_SIDE {
        let global_texel_y = (base_cell_row + index_to_i64(gy)) * COARSE_STEP_TEXELS;
        let fine_y = global_texel_y * 2 + 1;
        let face_v = (i64_to_f64(fine_y) / denom) * 2.0 - 1.0;

        for gx in 0..COARSE_SIDE {
            let global_texel_x = (base_cell_col + index_to_i64(gx)) * COARSE_STEP_TEXELS;
            let fine_x = global_texel_x * 2 + 1;
            let face_u = (i64_to_f64(fine_x) / denom) * 2.0 - 1.0;
            let unit_vec = face_uv_to_unit_vec(face, face_u, face_v);
            let warp = sphere_tangent_warp(unit_vec, face_right, face_down);
            let variant = variant_value(face, fine_x, fine_y, fine_per_tile);
            grid.push(CoarseControlSample {
                unit_vec,
                warp,
                variant,
            });
        }
    }
    grid
}

/// Computes a 3D sphere-tangent displacement vector projected onto the chunk face's `(u, v)`
/// axes, so border warps agree across both chunk edges and rotated cube-face seams.
fn sphere_tangent_warp(unit_vec: [f64; 3], face_right: [f64; 3], face_down: [f64; 3]) -> Vec2 {
    let mut disp_3d = [0.0_f64; 3];
    for &(feature_tiles, amplitude, seed) in &WARP_SPHERE_OCTAVES {
        let freq = TILES_PER_RADIAN / feature_tiles;
        let amp = f64::from(amplitude);
        disp_3d[0] += f64::from(sphere_noise_signed(unit_vec, freq, seed)) * amp;
        disp_3d[1] += f64::from(sphere_noise_signed(
            unit_vec,
            freq,
            seed ^ 0x5555_5555_5555_5555,
        )) * amp;
        disp_3d[2] += f64::from(sphere_noise_signed(
            unit_vec,
            freq,
            seed ^ 0xAAAA_AAAA_AAAA_AAAA,
        )) * amp;
    }
    let tan_u = normalize3_local(project_tangent_local(face_right, unit_vec));
    let tan_v = normalize3_local(project_tangent_local(face_down, unit_vec));
    Vec2::new(
        f64_to_f32(dot3_local(disp_3d, tan_u)),
        f64_to_f32(dot3_local(disp_3d, tan_v)),
    )
}

fn bilerp_coarse_control(
    grid: &[CoarseControlSample],
    cx: usize,
    cy: usize,
    frac_x: f32,
    frac_y: f32,
) -> CoarseControlSample {
    let w00 = (1.0 - frac_x) * (1.0 - frac_y);
    let w10 = frac_x * (1.0 - frac_y);
    let w01 = (1.0 - frac_x) * frac_y;
    let w11 = frac_x * frac_y;
    let s00 = grid[cy * COARSE_SIDE + cx];
    let s10 = grid[cy * COARSE_SIDE + cx + 1];
    let s01 = grid[(cy + 1) * COARSE_SIDE + cx];
    let s11 = grid[(cy + 1) * COARSE_SIDE + cx + 1];

    let warp = s00.warp * w00 + s10.warp * w10 + s01.warp * w01 + s11.warp * w11;
    let variant = (s00.variant * w00 + s10.variant * w10 + s01.variant * w01 + s11.variant * w11)
        .clamp(0.0, 0.999_999);
    let (d00, d10, d01, d11) = (
        f64::from(w00),
        f64::from(w10),
        f64::from(w01),
        f64::from(w11),
    );
    let unit_vec = normalize3_local([0_usize, 1, 2].map(|axis| {
        s00.unit_vec[axis] * d00
            + s10.unit_vec[axis] * d10
            + s01.unit_vec[axis] * d01
            + s11.unit_vec[axis] * d11
    }));

    CoarseControlSample {
        unit_vec,
        warp,
        variant,
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

/// Smooth per-tile physical and lighting inputs interpolated onto sub-tile shade texels.
#[derive(Clone, Copy)]
struct TileShadeSample {
    unit_vec: [f64; 3],
    continuous_elevation_ft: f32,
    hillshade: f32,
    water_weight: f32,
    moisture: f32,
    foliage: f32,
    snow_weight: f32,
    shore_tiles: f32,
    rock_crevice_ao: f32,
}

/// Cover cells over the [`COVER_SIDE`]² grid and sub-tile shade texels over the [`SHADE_SIDE`]²
/// grid (`2` texels per tile plus a 1-texel halo on each side).
fn bake_cover_and_shade(
    maps: &PlanetHeightmaps,
    climate: &PlanetClimateMaps,
    palette: &TerrainPalette,
    coord: ChunkCoord,
    touching: &[RegionInChunk<'_>],
) -> (Vec<CoverCell>, Vec<u8>) {
    let origin = coord.origin_tile();
    let (tiles, samples, shore) = sample_tile_window(maps, origin);
    let (continuous_elev, unit_vecs) = sample_continuous_elevation_window(maps, &tiles);

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
    let mut tile_shade = Vec::with_capacity(COVER_SIDE * COVER_SIDE);

    for cy in 0..COVER_SIDE {
        for cx in 0..COVER_SIDE {
            let win_idx = (cy + SHORE_PAD) * WINDOW_SIDE + cx + SHORE_PAD;
            let sample = samples[win_idx];
            let neighbors = [
                win_idx - 1,
                win_idx + 1,
                win_idx - WINDOW_SIDE,
                win_idx + WINDOW_SIDE,
            ];
            let slope_step_ft = neighbors
                .iter()
                .map(|&neighbor| {
                    (samples[neighbor].elevation_step_ft - sample.elevation_step_ft).abs()
                })
                .max()
                .unwrap_or(0);
            let tile_climate = sample_climate(climate, tiles[win_idx]);
            let kind = ground_cover(&GroundCoverInput {
                tile: tiles[win_idx],
                sample,
                climate: tile_climate,
                slope_step_ft,
                shore_distance_tiles: shore[win_idx],
            });
            cover[cy * COVER_SIDE + cx] = CoverCell {
                kind,
                tile_variant: palette.variant_at(kind, tiles[win_idx]),
            };

            let elev_idx = (cy + 1) * ELEVATION_WINDOW_SIDE + (cx + 1);
            let (slope_east, slope_south) = sobel_elevation_gradient(&continuous_elev, elev_idx);
            let unit_vec = unit_vecs[elev_idx];
            let hillshade =
                spherical_hillshade(unit_vec, face_right, face_down, slope_east, slope_south);
            let rock_crevice_ao =
                steep_rock_crevice_ao(sample, slope_step_ft, neighbors.map(|n| samples[n]));

            tile_shade.push(TileShadeSample {
                unit_vec,
                continuous_elevation_ft: f64_to_f32(continuous_elev[elev_idx]),
                hillshade: f64_to_f32(hillshade),
                water_weight: if sample.is_water { 1.0 } else { 0.0 },
                moisture: tile_climate.moisture.clamp(0.0, 1.0),
                foliage: tile_climate.foliage.clamp(0.0, 1.0),
                snow_weight: if kind == GroundCover::Snow { 1.0 } else { 0.0 },
                shore_tiles: f32::from(shore[win_idx]),
                rock_crevice_ao,
            });
        }
    }

    let shade = bake_subtile_shade(&tile_shade, origin, touching);
    (cover, shade)
}

/// Samples discrete tiles and exact chamfer shore distances across the padded window.
fn sample_tile_window(
    maps: &PlanetHeightmaps,
    origin: PlanetTile,
) -> (Vec<PlanetTile>, Vec<TileSample>, Vec<u8>) {
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
    (tiles, samples, shore)
}

/// Evaluates unquantized continuous `f64` elevation (in feet) and 3D unit-sphere vectors over
/// [`ELEVATION_WINDOW_SIDE`]² so Sobel hillshading has zero integer or 5-foot step striping.
fn sample_continuous_elevation_window(
    maps: &PlanetHeightmaps,
    tiles: &[PlanetTile],
) -> (Vec<f64>, Vec<[f64; 3]>) {
    let count = ELEVATION_WINDOW_SIDE * ELEVATION_WINDOW_SIDE;
    let mut elevations = Vec::with_capacity(count);
    let mut unit_vecs = Vec::with_capacity(count);
    let start = SHORE_PAD - 1;
    for ey in 0..ELEVATION_WINDOW_SIDE {
        for ex in 0..ELEVATION_WINDOW_SIDE {
            let tile = tiles[(ey + start) * WINDOW_SIDE + ex + start];
            let unit_vec = tile_to_unit_vec(tile);
            let macro_ft = sample_macro_elevation_ft(maps, tile);
            let detail_ft = sample_fractal_detail_ft(unit_vec, macro_ft);
            elevations.push(macro_ft + detail_ft);
            unit_vecs.push(unit_vec);
        }
    }
    (elevations, unit_vecs)
}

/// Smooth 3x3 Sobel elevation gradient `(east_ft_per_tile, south_ft_per_tile)` at `center`.
fn sobel_elevation_gradient(elevations: &[f64], center: usize) -> (f64, f64) {
    let stride = ELEVATION_WINDOW_SIDE;
    let nw = elevations[center - stride - 1];
    let north = elevations[center - stride];
    let ne = elevations[center - stride + 1];
    let west = elevations[center - 1];
    let east = elevations[center + 1];
    let sw = elevations[center + stride - 1];
    let south = elevations[center + stride];
    let se = elevations[center + stride + 1];
    let slope_east = ((ne - nw) + 2.0 * (east - west) + (se - sw)) * 0.125;
    let slope_south = ((sw - nw) + 2.0 * (south - north) + (se - ne)) * 0.125;
    (slope_east, slope_south)
}

/// Sculptural crevice ambient occlusion exclusively on steep exposed rock (`slope_step_ft >= 10`),
/// keeping walkable hillsides completely free of contour striping.
fn steep_rock_crevice_ao(
    sample: TileSample,
    slope_step_ft: i32,
    neighbors: [TileSample; 4],
) -> f32 {
    if sample.is_water || slope_step_ft < ROCK_LEDGE_MIN_STEP_FT {
        return 0.0;
    }
    let rise_ft = neighbors
        .iter()
        .map(|neighbor| (neighbor.elevation_step_ft - sample.elevation_step_ft).max(0))
        .max()
        .unwrap_or(0);
    let drop_ft = neighbors
        .iter()
        .map(|neighbor| (sample.elevation_step_ft - neighbor.elevation_step_ft).max(0))
        .max()
        .unwrap_or(0);
    let base_crevice = ((i64_to_f32(i64::from(rise_ft)) - 5.0) / 25.0).clamp(0.0, 1.0) * 0.28;
    let lip_rim = ((i64_to_f32(i64::from(drop_ft)) - 5.0) / 30.0).clamp(0.0, 1.0) * 0.12;
    (base_crevice + lip_rim).clamp(0.0, 0.34)
}

/// Synthesizes the [`SHADE_SIDE`]² sub-tile shade texture with exact cross-chunk seam parity.
fn bake_subtile_shade(
    tile_shade: &[TileShadeSample],
    origin: PlanetTile,
    touching: &[RegionInChunk<'_>],
) -> Vec<u8> {
    let mut shade = vec![0_u8; SHADE_SIDE * SHADE_SIDE * 4];
    let halo_i64 = index_to_i64(COVER_HALO);
    let max_cell = index_to_i64(COVER_SIDE - 2);

    for row in 0..SHADE_SIDE {
        let sub_row = index_to_i64(row) - 1;
        let quarter_row = 2 * sub_row - 1;
        let tile_row = quarter_row.div_euclid(4);
        let frac_y = i64_to_f32(quarter_row.rem_euclid(4)) * 0.25;
        let cy = usize::try_from((tile_row + halo_i64).clamp(0, max_cell)).unwrap_or(0);

        for col in 0..SHADE_SIDE {
            let sub_col = index_to_i64(col) - 1;
            let quarter_col = 2 * sub_col - 1;
            let tile_col = quarter_col.div_euclid(4);
            let frac_x = i64_to_f32(quarter_col.rem_euclid(4)) * 0.25;
            let cx = usize::try_from((tile_col + halo_i64).clamp(0, max_cell)).unwrap_or(0);

            let interpolated = bilerp_tile_shade(tile_shade, cx, cy, frac_x, frac_y);
            let painted = seam_exact_region_coverage(
                touching,
                (
                    i64::from(origin.x) + tile_col,
                    i64::from(origin.y) + tile_row,
                ),
                (frac_x, frac_y),
            );
            let rgb = subtile_shade_rgb(&interpolated, painted);
            let offset = (row * SHADE_SIDE + col) * 4;
            shade[offset] = unit_to_u8(rgb[0] * 0.5);
            shade[offset + 1] = unit_to_u8(rgb[1] * 0.5);
            shade[offset + 2] = unit_to_u8(rgb[2] * 0.5);
            shade[offset + 3] = 255;
        }
    }
    shade
}

/// Bilinearly interpolates 4 surrounding tile centers onto 1 sub-tile shade sample.
fn bilerp_tile_shade(
    grid: &[TileShadeSample],
    cx: usize,
    cy: usize,
    frac_x: f32,
    frac_y: f32,
) -> TileShadeSample {
    let w00 = (1.0 - frac_x) * (1.0 - frac_y);
    let w10 = frac_x * (1.0 - frac_y);
    let w01 = (1.0 - frac_x) * frac_y;
    let w11 = frac_x * frac_y;

    let s00 = grid[cy * COVER_SIDE + cx];
    let s10 = grid[cy * COVER_SIDE + cx + 1];
    let s01 = grid[(cy + 1) * COVER_SIDE + cx];
    let s11 = grid[(cy + 1) * COVER_SIDE + cx + 1];

    let blend = |f: fn(&TileShadeSample) -> f32| -> f32 {
        f(&s00) * w00 + f(&s10) * w10 + f(&s01) * w01 + f(&s11) * w11
    };
    let (w00_d, w10_d, w01_d, w11_d) = (
        f64::from(w00),
        f64::from(w10),
        f64::from(w01),
        f64::from(w11),
    );
    let unit_vec = normalize3_local([0_usize, 1, 2].map(|axis| {
        s00.unit_vec[axis] * w00_d
            + s10.unit_vec[axis] * w10_d
            + s01.unit_vec[axis] * w01_d
            + s11.unit_vec[axis] * w11_d
    }));

    TileShadeSample {
        unit_vec,
        continuous_elevation_ft: blend(|s| s.continuous_elevation_ft),
        hillshade: blend(|s| s.hillshade),
        water_weight: blend(|s| s.water_weight),
        moisture: blend(|s| s.moisture),
        foliage: blend(|s| s.foliage),
        snow_weight: blend(|s| s.snow_weight),
        shore_tiles: blend(|s| s.shore_tiles),
        rock_crevice_ao: blend(|s| s.rock_crevice_ao),
    }
}

/// Evaluates authored region painted coverage from canonical face integer tile `(global_tile_x,
/// global_tile_y)` plus sub-tile offset `0.5 + frac`, avoiding chunk-origin cancellation across
/// seams.
fn seam_exact_region_coverage(
    regions: &[RegionInChunk<'_>],
    (global_tile_x, global_tile_y): (i64, i64),
    (frac_x, frac_y): (f32, f32),
) -> f32 {
    if regions.is_empty() {
        return 0.0;
    }
    let mut uncovered = 1.0_f32;
    for region in regions {
        let local = Vec2::new(
            i64_to_f32(global_tile_x - region.paint.bounds.min_x) + 0.5 + frac_x,
            i64_to_f32(global_tile_y - region.paint.bounds.min_y) + 0.5 + frac_y,
        );
        if local.x < 0.0 || local.y < 0.0 || local.x > region.size.x || local.y > region.size.y {
            continue;
        }
        let alpha = polygon_alpha(local, &region.paint.ring, region.paint.edge_feather);
        if alpha <= 0.0 {
            continue;
        }
        for layer in &region.paint.layers {
            let (weight, _) = layer_coverage(layer, local);
            let amount = (alpha * weight).clamp(0.0, 1.0);
            uncovered *= 1.0 - amount;
        }
    }
    (1.0 - uncovered).clamp(0.0, 1.0)
}

/// Combines smooth NW hillshade, foliage-scaled dappled canopy light, shoreline wetness, steep
/// rock crevice occlusion, and shallow-to-deep water bathymetry into linear RGB shade.
fn subtile_shade_rgb(sample: &TileShadeSample, painted_coverage: f32) -> [f32; 3] {
    let unpainted = (1.0 - painted_coverage).clamp(0.0, 1.0);
    let land_rgb = land_shade_rgb(sample, unpainted);
    if sample.water_weight <= 0.001 {
        return land_rgb;
    }
    let water_rgb = water_shade_rgb(sample);
    if sample.water_weight >= 0.999 {
        return water_rgb;
    }
    let water_t = sample.water_weight.clamp(0.0, 1.0);
    [0_usize, 1, 2].map(|c| land_rgb[c] + (water_rgb[c] - land_rgb[c]) * water_t)
}

fn land_shade_rgb(sample: &TileShadeSample, unpainted: f32) -> [f32; 3] {
    let hill_factor = 0.60 + 0.64 * sample.hillshade;
    let sun_balance = ((sample.hillshade - 0.707) * 1.6).clamp(-1.0, 1.0);
    let sun_t = sun_balance * 0.5 + 0.5;
    let slope_sky_sun = [
        0.91 + 0.15 * sun_t,
        0.95 + 0.08 * sun_t,
        1.05 - 0.10 * sun_t,
    ];
    let wet = sample.moisture.clamp(0.0, 1.0);
    let climate_tint = [0_usize, 1, 2].map(|c| DRY_TINT[c] + (WET_TINT[c] - DRY_TINT[c]) * wet);
    let snow = sample.snow_weight.clamp(0.0, 1.0);

    let canopy_strength = smoothstep(0.32, 0.78, sample.foliage) * unpainted * (1.0 - snow * 0.85);
    let canopy_tint = if canopy_strength > 0.001 {
        let raw_canopy: f32 = CANOPY_OCTAVES
            .iter()
            .map(|&(feature_tiles, weight, seed)| {
                sphere_noise_signed(sample.unit_vec, TILES_PER_RADIAN / feature_tiles, seed)
                    * weight
            })
            .sum();
        let dapple = smoothstep(-0.56, 0.56, raw_canopy);
        [0_usize, 1, 2].map(|c| {
            let target =
                CANOPY_SHADOW_TINT[c] + (CANOPY_SUN_TINT[c] - CANOPY_SHADOW_TINT[c]) * dapple;
            1.0 + (target - 1.0) * canopy_strength
        })
    } else {
        [1.0; 3]
    };

    // Subtle open-sky cloud/sunbeam relief keeps meadows and savannas from reading flat.
    let open_strength = (1.0 - canopy_strength) * unpainted * (1.0 - snow);
    let open_tint = if open_strength > 0.001 {
        let cloud = 0.65
            * sphere_noise_signed(sample.unit_vec, TILES_PER_RADIAN / 18.0, OPEN_SKY_SEED)
            + 0.35
                * sphere_noise_signed(
                    sample.unit_vec,
                    TILES_PER_RADIAN / 5.5,
                    OPEN_SKY_SEED ^ 0x1111,
                );
        let open_t = smoothstep(-0.58, 0.58, cloud);
        [0_usize, 1, 2].map(|c| {
            let target = OPEN_CLOUD_TINT[c] + (OPEN_SUN_TINT[c] - OPEN_CLOUD_TINT[c]) * open_t;
            1.0 + (target - 1.0) * open_strength
        })
    } else {
        [1.0; 3]
    };

    // Glacial blue drift shadows and controlled exposure prevent snowfields from clipping white.
    let snow_tint = if snow > 0.001 {
        let drift = 0.62
            * sphere_noise_signed(sample.unit_vec, TILES_PER_RADIAN / 9.0, SNOW_DRIFT_SEED)
            + 0.38
                * sphere_noise_signed(
                    sample.unit_vec,
                    TILES_PER_RADIAN / 2.8,
                    SNOW_DRIFT_SEED ^ 0x2222,
                );
        let drift_t = smoothstep(-0.55, 0.55, drift * 0.65 + sun_balance * 0.35);
        [0_usize, 1, 2].map(|c| {
            let target = SNOW_SHADOW_TINT[c] + (SNOW_SUN_TINT[c] - SNOW_SHADOW_TINT[c]) * drift_t;
            1.0 + (target - 1.0) * snow
        })
    } else {
        [1.0; 3]
    };

    let shore_wobble =
        sphere_noise_signed(sample.unit_vec, TILES_PER_RADIAN / 6.0, SHORE_WET_SEED) * 0.45;
    let wet_shore = (1.0 - smoothstep(0.8, 3.8, sample.shore_tiles + shore_wobble))
        * unpainted
        * (1.0 - snow * 0.6);
    let shore_tint = [0_usize, 1, 2].map(|c| 1.0 + (WET_SHORE_TINT[c] - 1.0) * wet_shore);

    let ao = sample.rock_crevice_ao;
    let rock_tint = [1.0 - ao * 1.05, 1.0 - ao, 1.0 - ao * 0.88];

    [0_usize, 1, 2].map(|c| {
        hill_factor
            * slope_sky_sun[c]
            * climate_tint[c]
            * canopy_tint[c]
            * open_tint[c]
            * snow_tint[c]
            * shore_tint[c]
            * rock_tint[c]
    })
}

fn water_shade_rgb(sample: &TileShadeSample) -> [f32; 3] {
    let depth_ft = (-sample.continuous_elevation_ft).max(0.0);
    let surf_to_shelf = smoothstep(0.0, 28.0, depth_ft);
    let shelf_to_deep = smoothstep(28.0, 280.0, depth_ft);
    let near_shore = 1.0 - smoothstep(1.0, 9.0, sample.shore_tiles);
    let shoal =
        sphere_noise_signed(sample.unit_vec, TILES_PER_RADIAN / 18.0, WATER_SHOAL_SEED) * 0.04;
    let hill_factor = 0.86 + 0.26 * sample.hillshade + shoal;

    [0_usize, 1, 2].map(|c| {
        let shelf =
            SHALLOW_SURF_TINT[c] + (COASTAL_SHELF_TINT[c] - SHALLOW_SURF_TINT[c]) * surf_to_shelf;
        let deep = shelf + (DEEP_OCEAN_TINT[c] - shelf) * shelf_to_deep;
        let littoral = deep + (SHALLOW_SURF_TINT[c] - deep) * (near_shore * 0.40);
        hill_factor * littoral
    })
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

/// Cubic B-spline basis weights for `t` in `[0.0, 1.0]`, summing to `1.0` across 4 neighbor
/// tiles so natural cover transitions (`LushGrass <-> Grass`, `ShallowWater <-> DeepWater`,
/// `Dirt <-> Grass`) blend smoothly over ~3 tiles instead of a 1-tile line.
fn bspline_weights(param: f32) -> [f32; 4] {
    let inv = 1.0 - param;
    let param2 = param * param;
    let param3 = param2 * param;
    [
        (inv * inv * inv) / 6.0,
        (3.0 * param3 - 6.0 * param2 + 4.0) / 6.0,
        (-3.0 * param3 + 3.0 * param2 + 3.0 * param + 1.0) / 6.0,
        param3 / 6.0,
    ]
}

/// Gathers normalized cover scores around control texel `(rel_x, rel_y)`, using a crisp `2x2`
/// coastline check (keeping water/land aligned to tile passability) and a smooth `4x4` cubic
/// B-spline kernel within land or within water.
fn gather_cover_scores(
    cover: &[CoverCell],
    (rel_x, rel_y): (i64, i64),
    warp: Vec2,
) -> (
    [f32; GroundCover::ALL.len()],
    [bool; GroundCover::ALL.len()],
    bool,
) {
    let per_tile = i64_to_f32(TEXELS_PER_TILE_I64);
    let tile_col = rel_x.div_euclid(TEXELS_PER_TILE_I64);
    let tile_row = rel_y.div_euclid(TEXELS_PER_TILE_I64);
    let fraction = Vec2::new(
        (i64_to_f32(rel_x.rem_euclid(TEXELS_PER_TILE_I64)) + 0.5) / per_tile,
        (i64_to_f32(rel_y.rem_euclid(TEXELS_PER_TILE_I64)) + 0.5) / per_tile,
    );
    let halo = index_to_i64(COVER_HALO);
    let max_idx = index_to_i64(COVER_SIDE - 1);
    let cell_index = |tile: i64, offset: i64| -> usize {
        usize::try_from((tile + offset + halo).clamp(0, max_idx)).unwrap_or(0)
    };

    let shore_shift = fraction + warp * 0.50 - Vec2::splat(0.5);
    let shore_floor = shore_shift.floor();
    let shore_blend = shore_shift - shore_floor;
    let sx0 = cell_index(tile_col, f32_floor_to_i64(shore_floor.x));
    let sy0 = cell_index(tile_row, f32_floor_to_i64(shore_floor.y));
    let sx1 = (sx0 + 1).min(COVER_SIDE - 1);
    let sy1 = (sy0 + 1).min(COVER_SIDE - 1);
    let shore_taps = [
        (sx0, sy0, (1.0 - shore_blend.x) * (1.0 - shore_blend.y)),
        (sx1, sy0, shore_blend.x * (1.0 - shore_blend.y)),
        (sx0, sy1, (1.0 - shore_blend.x) * shore_blend.y),
        (sx1, sy1, shore_blend.x * shore_blend.y),
    ];
    let water_share: f32 = shore_taps
        .iter()
        .filter(|&&(x, y, _)| cover[y * COVER_SIDE + x].kind.is_water())
        .map(|&(_, _, w)| w)
        .sum();
    let mixed_shoreline = water_share > 0.01 && water_share < 0.99;

    let mut scores = [0.0_f32; GroundCover::ALL.len()];
    let mut seen = [false; GroundCover::ALL.len()];
    if mixed_shoreline {
        for &(x, y, weight) in &shore_taps {
            let slot = cover_slot_index(cover[y * COVER_SIDE + x].kind);
            scores[slot] += weight;
            seen[slot] = true;
        }
        return (scores, seen, true);
    }

    let shifted = fraction + warp - Vec2::splat(0.5);
    let floor = shifted.floor();
    let blend = shifted - floor;
    let step_col = f32_floor_to_i64(floor.x);
    let step_row = f32_floor_to_i64(floor.y);
    let wx = bspline_weights(blend.x);
    let wy = bspline_weights(blend.y);
    let target_water = water_share >= 0.5;
    let mut domain_sum = 0.0_f32;

    for (ky, &weight_y) in wy.iter().enumerate() {
        let cy = cell_index(tile_row, step_row + index_to_i64(ky) - 1);
        for (kx, &weight_x) in wx.iter().enumerate() {
            let cx = cell_index(tile_col, step_col + index_to_i64(kx) - 1);
            let kind = cover[cy * COVER_SIDE + cx].kind;
            if kind.is_water() == target_water {
                let weight = weight_x * weight_y;
                let slot = cover_slot_index(kind);
                scores[slot] += weight;
                seen[slot] = true;
                domain_sum += weight;
            }
        }
    }
    if domain_sum > 1.0e-5 {
        for score in &mut scores {
            *score /= domain_sum;
        }
    } else {
        let slot = cover_slot_index(cover[sy0 * COVER_SIDE + sx0].kind);
        scores[slot] = 1.0;
        seen[slot] = true;
    }
    (scores, seen, false)
}

/// Adds the cover weights of texel `(rel_x, rel_y)`, modulated by 3D sphere-space per-cover
/// edge noise and split across palette variants by [`TerrainPalette::variant_mix`].
fn add_cover_weights(
    palette: &TerrainPalette,
    cover: &[CoverCell],
    (rel_x, rel_y): (i64, i64),
    field: &CoarseControlSample,
    weights: &mut [f32; MAX_PALETTE_ENTRIES],
) {
    let (mut scores, seen, mixed_shoreline) =
        gather_cover_scores(cover, (rel_x, rel_y), field.warp);
    let active_count = seen.iter().filter(|&&active| active).count();
    if active_count <= 1 {
        let slot = seen.iter().position(|&active| active).unwrap_or(0);
        for (index, share) in palette.variant_mix(GroundCover::ALL[slot], field.variant) {
            weights[palette_slot(index)] += share;
        }
        return;
    }

    let mut best = f32::NEG_INFINITY;
    for (slot, &active) in seen.iter().enumerate() {
        if active {
            scores[slot] += sphere_edge_noise(field.unit_vec, slot);
            best = best.max(scores[slot]);
        }
    }
    let softness = if mixed_shoreline {
        SHORE_EDGE_SOFTNESS
    } else {
        LAND_EDGE_SOFTNESS
    };
    let mut kept_weights = [0.0_f32; GroundCover::ALL.len()];
    let mut total = 0.0_f32;
    for (slot, &active) in seen.iter().enumerate() {
        if active {
            let weight = smoothstep(best - softness, best, scores[slot]);
            kept_weights[slot] = weight;
            total += weight;
        }
    }
    if total <= 0.0 {
        return;
    }
    for (slot, &weight) in kept_weights.iter().enumerate() {
        if weight > 0.0 {
            let kind = GroundCover::ALL[slot];
            for (index, share) in palette.variant_mix(kind, field.variant) {
                weights[palette_slot(index)] += (weight / total) * share;
            }
        }
    }
}

fn cover_slot_index(cover: GroundCover) -> usize {
    GroundCover::ALL
        .iter()
        .position(|&candidate| candidate == cover)
        .unwrap_or(0)
}

/// 3D sphere-space edge score noise for cover `slot`, evaluated only where 2 or more covers
/// compete.
fn sphere_edge_noise(unit_vec: [f64; 3], slot: usize) -> f32 {
    let salt = EDGE_NOISE_SALT
        ^ (u64::try_from(slot)
            .unwrap_or(0)
            .wrapping_mul(0x9E37_79B9_7F4A));
    EDGE_SPHERE_OCTAVES
        .iter()
        .zip(0_u64..)
        .map(|(&(feature_tiles, amplitude), octave)| {
            sphere_noise_signed(
                unit_vec,
                TILES_PER_RADIAN / feature_tiles,
                salt.wrapping_add(octave),
            ) * amplitude
        })
        .sum()
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

fn dot3_local(lhs: [f64; 3], rhs: [f64; 3]) -> f64 {
    lhs[0] * rhs[0] + lhs[1] * rhs[1] + lhs[2] * rhs[2]
}

fn project_tangent_local(vec: [f64; 3], normal: [f64; 3]) -> [f64; 3] {
    let proj = dot3_local(vec, normal);
    [
        vec[0] - proj * normal[0],
        vec[1] - proj * normal[1],
        vec[2] - proj * normal[2],
    ]
}

fn normalize3_local(vec: [f64; 3]) -> [f64; 3] {
    let len = dot3_local(vec, vec).sqrt();
    if len <= 1.0e-12 {
        [0.0, 1.0, 0.0]
    } else {
        [vec[0] / len, vec[1] / len, vec[2] / len]
    }
}

fn index_to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "fine face coordinates stay below 2^26, exact in 53-bit f64"
)]
fn i64_to_f64(value: i64) -> f64 {
    value as f64
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
    reason = "hillshade, noise, and elevation values lie well inside f32 range"
)]
fn f64_to_f32(value: f64) -> f32 {
    value as f32
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "callers pass floors of small offsets in [-3, 3]"
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

    fn synthetic_planet(res: u32, elev_fn: impl Fn([f64; 3]) -> f64) -> PlanetHeightmaps {
        let res_f64 = f64::from(res);
        let faces = CubeFace::ALL.map(|face| {
            let mut samples = Vec::with_capacity(usize::try_from(res * res).expect("fits"));
            for py in 0..res {
                let fv = (f64::from(py) + 0.5) / res_f64 * 2.0 - 1.0;
                for px in 0..res {
                    let fu = (f64::from(px) + 0.5) / res_f64 * 2.0 - 1.0;
                    let ft = elev_fn(face_uv_to_unit_vec(face, fu, fv));
                    let encoded = (ft + f64::from(SEA_LEVEL_U16)).round().clamp(0.0, 65_535.0);
                    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    samples.push(encoded as u16);
                }
            }
            FaceHeightmap::new(res, samples).expect("valid synthetic face")
        });
        PlanetHeightmaps::new(faces).expect("valid synthetic planet")
    }

    fn climate_with_rgba(rgba: [u8; 4]) -> PlanetClimateMaps {
        let side = usize::try_from(MAP_RES * MAP_RES).expect("fits");
        let faces = CubeFace::ALL
            .map(|_| FaceClimateMap::new(MAP_RES, vec![rgba; side]).expect("valid climate face"));
        PlanetClimateMaps::new(faces).expect("valid climate")
    }

    fn temperate_climate() -> PlanetClimateMaps {
        climate_with_rgba([180, 130, 60, 0])
    }

    fn dense_forest_climate() -> PlanetClimateMaps {
        climate_with_rgba([180, 190, 235, 0])
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

    fn shade_texel(bake: &ChunkSplatBake, x: usize, y: usize) -> [u8; 4] {
        let offset = (y * SHADE_SIDE + x) * 4;
        bake.shade[offset..offset + 4].try_into().expect("4 bytes")
    }

    /// A square region covering `width x height` tiles at `(offset_x, offset_y)` inside `coord`,
    /// painted with 1 rect layer.
    fn rect_region_at(
        palette: &TerrainPalette,
        coord: ChunkCoord,
        id: &str,
        (offset_x, offset_y): (i64, i64),
        (width, height): (i64, i64),
    ) -> RegionPaint {
        let raw = format!(
            r#"{{"materials": [{{"id": "paint", "palette": "{id}"}}],
                "layers": [{{"material": "paint",
                             "shape": {{"kind": "rect", "min": [0, 0], "max": [{width}, {height}]}}}}]}}"#
        );
        let config: MapConfig = serde_json::from_str(&raw).expect("valid config");
        let origin = coord.origin_tile();
        let (min_x, min_y) = (
            i64::from(origin.x) + offset_x,
            i64::from(origin.y) + offset_y,
        );
        let (w_f32, h_f32) = (i64_to_f32(width), i64_to_f32(height));
        RegionPaint {
            face: coord.face,
            bounds: RegionBounds {
                min_x,
                min_y,
                max_x: min_x + width,
                max_y: min_y + height,
            },
            ring: vec![
                Vec2::ZERO,
                Vec2::new(w_f32, 0.0),
                Vec2::new(w_f32, h_f32),
                Vec2::new(0.0, h_f32),
            ],
            edge_feather: 1.0,
            layers: resolve_layers(&config, palette, &[]).expect("layers resolve"),
        }
    }

    fn square_region(palette: &TerrainPalette, coord: ChunkCoord, id: &str) -> RegionPaint {
        rect_region_at(palette, coord, id, (20, 20), (10, 10))
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
        let last_shade = SHADE_SIDE - 2;
        for i in 0..SHADE_SIDE {
            for halo in 0..2 {
                assert_eq!(
                    shade_texel(&left, last_shade + halo, i),
                    shade_texel(&right, halo, i)
                );
                assert_eq!(
                    shade_texel(&left, i, last_shade + halo),
                    shade_texel(&below, i, halo)
                );
            }
        }
    }

    #[test]
    fn adjacent_chunks_share_identical_border_shade_with_relief_foliage_and_spanning_region() {
        let maps = synthetic_planet(64, |[x, y, z]| 1_800.0 * x + 900.0 * y - 400.0 * z);
        let climate = dense_forest_climate();
        let palette = palette();
        let origin_chunk = mid_chunk(0, 0);
        let id = palette.entries()[0].id.clone();
        // Region spans across both the east (x = 64) and south (y = 64) chunk seams.
        let spanning = rect_region_at(&palette, origin_chunk, &id, (56, 56), (16, 16));
        let regions = [spanning];

        let top_left = bake_chunk_splat(&maps, &climate, &palette, origin_chunk, &regions);
        let top_right = bake_chunk_splat(&maps, &climate, &palette, mid_chunk(1, 0), &regions);
        let bottom_left = bake_chunk_splat(&maps, &climate, &palette, mid_chunk(0, 1), &regions);

        let last_shade = SHADE_SIDE - 2;
        for i in 0..SHADE_SIDE {
            for halo in 0..2 {
                assert_eq!(
                    shade_texel(&top_left, last_shade + halo, i),
                    shade_texel(&top_right, halo, i),
                    "east shade seam mismatch at row {i}, halo {halo}"
                );
                assert_eq!(
                    shade_texel(&top_left, i, last_shade + halo),
                    shade_texel(&bottom_left, i, halo),
                    "south shade seam mismatch at col {i}, halo {halo}"
                );
            }
        }
    }

    #[test]
    fn forest_canopy_creates_dappled_shade_suppressed_inside_painted_regions() {
        // Sea-level macro elevation (`0 ft`) clamps Layer 1 fractal relief to 0, isolating
        // canopy dapple from terrain hillshade.
        let maps = flat_planet(0);
        let palette = palette();
        let coord = mid_chunk(0, 0);
        let open_bake = bake_chunk_splat(&maps, &temperate_climate(), &palette, coord, &[]);
        let forest_bake = bake_chunk_splat(&maps, &dense_forest_climate(), &palette, coord, &[]);

        let shade_range = |bake: &ChunkSplatBake| -> u8 {
            let greens = (2..SHADE_SIDE - 2)
                .flat_map(|y| (2..SHADE_SIDE - 2).map(move |x| shade_texel(bake, x, y)[1]));
            let min_g = greens.clone().min().expect("non-empty");
            let max_g = greens.max().expect("non-empty");
            max_g - min_g
        };

        let open_range = shade_range(&open_bake);
        let forest_range = shade_range(&forest_bake);
        assert!(
            open_range <= 16,
            "low-foliage grassland must have only subtle sky drift (range={open_range})"
        );
        assert!(
            forest_range >= open_range * 3 && forest_range >= 36,
            "dense forest must exhibit strong sunlit-to-shaded canopy dapple (forest={forest_range}, open={open_range})"
        );

        let id = palette.entries()[0].id.clone();
        let road = rect_region_at(&palette, coord, &id, (10, 10), (44, 44));
        let painted_forest =
            bake_chunk_splat(&maps, &dense_forest_climate(), &palette, coord, &[road]);
        let interior_greens: Vec<u8> = (30..90)
            .map(|i| shade_texel(&painted_forest, i, 60)[1])
            .collect();
        let road_spread = interior_greens.iter().max().expect("non-empty")
            - interior_greens.iter().min().expect("non-empty");
        assert!(
            road_spread <= 1,
            "authored region paint must suppress artificial canopy darkening (spread={road_spread})"
        );
    }

    #[test]
    fn gentle_slopes_omit_rock_ao_while_steep_cliffs_darken_crevices() {
        let step_sample = |step_ft: i32| TileSample {
            elevation_ft: step_ft,
            elevation_step_ft: step_ft,
            is_water: false,
        };
        let gentle_center = step_sample(25);
        let gentle_neighbors = [
            step_sample(20),
            step_sample(30),
            step_sample(25),
            step_sample(25),
        ];
        assert_eq!(
            steep_rock_crevice_ao(gentle_center, 5, gentle_neighbors),
            0.0,
            "walkable 5-ft steps must never produce contour stripe darkening"
        );

        let cliff_base = step_sample(20);
        let cliff_neighbors = [
            step_sample(40),
            step_sample(20),
            step_sample(20),
            step_sample(20),
        ];
        assert!(
            steep_rock_crevice_ao(cliff_base, 20, cliff_neighbors) > 0.15,
            "steep rock cliff bases must receive sculptural crevice occlusion"
        );
    }

    #[test]
    fn shoreline_wetness_darkens_nearshore_land_and_water_deepens_offshore() {
        let unit = [0.0, 1.0, 0.0];
        let wet_beach = TileShadeSample {
            unit_vec: unit,
            continuous_elevation_ft: 3.0,
            hillshade: 0.707,
            water_weight: 0.0,
            moisture: 0.5,
            foliage: 0.1,
            snow_weight: 0.0,
            shore_tiles: 1.0,
            rock_crevice_ao: 0.0,
        };
        let dry_inland = TileShadeSample {
            shore_tiles: 12.0,
            ..wet_beach
        };
        let wet_rgb = subtile_shade_rgb(&wet_beach, 0.0);
        let dry_rgb = subtile_shade_rgb(&dry_inland, 0.0);
        assert!(
            wet_rgb[0] < dry_rgb[0] * 0.85,
            "nearshore sand/earth must darken with dampness: {wet_rgb:?} vs {dry_rgb:?}"
        );

        let shallow_water = TileShadeSample {
            continuous_elevation_ft: -3.0,
            water_weight: 1.0,
            shore_tiles: 1.0,
            ..wet_beach
        };
        let deep_water = TileShadeSample {
            continuous_elevation_ft: -400.0,
            water_weight: 1.0,
            shore_tiles: 16.0,
            ..wet_beach
        };
        let shallow_rgb = subtile_shade_rgb(&shallow_water, 0.0);
        let deep_rgb = subtile_shade_rgb(&deep_water, 0.0);
        assert!(
            shallow_rgb[1] > deep_rgb[1] * 1.15,
            "shallow surf must read brighter/warmer than deep ocean: {shallow_rgb:?} vs {deep_rgb:?}"
        );
    }

    #[test]
    fn dominant_indices_name_palette_entries() {
        let (maps, climate, palette) = (flat_planet(120), temperate_climate(), palette());
        let bake = bake_chunk_splat(&maps, &climate, &palette, mid_chunk(0, 0), &[]);
        let count = palette.entries().len();
        assert!(bake.ground.dominant.iter().all(|&i| usize::from(i) < count));
        assert!(bake.ground.region_painted.iter().all(|&painted| !painted));
        for chunk in bake.control.as_chunks::<4>().0 {
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
