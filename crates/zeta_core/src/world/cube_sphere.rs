//! Six-face cube-sphere coordinate system, S2-style tangent projection, and exact
//! 6 × 4 edge-adjacency stepping.
//!
//! Rules are defined in `docs/architecture/world_geography.md` Section 1, and the face
//! basis table matches `tools/build_planet_faces.py`.

use std::f64::consts::{FRAC_PI_4, PI};
use std::fmt;

use serde::{Deserialize, Serialize};

/// Number of 5-foot tactical squares along each edge of a cube face (`2^21`).
pub const FACE_TILES: u32 = 1 << 21;

/// Number of 5-foot tactical squares along each edge of a streaming chunk.
pub const CHUNK_SIZE: u32 = 64;

/// Number of streaming chunks along each edge of a cube face (`FACE_TILES / CHUNK_SIZE`).
pub const FACE_CHUNKS: u32 = FACE_TILES / CHUNK_SIZE;

/// One of the 6 square faces of the planetary cube-sphere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CubeFace {
    /// Positive X face (`+X` outward normal).
    PosX,
    /// Negative X face (`-X` outward normal).
    NegX,
    /// Positive Y face (`+Y` north polar cap).
    PosY,
    /// Negative Y face (`-Y` south polar cap).
    NegY,
    /// Positive Z face (`+Z` prime-meridian face).
    PosZ,
    /// Negative Z face (`-Z` antimeridian face).
    NegZ,
}

/// Orthonormal integer 3D basis vectors for a single cube face.
///
/// Matches the basis table in `tools/build_planet_faces.py`:
/// `right_u × (-down_v) == forward`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FaceBasis {
    /// Outward unit normal (`+W`).
    pub forward: [i8; 3],
    /// Local east axis (`+U`, increasing tile `x`).
    pub right_u: [i8; 3],
    /// Local south axis (`+V`, increasing tile `y`).
    pub down_v: [i8; 3],
}

/// Cardinal edge of a cube face crossed when stepping out of `[0, N)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaceEdge {
    /// East edge (`x >= N`, `+U` direction).
    PosU,
    /// West edge (`x < 0`, `-U` direction).
    NegU,
    /// South edge (`y >= N`, `+V` direction).
    PosV,
    /// North edge (`y < 0`, `-V` direction).
    NegV,
}

impl FaceEdge {
    /// Canonical index in `0..4`.
    const fn index(self) -> usize {
        match self {
            Self::PosU => 0,
            Self::NegU => 1,
            Self::PosV => 2,
            Self::NegV => 3,
        }
    }
}

/// Result of crossing 1 face edge: the neighboring face and the local 90-degree rotation
/// mapping the source face's `(dx, dy)` step directions into the destination face's frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EdgeTransition {
    /// Neighboring cube face across the crossed edge.
    pub face: CubeFace,
    /// Rotation mapping source-face directions into destination-face directions.
    pub rotation: FaceRotation,
}

impl CubeFace {
    /// All 6 cube faces in canonical order (`pos_x`, `neg_x`, `pos_y`, `neg_y`, `pos_z`, `neg_z`).
    pub const ALL: [Self; 6] = [
        Self::PosX,
        Self::NegX,
        Self::PosY,
        Self::NegY,
        Self::PosZ,
        Self::NegZ,
    ];

    /// Canonical index in `0..6` matching [`CubeFace::ALL`].
    pub const fn index(self) -> usize {
        match self {
            Self::PosX => 0,
            Self::NegX => 1,
            Self::PosY => 2,
            Self::NegY => 3,
            Self::PosZ => 4,
            Self::NegZ => 5,
        }
    }

