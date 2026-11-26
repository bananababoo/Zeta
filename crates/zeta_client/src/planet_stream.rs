//! Continuous `64x64` cube-sphere chunk streaming, floating visual origin, seam-crossing
//! view rotation, and authored region polygon intersection queries.
//!
//! Rules are defined in `docs/architecture/world_geography.md` Sections 1–4.
//!
//! Coordinate contract:
//! - Cube-face coordinates use `+x` (`+U`) east and `+y` (`+V`) south, matching
//!   `zeta_core::world` and row-major `64x64` image buffers (`y = 0` is top).
//! - View-frame coordinates use `+x` screen right and `+y` screen down (`-Y` in Bevy 2D).
//! - [`PlanetCameraFocus::view_rotation`] transforms directions from the view frame into
//!   `anchor.face`'s local frame (`v_anchor = view_rotation.rotate(v_view)`).
//! - Bevy's camera stays fixed at `(0.0, 0.0)` while chunk sprites are placed relative to
//!   [`PlanetCameraFocus::local_offset_tiles`] so crossing any chunk edge or cube-face seam
//!   introduces zero floating-point drift and zero visual jump.

use std::collections::{HashMap, HashSet};
use std::f32::consts::{FRAC_PI_2, PI};
use std::f64::consts::FRAC_1_SQRT_2;
use std::path::Path;
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::image::{Image, ImageSampler};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
// `bevy::tasks`: bevy_internal-0.20.1/src/lib.rs:100.
// `AsyncComputeTaskPool`: bevy_tasks-0.20.1/src/lib.rs:82 -> bevy_tasks-0.20.1/src/usages.rs:67.
// `Task`: bevy_tasks-0.20.1/src/lib.rs:79 -> async-task-4.7.1/src/task.rs:49.
// `block_on`: bevy_tasks-0.20.1/src/lib.rs:107 -> bevy_platform-0.20.1/src/future.rs:14.
// `poll_once`: bevy_tasks-0.20.1/src/lib.rs:85 -> futures-lite-2.6.1/src/future.rs:116.
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on, poll_once};
use bevy::window::RequestRedraw;
use zeta_core::world::{
    CHUNK_SIZE, ChunkCoord, ChunkTerrain, CubeFace, ELEVATION_STEP_FT, FACE_TILES, FaceRotation,
    PlanetHeightmaps, PlanetTile, RegionFootprint, RegionOverlap, TileSample, face_uv_to_unit_vec,
    polygons_overlap, sample_chunk, sample_tile, step_chunk, step_tile, tile_to_unit_vec,
    validate_non_overlapping,
};

use crate::planet_io::{PlanetIoError, load_planet_heightmaps, planet_faces_dir};

/// Default chunk streaming ring half-width (`R = 4` streams a `9x9` chunk grid).
pub const DEFAULT_STREAM_RADIUS_CHUNKS: i32 = 4;

/// Default rendered screen pixels per 5-foot tactical square.
pub const DEFAULT_TILE_PIXEL_SIZE: f32 = 3.0;

/// Extra chunk rings retained outside [`PlanetCameraFocus::stream_radius_chunks`] before
/// despawning, preventing texture thrashing when oscillating across chunk edges.
pub const KEEP_RADIUS_MARGIN: i32 = 1;

/// Maximum concurrent background chunk generation tasks dispatched on [`AsyncComputeTaskPool`].
pub const MAX_IN_FLIGHT_CHUNK_TASKS: usize = 16;

/// Maximum completed chunk textures uploaded to `Assets<Image>` in a single frame.
pub const MAX_CHUNK_UPLOADS_PER_FRAME: usize = 4;

/// Draw depth of streamed planetary terrain sprites (behind `MapPlugin` splatmaps at `z = 0.0`).
pub const CHUNK_Z_DEPTH: f32 = -1.0;

/// Side length of 1 streaming chunk in floating-point tiles (`64.0`).
#[expect(
    clippy::cast_precision_loss,
    reason = "CHUNK_SIZE is 64, which is exact in f32"
)]
const CHUNK_SIZE_F32: f32 = CHUNK_SIZE as f32;

/// Strict upper bound `< CHUNK_SIZE_F32` used when a 90-degree rotation maps exact `0.0` to `64.0`.
const MAX_LOCAL_OFFSET_F32: f32 = f32::from_bits(CHUNK_SIZE_F32.to_bits() - 1);

/// Loaded 6-face Layer 0 planetary elevation dataset stored as a Bevy resource.
#[derive(Resource, Clone, Debug)]
pub struct PlanetWorld {
    maps: Arc<PlanetHeightmaps>,
}

impl PlanetWorld {
    /// Wraps an already-constructed [`PlanetHeightmaps`] dataset.
    #[must_use]
    pub fn new(maps: PlanetHeightmaps) -> Self {
        Self {
            maps: Arc::new(maps),
        }
    }

    /// Loads all 6 Layer 0 face heightmaps from `dir`.
    ///
    /// # Errors
    /// Returns [`PlanetIoError`] if any face PNG is missing, malformed, or inconsistent.
    pub fn load_from_dir(dir: &Path) -> Result<Self, PlanetIoError> {
        load_planet_heightmaps(dir).map(Self::new)
    }

    /// Loads the workspace Sera Layer 0 face heightmaps from `assets/world/faces/`.
    ///
    /// # Errors
    /// Returns [`PlanetIoError`] if loading or validating `assets/world/faces/` fails.
    pub fn load_default() -> Result<Self, PlanetIoError> {
        Self::load_from_dir(&planet_faces_dir())
    }

    /// Returns the underlying [`PlanetHeightmaps`].
    #[must_use]
    pub fn maps(&self) -> &PlanetHeightmaps {
        &self.maps
    }

    /// Returns an `O(1)` cloned [`Arc`] handle to the underlying [`PlanetHeightmaps`] for
    /// background chunk generation tasks.
    #[must_use]
    pub fn maps_arc(&self) -> Arc<PlanetHeightmaps> {
        Arc::clone(&self.maps)
    }
}

/// Controls whether streamed `64x64` chunk sampling and RGBA8 texture generation execute
/// asynchronously on [`AsyncComputeTaskPool`] or synchronously on the main thread.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlanetStreamSyncMode {
    /// Offloads chunk sampling and rasterization to [`AsyncComputeTaskPool`] with a per-frame
    /// GPU upload budget (`MAX_CHUNK_UPLOADS_PER_FRAME`), falling back to synchronous generation
    /// if no task pool is initialized.
    #[default]
    Async,
    /// Generates missing chunks synchronously during the current update tick (for deterministic
    /// screenshots and headless tests).
    Blocking,
}

/// Live streaming counters exposed for viewer HUD overlays and diagnostics.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanetStreamStats {
    /// Number of currently spawned chunk sprite entities (including hysteresis keep-margin).
    pub active_chunks: usize,
    /// Number of chunks still awaiting generation or upload in the active streaming ring.
    pub pending_chunks: usize,
    /// Number of background chunk generation tasks currently in flight.
    pub in_flight_tasks: usize,
}

/// Visual shading mode used to rasterize streamed `64x64` chunk textures.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PlanetRenderMode {
    /// Cartographic hypsometric bathymetry and land elevation with 3D hillshade and 5-ft steps.
    #[default]
    ElevationShaded,
    /// High-contrast binary land (`elevation >= 0 ft`) versus ocean (`elevation < 0 ft`) mask.
    LandOceanMask,
}

impl PlanetRenderMode {
    /// Returns the opposite rendering mode.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::ElevationShaded => Self::LandOceanMask,
            Self::LandOceanMask => Self::ElevationShaded,
        }
    }

    /// Human-readable label for viewer HUDs.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ElevationShaded => "Elevation Shaded",
            Self::LandOceanMask => "Land / Ocean Mask",
        }
    }
}

/// Floating visual origin and streaming parameters for the continuous cube-sphere camera.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct PlanetCameraFocus {
    /// Cube-sphere chunk currently anchoring local visual coordinates near `(0.0, 0.0)`.
    pub anchor: ChunkCoord,
    /// Continuous offset in 5-foot tiles inside `anchor` (`[0.0, CHUNK_SIZE)` in `anchor.face` frame).
    pub local_offset_tiles: Vec2,
    /// Rotation mapping screen/view directions (`+x` right, `+y` down) into `anchor.face`'s frame.
    pub view_rotation: FaceRotation,
    /// Streaming ring half-width `R`; renders `(2 * R + 1) x (2 * R + 1)` chunks.
    pub stream_radius_chunks: i32,
    /// Screen/world pixels per 5-foot square.
    pub tile_pixel_size: f32,
}

impl Default for PlanetCameraFocus {
    fn default() -> Self {
        Self::sera_seam()
    }
}

impl PlanetCameraFocus {
    /// Dramatic real Sera coastal mountain slope on `PosX` (`~2,200 ft` relief across 1 macro pixel).
    #[must_use]
    pub const fn sera_coast() -> Self {
        Self {
            anchor: ChunkCoord {
                face: CubeFace::PosX,
                cx: 28_453,
                cy: 4_944,
            },
            local_offset_tiles: Vec2::new(32.0, 32.0),
            view_rotation: FaceRotation::Deg0,
            stream_radius_chunks: DEFAULT_STREAM_RADIUS_CHUNKS,
            tile_pixel_size: DEFAULT_TILE_PIXEL_SIZE,
        }
    }

