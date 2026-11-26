//! Client-side Layer 0 planetary heightmap loader and cube-sphere seam verifier.
//!
//! Rules: `docs/architecture/world_geography.md` Section 2.
//!
//! Decodes the 6 unsigned 16-bit grayscale PNG face heightmaps from `assets/world/faces/` in
//! canonical face order (`pos_x`, `neg_x`, `pos_y`, `neg_y`, `pos_z`, `neg_z`) using Bevy's
//! built-in PNG decoder (`bevy::image::Image::from_buffer`) without introducing additional
//! image-decoding dependencies.

use std::path::{Path, PathBuf};

use bevy::asset::RenderAssetUsages;
use bevy::image::{CompressedImageFormats, Image, ImageFormat, ImageSampler, ImageType};
use bevy::render::render_resource::TextureFormat;
use thiserror::Error;
use zeta_core::world::{FaceHeightmap, HeightmapError, PlanetHeightmaps};

use crate::terrain::assets_root;

/// Canonical ordering of the 6 cube-sphere faces (`docs/architecture/world_geography.md` Section 2).
pub const CANONICAL_FACE_NAMES: [&str; 6] = ["pos_x", "neg_x", "pos_y", "neg_y", "pos_z", "neg_z"];

/// Exact sea-level sample value (`0` feet) in Layer 0 heightmaps.
pub const SEA_LEVEL_U16: u16 = 32_768;

/// Directory containing the 6 Layer 0 cube-sphere heightmap PNGs, relative to `assets/`.
pub const PLANET_FACES_DIR: &str = "world/faces";

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const PNG_IHDR_CHUNK_LENGTH: u32 = 13;
const PNG_IHDR_MIN_BYTES: usize = 8 + 4 + 4 + 13 + 4;
const PNG_GRAYSCALE_COLOR_TYPE: u8 = 0;
const PNG_SIXTEEN_BIT_DEPTH: u8 = 16;
const RGBA16_BYTES_PER_PIXEL: usize = 8;

/// Decoded Layer 0 unsigned 16-bit heightmap for 1 cube-sphere face.
///
/// Samples are stored in row-major order (`y * width + x`) where `32768` represents exact sea
/// level (`0` feet) and each unit is `1` foot of elevation offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawFaceHeightmap {
    /// Canonical face name (`pos_x`, `neg_x`, `pos_y`, `neg_y`, `pos_z`, or `neg_z`).
    pub face_name: &'static str,
    /// Horizontal pixel resolution of this face.
    pub width: u32,
    /// Vertical pixel resolution of this face.
    pub height: u32,
    /// Row-major unsigned 16-bit elevation samples (`width * height` entries).
    pub samples: Vec<u16>,
}

impl RawFaceHeightmap {
    /// Returns the elevation sample at `(x, y)`, or `None` if out of bounds.
    pub fn sample(&self, x: u32, y: u32) -> Option<u16> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = usize::try_from(y)
            .ok()?
            .checked_mul(usize::try_from(self.width).ok()?)?
            .checked_add(usize::try_from(x).ok()?)?;
        self.samples.get(index).copied()
    }

    /// Returns whether this face has a non-zero square power-of-two resolution.
    pub fn is_square_power_of_two(&self) -> bool {
        self.width > 0 && self.width == self.height && self.width.is_power_of_two()
    }
}

