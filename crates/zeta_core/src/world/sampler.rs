//! Layer 0 planetary heightmap representation and Layer 1 deterministic wilderness
//! terrain sampling across cube-sphere boundaries.
//!
//! Rules are defined in `docs/architecture/world_geography.md` Sections 2 and 3.
//! Performs no input or output: callers supply the 6 face heightmaps in memory.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::cube_sphere::{
    CHUNK_SIZE, ChunkCoord, CubeFace, FACE_TILES, PlanetTile, step_grid, tile_to_unit_vec,
};

/// Encoded `u16` sample value representing exact sea level (`0` feet).
///
/// Values below `SEA_LEVEL_U16` are ocean depth; values at or above are land elevation.
pub const SEA_LEVEL_U16: u16 = 32_768;

/// Tactical elevation quantization step in feet.
pub const ELEVATION_STEP_FT: i32 = 5;

/// Maximum amplitude (in feet) of deterministic Layer 1 fractal detail added inland or offshore.
const MAX_FRACTAL_AMPLITUDE_FT: f64 = 35.0;

/// Fraction of `|macro_ft|` available for fractal relief near shorelines so macro land never
/// flips to water and macro water never flips to land.
const COASTLINE_CLAMP_RATIO: f64 = 0.25;

/// Failure constructing a [`FaceHeightmap`] or [`PlanetHeightmaps`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum HeightmapError {
    /// Face resolution was zero.
    #[error("heightmap resolution must be greater than 0")]
    ZeroResolution,
    /// Sample buffer length did not match `res * res`.
    #[error(
        "face heightmap for {face:?} expected {expected} samples for resolution {res}, got {actual}"
    )]
    SampleCountMismatch {
        /// Face whose buffer length failed validation, if known.
        face: Option<CubeFace>,
        /// Declared square resolution.
        res: u32,
        /// Expected number of samples (`res * res`).
        expected: usize,
        /// Actual sample vector length.
        actual: usize,
    },
    /// The 6 faces did not share 1 uniform resolution.
    #[error("face {face} has resolution {actual}, expected uniform planet resolution {expected}")]
    MismatchedFaceResolution {
        /// Face whose resolution differed from `PosX`.
        face: CubeFace,
        /// Resolution of `PosX`.
        expected: u32,
        /// Resolution of `face`.
        actual: u32,
    },
}

/// Square 16-bit elevation heightmap for 1 cube face (row-major, `y * res + x`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceHeightmap {
    /// Width and height of the heightmap in samples (for Layer 0 Sera faces, `1024`).
    pub res: u32,
    /// Row-major `u16` elevation samples where `SEA_LEVEL_U16` (`32768`) is `0` feet.
    pub samples: Vec<u16>,
}

impl FaceHeightmap {
    /// Creates a validated [`FaceHeightmap`].
    ///
    /// # Errors
    /// Returns [`HeightmapError::ZeroResolution`] if `res == 0`, or
    /// [`HeightmapError::SampleCountMismatch`] if `samples.len() != (res as usize) * (res as usize)`.
    pub fn new(res: u32, samples: Vec<u16>) -> Result<Self, HeightmapError> {
        if res == 0 {
            return Err(HeightmapError::ZeroResolution);
        }
        let side = usize::try_from(res).unwrap_or(usize::MAX);
        let expected = side.saturating_mul(side);
        if samples.len() != expected {
            return Err(HeightmapError::SampleCountMismatch {
                face: None,
                res,
                expected,
                actual: samples.len(),
            });
        }
        Ok(Self { res, samples })
    }

    /// Reads sample `(x, y)` or falls back to `SEA_LEVEL_U16` if out of range.
    pub fn get(&self, x: u32, y: u32) -> u16 {
        let Some(row) = usize::try_from(y).ok() else {
            return SEA_LEVEL_U16;
        };
        let Some(col) = usize::try_from(x).ok() else {
            return SEA_LEVEL_U16;
        };
        let Some(stride) = usize::try_from(self.res).ok() else {
            return SEA_LEVEL_U16;
        };
        if row >= stride || col >= stride {
            return SEA_LEVEL_U16;
        }
        row.checked_mul(stride)
            .and_then(|base| base.checked_add(col))
            .and_then(|idx| self.samples.get(idx).copied())
            .unwrap_or(SEA_LEVEL_U16)
    }
}

/// Complete 6-face Layer 0 elevation dataset indexed by [`CubeFace`].
///
/// Callers load raw PNG data outside `zeta_core` and pass the decoded faces in here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanetHeightmaps {
    faces: [FaceHeightmap; 6],
    res: u32,
}