    /// Real Sera coastline crossing the rotated `NegX <-> PosY` (`90`-degree) cube-face seam.
    #[must_use]
    pub const fn sera_seam() -> Self {
        Self {
            anchor: ChunkCoord {
                face: CubeFace::NegX,
                cx: 6_118,
                cy: 0,
            },
            local_offset_tiles: Vec2::new(32.0, 4.0),
            view_rotation: FaceRotation::Deg0,
            stream_radius_chunks: DEFAULT_STREAM_RADIUS_CHUNKS,
            tile_pixel_size: DEFAULT_TILE_PIXEL_SIZE,
        }
    }
}

/// Validated non-overlapping collection of authored region polygon footprints.
///
/// Ready to composite authored town overlays onto streamed chunks without overlap conflicts
/// (`docs/architecture/world_geography.md` Section 4).
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct PlanetRegions {
    regions: Vec<RegionFootprint>,
}

impl PlanetRegions {
    /// Validates `regions` for non-overlap and constructs a [`PlanetRegions`] resource.
    ///
    /// # Errors
    /// Returns every overlapping region pair reported by [`validate_non_overlapping`].
    pub fn new(regions: Vec<RegionFootprint>) -> Result<Self, Vec<RegionOverlap>> {
        validate_non_overlapping(&regions)?;
        Ok(Self { regions })
    }

    /// Returns all validated region footprints.
    #[must_use]
    pub fn regions(&self) -> &[RegionFootprint] {
        &self.regions
    }

    /// Returns all authored regions whose interior polygon overlaps `coord`'s `64x64` tile bounds.
    #[must_use]
    pub fn regions_intersecting_chunk(&self, coord: ChunkCoord) -> Vec<&RegionFootprint> {
        let min_x = i64::from(coord.cx) * i64::from(CHUNK_SIZE);
        let min_y = i64::from(coord.cy) * i64::from(CHUNK_SIZE);
        let max_x = min_x + i64::from(CHUNK_SIZE);
        let max_y = min_y + i64::from(CHUNK_SIZE);
        let chunk_rect = [
            [min_x, min_y],
            [max_x, min_y],
            [max_x, max_y],
            [min_x, max_y],
        ];

        self.regions
            .iter()
            .filter(|region| {
                region.face == coord.face
                    && polygons_overlap(&region.polygon, &chunk_rect).is_some()
            })
            .collect()
    }
}

/// Marks the 2D camera spawned by [`PlanetStreamPlugin`].
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanetCamera;

/// Marks 1 streamed `64x64` planetary terrain chunk sprite entity.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamedChunk {
    /// Planetary chunk coordinate rendered by this entity.
    pub coord: ChunkCoord,
}

/// Resolved placement and orientation of 1 chunk inside the active streaming ring.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StreamedChunkSlot {
    /// Planetary chunk coordinate on its cube face.
    pub coord: ChunkCoord,
    /// Grid offset `(view_dcx, view_dcy)` from the focus anchor in screen/view space.
    pub view_offset: IVec2,
    /// Rotation from view frame into `coord.face`'s local frame.
    pub view_to_chunk_rot: FaceRotation,
    /// Rotation from `coord.face`'s local frame into the screen/view frame.
    pub chunk_to_view_rot: FaceRotation,
    /// Bevy 2D world-space center of this chunk relative to the floating camera origin `(0, 0)`.
    pub world_center: Vec2,
}

#[derive(Clone, Debug)]
struct ActiveChunk {
    entity: Entity,
    image: Handle<Image>,
    rendered_mode: PlanetRenderMode,
}

struct GeneratedChunk {
    image: Image,
    mode: PlanetRenderMode,
}

struct PendingChunkTask {
    mode: PlanetRenderMode,
    task: Task<GeneratedChunk>,
}

/// Internal cache of currently spawned chunk sprites and in-flight async tasks keyed by [`ChunkCoord`].
#[derive(Resource, Default)]
struct PlanetStreamCache {
    active: HashMap<ChunkCoord, ActiveChunk>,
    pending: HashMap<ChunkCoord, PendingChunkTask>,
}

/// System set in which gameplay plugins update [`PlanetCameraFocus`] before chunk transforms
/// and streaming run in the same frame.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlanetFocusSync;

/// Streams a `(2*R + 1) x (2*R + 1)` ring of `64x64` cube-sphere chunks around
/// [`PlanetCameraFocus`] with a floating visual origin near `(0.0, 0.0)`.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlanetStreamPlugin;

impl Plugin for PlanetStreamPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlanetRenderMode>()
            .init_resource::<PlanetStreamSyncMode>()
            .init_resource::<PlanetCameraFocus>()
            .init_resource::<PlanetRegions>()
            .init_resource::<PlanetStreamStats>()
            .init_resource::<PlanetStreamCache>()
            .add_message::<RequestRedraw>()
            .add_systems(Startup, ensure_planet_world)
            .add_systems(
                Update,
                (recenter_focus_system, sync_streamed_chunks)
                    .chain()
                    .after(PlanetFocusSync),
            );
        // `App::is_plugin_added`: bevy_app-0.20.1/src/app.rs:570.
        // Avoid spawning a second `Camera2d` when `MapPlugin` already provides `MapCamera`.
        if !app.is_plugin_added::<crate::map::MapPlugin>() {
            app.add_systems(Startup, ensure_planet_camera);
        }
    }
}

/// Re-anchors `focus` whenever `local_offset_tiles` leaves `[0.0, CHUNK_SIZE)`.
///
/// Steps `focus.anchor` via [`step_chunk`], rotates the remaining local offset into the new
/// face's coordinate frame, and composes `focus.view_rotation` so crossing any of the 12
/// cube-face seams preserves exact planetary position and visual orientation without jitter.
pub fn recenter_focus(focus: &mut PlanetCameraFocus) {
    let col_step_f = (focus.local_offset_tiles.x / CHUNK_SIZE_F32).floor();
    let row_step_f = (focus.local_offset_tiles.y / CHUNK_SIZE_F32).floor();

    #[expect(
        clippy::cast_possible_truncation,
        reason = "pan offsets per step stay well inside i32 chunk-step range"
    )]
    let col_step = col_step_f as i32;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "pan offsets per step stay well inside i32 chunk-step range"
    )]
    let row_step = row_step_f as i32;

    if col_step == 0 && row_step == 0 {
        focus.local_offset_tiles.x = focus.local_offset_tiles.x.clamp(0.0, MAX_LOCAL_OFFSET_F32);
        focus.local_offset_tiles.y = focus.local_offset_tiles.y.clamp(0.0, MAX_LOCAL_OFFSET_F32);
        return;
    }

    let rem_col =
        (focus.local_offset_tiles.x - col_step_f * CHUNK_SIZE_F32).clamp(0.0, MAX_LOCAL_OFFSET_F32);
    let rem_row =
        (focus.local_offset_tiles.y - row_step_f * CHUNK_SIZE_F32).clamp(0.0, MAX_LOCAL_OFFSET_F32);

    let (next_anchor, step_rot) = step_chunk(focus.anchor, col_step, row_step);
    let rotated_local = rotate_local_tiles(step_rot, Vec2::new(rem_col, rem_row));

    focus.anchor = next_anchor;
    focus.local_offset_tiles = Vec2::new(
        rotated_local.x.clamp(0.0, MAX_LOCAL_OFFSET_F32),
        rotated_local.y.clamp(0.0, MAX_LOCAL_OFFSET_F32),
    );
    focus.view_rotation = focus.view_rotation.compose(step_rot);
}

/// Pans `focus` by `view_delta_tiles` in screen/view space (`+x` right, `+y` down) and
/// immediately re-anchors across chunk or face boundaries.
pub fn pan_focus_view(focus: &mut PlanetCameraFocus, view_delta_tiles: Vec2) {
    let anchor_delta = rotate_vec2_f32(focus.view_rotation, view_delta_tiles);
    focus.local_offset_tiles += anchor_delta;
    recenter_focus(focus);
}

/// Returns the discrete [`PlanetTile`] currently under `focus`.
#[must_use]
pub fn focus_tile(focus: &PlanetCameraFocus) -> PlanetTile {
    let mut normalized = *focus;
    recenter_focus(&mut normalized);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "recenter_focus clamps local_offset_tiles into [0.0, CHUNK_SIZE)"
    )]
    let local_x = (normalized.local_offset_tiles.x.floor() as u32).min(CHUNK_SIZE - 1);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "recenter_focus clamps local_offset_tiles into [0.0, CHUNK_SIZE)"
    )]
    let local_y = (normalized.local_offset_tiles.y.floor() as u32).min(CHUNK_SIZE - 1);
    normalized
        .anchor
        .tile_at(local_x, local_y)
        .unwrap_or_else(|| normalized.anchor.origin_tile())
}

/// Computes the exact continuous 3D unit-sphere direction of `focus` (sub-tile precision).
#[must_use]
pub fn focus_unit_vec(focus: &PlanetCameraFocus) -> [f64; 3] {
    let origin = focus.anchor.origin_tile();
    let tile_x = f64::from(origin.x) + f64::from(focus.local_offset_tiles.x);
    let tile_y = f64::from(origin.y) + f64::from(focus.local_offset_tiles.y);
    let scale = f64::from(FACE_TILES);
    let face_u = (tile_x / scale) * 2.0 - 1.0;
    let face_v = (tile_y / scale) * 2.0 - 1.0;
    face_uv_to_unit_vec(focus.anchor.face, face_u, face_v)
}