/// Errors produced when loading or validating Layer 0 planetary heightmap PNGs.
#[derive(Debug, Error)]
pub enum PlanetIoError {
    /// Reading a face PNG file from disk failed.
    #[error("could not read planet face heightmap {path}: {source}")]
    ReadFile {
        /// File path that could not be read.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// The file does not begin with a valid PNG signature and `IHDR` header.
    #[error("invalid PNG header in {path}: {reason} (expected a valid 16-bit grayscale PNG)")]
    InvalidPngHeader {
        /// File path with the malformed header.
        path: PathBuf,
        /// Description of the header mismatch.
        reason: &'static str,
    },
    /// The PNG header does not declare 16-bit grayscale pixels (`bit_depth = 16, color_type = 0`).
    #[error(
        "unsupported PNG pixel format in {path}: bit_depth={bit_depth}, color_type={color_type} \
         (only 16-bit grayscale PNGs with bit_depth=16 and color_type=0 are allowed)"
    )]
    NotSixteenBitGrayscale {
        /// File path with the unsupported PNG format.
        path: PathBuf,
        /// Declared PNG bit depth from `IHDR`.
        bit_depth: u8,
        /// Declared PNG color type from `IHDR`.
        color_type: u8,
    },
    /// Bevy's PNG decoder rejected the file bytes.
    #[error("could not decode PNG {path}: {reason}")]
    DecodePng {
        /// File path that failed PNG decoding.
        path: PathBuf,
        /// Error message reported by the decoder.
        reason: String,
    },
    /// The decoded Bevy `Image` did not use the expected 16-bit texture format.
    #[error(
        "unexpected decoded texture format for {path}: {format} \
         (expected TextureFormat::Rgba16Unorm or TextureFormat::R16Unorm)"
    )]
    UnexpectedTextureFormat {
        /// File path whose decoded texture format was unexpected.
        path: PathBuf,
        /// Debug representation of the decoded `TextureFormat`.
        format: String,
    },
    /// The decoded Bevy `Image` contained no CPU pixel buffer.
    #[error("decoded PNG {path} has no CPU pixel buffer (MAIN_WORLD usage is required)")]
    MissingPixelData {
        /// File path whose decoded image lacked pixel bytes.
        path: PathBuf,
    },
    /// Image dimensions are zero or disagree with the PNG `IHDR` chunk.
    #[error(
        "invalid dimensions for {path}: {width}x{height} \
         (width and height must be non-zero and match the PNG IHDR header)"
    )]
    InvalidDimensions {
        /// File path with invalid dimensions.
        path: PathBuf,
        /// Reported width in pixels.
        width: u32,
        /// Reported height in pixels.
        height: u32,
    },
    /// The decoded byte buffer length does not match `width * height * bytes_per_pixel`.
    #[error(
        "corrupt pixel buffer for {path}: got {actual_bytes} bytes, expected {expected_bytes} bytes"
    )]
    CorruptPixelBuffer {
        /// File path whose pixel buffer length mismatched.
        path: PathBuf,
        /// Expected byte length derived from dimensions and pixel stride.
        expected_bytes: usize,
        /// Actual byte length present in the decoded image.
        actual_bytes: usize,
    },
    /// Expanded RGBA16 pixel channels disagreed, indicating non-grayscale pixel data.
    #[error(
        "non-grayscale pixel at index {pixel_index} in {path}: rgb=({red}, {green}, {blue}) \
         (all three channels must match)"
    )]
    NonGrayscalePixel {
        /// File path containing non-grayscale channels.
        path: PathBuf,
        /// Linear pixel index where RGB channels diverged.
        pixel_index: usize,
        /// Red 16-bit channel value.
        red: u16,
        /// Green 16-bit channel value.
        green: u16,
        /// Blue 16-bit channel value.
        blue: u16,
    },
    /// One face PNG has different dimensions than the first loaded face.
    #[error(
        "face {face_name} has dimensions {width}x{height}, expected {expected_width}x{expected_height} \
         (all 6 planet faces must share identical dimensions)"
    )]
    InconsistentFaceDimensions {
        /// Canonical face name that disagreed with the reference face.
        face_name: &'static str,
        /// Width of the mismatched face.
        width: u32,
        /// Height of the mismatched face.
        height: u32,
        /// Expected width shared by the planet face set.
        expected_width: u32,
        /// Expected height shared by the planet face set.
        expected_height: u32,
    },
    /// Validated `zeta_core` planetary heightmap construction failed.
    #[error("invalid planetary heightmap dataset: {0}")]
    Heightmap(#[from] HeightmapError),
}