    /// Looks up a face by canonical index in `0..6`.
    pub const fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::PosX),
            1 => Some(Self::NegX),
            2 => Some(Self::PosY),
            3 => Some(Self::NegY),
            4 => Some(Self::PosZ),
            5 => Some(Self::NegZ),
            _ => None,
        }
    }

    /// Lowercase `snake_case` face identifier matching asset filenames (`"pos_x"`, etc.).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PosX => "pos_x",
            Self::NegX => "neg_x",
            Self::PosY => "pos_y",
            Self::NegY => "neg_y",
            Self::PosZ => "pos_z",
            Self::NegZ => "neg_z",
        }
    }

    /// Parses a lowercase `snake_case` face identifier (`"pos_x"`, etc.).
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "pos_x" => Some(Self::PosX),
            "neg_x" => Some(Self::NegX),
            "pos_y" => Some(Self::PosY),
            "neg_y" => Some(Self::NegY),
            "pos_z" => Some(Self::PosZ),
            "neg_z" => Some(Self::NegZ),
            _ => None,
        }
    }

    /// Returns the exact 3D basis vectors for this face matching `tools/build_planet_faces.py`.
    pub const fn basis(self) -> FaceBasis {
        match self {
            Self::PosX => FaceBasis {
                forward: [1, 0, 0],
                right_u: [0, 0, -1],
                down_v: [0, -1, 0],
            },
            Self::NegX => FaceBasis {
                forward: [-1, 0, 0],
                right_u: [0, 0, 1],
                down_v: [0, -1, 0],
            },
            Self::PosY => FaceBasis {
                forward: [0, 1, 0],
                right_u: [1, 0, 0],
                down_v: [0, 0, 1],
            },
            Self::NegY => FaceBasis {
                forward: [0, -1, 0],
                right_u: [1, 0, 0],
                down_v: [0, 0, -1],
            },
            Self::PosZ => FaceBasis {
                forward: [0, 0, 1],
                right_u: [1, 0, 0],
                down_v: [0, -1, 0],
            },
            Self::NegZ => FaceBasis {
                forward: [0, 0, -1],
                right_u: [-1, 0, 0],
                down_v: [0, -1, 0],
            },
        }
    }

    /// Looks up the exact neighbor face and frame rotation across `edge`.
    pub const fn neighbor(self, edge: FaceEdge) -> EdgeTransition {
        ADJACENCY[self.index()][edge.index()]
    }
}

impl fmt::Display for CubeFace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Clockwise 90-degree rotation in face grid coordinates (`+x` right, `+y` down).
///
/// Returned by [`step_tile`] and [`step_chunk`] to describe how directions in the origin
/// face's coordinate frame rotate into the destination face's coordinate frame.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum FaceRotation {
    /// No rotation (`0` degrees).
    #[default]
    Deg0,
    /// `90` degrees clockwise (`(+1, 0) -> (0, +1)`).
    Deg90,
    /// `180` degrees (`(+1, 0) -> (-1, 0)`).
    Deg180,
    /// `270` degrees clockwise (`(+1, 0) -> (0, -1)`).
    Deg270,
}

impl FaceRotation {
    /// Number of clockwise 90-degree turns in `0..4`.
    pub const fn quarter_turns(self) -> u8 {
        match self {
            Self::Deg0 => 0,
            Self::Deg90 => 1,
            Self::Deg180 => 2,
            Self::Deg270 => 3,
        }
    }

    /// Constructs a rotation from clockwise quarter turns modulo 4.
    pub const fn from_quarter_turns(turns: u8) -> Self {
        match turns % 4 {
            0 => Self::Deg0,
            1 => Self::Deg90,
            2 => Self::Deg180,
            _ => Self::Deg270,
        }
    }

    /// Composes `self` followed by `other`.
    #[must_use]
    pub const fn compose(self, other: Self) -> Self {
        Self::from_quarter_turns(self.quarter_turns() + other.quarter_turns())
    }

    /// Returns the inverse rotation such that `self.compose(self.inverse()) == Deg0`.
    #[must_use]
    pub const fn inverse(self) -> Self {
        Self::from_quarter_turns(4 - self.quarter_turns())
    }

    /// Rotates a 2D step vector `(dx, dy)` in face coordinates (`+x` east, `+y` south).
    pub const fn rotate_delta(self, delta_x: i64, delta_y: i64) -> (i64, i64) {
        match self {
            Self::Deg0 => (delta_x, delta_y),
            Self::Deg90 => (-delta_y, delta_x),
            Self::Deg180 => (-delta_x, -delta_y),
            Self::Deg270 => (delta_y, -delta_x),
        }
    }