/// Rotates a continuous local chunk offset in `[0.0, CHUNK_SIZE]^2` by `rotation`.
#[must_use]
pub fn rotate_local_tiles(rotation: FaceRotation, local: Vec2) -> Vec2 {
    match rotation {
        FaceRotation::Deg0 => local,
        FaceRotation::Deg90 => Vec2::new(CHUNK_SIZE_F32 - local.y, local.x),
        FaceRotation::Deg180 => Vec2::new(CHUNK_SIZE_F32 - local.x, CHUNK_SIZE_F32 - local.y),
        FaceRotation::Deg270 => Vec2::new(local.y, CHUNK_SIZE_F32 - local.x),
    }
}

/// Rotates a continuous 2D direction vector in `(+x right, +y down)` coordinates by `rotation`.
#[must_use]
pub fn rotate_vec2_f32(rotation: FaceRotation, delta: Vec2) -> Vec2 {
    match rotation {
        FaceRotation::Deg0 => delta,
        FaceRotation::Deg90 => Vec2::new(-delta.y, delta.x),
        FaceRotation::Deg180 => Vec2::new(-delta.x, -delta.y),
        FaceRotation::Deg270 => Vec2::new(delta.y, -delta.x),
    }
}

/// Converts a clockwise `chunk -> view` [`FaceRotation`] into a Bevy 2D counter-clockwise Z angle.
#[must_use]
pub const fn face_rotation_to_bevy_z_radians(chunk_to_view_rot: FaceRotation) -> f32 {
    match chunk_to_view_rot {
        FaceRotation::Deg0 => 0.0,
        FaceRotation::Deg90 => -FRAC_PI_2,
        FaceRotation::Deg180 => PI,
        FaceRotation::Deg270 => FRAC_PI_2,
    }
}

const fn slot_sort_key(slot: &StreamedChunkSlot) -> (i32, i32, i32) {
    (
        slot.view_offset.x.abs() + slot.view_offset.y.abs(),
        slot.view_offset.y,
        slot.view_offset.x,
    )
}

fn resolve_chunk_slot(
    focus: &PlanetCameraFocus,
    focus_in_view: Vec2,
    tile_px: f32,
    view_col: i32,
    view_row: i32,
) -> StreamedChunkSlot {
    let half_chunk = CHUNK_SIZE_F32 * 0.5;
    let (anchor_col_i64, anchor_row_i64) = focus
        .view_rotation
        .rotate_delta(i64::from(view_col), i64::from(view_row));
    let anchor_col = i32::try_from(anchor_col_i64).unwrap_or(0);
    let anchor_row = i32::try_from(anchor_row_i64).unwrap_or(0);

    let (coord, step_rot) = step_chunk(focus.anchor, anchor_col, anchor_row);
    let view_to_chunk_rot = focus.view_rotation.compose(step_rot);
    let chunk_to_view_rot = view_to_chunk_rot.inverse();

    #[expect(
        clippy::cast_precision_loss,
        reason = "view chunk offsets are small streaming ring integers"
    )]
    let world_x = (view_col as f32 * CHUNK_SIZE_F32 + half_chunk - focus_in_view.x) * tile_px;
    #[expect(
        clippy::cast_precision_loss,
        reason = "view chunk offsets are small streaming ring integers"
    )]
    let world_y = (-view_row as f32 * CHUNK_SIZE_F32 - half_chunk + focus_in_view.y) * tile_px;

    StreamedChunkSlot {
        coord,
        view_offset: IVec2::new(view_col, view_row),
        view_to_chunk_rot,
        chunk_to_view_rot,
        world_center: Vec2::new(world_x, world_y),
    }
}

/// Resolves the `(2*R + 1) x (2*R + 1)` streamed chunk ring around `focus.anchor`.
///
/// Each slot computes its cube-sphere [`ChunkCoord`] via [`step_chunk`], its orientation in
/// screen space, and its Bevy 2D world center relative to the floating camera origin `(0.0, 0.0)`.
#[must_use]
pub fn streamed_ring(focus: &PlanetCameraFocus) -> Vec<StreamedChunkSlot> {
    let radius = focus.stream_radius_chunks.max(0);
    let anchor_to_view = focus.view_rotation.inverse();
    let focus_in_view = rotate_local_tiles(anchor_to_view, focus.local_offset_tiles);
    let tile_px = focus.tile_pixel_size.max(0.1);

    let side = usize::try_from(radius.saturating_mul(2).saturating_add(1)).unwrap_or(1);
    let mut candidates = Vec::with_capacity(side.saturating_mul(side));

    for view_row in -radius..=radius {
        for view_col in -radius..=radius {
            candidates.push(resolve_chunk_slot(
                focus,
                focus_in_view,
                tile_px,
                view_col,
                view_row,
            ));
        }
    }

    // Near the 8 cube corners (3 faces meeting instead of 4), sort closest-first so any
    // corner-aliased ChunkCoord keeps its primary slot without spawning duplicate sprites.
    candidates.sort_by_key(slot_sort_key);

    let mut seen = HashSet::with_capacity(candidates.len());
    candidates
        .into_iter()
        .filter(|slot| seen.insert(slot.coord))
        .collect()
}

/// Resolves the active streaming ring together with the wider hysteresis keep-margin slot map
/// (`keep_radius = stream_radius_chunks + KEEP_RADIUS_MARGIN`).
fn streamed_ring_with_keep(
    focus: &PlanetCameraFocus,
) -> (
    Vec<StreamedChunkSlot>,
    HashMap<ChunkCoord, StreamedChunkSlot>,
) {
    let stream_radius = focus.stream_radius_chunks.max(0);
    let keep_radius = stream_radius.saturating_add(KEEP_RADIUS_MARGIN);
    let stream_ring = streamed_ring(focus);

    let keep_side = usize::try_from(keep_radius.saturating_mul(2).saturating_add(1)).unwrap_or(1);
    let mut keep_slots = HashMap::with_capacity(keep_side.saturating_mul(keep_side));
    for &slot in &stream_ring {
        keep_slots.insert(slot.coord, slot);
    }

    let anchor_to_view = focus.view_rotation.inverse();
    let focus_in_view = rotate_local_tiles(anchor_to_view, focus.local_offset_tiles);
    let tile_px = focus.tile_pixel_size.max(0.1);
    let mut margin_candidates = Vec::new();

    for view_row in -keep_radius..=keep_radius {
        for view_col in -keep_radius..=keep_radius {
            if view_col.abs() <= stream_radius && view_row.abs() <= stream_radius {
                continue;
            }
            margin_candidates.push(resolve_chunk_slot(
                focus,
                focus_in_view,
                tile_px,
                view_col,
                view_row,
            ));
        }
    }
    margin_candidates.sort_by_key(slot_sort_key);
    for slot in margin_candidates {
        keep_slots.entry(slot.coord).or_insert(slot);
    }

    (stream_ring, keep_slots)
}

/// Samples a `(CHUNK_SIZE + 2) x (CHUNK_SIZE + 2)` elevation and 5-ft step halo across chunk
/// and cube-face seams so finite-difference normals and contour steps are seamless at edges.
fn sample_chunk_halo(maps: &PlanetHeightmaps, terrain: &ChunkTerrain) -> (Vec<i32>, Vec<i32>) {
    let side = usize::try_from(CHUNK_SIZE).unwrap_or(64);
    let halo_side = side + 2;
    let mut halo_elev = vec![0_i32; halo_side * halo_side];
    let mut halo_step = vec![0_i32; halo_side * halo_side];

    for local_y in 0..CHUNK_SIZE {
        let hy = usize::try_from(local_y).unwrap_or(0) + 1;
        for local_x in 0..CHUNK_SIZE {
            let hx = usize::try_from(local_x).unwrap_or(0) + 1;
            let sample = terrain.tile(local_x, local_y).unwrap_or(TileSample {
                elevation_ft: 0,
                elevation_step_ft: 0,
                is_water: false,
            });
            halo_elev[hy * halo_side + hx] = sample.elevation_ft;
            halo_step[hy * halo_side + hx] = sample.elevation_step_ft;
        }
    }

    let origin = terrain.coord.origin_tile();
    let max_local = i64::from(CHUNK_SIZE);
    for edge_i in 0..max_local {
        let halo_idx = usize::try_from(edge_i).unwrap_or(0) + 1;

        let (top_tile, _) = step_tile(origin, edge_i, -1);
        let top_sample = sample_tile(maps, top_tile);
        halo_elev[halo_idx] = top_sample.elevation_ft;
        halo_step[halo_idx] = top_sample.elevation_step_ft;

        let (bot_tile, _) = step_tile(origin, edge_i, max_local);
        let bot_sample = sample_tile(maps, bot_tile);
        halo_elev[(halo_side - 1) * halo_side + halo_idx] = bot_sample.elevation_ft;
        halo_step[(halo_side - 1) * halo_side + halo_idx] = bot_sample.elevation_step_ft;

        let (left_tile, _) = step_tile(origin, -1, edge_i);
        let left_sample = sample_tile(maps, left_tile);
        halo_elev[halo_idx * halo_side] = left_sample.elevation_ft;
        halo_step[halo_idx * halo_side] = left_sample.elevation_step_ft;

        let (right_tile, _) = step_tile(origin, max_local, edge_i);
        let right_sample = sample_tile(maps, right_tile);
        halo_elev[halo_idx * halo_side + (halo_side - 1)] = right_sample.elevation_ft;
        halo_step[halo_idx * halo_side + (halo_side - 1)] = right_sample.elevation_step_ft;
    }

    (halo_elev, halo_step)
}