/// Resolves the workspace `assets/world/faces` directory.
pub fn planet_faces_dir() -> PathBuf {
    assets_root().join(PLANET_FACES_DIR)
}

/// Converts 6 decoded [`RawFaceHeightmap`] layers into a validated [`PlanetHeightmaps`].
///
/// # Errors
/// Returns [`HeightmapError`] if any face is non-square, zero-sized, or mismatches dimensions.
pub fn raw_faces_to_planet_heightmaps(
    faces: [RawFaceHeightmap; 6],
) -> Result<PlanetHeightmaps, HeightmapError> {
    let convert = |raw: RawFaceHeightmap| -> Result<FaceHeightmap, HeightmapError> {
        if raw.width != raw.height {
            return Err(HeightmapError::SampleCountMismatch {
                face: None,
                res: raw.width,
                expected: usize::try_from(raw.width)
                    .unwrap_or(usize::MAX)
                    .saturating_mul(usize::try_from(raw.width).unwrap_or(usize::MAX)),
                actual: raw.samples.len(),
            });
        }
        FaceHeightmap::new(raw.width, raw.samples)
    };
    let [pos_x, neg_x, pos_y, neg_y, pos_z, neg_z] = faces;
    PlanetHeightmaps::new([
        convert(pos_x)?,
        convert(neg_x)?,
        convert(pos_y)?,
        convert(neg_y)?,
        convert(pos_z)?,
        convert(neg_z)?,
    ])
}

/// Loads all 6 Layer 0 planetary heightmap PNGs from `dir` directly into [`PlanetHeightmaps`].
///
/// # Errors
/// Returns [`PlanetIoError`] if loading any face PNG or validating the dataset fails.
pub fn load_planet_heightmaps(dir: &Path) -> Result<PlanetHeightmaps, PlanetIoError> {
    let raw = load_planet_faces(dir)?;
    Ok(raw_faces_to_planet_heightmaps(raw)?)
}

/// Loads all 6 Layer 0 planetary heightmap PNGs from `dir` in canonical face order.
///
/// Expects `{pos_x,neg_x,pos_y,neg_y,pos_z,neg_z}_elevation.png` inside `dir`, each encoded as
/// a 16-bit grayscale PNG with identical non-zero dimensions.
///
/// # Errors
///
/// Returns [`PlanetIoError`] if any face file is missing, unreadable, not a valid 16-bit
/// grayscale PNG, fails decoding, or has dimensions that differ from the other faces.
pub fn load_planet_faces(dir: &Path) -> Result<[RawFaceHeightmap; 6], PlanetIoError> {
    let faces = [
        load_face(dir, CANONICAL_FACE_NAMES[0])?,
        load_face(dir, CANONICAL_FACE_NAMES[1])?,
        load_face(dir, CANONICAL_FACE_NAMES[2])?,
        load_face(dir, CANONICAL_FACE_NAMES[3])?,
        load_face(dir, CANONICAL_FACE_NAMES[4])?,
        load_face(dir, CANONICAL_FACE_NAMES[5])?,
    ];
    let expected_width = faces[0].width;
    let expected_height = faces[0].height;
    for face in &faces[1..] {
        if face.width != expected_width || face.height != expected_height {
            return Err(PlanetIoError::InconsistentFaceDimensions {
                face_name: face.face_name,
                width: face.width,
                height: face.height,
                expected_width,
                expected_height,
            });
        }
    }
    Ok(faces)
}

fn load_face(dir: &Path, face_name: &'static str) -> Result<RawFaceHeightmap, PlanetIoError> {
    let path = dir.join(format!("{face_name}_elevation.png"));
    let bytes = std::fs::read(&path).map_err(|source| PlanetIoError::ReadFile {
        path: path.clone(),
        source,
    })?;
    decode_grayscale_u16_png(&bytes, &path, face_name)
}

