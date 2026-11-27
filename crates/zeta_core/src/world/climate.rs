//! Planetary climate maps (temperature, moisture, foliage density, biome override) stacked on
//! the same 6-face cube-sphere grid as the Layer 0 heightmaps.
//!
//! Rules: `docs/architecture/world_geography.md`. The maps are produced offline by
//! `tools/build_climate_faces.py` as `assets/world/faces/{face}_climate.png` (RGBA8). Performs
//! no input or output: callers decode the PNGs and pass the texels in here.
//!
//! Channel encoding (mirrored by the generator; change both together):
//! - R: temperature, linear from [`TEMPERATURE_MIN_C`] (`0`) to [`TEMPERATURE_MAX_C`] (`255`),
//!   mean annual near-surface air temperature in degrees Celsius.
//! - G: moisture, linear `0.0` (arid) to `1.0` (saturated), a relative humidity index.
//! - B: foliage density, linear `0.0` (bare) to `1.0` (closed canopy).
//! - A: biome override id, `0` = none. Categorical: always nearest-sampled, never blended.

use std::num::NonZeroU8;

use thiserror::Error;

use super::cube_sphere::{CubeFace, PlanetTile};
use super::sampler::{sample_cross_face_smooth, tile_pixel};

/// Temperature in degrees Celsius encoded by red channel value `0`.
pub const TEMPERATURE_MIN_C: f32 = -50.0;

/// Temperature in degrees Celsius encoded by red channel value `255`.
pub const TEMPERATURE_MAX_C: f32 = 40.0;

/// Failure constructing a [`FaceClimateMap`] or [`PlanetClimateMaps`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ClimateMapError {
    /// Face resolution was zero.
    #[error("climate map resolution must be greater than 0")]
    ZeroResolution,
    /// Texel buffer length did not match `res * res`.
    #[error("climate map expected {expected} texels for resolution {res}, got {actual}")]
    TexelCountMismatch {
        /// Declared square resolution.
        res: u32,
        /// Expected number of texels (`res * res`).
        expected: usize,
        /// Actual texel vector length.
        actual: usize,
    },
    /// The 6 faces did not share 1 uniform resolution.
    #[error("climate face {face} has resolution {actual}, expected uniform resolution {expected}")]
    MismatchedFaceResolution {
        /// Face whose resolution differed from `PosX`.
        face: CubeFace,
        /// Resolution of `PosX`.
        expected: u32,
        /// Resolution of `face`.
        actual: u32,
    },
}

/// Square RGBA8 climate raster for 1 cube face (row-major, `y * res + x`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceClimateMap {
    res: u32,
    texels: Vec<[u8; 4]>,
}

impl FaceClimateMap {
    /// Creates a validated [`FaceClimateMap`] from row-major `[R, G, B, A]` texels.
    ///
    /// # Errors
    /// Returns [`ClimateMapError::ZeroResolution`] if `res == 0`, or
    /// [`ClimateMapError::TexelCountMismatch`] if `texels.len() != res * res`.
    pub fn new(res: u32, texels: Vec<[u8; 4]>) -> Result<Self, ClimateMapError> {
        if res == 0 {
            return Err(ClimateMapError::ZeroResolution);
        }
        let side = usize::try_from(res).unwrap_or(usize::MAX);
        let expected = side.saturating_mul(side);
        if texels.len() != expected {
            return Err(ClimateMapError::TexelCountMismatch {
                res,
                expected,
                actual: texels.len(),
            });
        }
        Ok(Self { res, texels })
    }

    /// Width and height of this face in texels.
    pub const fn res(&self) -> u32 {
        self.res
    }

    /// Reads texel `(x, y)`, or all zeros when out of range.
    pub fn get(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.res || y >= self.res {
            return [0; 4];
        }
        let index = u64::from(y) * u64::from(self.res) + u64::from(x);
        usize::try_from(index)
            .ok()
            .and_then(|idx| self.texels.get(idx).copied())
            .unwrap_or([0; 4])
    }
}

/// Complete 6-face climate dataset indexed by [`CubeFace`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanetClimateMaps {
    faces: [FaceClimateMap; 6],
    res: u32,
}

impl PlanetClimateMaps {
    /// Constructs a validated 6-face climate dataset ordered by [`CubeFace::ALL`].
    ///
    /// # Errors
    /// Returns [`ClimateMapError::MismatchedFaceResolution`] if the faces differ in resolution.
    pub fn new(faces: [FaceClimateMap; 6]) -> Result<Self, ClimateMapError> {
        let expected = faces[0].res;
        for (face, map) in CubeFace::ALL.into_iter().zip(faces.iter()) {
            if map.res != expected {
                return Err(ClimateMapError::MismatchedFaceResolution {
                    face,
                    expected,
                    actual: map.res,
                });
            }
        }
        Ok(Self {
            faces,
            res: expected,
        })
    }