impl PlanetHeightmaps {
    /// Constructs a validated 6-face planet heightmap ordered by [`CubeFace::ALL`].
    ///
    /// # Errors
    /// Returns an error if any face has resolution `0`, a mismatched sample count, or a
    /// resolution different from the other faces.
    pub fn new(faces: [FaceHeightmap; 6]) -> Result<Self, HeightmapError> {
        let expected_res = faces[0].res;
        if expected_res == 0 {
            return Err(HeightmapError::ZeroResolution);
        }
        for (face, face_map) in CubeFace::ALL.into_iter().zip(faces.iter()) {
            if face_map.res == 0 {
                return Err(HeightmapError::ZeroResolution);
            }
            if face_map.res != expected_res {
                return Err(HeightmapError::MismatchedFaceResolution {
                    face,
                    expected: expected_res,
                    actual: face_map.res,
                });
            }
            let side = usize::try_from(face_map.res).unwrap_or(usize::MAX);
            let expected_len = side.saturating_mul(side);
            if face_map.samples.len() != expected_len {
                return Err(HeightmapError::SampleCountMismatch {
                    face: Some(face),
                    res: face_map.res,
                    expected: expected_len,
                    actual: face_map.samples.len(),
                });
            }
        }
        Ok(Self {
            faces,
            res: expected_res,
        })
    }

    /// Returns the heightmap for `face`.
    pub fn face(&self, face: CubeFace) -> &FaceHeightmap {
        &self.faces[face.index()]
    }

    /// Returns the uniform sample resolution per face edge.
    pub const fn res(&self) -> u32 {
        self.res
    }

    /// Reads an arbitrary signed pixel coordinate on `face`, crossing onto neighboring faces
    /// via the exact cube-sphere adjacency table when `px` or `py` falls outside `[0, res)`.
    fn sample_pixel_cross_face(&self, face: CubeFace, pixel_x: i64, pixel_y: i64) -> f64 {
        let (target_face, sample_x, sample_y, _rot) =
            step_grid(face, self.res, i128::from(pixel_x), i128::from(pixel_y));
        let raw = self.face(target_face).get(sample_x, sample_y);
        f64::from(raw) - f64::from(SEA_LEVEL_U16)
    }
}

/// Deterministic Layer 0 + Layer 1 terrain sample for a single 5-foot square.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TileSample {
    /// Continuous-to-integer elevation in feet relative to sea level (`< 0` is ocean).
    pub elevation_ft: i32,
    /// Elevation quantized to [`ELEVATION_STEP_FT`] (5-foot) tactical steps.
    pub elevation_step_ft: i32,
    /// Whether this square is underwater (`elevation_ft < 0`).
    pub is_water: bool,
}

/// Deterministic `64 × 64` chunk of sampled wilderness terrain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkTerrain {
    /// Coordinate of this chunk on the cube-sphere.
    pub coord: ChunkCoord,
    /// Row-major (`local_y * CHUNK_SIZE + local_x`) terrain samples (`4096` tiles).
    pub tiles: Vec<TileSample>,
}

impl ChunkTerrain {
    /// Looks up the sample at `(local_x, local_y)` inside the chunk (`0..CHUNK_SIZE`).
    pub fn tile(&self, local_x: u32, local_y: u32) -> Option<TileSample> {
        if local_x >= CHUNK_SIZE || local_y >= CHUNK_SIZE {
            return None;
        }
        let index = usize::try_from(local_y * CHUNK_SIZE + local_x).ok()?;
        self.tiles.get(index).copied()
    }
}