fn validate_png_ihdr_grayscale_u16(bytes: &[u8], path: &Path) -> Result<(u32, u32), PlanetIoError> {
    if bytes.len() < PNG_IHDR_MIN_BYTES {
        return Err(PlanetIoError::InvalidPngHeader {
            path: path.to_path_buf(),
            reason: "file is shorter than a PNG signature and IHDR chunk",
        });
    }
    if bytes[..8] != PNG_SIGNATURE {
        return Err(PlanetIoError::InvalidPngHeader {
            path: path.to_path_buf(),
            reason: "missing PNG file signature",
        });
    }
    let chunk_length = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    if chunk_length != PNG_IHDR_CHUNK_LENGTH || &bytes[12..16] != b"IHDR" {
        return Err(PlanetIoError::InvalidPngHeader {
            path: path.to_path_buf(),
            reason: "first PNG chunk is not a 13-byte IHDR chunk",
        });
    }

    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if width == 0 || height == 0 {
        return Err(PlanetIoError::InvalidDimensions {
            path: path.to_path_buf(),
            width,
            height,
        });
    }

    let bit_depth = bytes[24];
    let color_type = bytes[25];
    if bit_depth != PNG_SIXTEEN_BIT_DEPTH || color_type != PNG_GRAYSCALE_COLOR_TYPE {
        return Err(PlanetIoError::NotSixteenBitGrayscale {
            path: path.to_path_buf(),
            bit_depth,
            color_type,
        });
    }

    Ok((width, height))
}