    /// Returns the climate raster for `face`.
    pub fn face(&self, face: CubeFace) -> &FaceClimateMap {
        &self.faces[face.index()]
    }

    /// Returns the uniform texel resolution per face edge.
    pub const fn res(&self) -> u32 {
        self.res
    }
}

/// Decoded climate at 1 5-foot square.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClimateSample {
    /// Mean annual near-surface air temperature in degrees Celsius.
    pub temperature: f32,
    /// Relative moisture index in `[0.0, 1.0]` (`0` arid, `1` saturated).
    pub moisture: f32,
    /// Foliage density in `[0.0, 1.0]` (`0` bare ground, `1` closed canopy).
    pub foliage: f32,
    /// Game Director biome override id from the alpha channel, `None` when the texel is `0`.
    pub biome_override: Option<NonZeroU8>,
}

/// Decodes a red channel value into degrees Celsius.
pub fn temperature_from_byte(value: u8) -> f32 {
    TEMPERATURE_MIN_C + f32::from(value) / 255.0 * (TEMPERATURE_MAX_C - TEMPERATURE_MIN_C)
}

/// Encodes degrees Celsius into a red channel value (clamped to the encodable range).
pub fn temperature_to_byte(celsius: f32) -> u8 {
    let normalized = (celsius - TEMPERATURE_MIN_C) / (TEMPERATURE_MAX_C - TEMPERATURE_MIN_C);
    unit_to_byte(normalized)
}

/// Encodes a `[0.0, 1.0]` value into a channel byte (clamped).
pub fn unit_to_byte(value: f32) -> u8 {
    let scaled = (value.clamp(0.0, 1.0) * 255.0).round();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "scaled is rounded and clamped into [0, 255]"
    )]
    let byte = scaled as u8;
    byte
}