    /// Rotates a grid cell `(x, y)` inside a square grid of side `grid_size`.
    pub const fn rotate_grid_coord(
        self,
        local_x: i128,
        local_y: i128,
        grid_size: i128,
    ) -> (i128, i128) {
        let max_index = grid_size - 1;
        match self {
            Self::Deg0 => (local_x, local_y),
            Self::Deg90 => (max_index - local_y, local_x),
            Self::Deg180 => (max_index - local_x, max_index - local_y),
            Self::Deg270 => (local_y, max_index - local_x),
        }
    }
}

/// Address of a single 5-foot tactical square on the cube-sphere.
///
/// Valid coordinates satisfy `x < FACE_TILES` and `y < FACE_TILES`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PlanetTile {
    /// Cube face containing this square.
    pub face: CubeFace,
    /// Column in `[0, FACE_TILES)` along the face's `+U` axis.
    pub x: u32,
    /// Row in `[0, FACE_TILES)` along the face's `+V` axis.
    pub y: u32,
}

impl PlanetTile {
    /// Creates a tile if `(x, y)` lies inside `[0, FACE_TILES) × [0, FACE_TILES)`.
    pub const fn new(face: CubeFace, x: u32, y: u32) -> Option<Self> {
        if x < FACE_TILES && y < FACE_TILES {
            Some(Self { face, x, y })
        } else {
            None
        }
    }

    /// Returns the streaming chunk containing this tile.
    pub const fn chunk_coord(self) -> ChunkCoord {
        ChunkCoord {
            face: self.face,
            cx: self.x / CHUNK_SIZE,
            cy: self.y / CHUNK_SIZE,
        }
    }

    /// Returns the `(local_x, local_y)` offset of this tile inside its `64 × 64` chunk.
    pub const fn local_in_chunk(self) -> (u32, u32) {
        (self.x % CHUNK_SIZE, self.y % CHUNK_SIZE)
    }

    /// Returns normalized face coordinates `(u, v)` in `[-1.0, 1.0]` at the center of this tile.
    pub fn center_uv(self) -> (f64, f64) {
        let scale = f64::from(FACE_TILES);
        let tile_u = (f64::from(self.x) + 0.5) / scale * 2.0 - 1.0;
        let tile_v = (f64::from(self.y) + 0.5) / scale * 2.0 - 1.0;
        (tile_u, tile_v)
    }
}

/// Address of a `64 × 64` streaming chunk on the cube-sphere.
///
/// Valid coordinates satisfy `cx < FACE_CHUNKS` and `cy < FACE_CHUNKS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ChunkCoord {
    /// Cube face containing this chunk.
    pub face: CubeFace,
    /// Chunk column in `[0, FACE_CHUNKS)`.
    pub cx: u32,
    /// Chunk row in `[0, FACE_CHUNKS)`.
    pub cy: u32,
}

impl ChunkCoord {
    /// Creates a chunk coordinate if `(cx, cy)` lies inside `[0, FACE_CHUNKS) × [0, FACE_CHUNKS)`.
    pub const fn new(face: CubeFace, cx: u32, cy: u32) -> Option<Self> {
        if cx < FACE_CHUNKS && cy < FACE_CHUNKS {
            Some(Self { face, cx, cy })
        } else {
            None
        }
    }

    /// Returns the top-left `(0, 0)` tile of this chunk.
    pub const fn origin_tile(self) -> PlanetTile {
        PlanetTile {
            face: self.face,
            x: self.cx.saturating_mul(CHUNK_SIZE),
            y: self.cy.saturating_mul(CHUNK_SIZE),
        }
    }

    /// Returns the tile at `(local_x, local_y)` inside this chunk if both offsets are `< CHUNK_SIZE`.
    pub const fn tile_at(self, local_x: u32, local_y: u32) -> Option<PlanetTile> {
        if local_x < CHUNK_SIZE && local_y < CHUNK_SIZE {
            PlanetTile::new(
                self.face,
                self.cx.saturating_mul(CHUNK_SIZE).saturating_add(local_x),
                self.cy.saturating_mul(CHUNK_SIZE).saturating_add(local_y),
            )
        } else {
            None
        }
    }
}

