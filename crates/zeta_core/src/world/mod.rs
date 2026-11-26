//! Continuous cube-sphere planetary geography (Layer 0 / Layer 1) and authored region
//! footprint validation.
//!
//! Rules are defined in `docs/architecture/world_geography.md`. All types and functions here
//! are pure Rust with zero input or output.

pub mod cube_sphere;
pub mod region;
pub mod sampler;

pub use cube_sphere::{
    CHUNK_SIZE, ChunkCoord, CubeFace, EdgeTransition, FACE_CHUNKS, FACE_TILES, FaceBasis, FaceEdge,
    FaceRotation, PlanetTile, face_uv_to_unit_vec, step_chunk, step_tile, tile_to_unit_vec,
    unit_vec_to_face_uv, unit_vec_to_tile,
};
pub use region::{
    RegionFootprint, RegionOverlap, point_in_polygon, polygons_overlap, validate_non_overlapping,
};
pub use sampler::{
    ChunkTerrain, ELEVATION_STEP_FT, FaceHeightmap, HeightmapError, PlanetHeightmaps,
    SEA_LEVEL_U16, TileSample, sample_chunk, sample_fractal_detail_ft, sample_macro_elevation_ft,
    sample_tile,
};