/// Bicubic-interpolates the Layer 0 macro elevation (in feet relative to sea level) at `tile`,
/// seamlessly sampling across neighboring cube faces at edges.
pub fn sample_macro_elevation_ft(maps: &PlanetHeightmaps, tile: PlanetTile) -> f64 {
    let normalized = normalize_tile(tile);
    let res_f64 = f64::from(maps.res());
    let tiles_f64 = f64::from(FACE_TILES);

    // Pixel centers in the heightmap lie at i + 0.5, matching `tools/build_planet_faces.py`.
    let float_x = (f64::from(normalized.x) + 0.5) * res_f64 / tiles_f64 - 0.5;
    let float_y = (f64::from(normalized.y) + 0.5) * res_f64 / tiles_f64 - 0.5;

    let floor_col = float_x.floor();
    let floor_row = float_y.floor();
    let frac_x = float_x - floor_col;
    let frac_y = float_y - floor_row;

    #[allow(
        clippy::cast_possible_truncation,
        reason = "floor_col and floor_row are bounded by [-1, res] and fit in i64"
    )]
    let base_x = floor_col as i64;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "floor_col and floor_row are bounded by [-1, res] and fit in i64"
    )]
    let base_y = floor_row as i64;

    // Near the 8 cube corners (where 3 faces meet instead of 4), a 4x4 stencil reaches
    // 2 pixels past 2 orthogonal edges simultaneously. Smoothly blending to cross-face
    // bilinear within [1.5, 2.5] pixels of a corner eliminates corner-stencil asymmetry
    // while keeping pure Catmull-Rom bicubic everywhere else on the planet.
    let dist_x = (float_x + 0.5).min((res_f64 - 0.5) - float_x).max(0.0);
    let dist_y = (float_y + 0.5).min((res_f64 - 0.5) - float_y).max(0.0);
    let corner_blend = corner_edge_weight(dist_x) * corner_edge_weight(dist_y);

    if corner_blend >= 1.0 {
        return sample_bilinear_cross_face(maps, normalized.face, base_x, base_y, frac_x, frac_y);
    }

    let bicubic = sample_bicubic_cross_face(maps, normalized.face, base_x, base_y, frac_x, frac_y);
    if corner_blend <= 0.0 {
        bicubic
    } else {
        let bilinear =
            sample_bilinear_cross_face(maps, normalized.face, base_x, base_y, frac_x, frac_y);
        lerp(bicubic, bilinear, corner_blend)
    }
}

fn corner_edge_weight(edge_distance_pixels: f64) -> f64 {
    1.0 - quintic_fade((edge_distance_pixels - 1.5).clamp(0.0, 1.0))
}

fn sample_bilinear_cross_face(
    maps: &PlanetHeightmaps,
    face: CubeFace,
    base_x: i64,
    base_y: i64,
    frac_x: f64,
    frac_y: f64,
) -> f64 {
    let res = i64::from(maps.res());
    let coords = [
        (base_x, base_y),
        (base_x + 1, base_y),
        (base_x, base_y + 1),
        (base_x + 1, base_y + 1),
    ];

    // At a cube corner, 3 faces meet instead of 4, so 1 of the 4 bilinear taps falls outside
    // both face edges simultaneously. Filling that virtual corner with the mean of the 3 real
    // corner pixels makes the bilinear surface identical across all 3 edges and the vertex.
    let mut values = [0.0; 4];
    let mut virtual_corner: Option<usize> = None;
    let mut real_sum = 0.0;
    for (idx, &(px, py)) in coords.iter().enumerate() {
        let out_x = px < 0 || px >= res;
        let out_y = py < 0 || py >= res;
        if out_x && out_y {
            virtual_corner = Some(idx);
        } else {
            let val = maps.sample_pixel_cross_face(face, px, py);
            values[idx] = val;
            real_sum += val;
        }
    }
    if let Some(corner_idx) = virtual_corner {
        values[corner_idx] = real_sum / 3.0;
    }

    let top = lerp(values[0], values[1], frac_x);
    let bottom = lerp(values[2], values[3], frac_x);
    lerp(top, bottom, frac_y)
}

fn sample_bicubic_cross_face(
    maps: &PlanetHeightmaps,
    face: CubeFace,
    base_x: i64,
    base_y: i64,
    frac_x: f64,
    frac_y: f64,
) -> f64 {
    let weights_x = catmull_rom_weights(frac_x);
    let weights_y = catmull_rom_weights(frac_y);
    let tap_offsets: [i64; 4] = [-1, 0, 1, 2];

    let mut sum = 0.0;
    for (&offset_y, &weight_y) in tap_offsets.iter().zip(weights_y.iter()) {
        let mut row_sum = 0.0;
        for (&offset_x, &weight_x) in tap_offsets.iter().zip(weights_x.iter()) {
            let sample = maps.sample_pixel_cross_face(face, base_x + offset_x, base_y + offset_y);
            row_sum += sample * weight_x;
        }
        sum += row_sum * weight_y;
    }
    sum
}

/// Computes deterministic 3D sphere-space fractal relief (in feet) clamped by `macro_ft`
/// so macro coastlines never drift across sea level.
pub fn sample_fractal_detail_ft(unit_vec: [f64; 3], macro_ft: f64) -> f64 {
    let amplitude = (macro_ft.abs() * COASTLINE_CLAMP_RATIO).min(MAX_FRACTAL_AMPLITUDE_FT);
    if amplitude <= 0.0 {
        return 0.0;
    }
    let noise = fbm_3d(unit_vec);
    noise * amplitude
}