/// Projects normalized cube-face coordinates `(u, v)` in `[-1.0, 1.0]` to a unit-sphere
/// direction `[x, y, z]` using the S2-style tangent warp `tan(pi / 4 * u)`.
pub fn face_uv_to_unit_vec(face: CubeFace, face_u: f64, face_v: f64) -> [f64; 3] {
    let tan_u = (FRAC_PI_4 * face_u).tan();
    let tan_v = (FRAC_PI_4 * face_v).tan();
    let basis = face.basis();

    let world_x = f64::from(basis.forward[0])
        + tan_u * f64::from(basis.right_u[0])
        + tan_v * f64::from(basis.down_v[0]);
    let world_y = f64::from(basis.forward[1])
        + tan_u * f64::from(basis.right_u[1])
        + tan_v * f64::from(basis.down_v[1]);
    let world_z = f64::from(basis.forward[2])
        + tan_u * f64::from(basis.right_u[2])
        + tan_v * f64::from(basis.down_v[2]);

    let inv_len = 1.0 / (world_x * world_x + world_y * world_y + world_z * world_z).sqrt();
    [world_x * inv_len, world_y * inv_len, world_z * inv_len]
}

/// Projects any non-zero 3D direction vector onto its dominant cube face and normalized
/// S2-tangent coordinates `(face, u, v)` with `u, v` in `[-1.0, 1.0]`.
///
/// For the degenerate zero vector `[0.0, 0.0, 0.0]`, returns `(CubeFace::PosX, 0.0, 0.0)`.
pub fn unit_vec_to_face_uv(direction: [f64; 3]) -> (CubeFace, f64, f64) {
    let abs_x = direction[0].abs();
    let abs_y = direction[1].abs();
    let abs_z = direction[2].abs();

    if abs_x == 0.0 && abs_y == 0.0 && abs_z == 0.0 {
        return (CubeFace::PosX, 0.0, 0.0);
    }

    let face = if abs_x >= abs_y && abs_x >= abs_z {
        if direction[0] >= 0.0 {
            CubeFace::PosX
        } else {
            CubeFace::NegX
        }
    } else if abs_y >= abs_x && abs_y >= abs_z {
        if direction[1] >= 0.0 {
            CubeFace::PosY
        } else {
            CubeFace::NegY
        }
    } else if direction[2] >= 0.0 {
        CubeFace::PosZ
    } else {
        CubeFace::NegZ
    };

    let basis = face.basis();
    let w_comp = dot_f64_i8(direction, basis.forward);
    let tan_u = dot_f64_i8(direction, basis.right_u) / w_comp;
    let tan_v = dot_f64_i8(direction, basis.down_v) / w_comp;

    let inv_quarter_pi = 4.0 / PI;
    let face_u = (inv_quarter_pi * tan_u.atan()).clamp(-1.0, 1.0);
    let face_v = (inv_quarter_pi * tan_v.atan()).clamp(-1.0, 1.0);
    (face, face_u, face_v)
}

/// Projects the center of a [`PlanetTile`] onto the unit sphere.
pub fn tile_to_unit_vec(tile: PlanetTile) -> [f64; 3] {
    let (face_u, face_v) = tile.center_uv();
    face_uv_to_unit_vec(tile.face, face_u, face_v)
}

/// Maps a 3D unit-sphere direction to the containing [`PlanetTile`].
pub fn unit_vec_to_tile(direction: [f64; 3]) -> PlanetTile {
    let (face, face_u, face_v) = unit_vec_to_face_uv(direction);
    let scale = f64::from(FACE_TILES);
    let max_coord = f64::from(FACE_TILES - 1);
    let raw_x = (f64::midpoint(face_u, 1.0) * scale)
        .floor()
        .clamp(0.0, max_coord);
    let raw_y = (f64::midpoint(face_v, 1.0) * scale)
        .floor()
        .clamp(0.0, max_coord);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "raw_x and raw_y are floored and clamped into [0, FACE_TILES - 1]"
    )]
    PlanetTile {
        face,
        x: raw_x as u32,
        y: raw_y as u32,
    }
}