fn decode_grayscale_u16_png(
    bytes: &[u8],
    path: &Path,
    face_name: &'static str,
) -> Result<RawFaceHeightmap, PlanetIoError> {
    let (header_width, header_height) = validate_png_ihdr_grayscale_u16(bytes, path)?;

    // `Image::from_buffer`: bevy_image-0.20.1/src/image.rs:1589.
    // `ImageType::Format`: bevy_image-0.20.1/src/image.rs:2343.
    // `ImageFormat::Png`: bevy_image-0.20.1/src/image.rs:297.
    // `CompressedImageFormats::NONE`: bevy_image-0.20.1/src/image.rs:2392.
    // `ImageSampler::Default`: bevy_image-0.20.1/src/image.rs:700.
    // `RenderAssetUsages::MAIN_WORLD`: bevy_asset-0.20.1/src/render_asset.rs:34.
    let image = Image::from_buffer(
        bytes,
        ImageType::Format(ImageFormat::Png),
        CompressedImageFormats::NONE,
        false,
        ImageSampler::Default,
        RenderAssetUsages::MAIN_WORLD,
    )
    .map_err(|error| PlanetIoError::DecodePng {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;

    // `Image::width` / `Image::height`: bevy_image-0.20.1/src/image.rs:1304, 1310.
    let width = image.width();
    let height = image.height();
    if width != header_width || height != header_height {
        return Err(PlanetIoError::InvalidDimensions {
            path: path.to_path_buf(),
            width,
            height,
        });
    }

    let pixel_count = usize::try_from(width)
        .ok()
        .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
        .ok_or_else(|| PlanetIoError::InvalidDimensions {
            path: path.to_path_buf(),
            width,
            height,
        })?;

    let samples = extract_grayscale_u16_samples(&image, path, width, height, pixel_count)?;

    Ok(RawFaceHeightmap {
        face_name,
        width,
        height,
        samples,
    })
}

fn extract_grayscale_u16_samples(
    image: &Image,
    path: &Path,
    width: u32,
    height: u32,
    pixel_count: usize,
) -> Result<Vec<u16>, PlanetIoError> {
    let raw_bytes = image
        .data
        .as_deref()
        .ok_or_else(|| PlanetIoError::MissingPixelData {
            path: path.to_path_buf(),
        })?;

    // `Image::from_buffer` expands `Luma16` PNGs to `TextureFormat::Rgba16Unorm` using
    // native-endian `u16` channels (`bevy_image-0.20.1/src/image_texture_conversion.rs:30-32, 107-115`).
    match image.texture_descriptor.format {
        TextureFormat::Rgba16Unorm => {
            let expected_bytes =
                pixel_count
                    .checked_mul(RGBA16_BYTES_PER_PIXEL)
                    .ok_or_else(|| PlanetIoError::InvalidDimensions {
                        path: path.to_path_buf(),
                        width,
                        height,
                    })?;
            if raw_bytes.len() != expected_bytes {
                return Err(PlanetIoError::CorruptPixelBuffer {
                    path: path.to_path_buf(),
                    expected_bytes,
                    actual_bytes: raw_bytes.len(),
                });
            }
            let mut samples = Vec::with_capacity(pixel_count);
            for (pixel_index, chunk) in raw_bytes
                .as_chunks::<RGBA16_BYTES_PER_PIXEL>()
                .0
                .iter()
                .enumerate()
            {
                let red = u16::from_ne_bytes([chunk[0], chunk[1]]);
                let green = u16::from_ne_bytes([chunk[2], chunk[3]]);
                let blue = u16::from_ne_bytes([chunk[4], chunk[5]]);
                if red != green || green != blue {
                    return Err(PlanetIoError::NonGrayscalePixel {
                        path: path.to_path_buf(),
                        pixel_index,
                        red,
                        green,
                        blue,
                    });
                }
                samples.push(red);
            }
            Ok(samples)
        }
        TextureFormat::R16Unorm => {
            let expected_bytes =
                pixel_count
                    .checked_mul(2)
                    .ok_or_else(|| PlanetIoError::InvalidDimensions {
                        path: path.to_path_buf(),
                        width,
                        height,
                    })?;
            if raw_bytes.len() != expected_bytes {
                return Err(PlanetIoError::CorruptPixelBuffer {
                    path: path.to_path_buf(),
                    expected_bytes,
                    actual_bytes: raw_bytes.len(),
                });
            }
            Ok(raw_bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|chunk| u16::from_ne_bytes([chunk[0], chunk[1]]))
                .collect())
        }
        other => Err(PlanetIoError::UnexpectedTextureFormat {
            path: path.to_path_buf(),
            format: format!("{other:?}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    struct FaceBasis {
        forward: [f64; 3],
        right_u: [f64; 3],
        down_v: [f64; 3],
    }

    /// Cube-face basis vectors matching `tools/build_planet_faces.py`.
    const FACE_BASES: [FaceBasis; 6] = [
        FaceBasis {
            forward: [1.0, 0.0, 0.0],
            right_u: [0.0, 0.0, -1.0],
            down_v: [0.0, -1.0, 0.0],
        },
        FaceBasis {
            forward: [-1.0, 0.0, 0.0],
            right_u: [0.0, 0.0, 1.0],
            down_v: [0.0, -1.0, 0.0],
        },
        FaceBasis {
            forward: [0.0, 1.0, 0.0],
            right_u: [1.0, 0.0, 0.0],
            down_v: [0.0, 0.0, 1.0],
        },
        FaceBasis {
            forward: [0.0, -1.0, 0.0],
            right_u: [1.0, 0.0, 0.0],
            down_v: [0.0, 0.0, -1.0],
        },
        FaceBasis {
            forward: [0.0, 0.0, 1.0],
            right_u: [1.0, 0.0, 0.0],
            down_v: [0.0, -1.0, 0.0],
        },
        FaceBasis {
            forward: [0.0, 0.0, -1.0],
            right_u: [-1.0, 0.0, 0.0],
            down_v: [0.0, -1.0, 0.0],
        },
    ];

    #[derive(Clone, Copy, Debug)]
    enum FaceEdge {
        Left,
        Right,
        Top,
        Bottom,
    }

    const ALL_EDGES: [FaceEdge; 4] = [
        FaceEdge::Left,
        FaceEdge::Right,
        FaceEdge::Top,
        FaceEdge::Bottom,
    ];

    fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    fn scale(v: [f64; 3], factor: f64) -> [f64; 3] {
        [v[0] * factor, v[1] * factor, v[2] * factor]
    }

    fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
        [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
    }

    fn normalize(v: [f64; 3]) -> [f64; 3] {
        let len = dot(v, v).sqrt();
        [v[0] / len, v[1] / len, v[2] / len]
    }

    /// Unit-sphere direction of pixel `(pixel_x, pixel_y)` using the tangent warp in
    /// `tools/build_planet_faces.py`.
    fn pixel_direction(face_index: usize, pixel_x: u32, pixel_y: u32, resolution: u32) -> [f64; 3] {
        let basis = FACE_BASES[face_index];
        let norm_u = (f64::from(pixel_x) + 0.5) / f64::from(resolution) * 2.0 - 1.0;
        let norm_v = (f64::from(pixel_y) + 0.5) / f64::from(resolution) * 2.0 - 1.0;
        let tan_u = (norm_u * std::f64::consts::FRAC_PI_4).tan();
        let tan_v = (norm_v * std::f64::consts::FRAC_PI_4).tan();
        normalize(add(
            add(basis.forward, scale(basis.right_u, tan_u)),
            scale(basis.down_v, tan_v),
        ))
    }

    /// Maps `(face_index, edge, index)` to its own pixel `(x, y)` and the matched adjacent pixel
    /// `(neighbor_face, nx, ny)` across the cube seam, derived strictly from `FACE_BASES`.
    fn matched_seam_pixels(
        face_index: usize,
        edge: FaceEdge,
        index: u32,
        resolution: u32,
    ) -> ((u32, u32), (usize, u32, u32)) {
        let basis = FACE_BASES[face_index];
        let last = resolution - 1;
        let (own_pixel, edge_normal, edge_tangent) = match edge {
            FaceEdge::Left => ((0, index), scale(basis.right_u, -1.0), basis.down_v),
            FaceEdge::Right => ((last, index), basis.right_u, basis.down_v),
            FaceEdge::Top => ((index, 0), scale(basis.down_v, -1.0), basis.right_u),
            FaceEdge::Bottom => ((index, last), basis.down_v, basis.right_u),
        };

        let neighbor_face = FACE_BASES
            .iter()
            .position(|neighbor| dot(neighbor.forward, edge_normal) > 0.5)
            .expect("every cube face edge borders a unique neighboring face");

        let neighbor = FACE_BASES[neighbor_face];

        // On the shared cube edge, point `P = forward + edge_normal + t * edge_tangent` has
        // `dot(P, neighbor.forward) = +1`. Projecting `P` onto `(neighbor.right_u, neighbor.down_v)`
        // fixes one axis to +/-1 (the neighbor's boundary pixel) and maps `t` to +/-t (index or mirror).
        let resolve_axis = |axis: [f64; 3]| -> u32 {
            let boundary = dot(basis.forward, axis);
            let along = dot(edge_tangent, axis);
            if boundary > 0.5 {
                last
            } else if boundary < -0.5 {
                0
            } else if along > 0.5 {
                index
            } else {
                assert!(
                    along < -0.5,
                    "orthonormal cube basis must align with boundary or tangent"
                );
                last - index
            }
        };

        let neighbor_x = resolve_axis(neighbor.right_u);
        let neighbor_y = resolve_axis(neighbor.down_v);
        (own_pixel, (neighbor_face, neighbor_x, neighbor_y))
    }

    #[test]
    fn all_planet_face_heightmaps_exist_and_decode_as_1024_grayscale_with_land_and_ocean() {
        let faces = load_planet_faces(&planet_faces_dir()).expect("valid 6 Layer 0 face PNGs");

        for (index, face) in faces.iter().enumerate() {
            assert_eq!(face.face_name, CANONICAL_FACE_NAMES[index]);
            assert!(
                face.is_square_power_of_two(),
                "face {} must have square power-of-two resolution, got {}x{}",
                face.face_name,
                face.width,
                face.height
            );
            assert_eq!(
                (face.width, face.height),
                (1024, 1024),
                "Layer 0 face {} must be 1024x1024",
                face.face_name
            );
            assert_eq!(
                face.samples.len(),
                (face.width as usize) * (face.height as usize),
                "face {} sample count must equal width * height",
                face.face_name
            );

            let has_ocean = face.samples.iter().any(|&sample| sample < SEA_LEVEL_U16);
            let has_land = face.samples.iter().any(|&sample| sample >= SEA_LEVEL_U16);
            assert!(
                has_ocean,
                "face {} must contain ocean samples (< {SEA_LEVEL_U16})",
                face.face_name
            );
            assert!(
                has_land,
                "face {} must contain land samples (>= {SEA_LEVEL_U16})",
                face.face_name
            );

            // Prove genuine 16-bit decoding (not 8-bit truncation expanded via `* 257` or `<< 8`).
            let mut seen = vec![false; usize::from(u16::MAX) + 1];
            for &sample in &face.samples {
                seen[usize::from(sample)] = true;
            }
            let distinct_values = seen.iter().filter(|&&present| present).count();
            assert!(
                distinct_values > 256,
                "face {} only has {distinct_values} distinct values (expected > 256 for 16-bit depth)",
                face.face_name
            );
            assert!(
                face.samples
                    .iter()
                    .any(|&sample| sample % 257 != 0 && sample % 256 != 0),
                "face {} samples appear quantized to 8-bit multiples",
                face.face_name
            );
        }
    }

    #[test]
    fn planet_face_seams_are_continuous_across_all_twelve_cube_edges() {
        let faces = load_planet_faces(&planet_faces_dir()).expect("valid 6 Layer 0 face PNGs");
        let resolution = faces[0].width;

        // Baseline: mean absolute elevation change between adjacent interior pixels across all faces.
        let mut interior_diff_sum: u64 = 0;
        let mut interior_pair_count: u64 = 0;
        for face in &faces {
            for y in 0..resolution {
                for x in 0..(resolution - 1) {
                    let left = face.sample(x, y).expect("in-bounds interior sample");
                    let right = face.sample(x + 1, y).expect("in-bounds interior sample");
                    interior_diff_sum += u64::from(left.abs_diff(right));
                    interior_pair_count += 1;
                }
            }
            for y in 0..(resolution - 1) {
                for x in 0..resolution {
                    let top = face.sample(x, y).expect("in-bounds interior sample");
                    let bottom = face.sample(x, y + 1).expect("in-bounds interior sample");
                    interior_diff_sum += u64::from(top.abs_diff(bottom));
                    interior_pair_count += 1;
                }
            }
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "test diagnostic averages across 1024x1024 grids do not require >53-bit precision"
        )]
        let interior_mean_diff = interior_diff_sum as f64 / interior_pair_count as f64;

        // Expected 1-pixel angular separation across a cube seam: 2 * atan(tan(pi/4) / N) <= ~0.002 rad.
        let max_expected_angle_rad = std::f64::consts::PI / f64::from(resolution);

        let mut seam_diff_sum: u64 = 0;
        let mut seam_pair_count: u64 = 0;

        for face_index in 0..6 {
            for edge in ALL_EDGES {
                let mut edge_diff_sum: u64 = 0;
                let mut edge_max_diff: u16 = 0;
                for i in 0..resolution {
                    let ((px, py), (neighbor_face, nx, ny)) =
                        matched_seam_pixels(face_index, edge, i, resolution);

                    let dir_a = pixel_direction(face_index, px, py, resolution);
                    let dir_b = pixel_direction(neighbor_face, nx, ny, resolution);
                    let angle_rad = dot(dir_a, dir_b).clamp(-1.0, 1.0).acos();
                    assert!(
                        angle_rad <= max_expected_angle_rad,
                        "seam geometry mismatch on {} {edge:?} pixel {i}: angular separation {angle_rad:.6} rad exceeds {max_expected_angle_rad:.6} rad",
                        CANONICAL_FACE_NAMES[face_index]
                    );

                    let sample_a = faces[face_index]
                        .sample(px, py)
                        .expect("in-bounds face edge sample");
                    let sample_b = faces[neighbor_face]
                        .sample(nx, ny)
                        .expect("in-bounds neighbor edge sample");
                    let diff = sample_a.abs_diff(sample_b);
                    edge_diff_sum += u64::from(diff);
                    edge_max_diff = edge_max_diff.max(diff);
                    seam_diff_sum += u64::from(diff);
                    seam_pair_count += 1;
                }

                let edge_mean_diff = f64::from(u32::try_from(edge_diff_sum).unwrap_or(u32::MAX))
                    / f64::from(resolution);
                // A flipped or misoriented face edge jumps by thousands of feet on average;
                // a smooth continuous seam stays close to interior 1-pixel gradients.
                let max_allowed_edge_mean = (interior_mean_diff * 3.0).max(250.0);
                assert!(
                    edge_mean_diff <= max_allowed_edge_mean,
                    "seam discontinuity on {} {edge:?}: mean diff {edge_mean_diff:.2} ft \
                     (max {edge_max_diff} ft) exceeds threshold {max_allowed_edge_mean:.2} ft \
                     (interior mean {interior_mean_diff:.2} ft)",
                    CANONICAL_FACE_NAMES[face_index]
                );
            }
        }

        #[expect(
            clippy::cast_precision_loss,
            reason = "test diagnostic averages across 24 * 1024 seam pairs fit comfortably in f64"
        )]
        let overall_seam_mean_diff = seam_diff_sum as f64 / seam_pair_count as f64;
        assert!(
            overall_seam_mean_diff <= interior_mean_diff * 2.0,
            "overall seam mean diff ({overall_seam_mean_diff:.2} ft) should stay within 2x interior \
             1-pixel mean diff ({interior_mean_diff:.2} ft)"
        );
    }

    #[test]
    fn png_header_validation_rejects_non_png_and_non_16bit_grayscale() {
        let dummy_path = Path::new("synthetic_face.png");

        assert!(matches!(
            decode_grayscale_u16_png(b"not a png", dummy_path, "pos_x"),
            Err(PlanetIoError::InvalidPngHeader { .. })
        ));

        // Valid PNG signature + IHDR chunk declaring 8-bit grayscale (bit_depth=8, color_type=0).
        let mut eight_bit_header = Vec::from(PNG_SIGNATURE);
        eight_bit_header.extend_from_slice(&13_u32.to_be_bytes());
        eight_bit_header.extend_from_slice(b"IHDR");
        eight_bit_header.extend_from_slice(&4_u32.to_be_bytes()); // width
        eight_bit_header.extend_from_slice(&4_u32.to_be_bytes()); // height
        eight_bit_header.push(8); // bit_depth
        eight_bit_header.push(0); // color_type = grayscale
        eight_bit_header.extend_from_slice(&[0, 0, 0]); // compression, filter, interlace
        eight_bit_header.extend_from_slice(&[0, 0, 0, 0]); // placeholder CRC

        assert!(matches!(
            decode_grayscale_u16_png(&eight_bit_header, dummy_path, "pos_x"),
            Err(PlanetIoError::NotSixteenBitGrayscale {
                bit_depth: 8,
                color_type: 0,
                ..
            })
        ));

        // 16-bit RGB header (bit_depth=16, color_type=2) must also be rejected.
        let mut rgb16_header = eight_bit_header;
        rgb16_header[24] = 16;
        rgb16_header[25] = 2;

        assert!(matches!(
            decode_grayscale_u16_png(&rgb16_header, dummy_path, "pos_x"),
            Err(PlanetIoError::NotSixteenBitGrayscale {
                bit_depth: 16,
                color_type: 2,
                ..
            })
        ));
    }

    #[test]
    fn load_planet_faces_returns_read_file_error_for_missing_directory() {
        let missing = Path::new("this_directory_does_not_exist_for_planet_io_test");
        assert!(matches!(
            load_planet_faces(missing),
            Err(PlanetIoError::ReadFile { .. })
        ));
    }
}