/// Deterministically samples Layer 0 + Layer 1 wilderness terrain for a single 5-foot square.
pub fn sample_tile(maps: &PlanetHeightmaps, tile: PlanetTile) -> TileSample {
    let normalized = normalize_tile(tile);
    let macro_ft = sample_macro_elevation_ft(maps, normalized);
    let unit_vec = tile_to_unit_vec(normalized);
    let detail_ft = sample_fractal_detail_ft(unit_vec, macro_ft);
    let combined_ft = macro_ft + detail_ft;

    let is_water = macro_ft < 0.0;
    let rounded = combined_ft
        .round()
        .clamp(f64::from(i32::MIN / 2), f64::from(i32::MAX / 2));
    #[allow(
        clippy::cast_possible_truncation,
        reason = "rounded is clamped well inside i32 range"
    )]
    let mut elevation_ft = rounded as i32;

    // Enforce exact land/water parity with macro sea level (coastlines never drift).
    if is_water && elevation_ft >= 0 {
        elevation_ft = -1;
    } else if !is_water && elevation_ft < 0 {
        elevation_ft = 0;
    }

    let elevation_step_ft = elevation_ft.div_euclid(ELEVATION_STEP_FT) * ELEVATION_STEP_FT;
    TileSample {
        elevation_ft,
        elevation_step_ft,
        is_water,
    }
}

/// Deterministically samples all `64 × 64` tiles of a streaming chunk in row-major order.
pub fn sample_chunk(maps: &PlanetHeightmaps, coord: ChunkCoord) -> ChunkTerrain {
    let tile_count = (CHUNK_SIZE as usize).saturating_mul(CHUNK_SIZE as usize);
    let mut tiles = Vec::with_capacity(tile_count);
    let base_x = coord.cx.saturating_mul(CHUNK_SIZE);
    let base_y = coord.cy.saturating_mul(CHUNK_SIZE);

    for local_y in 0..CHUNK_SIZE {
        for local_x in 0..CHUNK_SIZE {
            let tile = PlanetTile {
                face: coord.face,
                x: base_x.saturating_add(local_x),
                y: base_y.saturating_add(local_y),
            };
            tiles.push(sample_tile(maps, tile));
        }
    }

    ChunkTerrain { coord, tiles }
}

fn normalize_tile(tile: PlanetTile) -> PlanetTile {
    if tile.x < FACE_TILES && tile.y < FACE_TILES {
        tile
    } else {
        let (face, x, y, _rot) = step_grid(
            tile.face,
            FACE_TILES,
            i128::from(tile.x),
            i128::from(tile.y),
        );
        PlanetTile { face, x, y }
    }
}

/// Standard Catmull-Rom cubic interpolation weights for `t` in `[0.0, 1.0]`.
fn catmull_rom_weights(param: f64) -> [f64; 4] {
    let t2 = param * param;
    let t3 = t2 * param;
    [
        -0.5 * param + t2 - 0.5 * t3,
        1.0 - 2.5 * t2 + 1.5 * t3,
        0.5 * param + 2.0 * t2 - 1.5 * t3,
        -0.5 * t2 + 0.5 * t3,
    ]
}

/// Four-octave deterministic 3D value noise evaluated on the unit sphere (in `[-1.0, 1.0]`).
///
/// Evaluating in 3D sphere space guarantees identical values and smooth gradients across all
/// 12 cube-face seams and 8 corners.
fn fbm_3d(unit_vec: [f64; 3]) -> f64 {
    const OCTAVES: [(f64, f64, u64); 4] = [
        (4_096.0, 0.50, 0x9E37_79B9_7F4A_7C15),
        (12_288.0, 0.25, 0xBF58_476D_1CE4_E5B9),
        (32_768.0, 0.15, 0x94D0_49BB_1331_11EB),
        (81_920.0, 0.10, 0xD6E8_FEB8_6659_FD93),
    ];

    let mut total = 0.0;
    for (frequency, weight, seed) in OCTAVES {
        total += weight
            * value_noise_3d(
                unit_vec[0] * frequency,
                unit_vec[1] * frequency,
                unit_vec[2] * frequency,
                seed,
            );
    }
    total.clamp(-1.0, 1.0)
}