/// Steps from `tile` by `(delta_x, delta_y)` 5-foot squares across the cube-sphere,
/// seamlessly handling single-edge, multi-face, and diagonal corner crossings.
///
/// Returns `(destination, rotation)` where `rotation` transforms step directions from
/// `tile.face`'s local frame into `destination.face`'s local frame.
pub fn step_tile(tile: PlanetTile, delta_x: i64, delta_y: i64) -> (PlanetTile, FaceRotation) {
    let (face, x, y, rotation) = step_grid(
        tile.face,
        FACE_TILES,
        i128::from(tile.x) + i128::from(delta_x),
        i128::from(tile.y) + i128::from(delta_y),
    );
    (PlanetTile { face, x, y }, rotation)
}

/// Steps from `chunk` by `(col_step, row_step)` chunks across the cube-sphere,
/// seamlessly handling single-edge, multi-face, and diagonal corner crossings.
///
/// Returns `(destination, rotation)` where `rotation` transforms step directions from
/// `chunk.face`'s local frame into `destination.face`'s local frame.
pub fn step_chunk(chunk: ChunkCoord, col_step: i32, row_step: i32) -> (ChunkCoord, FaceRotation) {
    let (face, cx, cy, rotation) = step_grid(
        chunk.face,
        FACE_CHUNKS,
        i128::from(chunk.cx) + i128::from(col_step),
        i128::from(chunk.cy) + i128::from(row_step),
    );
    (ChunkCoord { face, cx, cy }, rotation)
}

/// Resolves an arbitrary signed integer coordinate `(coord_x, coord_y)` relative to a square
/// face grid of side `grid_size` (`>= 1`) into an in-bounds `(face, x, y, rotation)`.
///
/// Shared by tile stepping, chunk stepping, and cross-face heightmap sampling so all 3
/// resolutions obey the exact same cube-sphere topology.
pub(crate) fn step_grid(
    start_face: CubeFace,
    grid_size: u32,
    start_x: i128,
    start_y: i128,
) -> (CubeFace, u32, u32, FaceRotation) {
    let size = i128::from(grid_size.max(1));
    let circumference = size * 4;

    let mut face = start_face;
    let mut coord_x = reduce_circumference(start_x, circumference);
    let mut coord_y = reduce_circumference(start_y, circumference);
    let mut rotation = FaceRotation::Deg0;

    while coord_x < 0 || coord_x >= size || coord_y < 0 || coord_y >= size {
        let excess_x = axis_excess(coord_x, size);
        let excess_y = axis_excess(coord_y, size);

        let (edge, shifted_x, shifted_y) = if excess_x >= excess_y {
            if coord_x >= size {
                (FaceEdge::PosU, coord_x - size, coord_y)
            } else {
                (FaceEdge::NegU, coord_x + size, coord_y)
            }
        } else if coord_y >= size {
            (FaceEdge::PosV, coord_x, coord_y - size)
        } else {
            (FaceEdge::NegV, coord_x, coord_y + size)
        };

        let transition = face.neighbor(edge);
        let (next_x, next_y) = transition
            .rotation
            .rotate_grid_coord(shifted_x, shifted_y, size);
        face = transition.face;
        coord_x = next_x;
        coord_y = next_y;
        rotation = rotation.compose(transition.rotation);
    }

    let final_x = u32::try_from(coord_x).expect("step_grid loop terminates with 0 <= x < size");
    let final_y = u32::try_from(coord_y).expect("step_grid loop terminates with 0 <= y < size");
    (face, final_x, final_y, rotation)
}

/// Removes full 4-face planetary laps so arbitrarily large steps terminate in O(1) iterations
/// while leaving all normal steps (`|coord| < 4 * size`) untouched.
const fn reduce_circumference(coord: i128, circumference: i128) -> i128 {
    if coord >= circumference || coord <= -circumference {
        coord % circumference
    } else {
        coord
    }
}