/// Rasterizes 1 `64x64` [`ChunkTerrain`] into an RGBA8 sRGB pixel buffer (`64 * 64 * 4` bytes).
///
/// Uses a 1-tile cross-face halo around the chunk and 3D unit-sphere tangent hillshading so
/// illumination, bathymetry, elevation bands, and 5-foot contour steps remain seamless across
/// chunk boundaries and all 12 rotated cube-face seams.
#[must_use]
pub fn rasterize_chunk_rgba8(
    maps: &PlanetHeightmaps,
    terrain: &ChunkTerrain,
    mode: PlanetRenderMode,
) -> Vec<u8> {
    let side = usize::try_from(CHUNK_SIZE).unwrap_or(64);
    let mut pixels = vec![0_u8; side * side * 4];

    if mode == PlanetRenderMode::LandOceanMask {
        for local_y in 0..CHUNK_SIZE {
            for local_x in 0..CHUNK_SIZE {
                let sample = terrain.tile(local_x, local_y).unwrap_or(TileSample {
                    elevation_ft: 0,
                    elevation_step_ft: 0,
                    is_water: false,
                });
                let color = if sample.is_water {
                    [28_u8, 72, 118, 255]
                } else {
                    [202_u8, 194, 154, 255]
                };
                let idx = (usize::try_from(local_y).unwrap_or(0) * side
                    + usize::try_from(local_x).unwrap_or(0))
                    * 4;
                pixels[idx..idx + 4].copy_from_slice(&color);
            }
        }
        return pixels;
    }

    let halo_side = side + 2;
    let (halo_elev, halo_step) = sample_chunk_halo(maps, terrain);
    let origin = terrain.coord.origin_tile();
    let basis = terrain.coord.face.basis();
    let face_right = [
        f64::from(basis.right_u[0]),
        f64::from(basis.right_u[1]),
        f64::from(basis.right_u[2]),
    ];
    let face_down = [
        f64::from(basis.down_v[0]),
        f64::from(basis.down_v[1]),
        f64::from(basis.down_v[2]),
    ];

    for local_y in 0..CHUNK_SIZE {
        let hy = usize::try_from(local_y).unwrap_or(0) + 1;
        for local_x in 0..CHUNK_SIZE {
            let hx = usize::try_from(local_x).unwrap_or(0) + 1;
            let sample = terrain.tile(local_x, local_y).unwrap_or(TileSample {
                elevation_ft: 0,
                elevation_step_ft: 0,
                is_water: false,
            });

            let left_e = halo_elev[hy * halo_side + (hx - 1)];
            let right_e = halo_elev[hy * halo_side + (hx + 1)];
            let up_e = halo_elev[(hy - 1) * halo_side + hx];
            let down_e = halo_elev[(hy + 1) * halo_side + hx];

            let slope_east = f64::from(right_e - left_e) * 0.5;
            let slope_south = f64::from(down_e - up_e) * 0.5;

            let tile = terrain.coord.tile_at(local_x, local_y).unwrap_or(origin);
            let hillshade = spherical_hillshade(
                tile_to_unit_vec(tile),
                face_right,
                face_down,
                slope_east,
                slope_south,
            );
            let base_rgb = hypsometric_color(sample);

            let center_step = halo_step[hy * halo_side + hx];
            let has_lower_step = halo_step[hy * halo_side + (hx - 1)] < center_step
                || halo_step[hy * halo_side + (hx + 1)] < center_step
                || halo_step[(hy - 1) * halo_side + hx] < center_step
                || halo_step[(hy + 1) * halo_side + hx] < center_step;
            let is_shoreline =
                !sample.is_water && (left_e < 0 || right_e < 0 || up_e < 0 || down_e < 0);

            let mut shade_factor = if sample.is_water {
                0.88 + 0.24 * hillshade
            } else {
                0.70 + 0.52 * hillshade
            };
            if has_lower_step {
                let is_major_contour = center_step.rem_euclid(ELEVATION_STEP_FT * 5) == 0;
                shade_factor *= if sample.is_water {
                    0.96
                } else if is_major_contour {
                    0.87
                } else {
                    0.93
                };
            }
            if is_shoreline {
                shade_factor *= 1.08;
            }

            let idx = (usize::try_from(local_y).unwrap_or(0) * side
                + usize::try_from(local_x).unwrap_or(0))
                * 4;
            pixels[idx] = scale_channel(base_rgb[0], shade_factor);
            pixels[idx + 1] = scale_channel(base_rgb[1], shade_factor);
            pixels[idx + 2] = scale_channel(base_rgb[2], shade_factor);
            pixels[idx + 3] = 255;
        }
    }

    pixels
}

