//! Per-chunk ground facts produced by the terrain splat bake.
//!
//! Every streamed planet chunk entity drawn by the splat path carries a [`ChunkGroundCover`].
//! It is the only input clutter placement reads, so its shape is a contract
//! (`terrain_contract.md` Section 4): change it only through the coordinator.

use bevy::prelude::*;
use zeta_core::world::{CHUNK_SIZE, ChunkCoord};

/// Tiles per chunk side as a `usize` index bound.
pub const CHUNK_TILES: usize = CHUNK_SIZE as usize;
/// Tiles per chunk.
pub const CHUNK_TILE_COUNT: usize = CHUNK_TILES * CHUNK_TILES;

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
        let (x, y) = (usize::try_from(local_x).ok()?, usize::try_from(local_y).ok()?);
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