const fn axis_excess(coord: i128, size: i128) -> i128 {
    if coord < 0 {
        -coord
    } else if coord >= size {
        coord - size + 1
    } else {
        0
    }
}

fn dot_f64_i8(vec: [f64; 3], axis: [i8; 3]) -> f64 {
    vec[0] * f64::from(axis[0]) + vec[1] * f64::from(axis[1]) + vec[2] * f64::from(axis[2])
}

const fn negate_i8(vec: [i8; 3]) -> [i8; 3] {
    [-vec[0], -vec[1], -vec[2]]
}

const fn dot_i8(lhs: [i8; 3], rhs: [i8; 3]) -> i8 {
    lhs[0] * rhs[0] + lhs[1] * rhs[1] + lhs[2] * rhs[2]
}

const fn face_from_forward(forward: [i8; 3]) -> CubeFace {
    match forward {
        [1, 0, 0] => CubeFace::PosX,
        [-1, 0, 0] => CubeFace::NegX,
        [0, 1, 0] => CubeFace::PosY,
        [0, -1, 0] => CubeFace::NegY,
        [0, 0, 1] => CubeFace::PosZ,
        [0, 0, -1] => CubeFace::NegZ,
        _ => panic!("cube face forward vector must be a signed cardinal unit axis"),
    }
}

const fn compute_edge_transition(face: CubeFace, edge: FaceEdge) -> EdgeTransition {
    let basis = face.basis();
    let (next_forward, folded_u, folded_v) = match edge {
        FaceEdge::PosU => (basis.right_u, negate_i8(basis.forward), basis.down_v),
        FaceEdge::NegU => (negate_i8(basis.right_u), basis.forward, basis.down_v),
        FaceEdge::PosV => (basis.down_v, basis.right_u, negate_i8(basis.forward)),
        FaceEdge::NegV => (negate_i8(basis.down_v), basis.right_u, basis.forward),
    };
    let next_face = face_from_forward(next_forward);
    let next_basis = next_face.basis();
    let u_in_next_x = dot_i8(folded_u, next_basis.right_u);
    let u_in_next_y = dot_i8(folded_u, next_basis.down_v);
    let rotation = match (u_in_next_x, u_in_next_y) {
        (1, 0) => FaceRotation::Deg0,
        (0, 1) => FaceRotation::Deg90,
        (-1, 0) => FaceRotation::Deg180,
        (0, -1) => FaceRotation::Deg270,
        _ => panic!("folded basis must align with destination face axes"),
    };
    let v_in_next_x = dot_i8(folded_v, next_basis.right_u) as i64;
    let v_in_next_y = dot_i8(folded_v, next_basis.down_v) as i64;
    let (expected_v_col, expected_v_row) = rotation.rotate_delta(0, 1);
    assert!(
        v_in_next_x == expected_v_col && v_in_next_y == expected_v_row,
        "folded down_v axis must match right-handed rotation"
    );
    EdgeTransition {
        face: next_face,
        rotation,
    }
}

const fn build_face_transitions(face: CubeFace) -> [EdgeTransition; 4] {
    [
        compute_edge_transition(face, FaceEdge::PosU),
        compute_edge_transition(face, FaceEdge::NegU),
        compute_edge_transition(face, FaceEdge::PosV),
        compute_edge_transition(face, FaceEdge::NegV),
    ]
}