/// Samples the climate at `tile`.
///
/// Temperature, moisture, and foliage are Catmull-Rom bicubic interpolated across cube-face
/// seams with the same cross-face pattern as [`super::sample_macro_elevation_ft`], so they are
/// continuous across all 12 face edges. The biome override is nearest-sampled.
pub fn sample_climate(maps: &PlanetClimateMaps, tile: PlanetTile) -> ClimateSample {
    let [red, green, blue] = sample_cross_face_smooth(maps.res(), tile, |face, x, y| {
        let [r, g, b, _] = maps.face(face).get(x, y);
        [f64::from(r), f64::from(g), f64::from(b)]
    });
    let to_unit = |value: f64| -> f32 {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "value is clamped into [0, 1], well inside f32 range"
        )]
        let unit = (value / 255.0).clamp(0.0, 1.0) as f32;
        unit
    };
    let (face, pixel_x, pixel_y) = tile_pixel(maps.res(), tile);
    let [_, _, _, override_id] = maps.face(face).get(pixel_x, pixel_y);
    ClimateSample {
        temperature: TEMPERATURE_MIN_C + to_unit(red) * (TEMPERATURE_MAX_C - TEMPERATURE_MIN_C),
        moisture: to_unit(green),
        foliage: to_unit(blue),
        biome_override: NonZeroU8::new(override_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::cube_sphere::{FACE_TILES, face_uv_to_unit_vec, step_tile};

    /// Builds a synthetic planet whose climate channels are smooth functions of direction and
    /// whose alpha channel holds the face index + 1.
    fn synthetic_planet(res: u32) -> PlanetClimateMaps {
        let res_f64 = f64::from(res);
        let faces: Vec<FaceClimateMap> = CubeFace::ALL
            .into_iter()
            .map(|face| {
                let mut texels = Vec::new();
                for pixel_y in 0..res {
                    let face_v = (f64::from(pixel_y) + 0.5) / res_f64 * 2.0 - 1.0;
                    for pixel_x in 0..res {
                        let face_u = (f64::from(pixel_x) + 0.5) / res_f64 * 2.0 - 1.0;
                        let [x, y, z] = face_uv_to_unit_vec(face, face_u, face_v);
                        #[allow(clippy::cast_possible_truncation)]
                        let channel = |value: f64| unit_to_byte(value as f32);
                        texels.push([
                            channel(0.5 + 0.4 * x),
                            channel(0.5 + 0.4 * y * z),
                            channel(0.5 - 0.3 * z + 0.1 * x),
                            u8::try_from(face.index() + 1).expect("fits"),
                        ]);
                    }
                }
                FaceClimateMap::new(res, texels).expect("valid face")
            })
            .collect();
        PlanetClimateMaps::new(faces.try_into().expect("6 faces")).expect("valid planet")
    }

    #[test]
    fn climate_is_continuous_across_all_face_edges() {
        let planet = synthetic_planet(64);
        let last = FACE_TILES - 1;
        let offsets = [0, 17, FACE_TILES / 3, FACE_TILES / 2, last - 17, last];
        for face in CubeFace::ALL {
            for &offset in &offsets {
                let edges = [
                    (
                        PlanetTile {
                            face,
                            x: last,
                            y: offset,
                        },
                        1,
                        0,
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
                ];
                for (tile_a, step_x, step_y) in edges {
                    let (tile_b, _) = step_tile(tile_a, step_x, step_y);
                    assert_ne!(tile_a.face, tile_b.face);
                    let a = sample_climate(&planet, tile_a);
                    let b = sample_climate(&planet, tile_b);
                    let range = TEMPERATURE_MAX_C - TEMPERATURE_MIN_C;
                    assert!(
                        (a.temperature - b.temperature).abs() / range < 1e-3
                            && (a.moisture - b.moisture).abs() < 1e-3
                            && (a.foliage - b.foliage).abs() < 1e-3,
                        "climate seam jump between {tile_a:?} ({a:?}) and {tile_b:?} ({b:?})"
                    );
                }
            }
        }
    }

    #[test]
    fn sampling_is_deterministic_and_reproduces_texel_centers() {
        let planet = synthetic_planet(16);
        let tiles_per_texel = FACE_TILES / 16;
        // A tile at the center of texel (5, 9) reproduces that texel exactly.
        let tile = PlanetTile {
            face: CubeFace::PosZ,
            x: 5 * tiles_per_texel + tiles_per_texel / 2,
            y: 9 * tiles_per_texel + tiles_per_texel / 2,
        };
        let first = sample_climate(&planet, tile);
        assert_eq!(first, sample_climate(&planet, tile));
        let [r, g, b, a] = planet.face(CubeFace::PosZ).get(5, 9);
        assert!((first.temperature - temperature_from_byte(r)).abs() < 0.5);
        assert!((first.moisture - f32::from(g) / 255.0).abs() < 0.01);
        assert!((first.foliage - f32::from(b) / 255.0).abs() < 0.01);
        assert_eq!(first.biome_override, NonZeroU8::new(a));
    }

    #[test]
    fn biome_override_is_nearest_sampled_never_blended() {
        let planet = synthetic_planet(16);
        // On every face, the last tile before the edge still reads that face's own id.
        for face in CubeFace::ALL {
            let expected = NonZeroU8::new(u8::try_from(face.index() + 1).expect("fits"));
            for tile in [
                PlanetTile { face, x: 0, y: 0 },
                PlanetTile {
                    face,
                    x: FACE_TILES - 1,
                    y: FACE_TILES / 2,
                },
            ] {
                assert_eq!(sample_climate(&planet, tile).biome_override, expected);
            }
        }
    }

    #[test]
    fn temperature_encoding_round_trips() {
        for byte in [0_u8, 1, 128, 254, 255] {
            assert_eq!(temperature_to_byte(temperature_from_byte(byte)), byte);
        }
        assert_eq!(temperature_to_byte(-200.0), 0);
        assert_eq!(temperature_to_byte(200.0), 255);
    }

    #[test]
    fn constructors_reject_invalid_dimensions() {
        assert_eq!(
            FaceClimateMap::new(0, vec![]),
            Err(ClimateMapError::ZeroResolution)
        );
        assert!(matches!(
            FaceClimateMap::new(4, vec![[0; 4]; 3]),
            Err(ClimateMapError::TexelCountMismatch {
                expected: 16,
                actual: 3,
                ..
            })
        ));
        let small = FaceClimateMap::new(2, vec![[0; 4]; 4]).expect("valid");
        let large = FaceClimateMap::new(4, vec![[0; 4]; 16]).expect("valid");
        assert!(matches!(
            PlanetClimateMaps::new([
                small.clone(),
                small.clone(),
                large,
                small.clone(),
                small.clone(),
                small
            ]),
            Err(ClimateMapError::MismatchedFaceResolution {
                face: CubeFace::PosY,
                ..
            })
        ));
    }
}
