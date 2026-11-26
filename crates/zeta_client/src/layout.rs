//! Where grid squares sit in the 2D world, shared by drawing the map, placing speech bubbles,
//! and turning mouse clicks into squares.
//!
//! Rules: `docs/architecture/technical_architecture.md` Section 5 and
//! `docs/architecture/world_geography.md` Section 1. With [`FloatingOrigin`] at `(0.0, 0.0)`,
//! square `(0, 0)` has its top-left corner at the world origin; `y` grows south on the grid
//! and down on screen, so world `y` is negative. Subtracting [`FloatingOrigin`] in `f64`
//! before converting to `f32` keeps sub-pixel precision across an entire `2^21 x 2^21` face.

use bevy::prelude::{Resource, Vec2};
use zeta_core::grid::{GridPos, MapBounds};

/// Continuous face tile coordinate currently mapped to Bevy world `(0.0, 0.0)`.
///
/// Defaults to `(0.0, 0.0)` so legacy origin-relative code and unit tests behave unchanged.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct FloatingOrigin {
    /// Continuous face `+x` tile coordinate mapped to Bevy `x = 0.0`.
    pub tile_x: f64,
    /// Continuous face `+y` tile coordinate mapped to Bevy `y = 0.0`.
    pub tile_y: f64,
}

impl FloatingOrigin {
    /// Anchors the floating visual origin at the center of `position`.
    #[must_use]
    pub fn from_tile_center(position: GridPos) -> Self {
        Self {
            tile_x: f64::from(position.x) + 0.5,
            tile_y: f64::from(position.y) + 0.5,
        }
    }
}

/// Bevy 2D world position of continuous face tile point `(tile_x, tile_y)` relative to `origin`.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    reason = "local offsets relative to FloatingOrigin stay near 0 in f32"
)]
pub fn tile_point_at(tile_x: f64, tile_y: f64, tile_size: f32, origin: FloatingOrigin) -> Vec2 {
    let scale = f64::from(tile_size);
    let world_x = (tile_x - origin.tile_x) * scale;
    let world_y = -(tile_y - origin.tile_y) * scale;
    Vec2::new(world_x as f32, world_y as f32)
}

/// World position of the center of `position` relative to `origin` (`f64` precision).
#[must_use]
pub fn square_center_at(position: GridPos, tile_size: f32, origin: FloatingOrigin) -> Vec2 {
    tile_point_at(
        f64::from(position.x) + 0.5,
        f64::from(position.y) + 0.5,
        tile_size,
        origin,
    )
}

/// World position of the center of `position` relative to the default `(0, 0)` origin.
#[must_use]
pub fn square_center(position: GridPos, tile_size: f32) -> Vec2 {
    square_center_at(position, tile_size, FloatingOrigin::default())
}

/// World position of the center of the whole map relative to the default `(0, 0)` origin.
#[must_use]
pub fn map_center(bounds: MapBounds, tile_size: f32) -> Vec2 {
    tile_point_at(
        f64::from(bounds.width) * 0.5,
        f64::from(bounds.height) * 0.5,
        tile_size,
        FloatingOrigin::default(),
    )
}

/// The square under a world position relative to `origin`, or `None` outside `bounds`.
#[must_use]
pub fn square_at_origin(
    world: Vec2,
    bounds: MapBounds,
    tile_size: f32,
    origin: FloatingOrigin,
) -> Option<GridPos> {
    if !world.x.is_finite() || !world.y.is_finite() || !tile_size.is_finite() || tile_size <= 0.0 {
        return None;
    }
    let scale = f64::from(tile_size);
    let column = world_to_grid_f64(origin.tile_x + f64::from(world.x) / scale, bounds.width)?;
    let row = world_to_grid_f64(origin.tile_y - f64::from(world.y) / scale, bounds.height)?;
    Some(GridPos::new(column, row))
}

/// The square under a world position relative to the default `(0, 0)` origin, or `None` outside
/// `bounds`.
#[must_use]
pub fn square_at(world: Vec2, bounds: MapBounds, tile_size: f32) -> Option<GridPos> {
    square_at_origin(world, bounds, tile_size, FloatingOrigin::default())
}

/// The grid index containing `squares`, or `None` when it is outside `0..limit` or not finite.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is floored and checked to lie in 0..limit first, so the cast is exact"
)]
fn world_to_grid_f64(squares: f64, limit: u32) -> Option<u32> {
    let floored = squares.floor();
    (floored >= 0.0 && floored < f64::from(limit)).then_some(floored as u32)
}

#[cfg(test)]
mod tests {
    use zeta_core::world::FACE_TILES;

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

    #[test]
    fn floating_origin_preserves_sub_pixel_precision_across_the_full_face() {
        let face_bounds = MapBounds {
            width: FACE_TILES,
            height: FACE_TILES,
        };
        for anchor in [
            GridPos::new(1_819_648, 318_464),
            GridPos::new(FACE_TILES - 2, FACE_TILES - 2),
        ] {
            let origin = FloatingOrigin::from_tile_center(anchor);
            assert_eq!(square_center_at(anchor, 32.0, origin), Vec2::ZERO);
            assert_eq!(
                square_at_origin(Vec2::ZERO, face_bounds, 32.0, origin),
                Some(anchor)
            );

            for dx in -5_i32..=1 {
                for dy in -5_i32..=1 {
                    let target = GridPos::new(
                        anchor.x.saturating_add_signed(dx),
                        anchor.y.saturating_add_signed(dy),
                    );
                    let world = square_center_at(target, 32.0, origin);
                    assert_eq!(
                        square_at_origin(world, face_bounds, 32.0, origin),
                        Some(target)
                    );
                }
            }
        }
    }
}