fn value_noise_3d(coord_x: f64, coord_y: f64, coord_z: f64, seed: u64) -> f64 {
    let floor_x = coord_x.floor();
    let floor_y = coord_y.floor();
    let floor_z = coord_z.floor();

    let fade_x = quintic_fade(coord_x - floor_x);
    let fade_y = quintic_fade(coord_y - floor_y);
    let fade_z = quintic_fade(coord_z - floor_z);

    #[allow(
        clippy::cast_possible_truncation,
        reason = "unit-sphere coordinates scaled by <= 81920 fit comfortably in i64"
    )]
    let ix = floor_x as i64;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "unit-sphere coordinates scaled by <= 81920 fit comfortably in i64"
    )]
    let iy = floor_y as i64;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "unit-sphere coordinates scaled by <= 81920 fit comfortably in i64"
    )]
    let iz = floor_z as i64;

    let c000 = lattice_hash_signed(ix, iy, iz, seed);
    let c100 = lattice_hash_signed(ix + 1, iy, iz, seed);
    let c010 = lattice_hash_signed(ix, iy + 1, iz, seed);
    let c110 = lattice_hash_signed(ix + 1, iy + 1, iz, seed);
    let c001 = lattice_hash_signed(ix, iy, iz + 1, seed);
    let c101 = lattice_hash_signed(ix + 1, iy, iz + 1, seed);
    let c011 = lattice_hash_signed(ix, iy + 1, iz + 1, seed);
    let c111 = lattice_hash_signed(ix + 1, iy + 1, iz + 1, seed);

    let x00 = lerp(c000, c100, fade_x);
    let x10 = lerp(c010, c110, fade_x);
    let x01 = lerp(c001, c101, fade_x);
    let x11 = lerp(c011, c111, fade_x);

    let y0 = lerp(x00, x10, fade_y);
    let y1 = lerp(x01, x11, fade_y);
    lerp(y0, y1, fade_z)
}

fn quintic_fade(param: f64) -> f64 {
    param * param * param * (param * (param * 6.0 - 15.0) + 10.0)
}

fn lerp(start: f64, end: f64, factor: f64) -> f64 {
    start + (end - start) * factor
}

