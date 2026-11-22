//! Grid movement rules: paths over passable squares inside the map bounds.
//!
//! Rules: `docs/prototypes/realism_prototype.md` Section 2 (direct movement) on the grid of
//! `docs/architecture/technical_architecture.md` Section 1. A diagonal step costs 1 square,
//! matching [`GridPos::grid_distance`]. The search is breadth-first with a fixed neighbor
//! order, so the same map always gives the same path.

use std::collections::{BTreeMap, VecDeque};

use thiserror::Error;

use crate::grid::{GridPos, MapBounds};

/// Why no path to a square exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum PathError {
    /// The target square is outside the map.
    #[error("square ({x}, {y}) is outside the map")]
    OutsideMap {
        /// Column of the rejected square.
        x: u32,
        /// Row of the rejected square.
        y: u32,
    },
    /// Something impassable stands on the target square.
    #[error("square ({x}, {y}) is blocked")]
    Blocked {
        /// Column of the rejected square.
        x: u32,
        /// Row of the rejected square.
        y: u32,
    },
    /// No path of at most `max_squares` steps reaches the target square.
    #[error("no path of at most {max_squares} squares reaches the target")]
    Unreachable {
        /// The longest path allowed.
        max_squares: u32,
    },
}

/// Neighbor order: orthogonal steps first, so straight paths are preferred over zigzags.
const STEPS: [(i64, i64); 8] = [
    (0, -1),
    (1, 0),
    (0, 1),
    (-1, 0),
    (1, -1),
    (1, 1),
    (-1, 1),
    (-1, -1),
];

/// Finds the shortest path from `start` to `target` over squares inside `bounds` that
/// `is_blocked` does not report, using at most `max_squares` steps.
///
/// The returned path holds every square stepped onto, ending with `target`; it is empty
/// when `start` equals `target`. The start square itself is never checked, so a mover
/// standing on its own impassable square can still leave it.
///
/// # Errors
///
/// Returns [`PathError`] when the target is outside the map, blocked, or out of reach.
pub fn find_path(
    start: GridPos,
    target: GridPos,
    bounds: MapBounds,
    max_squares: u32,
    is_blocked: impl Fn(GridPos) -> bool,
) -> Result<Vec<GridPos>, PathError> {
    if !bounds.contains(target.x, target.y) {
        return Err(PathError::OutsideMap {
            x: target.x,
            y: target.y,
        });
    }
    if start == target {
        return Ok(Vec::new());
    }
    if is_blocked(target) {
        return Err(PathError::Blocked {
            x: target.x,
            y: target.y,
        });
    }
    let mut came_from: BTreeMap<GridPos, GridPos> = BTreeMap::new();
    let mut frontier = VecDeque::from([(start, 0_u32)]);
    while let Some((square, steps)) = frontier.pop_front() {
        if steps >= max_squares {
            continue;
        }
        for next in neighbors(square, bounds) {
            if next == start || came_from.contains_key(&next) || is_blocked(next) {
                continue;
            }
            came_from.insert(next, square);
            if next == target {
                return Ok(trace_back(start, target, &came_from));
            }
            frontier.push_back((next, steps + 1));
        }
    }
    Err(PathError::Unreachable { max_squares })
}

/// The squares around `square` that lie inside `bounds`, in [`STEPS`] order.
fn neighbors(square: GridPos, bounds: MapBounds) -> impl Iterator<Item = GridPos> {
    STEPS.iter().filter_map(move |&(delta_x, delta_y)| {
        let x = u32::try_from(i64::from(square.x) + delta_x).ok()?;
        let y = u32::try_from(i64::from(square.y) + delta_y).ok()?;
        bounds.contains(x, y).then_some(GridPos::new(x, y))
    })
}

fn trace_back(
    start: GridPos,
    target: GridPos,
    came_from: &BTreeMap<GridPos, GridPos>,
) -> Vec<GridPos> {
    let mut path = vec![target];
    let mut square = target;
    while let Some(&previous) = came_from.get(&square) {
        if previous == start {
            break;
        }
        path.push(previous);
        square = previous;
    }
    path.reverse();
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: MapBounds = MapBounds {
        width: 10,
        height: 10,
    };

    fn open(_: GridPos) -> bool {
        false
    }

    #[test]
    fn a_diagonal_path_costs_one_square_per_step() {
        let path = find_path(GridPos::new(0, 0), GridPos::new(3, 3), BOUNDS, 3, open)
            .expect("diagonal is 3 squares");
        assert_eq!(
            path,
            [GridPos::new(1, 1), GridPos::new(2, 2), GridPos::new(3, 3)]
        );
    }

    #[test]
    fn standing_still_is_an_empty_path() {
        let square = GridPos::new(4, 4);
        assert_eq!(find_path(square, square, BOUNDS, 0, open), Ok(Vec::new()));
    }

    #[test]
    fn a_wall_is_walked_around_through_its_gap() {
        // A wall along x = 5 with a gap at y = 9.
        let wall = |square: GridPos| square.x == 5 && square.y != 9;
        let path = find_path(GridPos::new(4, 5), GridPos::new(6, 5), BOUNDS, 20, wall)
            .expect("path through the gap");
        assert!(path.contains(&GridPos::new(5, 9)));
        assert_eq!(path.last(), Some(&GridPos::new(6, 5)));
        assert!(path.iter().all(|&square| !wall(square)));
    }

    #[test]
    fn a_path_longer_than_the_limit_is_unreachable() {
        assert_eq!(
            find_path(GridPos::new(0, 0), GridPos::new(9, 0), BOUNDS, 8, open),
            Err(PathError::Unreachable { max_squares: 8 })
        );
    }

    #[test]
    fn a_sealed_square_is_unreachable() {
        let ring = |square: GridPos| square.x.abs_diff(5) <= 1 && square.y.abs_diff(5) <= 1;
        let sealed = |square: GridPos| ring(square) && square != GridPos::new(5, 5);
        assert_eq!(
            find_path(GridPos::new(0, 0), GridPos::new(5, 5), BOUNDS, 50, sealed),
            Err(PathError::Unreachable { max_squares: 50 })
        );
    }

    #[test]
    fn blocked_and_outside_targets_are_rejected_before_searching() {
        let start = GridPos::new(0, 0);
        assert_eq!(
            find_path(start, GridPos::new(10, 0), BOUNDS, 50, open),
            Err(PathError::OutsideMap { x: 10, y: 0 })
        );
        let blocked = |square: GridPos| square == GridPos::new(1, 0);
        assert_eq!(
            find_path(start, GridPos::new(1, 0), BOUNDS, 50, blocked),
            Err(PathError::Blocked { x: 1, y: 0 })
        );
    }

    #[test]
    fn the_same_map_always_gives_the_same_path() {
        let first = find_path(GridPos::new(0, 0), GridPos::new(2, 0), BOUNDS, 5, open);
        let second = find_path(GridPos::new(0, 0), GridPos::new(2, 0), BOUNDS, 5, open);
        assert_eq!(first, second);
        assert_eq!(first, Ok(vec![GridPos::new(1, 0), GridPos::new(2, 0)]));
    }
}