/// Compile-time 6 × 4 adjacency table derived directly from the cube face basis vectors.
const ADJACENCY: [[EdgeTransition; 4]; 6] = [
    build_face_transitions(CubeFace::PosX),
    build_face_transitions(CubeFace::NegX),
    build_face_transitions(CubeFace::PosY),
    build_face_transitions(CubeFace::NegY),
    build_face_transitions(CubeFace::PosZ),
    build_face_transitions(CubeFace::NegZ),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn vec_distance(lhs: [f64; 3], rhs: [f64; 3]) -> f64 {
        let dx = lhs[0] - rhs[0];
        let dy = lhs[1] - rhs[1];
        let dz = lhs[2] - rhs[2];
        (dx * dx + dy * dy + dz * dz).sqrt()
    }

    #[test]
    fn face_uv_and_unit_vec_round_trip_across_all_faces_and_corners() {
        let interior_samples = [-0.999_9, -0.75, -0.25, 0.0, 0.25, 0.75, 0.999_9];
        for face in CubeFace::ALL {
            for &face_u in &interior_samples {
                for &face_v in &interior_samples {
                    let unit = face_uv_to_unit_vec(face, face_u, face_v);
                    let len = (unit[0] * unit[0] + unit[1] * unit[1] + unit[2] * unit[2]).sqrt();
                    assert!((len - 1.0).abs() < 1e-12, "not unit length: {len}");

                    let (round_face, round_u, round_v) = unit_vec_to_face_uv(unit);
                    assert_eq!(round_face, face, "face mismatch at ({face_u}, {face_v})");
                    assert!(
                        (round_u - face_u).abs() < 1e-12,
                        "u mismatch on {face}: {round_u} vs {face_u}"
                    );
                    assert!(
                        (round_v - face_v).abs() < 1e-12,
                        "v mismatch on {face}: {round_v} vs {face_v}"
                    );
                }
            }

            // Corners and edges belong to 2 or 3 faces simultaneously; verify the 3D point
            // round-trips to the exact same unit vector regardless of tie-breaking face.
            for &face_u in &[-1.0_f64, 0.0, 1.0] {
                for &face_v in &[-1.0_f64, 0.0, 1.0] {
                    let unit = face_uv_to_unit_vec(face, face_u, face_v);
                    let (round_face, round_u, round_v) = unit_vec_to_face_uv(unit);
                    let rebuilt = face_uv_to_unit_vec(round_face, round_u, round_v);
                    assert!(
                        vec_distance(unit, rebuilt) < 1e-12,
                        "corner/edge mismatch on {face} ({face_u}, {face_v}): {unit:?} vs {rebuilt:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn edge_crossing_round_trips_across_all_six_faces_and_four_edges() {
        let last = FACE_TILES - 1;
        let sample_positions = [0_u32, 1, 63, 64, 123_456, FACE_TILES / 2, last - 1, last];

        for face in CubeFace::ALL {
            for &pos in &sample_positions {
                let edge_cases = [
                    (
                        PlanetTile {
                            face,
                            x: last,
                            y: pos,
                        },
                        1_i64,
                        0_i64,
                    ),
                    (PlanetTile { face, x: 0, y: pos }, -1_i64, 0_i64),
                    (
                        PlanetTile {
                            face,
                            x: pos,
                            y: last,
                        },
                        0_i64,
                        1_i64,
                    ),
                    (PlanetTile { face, x: pos, y: 0 }, 0_i64, -1_i64),
                ];

                for (start, step_x, step_y) in edge_cases {
                    let (crossed, rotation) = step_tile(start, step_x, step_y);
                    assert_ne!(crossed.face, start.face, "must cross to neighbor face");

                    // 3D centers of adjacent tiles across a seam must be separated by ~1 tile width.
                    let start_vec = tile_to_unit_vec(start);
                    let crossed_vec = tile_to_unit_vec(crossed);
                    let arc_dist = vec_distance(start_vec, crossed_vec);
                    assert!(
                        arc_dist < 2.0e-6,
                        "seam tiles too far apart: {start:?} -> {crossed:?}, dist={arc_dist}"
                    );

                    // Step back using the returned rotation and confirm exact origin and inverse rotation.
                    let (return_col, return_row) = rotation.rotate_delta(-step_x, -step_y);
                    let (returned, back_rot) = step_tile(crossed, return_col, return_row);
                    assert_eq!(
                        returned, start,
                        "round-trip failed: {start:?} + ({step_x}, {step_y}) -> {crossed:?} ({rotation:?}) -> {returned:?}"
                    );
                    assert_eq!(rotation.compose(back_rot), FaceRotation::Deg0);
                }
            }
        }
    }

    #[test]
    fn chunk_crossing_round_trips_and_matches_tile_crossing() {
        let last_chunk = FACE_CHUNKS - 1;
        for face in CubeFace::ALL {
            let cases = [
                (
                    ChunkCoord {
                        face,
                        cx: last_chunk,
                        cy: 17,
                    },
                    1_i32,
                    0_i32,
                ),
                (
                    ChunkCoord {
                        face,
                        cx: 0,
                        cy: 17,
                    },
                    -1_i32,
                    0_i32,
                ),
                (
                    ChunkCoord {
                        face,
                        cx: 17,
                        cy: last_chunk,
                    },
                    0_i32,
                    1_i32,
                ),
                (
                    ChunkCoord {
                        face,
                        cx: 17,
                        cy: 0,
                    },
                    0_i32,
                    -1_i32,
                ),
            ];
            for (start, dcx, dcy) in cases {
                let (crossed, rotation) = step_chunk(start, dcx, dcy);
                assert_ne!(crossed.face, start.face);

                let (return_col, return_row) =
                    rotation.rotate_delta(-i64::from(dcx), -i64::from(dcy));
                let return_col_i32 = i32::try_from(return_col).expect("unit step fits in i32");
                let return_row_i32 = i32::try_from(return_row).expect("unit step fits in i32");
                let (returned, back_rot) = step_chunk(crossed, return_col_i32, return_row_i32);
                assert_eq!(returned, start);
                assert_eq!(rotation.compose(back_rot), FaceRotation::Deg0);

                // Crossing a chunk boundary by 1 chunk must agree with stepping its center tile by CHUNK_SIZE.
                let mid_tile = start
                    .tile_at(CHUNK_SIZE / 2, CHUNK_SIZE / 2)
                    .expect("valid local tile");
                let (crossed_tile, tile_rot) = step_tile(
                    mid_tile,
                    i64::from(dcx) * i64::from(CHUNK_SIZE),
                    i64::from(dcy) * i64::from(CHUNK_SIZE),
                );
                assert_eq!(crossed_tile.chunk_coord(), crossed);
                assert_eq!(tile_rot, rotation);
            }
        }
    }

    #[test]
    fn diagonal_corner_and_multi_face_steps_stay_in_bounds_and_loop_cleanly() {
        let last = FACE_TILES - 1;
        for face in CubeFace::ALL {
            for (x, y, dx, dy) in [
                (last, 0, 1_i64, -1_i64),
                (last, last, 1, 1),
                (0, 0, -1, -1),
                (0, last, -1, 1),
                (last, 0, 5, -3),
                (0, last, -7, 9),
            ] {
                let start = PlanetTile { face, x, y };
                let (dest, _rot) = step_tile(start, dx, dy);
                assert!(dest.x < FACE_TILES && dest.y < FACE_TILES);
                assert_ne!(dest.face, face);
            }

            // Full 4-face planetary lap returns to the exact starting tile with Deg0 rotation.
            let mid = PlanetTile {
                face,
                x: 123_456,
                y: 654_321,
            };
            let full_lap = i64::from(FACE_TILES) * 4;
            assert_eq!(step_tile(mid, full_lap, 0), (mid, FaceRotation::Deg0));
            assert_eq!(step_tile(mid, -full_lap, 0), (mid, FaceRotation::Deg0));
            assert_eq!(step_tile(mid, 0, full_lap), (mid, FaceRotation::Deg0));
            assert_eq!(step_tile(mid, 0, -full_lap), (mid, FaceRotation::Deg0));
            // Huge multi-lap step must terminate immediately and match modulo 4*FACE_TILES.
            let huge = full_lap * 1_000_000 + 42;
            assert_eq!(step_tile(mid, huge, 0), step_tile(mid, 42, 0));
        }
    }

    #[test]
    fn cube_face_serde_uses_snake_case_names() {
        for face in CubeFace::ALL {
            let json = serde_json::to_string(&face).expect("serialize face");
            assert_eq!(json, format!("\"{}\"", face.as_str()));
            let parsed: CubeFace = serde_json::from_str(&json).expect("deserialize face");
            assert_eq!(parsed, face);
            assert_eq!(CubeFace::from_name(face.as_str()), Some(face));
        }
    }
}