/// Maps an integer 3D lattice point to a deterministic value in `[-1.0, 1.0]` using splitmix64.
fn lattice_hash_signed(ix: i64, iy: i64, iz: i64, seed: u64) -> f64 {
    let mut state = seed;
    state = splitmix64(state ^ ix.cast_unsigned());
    state = splitmix64(state ^ iy.cast_unsigned());
    state = splitmix64(state ^ iz.cast_unsigned());
    // Use top 53 bits for exact uniform f64 in [0.0, 1.0), then map to [-1.0, 1.0).
    let mantissa = state >> 11;
    #[allow(
        clippy::cast_precision_loss,
        reason = "mantissa uses exactly 53 bits, which fits losslessly in f64"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::cube_sphere::{FACE_CHUNKS, face_uv_to_unit_vec, step_tile};

    /// Builds a synthetic 6-face planet heightmap from a smooth spherical elevation function (in feet).
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
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    samples.push(encoded as u16);
                }
            }
            faces.push(FaceHeightmap::new(res, samples).expect("valid synthetic face"));
        }
        let array: [FaceHeightmap; 6] = faces.try_into().expect("6 faces");
        PlanetHeightmaps::new(array).expect("valid synthetic planet")
    }

    #[test]
    fn chunk_seam_continuity_holds_across_all_twelve_cube_edges() {
        // Smooth spherical harmonic elevation spanning ocean (-4000 ft) to highlands (+4000 ft).
        let planet = build_synthetic_planet(64, |[x, y, z]| {
            2400.0 * x + 1800.0 * y * z - 1200.0 * (x * x - z * z) + 600.0 * y
        });

        let last_tile = FACE_TILES - 1;
        let sample_offsets = [
            0_u32,
            16,
            FACE_TILES / 4,
            FACE_TILES / 2,
            (FACE_TILES / 4) * 3,
            last_tile - 16,
            last_tile,
        ];

        for face in CubeFace::ALL {
            for &offset in &sample_offsets {
                let edges = [
                    (
                        PlanetTile {
                            face,
                            x: last_tile,
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
                        -1_i64,
                        0_i64,
                    ),
                    (
                        PlanetTile {
                            face,
                            x: offset,
                            y: last_tile,
                        },
                        0_i64,
                        1_i64,
                    ),
                    (
                        PlanetTile {
                            face,
                            x: offset,
                            y: 0,
                        },
                        0_i64,
                        -1_i64,
                    ),
                ];

                for (tile_a, step_x, step_y) in edges {
                    let (tile_b, _rot) = step_tile(tile_a, step_x, step_y);
                    assert_ne!(tile_a.face, tile_b.face);

                    let macro_a = sample_macro_elevation_ft(&planet, tile_a);
                    let macro_b = sample_macro_elevation_ft(&planet, tile_b);
                    assert!(
                        (macro_a - macro_b).abs() < 1.0,
                        "macro seam discontinuity between {tile_a:?} ({macro_a}) and {tile_b:?} ({macro_b})"
                    );

                    let sample_a = sample_tile(&planet, tile_a);
                    let sample_b = sample_tile(&planet, tile_b);
                    let diff_ft = (sample_a.elevation_ft - sample_b.elevation_ft).abs();
                    assert!(
                        diff_ft <= 2,
                        "sampled seam discontinuity between {tile_a:?} ({sample_a:?}) and {tile_b:?} ({sample_b:?}): diff={diff_ft}ft"
                    );
                }
            }
        }
    }

    #[test]
    fn fractal_detail_never_flips_macro_land_to_water_or_water_to_land() {
        let land_planet = build_synthetic_planet(16, |_| 1.0);
        let sea_level_planet = build_synthetic_planet(16, |_| 0.0);
        let shallow_ocean_planet = build_synthetic_planet(16, |_| -1.0);

        for face in CubeFace::ALL {
            for idx in [0_u32, 33, 500_000, FACE_TILES - 1] {
                let tile = PlanetTile {
                    face,
                    x: idx,
                    y: FACE_TILES - 1 - idx,
                };
                let land = sample_tile(&land_planet, tile);
                assert!(!land.is_water && land.elevation_ft >= 0 && land.elevation_step_ft >= 0);

                let zero = sample_tile(&sea_level_planet, tile);
                assert!(!zero.is_water && zero.elevation_ft == 0 && zero.elevation_step_ft == 0);

                let ocean = sample_tile(&shallow_ocean_planet, tile);
                assert!(
                    ocean.is_water
                        && ocean.elevation_ft <= -1
                        && ocean.elevation_step_ft <= -ELEVATION_STEP_FT
                );
            }
        }
    }

    #[test]
    fn sample_chunk_is_deterministic_and_matches_sample_tile() {
        let planet = build_synthetic_planet(32, |[x, y, z]| 1500.0 * x + 900.0 * y - 700.0 * z);
        let coord = ChunkCoord {
            face: CubeFace::PosZ,
            cx: FACE_CHUNKS / 2,
            cy: FACE_CHUNKS / 2,
        };
        let chunk_first = sample_chunk(&planet, coord);
        let chunk_second = sample_chunk(&planet, coord);
        assert_eq!(chunk_first, chunk_second);
        assert_eq!(
            chunk_first.tiles.len(),
            usize::try_from(CHUNK_SIZE * CHUNK_SIZE).expect("fits")
        );

        for local_y in [0_u32, 17, CHUNK_SIZE - 1] {
            for local_x in [0_u32, 29, CHUNK_SIZE - 1] {
                let expected = sample_tile(
                    &planet,
                    coord.tile_at(local_x, local_y).expect("valid chunk tile"),
                );
                assert_eq!(chunk_first.tile(local_x, local_y), Some(expected));
            }
        }
    }

    #[test]
    fn heightmap_constructors_reject_invalid_dimensions() {
        assert_eq!(
            FaceHeightmap::new(0, vec![]),
            Err(HeightmapError::ZeroResolution)
        );
        assert!(matches!(
            FaceHeightmap::new(4, vec![SEA_LEVEL_U16; 10]),
            Err(HeightmapError::SampleCountMismatch {
                expected: 16,
                actual: 10,
                ..
            })
        ));

        let good = FaceHeightmap::new(4, vec![SEA_LEVEL_U16; 16]).expect("valid 4x4");
        let wrong_res = FaceHeightmap::new(8, vec![SEA_LEVEL_U16; 64]).expect("valid 8x8");
        let mismatch = PlanetHeightmaps::new([
            good.clone(),
            wrong_res,
            good.clone(),
            good.clone(),
            good.clone(),
            good,
        ]);
        assert_eq!(
            mismatch,
            Err(HeightmapError::MismatchedFaceResolution {
                face: CubeFace::NegX,
                expected: 4,
                actual: 8,
            })
        );
    }
}