/// Builds a `64x64` Bevy [`Image`] texture for 1 streamed chunk.
#[must_use]
pub fn build_chunk_image(
    maps: &PlanetHeightmaps,
    terrain: &ChunkTerrain,
    mode: PlanetRenderMode,
) -> Image {
    let data = rasterize_chunk_rgba8(maps, terrain, mode);
    // `Image::new`: bevy_image-0.20.1/src/image.rs:1123.
    // `Extent3d`: wgpu-types-30.0.2/src/origin_extent.rs:93.
    // `TextureDimension::D2`: wgpu-types-30.0.2/src/texture.rs:26.
    // `TextureFormat::Rgba8UnormSrgb`: wgpu-types-30.0.2/src/texture/format.rs:187.
    // `RenderAssetUsages::RENDER_WORLD`: bevy_asset-0.20.1/src/render_asset.rs:34.
    let mut image = Image::new(
        Extent3d {
            width: CHUNK_SIZE,
            height: CHUNK_SIZE,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    // `ImageSampler::nearest`: bevy_image-0.20.1/src/image.rs:714.
    // Nearest sampling prevents edge-clamp seams between adjacent chunk sprites and keeps 5-ft steps crisp.
    image.sampler = ImageSampler::nearest();
    image
}

/// Computes face-independent NW cartographic hillshading in `[0.0, 1.0]` using 3D unit-sphere
/// tangent vectors so lighting is strictly continuous across all 12 cube-face seams.
fn spherical_hillshade(
    unit_vec: [f64; 3],
    face_right: [f64; 3],
    face_down: [f64; 3],
    slope_east: f64,
    slope_south: f64,
) -> f64 {
    // Project cube face axes onto the sphere tangent plane at unit_vec.
    let tan_u = normalize3(project_tangent(face_right, unit_vec));
    let tan_v = normalize3(project_tangent(face_down, unit_vec));

    // Scale vertical gradient (feet per 5-ft tile) with gentle cartographic exaggeration.
    let exaggeration = 1.1;
    let grad_3d = [
        (slope_east * tan_u[0] + slope_south * tan_v[0]) * exaggeration,
        (slope_east * tan_u[1] + slope_south * tan_v[1]) * exaggeration,
        (slope_east * tan_u[2] + slope_south * tan_v[2]) * exaggeration,
    ];

    // Smooth global tangent NW light vector on the sphere (blends smoothly near exact poles).
    let horiz_sq = unit_vec[0] * unit_vec[0] + unit_vec[2] * unit_vec[2];
    let light_tangent = if horiz_sq > 1.0e-6 {
        let east = normalize3([unit_vec[2], 0.0, -unit_vec[0]]);
        let north = normalize3(cross3(unit_vec, east));
        normalize3([
            -FRAC_1_SQRT_2 * east[0] + FRAC_1_SQRT_2 * north[0],
            -FRAC_1_SQRT_2 * east[1] + FRAC_1_SQRT_2 * north[1],
            -FRAC_1_SQRT_2 * east[2] + FRAC_1_SQRT_2 * north[2],
        ])
    } else {
        normalize3(project_tangent(
            [-FRAC_1_SQRT_2, 0.0, -FRAC_1_SQRT_2],
            unit_vec,
        ))
    };

    // Surface normal in tangent+up frame: N = normalize(-grad_3d + 5.0 * unit_vec).
    let normal = normalize3([
        -grad_3d[0] + 5.0 * unit_vec[0],
        -grad_3d[1] + 5.0 * unit_vec[1],
        -grad_3d[2] + 5.0 * unit_vec[2],
    ]);
    // Sun altitude ~45 deg above local horizon from NW.
    let light_dir = normalize3([
        FRAC_1_SQRT_2 * light_tangent[0] + FRAC_1_SQRT_2 * unit_vec[0],
        FRAC_1_SQRT_2 * light_tangent[1] + FRAC_1_SQRT_2 * unit_vec[1],
        FRAC_1_SQRT_2 * light_tangent[2] + FRAC_1_SQRT_2 * unit_vec[2],
    ]);

    dot3(normal, light_dir).clamp(0.0, 1.0)
}

fn hypsometric_color(sample: TileSample) -> [f64; 3] {
    let elev = f64::from(sample.elevation_ft);
    if sample.is_water {
        const OCEAN_STOPS: [(f64, [f64; 3]); 5] = [
            (-5_000.0, [12.0, 38.0, 74.0]),
            (-1_500.0, [20.0, 62.0, 108.0]),
            (-350.0, [34.0, 96.0, 142.0]),
            (-60.0, [52.0, 130.0, 168.0]),
            (-1.0, [76.0, 158.0, 184.0]),
        ];
        sample_ramp(&OCEAN_STOPS, elev)
    } else {
        const LAND_STOPS: [(f64, [f64; 3]); 8] = [
            (0.0, [212.0, 196.0, 150.0]),
            (25.0, [138.0, 168.0, 102.0]),
            (180.0, [108.0, 148.0, 84.0]),
            (550.0, [138.0, 146.0, 84.0]),
            (1_300.0, [172.0, 150.0, 104.0]),
            (2_800.0, [146.0, 118.0, 92.0]),
            (4_800.0, [158.0, 154.0, 150.0]),
            (7_500.0, [238.0, 240.0, 244.0]),
        ];
        sample_ramp(&LAND_STOPS, elev)
    }
}

fn sample_ramp(stops: &[(f64, [f64; 3])], value: f64) -> [f64; 3] {
    if value <= stops[0].0 {
        return stops[0].1;
    }
    for window in stops.windows(2) {
        let (v0, c0) = window[0];
        let (v1, c1) = window[1];
        if value <= v1 {
            let t = ((value - v0) / (v1 - v0)).clamp(0.0, 1.0);
            return [
                c0[0] + (c1[0] - c0[0]) * t,
                c0[1] + (c1[1] - c0[1]) * t,
                c0[2] + (c1[2] - c0[2]) * t,
            ];
        }
    }
    stops[stops.len() - 1].1
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped into [0.0, 255.0] before casting to u8"
)]
fn scale_channel(channel: f64, factor: f64) -> u8 {
    (channel * factor).round().clamp(0.0, 255.0) as u8
}

fn dot3(lhs: [f64; 3], rhs: [f64; 3]) -> f64 {
    lhs[0] * rhs[0] + lhs[1] * rhs[1] + lhs[2] * rhs[2]
}

fn cross3(lhs: [f64; 3], rhs: [f64; 3]) -> [f64; 3] {
    [
        lhs[1] * rhs[2] - lhs[2] * rhs[1],
        lhs[2] * rhs[0] - lhs[0] * rhs[2],
        lhs[0] * rhs[1] - lhs[1] * rhs[0],
    ]
}

fn project_tangent(vec: [f64; 3], normal: [f64; 3]) -> [f64; 3] {
    let proj = dot3(vec, normal);
    [
        vec[0] - proj * normal[0],
        vec[1] - proj * normal[1],
        vec[2] - proj * normal[2],
    ]
}

fn normalize3(vec: [f64; 3]) -> [f64; 3] {
    let len = dot3(vec, vec).sqrt();
    if len <= 1.0e-12 {
        [0.0, 1.0, 0.0]
    } else {
        [vec[0] / len, vec[1] / len, vec[2] / len]
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Bevy system parameters are passed by value"
)]
fn ensure_planet_world(mut commands: Commands, existing: Option<Res<PlanetWorld>>) {
    if existing.is_some() {
        return;
    }
    match PlanetWorld::load_default() {
        Ok(world) => commands.insert_resource(world),
        Err(error) => error!("could not load Sera planet faces: {error}"),
    }
}

fn ensure_planet_camera(mut commands: Commands, cameras: Query<(), With<Camera>>) {
    if cameras.is_empty() {
        // `Camera2d`: bevy_camera-0.20.1/src/components.rs:16.
        commands.spawn((PlanetCamera, Camera2d));
    }
}

fn recenter_focus_system(mut focus: ResMut<PlanetCameraFocus>) {
    let before = *focus;
    let mut updated = before;
    recenter_focus(&mut updated);
    if updated != before {
        *focus = updated;
    }
}

fn chunk_transform(slot: &StreamedChunkSlot) -> Transform {
    let angle = face_rotation_to_bevy_z_radians(slot.chunk_to_view_rot);
    // `Transform::from_xyz`: bevy_transform-0.20.1/src/components/transform.rs:119.
    // `Transform::with_rotation`: bevy_transform-0.20.1/src/components/transform.rs:250.
    // `Quat::from_rotation_z`: glam-0.33.12/src/f32/coresimd/quat.rs:182.
    Transform::from_xyz(slot.world_center.x, slot.world_center.y, CHUNK_Z_DEPTH)
        .with_rotation(Quat::from_rotation_z(angle))
}

fn apply_chunk_image(
    slot: &StreamedChunkSlot,
    generated: GeneratedChunk,
    chunk_world_size: Vec2,
    commands: &mut Commands,
    images: Option<&mut Assets<Image>>,
    active: &mut HashMap<ChunkCoord, ActiveChunk>,
    chunk_transforms: &mut Query<(&mut Transform, &mut Sprite), With<StreamedChunk>>,
) {
    let desired_transform = chunk_transform(slot);
    if let Some(entry) = active.get_mut(&slot.coord) {
        let new_image = if let Some(images) = images {
            let old_handle = entry.image.clone();
            // `Assets::add`: bevy_asset-0.20.1/src/assets.rs:401.
            let handle = images.add(generated.image);
            // `Assets::remove`: bevy_asset-0.20.1/src/assets.rs:470.
            images.remove(&old_handle);
            handle
        } else {
            Handle::default()
        };
        entry.image = new_image.clone();
        entry.rendered_mode = generated.mode;
        if let Ok((mut transform, mut sprite)) = chunk_transforms.get_mut(entry.entity) {
            *transform = desired_transform;
            sprite.image = new_image;
            sprite.custom_size = Some(chunk_world_size);
        }
        return;
    }

    let handle = if let Some(images) = images {
        images.add(generated.image)
    } else {
        Handle::default()
    };

    // `Sprite::from_image`: bevy_sprite-0.20.1/src/sprite.rs:56.
    // `Sprite::custom_size`: bevy_sprite-0.20.1/src/sprite.rs:31.
    let mut sprite = Sprite::from_image(handle.clone());
    sprite.custom_size = Some(chunk_world_size);

    let entity = commands
        .spawn((
            StreamedChunk { coord: slot.coord },
            sprite,
            desired_transform,
        ))
        .id();

    active.insert(
        slot.coord,
        ActiveChunk {
            entity,
            image: handle,
            rendered_mode: generated.mode,
        },
    );
}

fn dispatch_async_chunks(
    pool: &AsyncComputeTaskPool,
    world: &PlanetWorld,
    stream_ring: &[StreamedChunkSlot],
    mode: PlanetRenderMode,
    cache: &mut PlanetStreamCache,
) {
    // Dispatch missing or stale-mode chunks closest-to-camera first up to `MAX_IN_FLIGHT_CHUNK_TASKS`.
    for slot in stream_ring {
        if cache.pending.len() >= MAX_IN_FLIGHT_CHUNK_TASKS {
            break;
        }
        let up_to_date = cache
            .active
            .get(&slot.coord)
            .is_some_and(|entry| entry.rendered_mode == mode);
        if up_to_date || cache.pending.contains_key(&slot.coord) {
            continue;
        }
        let maps = world.maps_arc();
        let coord = slot.coord;
        // `TaskPool::spawn`: bevy_tasks-0.20.1/src/task_pool.rs:566.
        let task = pool.spawn(async move {
            let terrain = sample_chunk(&maps, coord);
            let image = build_chunk_image(&maps, &terrain, mode);
            GeneratedChunk { image, mode }
        });
        cache.pending.insert(coord, PendingChunkTask { mode, task });
    }
}

fn poll_completed_async_chunks(
    keep_slots: &HashMap<ChunkCoord, StreamedChunkSlot>,
    chunk_world_size: Vec2,
    commands: &mut Commands,
    mut images: Option<&mut Assets<Image>>,
    cache: &mut PlanetStreamCache,
    chunk_transforms: &mut Query<(&mut Transform, &mut Sprite), With<StreamedChunk>>,
) {
    // Poll completed tasks closest-to-camera first, uploading at most
    // `MAX_CHUNK_UPLOADS_PER_FRAME` textures so the main thread never stutters.
    let mut pending_order: Vec<ChunkCoord> = cache.pending.keys().copied().collect();
    pending_order.sort_by_key(|coord| {
        keep_slots
            .get(coord)
            .map_or((i32::MAX, i32::MAX, i32::MAX), slot_sort_key)
    });

    let mut uploads_this_frame = 0_usize;
    for coord in pending_order {
        if uploads_this_frame >= MAX_CHUNK_UPLOADS_PER_FRAME {
            break;
        }
        let finished = cache.pending.get_mut(&coord).and_then(|pending| {
            // `block_on`: bevy_platform-0.20.1/src/future.rs:14.
            // `poll_once`: futures-lite-2.6.1/src/future.rs:116.
            // `Task: Unpin + Future`: async-task-4.7.1/src/task.rs:60,447.
            block_on(poll_once(&mut pending.task))
        });
        if let Some(generated) = finished {
            cache.pending.remove(&coord);
            if let Some(slot) = keep_slots.get(&coord) {
                apply_chunk_image(
                    slot,
                    generated,
                    chunk_world_size,
                    commands,
                    images.as_deref_mut(),
                    &mut cache.active,
                    chunk_transforms,
                );
                uploads_this_frame += 1;
            }
        }
    }
}

#[derive(Clone, Copy)]
struct ChunkRenderConfig {
    mode: PlanetRenderMode,
    world_size: Vec2,
}

fn sync_chunks_blocking(
    world: &PlanetWorld,
    stream_ring: &[StreamedChunkSlot],
    config: ChunkRenderConfig,
    commands: &mut Commands,
    mut images: Option<&mut Assets<Image>>,
    cache: &mut PlanetStreamCache,
    chunk_transforms: &mut Query<(&mut Transform, &mut Sprite), With<StreamedChunk>>,
) {
    cache.pending.clear();
    for slot in stream_ring {
        let up_to_date = cache
            .active
            .get(&slot.coord)
            .is_some_and(|entry| entry.rendered_mode == config.mode);
        if up_to_date {
            continue;
        }
        let terrain = sample_chunk(world.maps(), slot.coord);
        let generated = GeneratedChunk {
            image: build_chunk_image(world.maps(), &terrain, config.mode),
            mode: config.mode,
        };
        apply_chunk_image(
            slot,
            generated,
            config.world_size,
            commands,
            images.as_deref_mut(),
            &mut cache.active,
            chunk_transforms,
        );
    }
}

#[expect(
    clippy::needless_pass_by_value,
    clippy::too_many_arguments,
    reason = "Bevy system parameters are passed by value"
)]
fn sync_streamed_chunks(
    mut commands: Commands,
    world: Option<Res<PlanetWorld>>,
    focus: Res<PlanetCameraFocus>,
    mode: Res<PlanetRenderMode>,
    sync_mode: Res<PlanetStreamSyncMode>,
    mut images: Option<ResMut<Assets<Image>>>,
    mut cache: ResMut<PlanetStreamCache>,
    mut stats: ResMut<PlanetStreamStats>,
    mut chunk_transforms: Query<(&mut Transform, &mut Sprite), With<StreamedChunk>>,
    mut redraws: MessageWriter<RequestRedraw>,
) {
    let Some(world) = world else {
        return;
    };

    let (stream_ring, keep_slots) = streamed_ring_with_keep(&focus);

    // Despawn chunks outside the hysteresis keep-margin (`keep_radius = stream_radius + 1`)
    // and release their GPU textures.
    let stale: Vec<ChunkCoord> = cache
        .active
        .keys()
        .copied()
        .filter(|coord| !keep_slots.contains_key(coord))
        .collect();
    for coord in stale {
        if let Some(entry) = cache.active.remove(&coord) {
            commands.entity(entry.entity).despawn();
            if let Some(images) = images.as_deref_mut() {
                // `Assets::remove`: bevy_asset-0.20.1/src/assets.rs:470.
                images.remove(&entry.image);
            }
        }
    }

    // Cancel pending background tasks outside `keep_radius` or rendered for a stale mode.
    // Dropping a `Task` cancels it: async-task-4.7.1/src/task.rs:49,440.
    cache
        .pending
        .retain(|coord, pending| keep_slots.contains_key(coord) && pending.mode == *mode);

    let chunk_world_size = Vec2::splat(CHUNK_SIZE_F32 * focus.tile_pixel_size.max(0.1));

    // Keep all active chunks (both inside `stream_ring` and inside the hysteresis keep-margin)
    // locked to the floating camera origin.
    for (coord, entry) in &cache.active {
        if let Some(slot) = keep_slots.get(coord)
            && let Ok((mut transform, mut sprite)) = chunk_transforms.get_mut(entry.entity)
        {
            *transform = chunk_transform(slot);
            sprite.custom_size = Some(chunk_world_size);
        }
    }

    // `AsyncComputeTaskPool::try_get`: bevy_tasks-0.20.1/src/usages.rs:21.
    // Falls back to synchronous generation when no task pool is initialized (e.g. headless tests).
    let async_pool = match *sync_mode {
        PlanetStreamSyncMode::Async => AsyncComputeTaskPool::try_get(),
        PlanetStreamSyncMode::Blocking => None,
    };

    if let Some(pool) = async_pool {
        poll_completed_async_chunks(
            &keep_slots,
            chunk_world_size,
            &mut commands,
            images.as_deref_mut(),
            &mut cache,
            &mut chunk_transforms,
        );
        dispatch_async_chunks(pool, &world, &stream_ring, *mode, &mut cache);
    } else {
        sync_chunks_blocking(
            &world,
            &stream_ring,
            ChunkRenderConfig {
                mode: *mode,
                world_size: chunk_world_size,
            },
            &mut commands,
            images.as_deref_mut(),
            &mut cache,
            &mut chunk_transforms,
        );
    }

    let missing_in_ring = stream_ring
        .iter()
        .filter(|slot| {
            !cache
                .active
                .get(&slot.coord)
                .is_some_and(|entry| entry.rendered_mode == *mode)
        })
        .count();
    stats.active_chunks = cache.active.len();
    stats.in_flight_tasks = cache.pending.len();
    stats.pending_chunks = missing_in_ring.max(cache.pending.len());
    if stats.pending_chunks > 0 {
        redraws.write(RequestRedraw);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeta_core::world::{FACE_CHUNKS, FaceHeightmap, SEA_LEVEL_U16};

    fn vec_distance(lhs: [f64; 3], rhs: [f64; 3]) -> f64 {
        let dx = lhs[0] - rhs[0];
        let dy = lhs[1] - rhs[1];
        let dz = lhs[2] - rhs[2];
        (dx * dx + dy * dy + dz * dz).sqrt()
    }

    fn build_synthetic_planet(res: u32, elev_fn: impl Fn([f64; 3]) -> f64) -> PlanetHeightmaps {
        let mut faces = Vec::with_capacity(6);
        let res_f64 = f64::from(res);
        for face in CubeFace::ALL {
            let mut samples = Vec::with_capacity(usize::try_from(res * res).expect("fits"));
            for pixel_y in 0..res {
                let face_v = (f64::from(pixel_y) + 0.5) / res_f64 * 2.0 - 1.0;
                for pixel_x in 0..res {
                    let face_u = (f64::from(pixel_x) + 0.5) / res_f64 * 2.0 - 1.0;
                    let unit = face_uv_to_unit_vec(face, face_u, face_v);
                    let ft = elev_fn(unit);
                    let encoded = (ft + f64::from(SEA_LEVEL_U16)).round().clamp(0.0, 65_535.0);
                    #[expect(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "encoded is rounded and clamped into [0, 65535]"
                    )]
                    samples.push(encoded as u16);
                }
            }
            faces.push(FaceHeightmap::new(res, samples).expect("valid synthetic face"));
        }
        let array: [FaceHeightmap; 6] = faces.try_into().expect("6 faces");
        PlanetHeightmaps::new(array).expect("valid synthetic planet")
    }

    /// Maps a screen-oriented `(view_col, view_row)` pixel inside a rendered chunk `[0, CHUNK_SIZE)^2`
    /// back into the chunk's native `(local_x, local_y)` tile coordinate using `view_to_chunk_rot`.
    fn view_pixel_to_chunk_tile(
        slot: &StreamedChunkSlot,
        view_col: u32,
        view_row: u32,
    ) -> (u32, u32) {
        let (lx, ly) = slot.view_to_chunk_rot.rotate_grid_coord(
            i128::from(view_col),
            i128::from(view_row),
            i128::from(CHUNK_SIZE),
        );
        (
            u32::try_from(lx).expect("rotated pixel in bounds"),
            u32::try_from(ly).expect("rotated pixel in bounds"),
        )
    }

    #[test]
    fn recenter_focus_steps_interior_chunks_accurately() {
        let mid_chunk = FACE_CHUNKS / 2;
        let mut interior = PlanetCameraFocus {
            anchor: ChunkCoord {
                face: CubeFace::PosZ,
                cx: mid_chunk,
                cy: mid_chunk,
            },
            local_offset_tiles: Vec2::new(150.25, -70.75),
            view_rotation: FaceRotation::Deg0,
            stream_radius_chunks: 2,
            tile_pixel_size: 3.0,
        };
        let before_interior = focus_unit_vec(&interior);
        recenter_focus(&mut interior);
        assert_eq!(interior.anchor.face, CubeFace::PosZ);
        assert_eq!(interior.anchor.cx, mid_chunk + 2);
        assert_eq!(interior.anchor.cy, mid_chunk - 2);
        assert!((interior.local_offset_tiles.x - 22.25).abs() < 1e-4);
        assert!((interior.local_offset_tiles.y - 57.25).abs() < 1e-4);
        assert!(vec_distance(before_interior, focus_unit_vec(&interior)) < 1e-6);
    }

    #[test]
    fn recenter_focus_preserves_position_across_all_face_seams() {
        let last_chunk = FACE_CHUNKS - 1;
        for face in CubeFace::ALL {
            let edge_cases = [
                (
                    ChunkCoord {
                        face,
                        cx: last_chunk,
                        cy: 12_345,
                    },
                    Vec2::new(63.25, 19.75),
                    Vec2::new(4.5, 0.0),
                ),
                (
                    ChunkCoord {
                        face,
                        cx: 0,
                        cy: 12_345,
                    },
                    Vec2::new(1.25, 19.75),
                    Vec2::new(-4.5, 0.0),
                ),
                (
                    ChunkCoord {
                        face,
                        cx: 12_345,
                        cy: last_chunk,
                    },
                    Vec2::new(19.75, 62.5),
                    Vec2::new(0.0, 4.5),
                ),
                (
                    ChunkCoord {
                        face,
                        cx: 12_345,
                        cy: 0,
                    },
                    Vec2::new(19.75, 1.5),
                    Vec2::new(0.0, -4.5),
                ),
            ];

            for (start_anchor, start_offset, pan_delta) in edge_cases {
                let mut before = PlanetCameraFocus {
                    anchor: start_anchor,
                    local_offset_tiles: start_offset,
                    view_rotation: FaceRotation::Deg0,
                    stream_radius_chunks: 1,
                    tile_pixel_size: 2.0,
                };
                before.local_offset_tiles += pan_delta;
                let expected_unit = focus_unit_vec(&before);
                let ring_before = streamed_ring(&before);

                let mut after = before;
                recenter_focus(&mut after);

                assert_ne!(after.anchor.face, start_anchor.face, "must cross seam");
                assert!(
                    after.local_offset_tiles.x >= 0.0
                        && after.local_offset_tiles.x < CHUNK_SIZE_F32
                        && after.local_offset_tiles.y >= 0.0
                        && after.local_offset_tiles.y < CHUNK_SIZE_F32,
                    "local offset out of [0, 64): {:?}",
                    after.local_offset_tiles
                );

                let dist = vec_distance(expected_unit, focus_unit_vec(&after));
                assert!(
                    dist < 1e-5,
                    "seam crossing drifted planetary position on {face}: dist={dist}"
                );

                let ring_after = streamed_ring(&after);
                let shared_count = ring_before
                    .iter()
                    .filter(|slot_b| {
                        if let Some(slot_a) = ring_after.iter().find(|s| s.coord == slot_b.coord) {
                            let pos_diff = (slot_a.world_center - slot_b.world_center).length();
                            assert!(
                                pos_diff < 1e-3,
                                "visual jump across {face} seam for {:?}: {pos_diff} px",
                                slot_b.coord
                            );
                            assert_eq!(
                                slot_a.chunk_to_view_rot, slot_b.chunk_to_view_rot,
                                "rotation jump across {face} seam"
                            );
                            true
                        } else {
                            false
                        }
                    })
                    .count();
                assert!(shared_count >= 4, "expected overlapping chunks across seam");
            }
        }
    }

    #[test]
    fn streamed_ring_aligns_edges_across_all_twelve_synthetic_seams() {
        let synthetic = build_synthetic_planet(64, |[x, y, z]| {
            2400.0 * x + 1800.0 * y * z - 1200.0 * (x * x - z * z) + 600.0 * y
        });
        let last_chunk = FACE_CHUNKS - 1;

        for face in CubeFace::ALL {
            let seam_anchors = [
                (
                    ChunkCoord {
                        face,
                        cx: last_chunk,
                        cy: FACE_CHUNKS / 3,
                    },
                    IVec2::new(1, 0),
                ),
                (
                    ChunkCoord {
                        face,
                        cx: 0,
                        cy: FACE_CHUNKS / 3,
                    },
                    IVec2::new(-1, 0),
                ),
                (
                    ChunkCoord {
                        face,
                        cx: FACE_CHUNKS / 3,
                        cy: last_chunk,
                    },
                    IVec2::new(0, 1),
                ),
                (
                    ChunkCoord {
                        face,
                        cx: FACE_CHUNKS / 3,
                        cy: 0,
                    },
                    IVec2::new(0, -1),
                ),
            ];

            for (anchor, neighbor_view_offset) in seam_anchors {
                let focus = PlanetCameraFocus {
                    anchor,
                    local_offset_tiles: Vec2::new(32.0, 32.0),
                    view_rotation: FaceRotation::Deg0,
                    stream_radius_chunks: 1,
                    tile_pixel_size: 2.0,
                };
                let ring = streamed_ring(&focus);
                assert_eq!(ring.len(), 9, "3x3 ring must contain 9 chunks");

                let center_slot = ring
                    .iter()
                    .find(|s| s.view_offset == IVec2::ZERO)
                    .expect("center slot");
                let neighbor_slot = ring
                    .iter()
                    .find(|s| s.view_offset == neighbor_view_offset)
                    .expect("neighbor slot across seam");
                assert_ne!(center_slot.coord.face, neighbor_slot.coord.face);

                let center_terrain = sample_chunk(&synthetic, center_slot.coord);
                let neighbor_terrain = sample_chunk(&synthetic, neighbor_slot.coord);

                for idx in 0..CHUNK_SIZE {
                    let (center_col, center_row, neighbor_col, neighbor_row) =
                        match (neighbor_view_offset.x, neighbor_view_offset.y) {
                            (1, 0) => (CHUNK_SIZE - 1, idx, 0, idx),
                            (-1, 0) => (0, idx, CHUNK_SIZE - 1, idx),
                            (0, 1) => (idx, CHUNK_SIZE - 1, idx, 0),
                            (0, -1) => (idx, 0, idx, CHUNK_SIZE - 1),
                            _ => unreachable!(),
                        };
                    let (clx, cly) = view_pixel_to_chunk_tile(center_slot, center_col, center_row);
                    let (nlx, nly) =
                        view_pixel_to_chunk_tile(neighbor_slot, neighbor_col, neighbor_row);

                    let sample_a = center_terrain.tile(clx, cly).expect("valid center edge");
                    let sample_b = neighbor_terrain
                        .tile(nlx, nly)
                        .expect("valid neighbor edge");
                    let diff_ft = (sample_a.elevation_ft - sample_b.elevation_ft).abs();
                    assert!(
                        diff_ft <= 2,
                        "rendered seam mismatch on {face} -> {} at edge pixel {idx}: {sample_a:?} vs {sample_b:?} (rot={:?})",
                        neighbor_slot.coord.face,
                        neighbor_slot.chunk_to_view_rot
                    );
                }
            }
        }
    }

    #[test]
    fn streamed_ring_aligns_real_sera_rotated_seam() {
        let sera = PlanetWorld::load_default().expect("workspace Sera face PNGs load");
        let seam_focus = PlanetCameraFocus {
            stream_radius_chunks: 2,
            ..PlanetCameraFocus::sera_seam()
        };
        let ring = streamed_ring(&seam_focus);
        assert_eq!(ring.len(), 25, "5x5 ring must contain 25 chunks");

        let center_slot = ring
            .iter()
            .find(|s| s.view_offset == IVec2::ZERO)
            .expect("center slot");
        let north_slot = ring
            .iter()
            .find(|s| s.view_offset == IVec2::new(0, -1))
            .expect("north slot across NegX->PosY seam");
        assert_eq!(center_slot.coord.face, CubeFace::NegX);
        assert_eq!(north_slot.coord.face, CubeFace::PosY);
        assert_eq!(north_slot.view_to_chunk_rot, FaceRotation::Deg90);

        let south_terrain = sample_chunk(sera.maps(), center_slot.coord);
        let north_terrain = sample_chunk(sera.maps(), north_slot.coord);
        for idx in 0..CHUNK_SIZE {
            let (slx, sly) = view_pixel_to_chunk_tile(center_slot, idx, 0);
            let (nlx, nly) = view_pixel_to_chunk_tile(north_slot, idx, CHUNK_SIZE - 1);
            let south_sample = south_terrain.tile(slx, sly).expect("south edge");
            let north_sample = north_terrain.tile(nlx, nly).expect("north edge");
            assert!(
                (south_sample.elevation_ft - north_sample.elevation_ft).abs() <= 4,
                "Sera NegX<->PosY seam jump at col {idx}: {south_sample:?} vs {north_sample:?}"
            );
        }
    }

    #[test]
    fn planet_regions_rejects_overlap_accepts_shared_edge_and_queries_chunk_intersections() {
        let west = RegionFootprint {
            id: "west_harbor".to_owned(),
            face: CubeFace::PosZ,
            polygon: vec![[0, 0], [128, 0], [128, 128], [0, 128]],
        };
        let east_adjacent = RegionFootprint {
            id: "east_market".to_owned(),
            face: CubeFace::PosZ,
            // Shares x = 128 boundary edge with west_harbor: strictly valid.
            polygon: vec![[128, 0], [256, 0], [256, 128], [128, 128]],
        };
        let regions = PlanetRegions::new(vec![west.clone(), east_adjacent.clone()])
            .expect("adjacent shared-edge regions must be accepted");
        assert_eq!(regions.regions().len(), 2);

        // Chunk (0, 0) spans [0..64, 0..64] -> intersects west_harbor only.
        let chunk_00 = regions.regions_intersecting_chunk(ChunkCoord {
            face: CubeFace::PosZ,
            cx: 0,
            cy: 0,
        });
        assert_eq!(chunk_00.len(), 1);
        assert_eq!(chunk_00[0].id, "west_harbor");

        // Chunk (2, 0) spans [128..192, 0..64] -> shares edge x=128 with west_harbor, inside east_market.
        let chunk_20 = regions.regions_intersecting_chunk(ChunkCoord {
            face: CubeFace::PosZ,
            cx: 2,
            cy: 0,
        });
        assert_eq!(chunk_20.len(), 1);
        assert_eq!(chunk_20[0].id, "east_market");

        // Chunk on a different face must return empty.
        assert_eq!(
            regions.regions_intersecting_chunk(ChunkCoord {
                face: CubeFace::PosX,
                cx: 0,
                cy: 0,
            }),
            Vec::<&RegionFootprint>::new()
        );

        // Overlapping polygon must be rejected with Err.
        let intruder = RegionFootprint {
            id: "overlapping_keep".to_owned(),
            face: CubeFace::PosZ,
            polygon: vec![[100, 20], [160, 20], [160, 80], [100, 80]],
        };
        let overlaps = PlanetRegions::new(vec![west, east_adjacent, intruder])
            .expect_err("overlapping regions must be rejected");
        assert_eq!(overlaps.len(), 2);
    }

    #[test]
    fn planet_world_maps_arc_shares_allocation() {
        let synthetic = build_synthetic_planet(8, |[x, _y, _z]| 100.0 * x);
        let world = PlanetWorld::new(synthetic);
        let arc_a = world.maps_arc();
        let arc_b = world.maps_arc();
        assert!(Arc::ptr_eq(&arc_a, &arc_b));
    }

    fn build_test_stream_app(sync_mode: PlanetStreamSyncMode, cx: u32, cy: u32) -> App {
        let synthetic = build_synthetic_planet(16, |[x, y, _z]| 500.0 * x + 300.0 * y);
        let mut app = App::new();
        app.insert_resource(PlanetWorld::new(synthetic))
            .insert_resource(sync_mode)
            .insert_resource(PlanetCameraFocus {
                anchor: ChunkCoord {
                    face: CubeFace::PosX,
                    cx,
                    cy,
                },
                local_offset_tiles: Vec2::new(32.0, 32.0),
                view_rotation: FaceRotation::Deg0,
                stream_radius_chunks: 1,
                tile_pixel_size: 2.0,
            })
            .insert_resource(Assets::<Image>::default())
            .add_plugins(PlanetStreamPlugin);
        app
    }

    #[test]
    fn planet_stream_blocking_spawns_at_chunk_z_depth_and_toggles_mode() {
        let mut app = build_test_stream_app(PlanetStreamSyncMode::Blocking, 100, 100);
        app.update();

        let initial: Vec<(ChunkCoord, f32)> = app
            .world_mut()
            .query::<(&StreamedChunk, &Transform)>()
            .iter(app.world())
            .map(|(c, t)| (c.coord, t.translation.z))
            .collect();
        assert_eq!(initial.len(), 9, "R=1 spawns 3x3=9 chunks");
        for (_, z) in &initial {
            assert!(
                (*z - CHUNK_Z_DEPTH).abs() < 1e-6,
                "chunk z-depth must be {CHUNK_Z_DEPTH}, got {z}"
            );
        }

        // Toggling render mode updates textures without duplicating entities.
        *app.world_mut().resource_mut::<PlanetRenderMode>() = PlanetRenderMode::LandOceanMask;
        app.update();
        let after_toggle = app
            .world_mut()
            .query::<&StreamedChunk>()
            .iter(app.world())
            .count();
        assert_eq!(after_toggle, 9);
        let stats = *app.world().resource::<PlanetStreamStats>();
        assert_eq!(stats.pending_chunks, 0);
        assert_eq!(stats.in_flight_tasks, 0);
    }

    #[test]
    fn planet_stream_hysteresis_keep_margin_retains_one_ring_and_despawns_two_rings() {
        let mut app = build_test_stream_app(PlanetStreamSyncMode::Blocking, 100, 100);
        app.update();

        let west_edge_coord = ChunkCoord {
            face: CubeFace::PosX,
            cx: 99,
            cy: 100,
        };
        let west_edge_entity = app
            .world_mut()
            .query::<(Entity, &StreamedChunk)>()
            .iter(app.world())
            .find(|(_, c)| c.coord == west_edge_coord)
            .map(|(e, _)| e)
            .expect("west edge chunk spawned");

        // Pan 1 chunk east: `cx = 99` is 1 ring outside `stream_radius` (within `keep_radius`),
        // so it stays cached while `cx = 102` spawns.
        {
            let mut focus = app.world_mut().resource_mut::<PlanetCameraFocus>();
            pan_focus_view(&mut focus, Vec2::new(CHUNK_SIZE_F32, 0.0));
        }
        app.update();

        let after_one_step: Vec<(Entity, ChunkCoord)> = app
            .world_mut()
            .query::<(Entity, &StreamedChunk)>()
            .iter(app.world())
            .map(|(e, c)| (e, c.coord))
            .collect();
        assert_eq!(after_one_step.len(), 12);
        assert!(
            after_one_step.contains(&(west_edge_entity, west_edge_coord)),
            "chunk 1 ring outside stream_radius must retain its entity without thrashing"
        );
        let east_entered = ChunkCoord {
            face: CubeFace::PosX,
            cx: 102,
            cy: 100,
        };
        assert!(
            after_one_step
                .iter()
                .any(|(_, coord)| *coord == east_entered),
            "newly entered east column must spawn"
        );

        // Oscillating back 1 chunk west reuses the exact same cached entity.
        {
            let mut focus = app.world_mut().resource_mut::<PlanetCameraFocus>();
            pan_focus_view(&mut focus, Vec2::new(-CHUNK_SIZE_F32, 0.0));
        }
        app.update();
        let reused = app
            .world_mut()
            .query::<(Entity, &StreamedChunk)>()
            .iter(app.world())
            .any(|(e, c)| e == west_edge_entity && c.coord == west_edge_coord);
        assert!(reused, "oscillating back must reuse cached entity");

        // Pan 2 chunks east from anchor 100 -> 102: `cx = 99` is now 2 rings outside `stream_radius`
        // (outside `keep_radius = 2`) and must despawn.
        {
            let mut focus = app.world_mut().resource_mut::<PlanetCameraFocus>();
            pan_focus_view(&mut focus, Vec2::new(CHUNK_SIZE_F32 * 2.0, 0.0));
        }
        app.update();
        let after_two_steps: Vec<ChunkCoord> = app
            .world_mut()
            .query::<&StreamedChunk>()
            .iter(app.world())
            .map(|c| c.coord)
            .collect();
        assert!(
            !after_two_steps.contains(&west_edge_coord),
            "chunk 2 rings outside stream_radius must despawn"
        );
        assert!(after_two_steps.contains(&ChunkCoord {
            face: CubeFace::PosX,
            cx: 100,
            cy: 100,
        }));
        assert!(after_two_steps.contains(&ChunkCoord {
            face: CubeFace::PosX,
            cx: 103,
            cy: 100,
        }));
    }

    #[test]
    fn planet_stream_async_mode_respects_budget_completes_and_cancels_stale_tasks() {
        use std::time::Duration;
        // `AsyncComputeTaskPool::get_or_init`: bevy_tasks-0.20.1/src/usages.rs:15.
        // `TaskPool::default`: bevy_tasks-0.20.1/src/task_pool.rs:610.
        AsyncComputeTaskPool::get_or_init(bevy::tasks::TaskPool::default);

        let synthetic = build_synthetic_planet(16, |[x, y, _z]| 400.0 * x - 250.0 * y);
        let mut app = App::new();
        app.insert_resource(PlanetWorld::new(synthetic))
            .insert_resource(PlanetStreamSyncMode::Async)
            .insert_resource(PlanetCameraFocus {
                anchor: ChunkCoord {
                    face: CubeFace::PosX,
                    cx: 200,
                    cy: 200,
                },
                local_offset_tiles: Vec2::new(32.0, 32.0),
                view_rotation: FaceRotation::Deg0,
                stream_radius_chunks: 1,
                tile_pixel_size: 2.0,
            })
            .insert_resource(Assets::<Image>::default())
            .add_plugins(PlanetStreamPlugin);

        let mut prev_active = 0_usize;
        let mut settled = false;
        for _ in 0..200 {
            app.update();
            let stats = *app.world().resource::<PlanetStreamStats>();
            assert!(
                stats.in_flight_tasks <= MAX_IN_FLIGHT_CHUNK_TASKS,
                "in-flight tasks must stay within cap"
            );
            let delta = stats.active_chunks.saturating_sub(prev_active);
            assert!(
                delta <= MAX_CHUNK_UPLOADS_PER_FRAME,
                "uploaded {delta} chunks in 1 frame, exceeding budget {MAX_CHUNK_UPLOADS_PER_FRAME}"
            );
            prev_active = stats.active_chunks;
            if stats.active_chunks == 9 && stats.pending_chunks == 0 && stats.in_flight_tasks == 0 {
                settled = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(settled, "async streaming must settle all 9 chunks");

        // Jump far outside keep_radius (`+10` chunks): all old chunks despawn immediately and
        // new async tasks stream the new ring to completion.
        {
            let mut focus = app.world_mut().resource_mut::<PlanetCameraFocus>();
            pan_focus_view(&mut focus, Vec2::new(CHUNK_SIZE_F32 * 10.0, 0.0));
        }
        app.update();
        let after_jump_entities = app
            .world_mut()
            .query::<&StreamedChunk>()
            .iter(app.world())
            .count();
        assert_eq!(
            after_jump_entities, 0,
            "all old chunks outside keep_radius must despawn on jump before new async uploads finish"
        );
        let jump_stats = *app.world().resource::<PlanetStreamStats>();
        assert!(
            jump_stats.in_flight_tasks > 0 && jump_stats.in_flight_tasks <= 9,
            "new ring tasks must be dispatched asynchronously after jump"
        );

        // Immediately jump again before those tasks finish: stale pending tasks must cancel cleanly.
        {
            let mut focus = app.world_mut().resource_mut::<PlanetCameraFocus>();
            pan_focus_view(&mut focus, Vec2::new(0.0, CHUNK_SIZE_F32 * 10.0));
        }
        let mut final_settled = false;
        for _ in 0..200 {
            app.update();
            let stats = *app.world().resource::<PlanetStreamStats>();
            if stats.active_chunks == 9 && stats.pending_chunks == 0 && stats.in_flight_tasks == 0 {
                final_settled = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            final_settled,
            "async streaming must settle cleanly after cancelling stale tasks"
        );
        let final_coords: Vec<ChunkCoord> = app
            .world_mut()
            .query::<&StreamedChunk>()
            .iter(app.world())
            .map(|c| c.coord)
            .collect();
        assert!(
            final_coords.contains(&ChunkCoord {
                face: CubeFace::PosX,
                cx: 210,
                cy: 210,
            }),
            "final anchor chunk must be active"
        );
    }
}
