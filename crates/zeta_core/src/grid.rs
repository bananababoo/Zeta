//! Grid positions, map bounds, distance, and line of sight on the 5-foot grid.
//!
//! `GridPos` is a fixed core type named in `docs/architecture/technical_architecture.md`
//! Section 1.

use serde::{Deserialize, Serialize};

/// A square on the 5-foot grid. `x` grows to the east and `y` grows to the south.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Component))]
pub struct GridPos {
    /// Column, from 0 at the west edge.
    pub x: u32,
    /// Row, from 0 at the north edge.
    pub y: u32,
}

impl GridPos {
    /// Creates a position.
    pub const fn new(x: u32, y: u32) -> Self {
        Self { x, y }
    }

    /// Distance in squares where a diagonal step counts as 1 square, the default 5e grid rule.
    pub const fn grid_distance(self, other: Self) -> u32 {
        let dx = self.x.abs_diff(other.x);
        let dy = self.y.abs_diff(other.y);
        if dx > dy { dx } else { dy }
    }
}

/// The size of a map in squares. Valid positions run from `(0, 0)` to
/// `(width - 1, height - 1)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Resource))]
pub struct MapBounds {
    /// Number of columns.
    pub width: u32,
    /// Number of rows.
    pub height: u32,
}

impl MapBounds {
    /// Whether the square `(x, y)` is inside the map.
    pub const fn contains(self, x: u32, y: u32) -> bool {
        x < self.width && y < self.height
    }
}

/// Whether `from` can see `to`, given which squares block sight.
///
/// The endpoints never block. A line is traced in both directions and sight exists if
/// either is clear, so the result is the same whichever end looks. What blocks sight is
/// decided by the caller, which keeps this function free of any entity type.
pub fn has_line_of_sight(
    from: GridPos,
    to: GridPos,
    blocks_sight: impl Fn(GridPos) -> bool,
) -> bool {
    line_is_clear(from, to, &blocks_sight) || line_is_clear(to, from, &blocks_sight)
}

/// Traces a Bresenham line from `from` to `to` and checks every square strictly between.
fn line_is_clear(from: GridPos, to: GridPos, blocks_sight: &impl Fn(GridPos) -> bool) -> bool {
    let (mut x, mut y) = (i64::from(from.x), i64::from(from.y));
    let (end_x, end_y) = (i64::from(to.x), i64::from(to.y));
    let delta_x = (end_x - x).abs();
    let delta_y = -(end_y - y).abs();
    let step_x = if x < end_x { 1 } else { -1 };
    let step_y = if y < end_y { 1 } else { -1 };
    let mut error = delta_x + delta_y;
    loop {
        if (x, y) == (end_x, end_y) {
            return true;
        }
        let doubled = 2 * error;
        if doubled >= delta_y {
            error += delta_y;
            x += step_x;
        }
        if doubled <= delta_x {
            error += delta_x;
            y += step_y;
        }
        if (x, y) == (end_x, end_y) {
            return true;
        }
        // Every square strictly between the endpoints lies inside their bounding box, so
        // both coordinates are between 2 valid u32 values.
        let square = GridPos::new(
            u32::try_from(x).expect("line stays between u32 endpoints"),
            u32::try_from(y).expect("line stays between u32 endpoints"),
        );
        if blocks_sight(square) {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagonal_steps_count_as_one_square() {
        assert_eq!(GridPos::new(0, 0).grid_distance(GridPos::new(3, 5)), 5);
        assert_eq!(GridPos::new(4, 4).grid_distance(GridPos::new(4, 4)), 0);
    }

    #[test]
    fn bounds_exclude_the_width_and_height() {
        let bounds = MapBounds {
            width: 10,
            height: 5,
        };
        assert!(bounds.contains(9, 4));
        assert!(!bounds.contains(10, 0));
        assert!(!bounds.contains(0, 5));
    }

    #[test]
    fn open_ground_has_line_of_sight() {
        assert!(has_line_of_sight(
            GridPos::new(0, 0),
            GridPos::new(7, 3),
            |_| false
        ));
    }

    #[test]
    fn a_wall_between_blocks_sight() {
        let wall = |square: GridPos| square.x == 3;
        assert!(!has_line_of_sight(
            GridPos::new(0, 2),
            GridPos::new(6, 2),
            wall
        ));
    }

    #[test]
    fn endpoints_never_block_their_own_sight() {
        let everything_blocks = |_| true;
        assert!(has_line_of_sight(
            GridPos::new(2, 2),
            GridPos::new(3, 3),
            everything_blocks
        ));
    }

    #[test]
    fn line_of_sight_is_symmetric() {
        let pillar = |square: GridPos| square == GridPos::new(2, 1);
        for (a, b) in [
            (GridPos::new(0, 0), GridPos::new(5, 2)),
            (GridPos::new(1, 0), GridPos::new(4, 3)),
            (GridPos::new(0, 1), GridPos::new(5, 1)),
        ] {
            assert_eq!(
                has_line_of_sight(a, b, pillar),
                has_line_of_sight(b, a, pillar),
                "{a:?} and {b:?}"
            );
        }
    }
}
