//! Where grid squares sit in the 2D world, shared by drawing the map, placing speech bubbles,
//! and turning mouse clicks into squares.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 5. Square `(0, 0)` has its
//! top-left corner at the world origin; `y` grows south on the grid and down on screen, so
//! world `y` is negative.

use bevy::prelude::Vec2;
use zeta_core::grid::{GridPos, MapBounds};

/// World position of the center of `position`.
pub fn square_center(position: GridPos, tile_size: f32) -> Vec2 {
    Vec2::new(
        (grid_to_world(position.x) + 0.5) * tile_size,
        -(grid_to_world(position.y) + 0.5) * tile_size,
    )
}

/// World position of the center of the whole map.
pub fn map_center(bounds: MapBounds, tile_size: f32) -> Vec2 {
    Vec2::new(
        grid_to_world(bounds.width) * tile_size / 2.0,
        -grid_to_world(bounds.height) * tile_size / 2.0,
    )
}

/// The square under a world position, or `None` outside the map.
pub fn square_at(world: Vec2, bounds: MapBounds, tile_size: f32) -> Option<GridPos> {
    let column = world_to_grid(world.x / tile_size, bounds.width)?;
    let row = world_to_grid(-world.y / tile_size, bounds.height)?;
    Some(GridPos::new(column, row))
}

#[allow(
    clippy::cast_precision_loss,
    reason = "map sizes stay far below 2^24 squares, where every u32 is exact in f32"
)]
fn grid_to_world(index: u32) -> f32 {
    index as f32
}

/// The grid index containing `squares`, or `None` when it is outside `0..limit` or not a
/// number.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is floored and checked to lie in 0..limit first, so the cast is exact"
)]
fn world_to_grid(squares: f32, limit: u32) -> Option<u32> {
    let floored = squares.floor();
    (floored >= 0.0 && floored < grid_to_world(limit)).then_some(floored as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: MapBounds = MapBounds {
        width: 20,
        height: 10,
    };

    #[test]
    fn square_center_puts_the_origin_square_half_a_tile_from_the_world_origin() {
        assert_eq!(
            square_center(GridPos::new(0, 0), 32.0),
            Vec2::new(16.0, -16.0)
        );
        assert_eq!(
            square_center(GridPos::new(2, 3), 10.0),
            Vec2::new(25.0, -35.0)
        );
    }

    #[test]
    fn square_at_inverts_square_center_for_every_square() {
        for x in 0..BOUNDS.width {
            for y in 0..BOUNDS.height {
                let position = GridPos::new(x, y);
                let center = square_center(position, 32.0);
                assert_eq!(square_at(center, BOUNDS, 32.0), Some(position));
            }
        }
    }

    #[test]
    fn square_at_rejects_points_outside_the_map() {
        assert_eq!(square_at(Vec2::new(-1.0, -5.0), BOUNDS, 32.0), None);
        assert_eq!(square_at(Vec2::new(5.0, 1.0), BOUNDS, 32.0), None);
        assert_eq!(square_at(Vec2::new(640.0, -5.0), BOUNDS, 32.0), None);
        assert_eq!(square_at(Vec2::new(5.0, -320.0), BOUNDS, 32.0), None);
        assert_eq!(square_at(Vec2::new(f32::NAN, -5.0), BOUNDS, 32.0), None);
    }

    #[test]
    fn map_center_is_the_middle_of_the_map() {
        assert_eq!(map_center(BOUNDS, 32.0), Vec2::new(320.0, -160.0));
    }
}
