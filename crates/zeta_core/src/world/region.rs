//! Authored map region footprints and exact non-overlap validation in face 5-foot tile
//! coordinates.
//!
//! Rules are defined in `docs/architecture/world_geography.md` Section 4. Adjacent regions
//! may share boundary edges or vertices cleanly; only positive-area interior intersection
//! counts as an overlap.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::cube_sphere::CubeFace;

/// Explicit polygon footprint claimed by 1 authored map region on a cube face.
///
/// Coordinates are 5-foot squares in the face's local grid (`[x, y]`). Polygons may be
/// passed either open (`[v0, v1, ..., vn]`) or closed (`[v0, v1, ..., vn, v0]`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionFootprint {
    /// Unique identifier of the authored region.
    pub id: String,
    /// Cube face containing this region's footprint.
    pub face: CubeFace,
    /// Simple polygon boundary vertices in face 5-foot tile coordinates.
    pub polygon: Vec<[i64; 2]>,
}

/// Collision report when 2 authored region footprints have overlapping interiors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionOverlap {
    /// First colliding region identifier.
    pub region_a: String,
    /// Second colliding region identifier.
    pub region_b: String,
    /// Cube face on which the overlap occurred.
    pub face: CubeFace,
    /// Representative face 5-foot tile coordinate inside or on the overlap.
    pub witness: [i64; 2],
}

impl fmt::Display for RegionOverlap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "regions '{}' and '{}' overlap on face {} near ({}, {})",
            self.region_a, self.region_b, self.face, self.witness[0], self.witness[1]
        )
    }
}

/// Returns `true` if `pt` lies inside or on the boundary of simple polygon `poly`.
///
/// Uses exact `i128` arithmetic (no floating-point rounding) and supports arbitrary
/// convex and concave polygons, whether passed open or closed.
pub fn point_in_polygon(point: [i64; 2], poly: &[[i64; 2]]) -> bool {
    let ring = normalize_ring(poly);
    if ring.len() < 3 {
        return false;
    }
    matches!(
        classify_rational_point(i128::from(point[0]), i128::from(point[1]), 1, &ring),
        PointRelation::Inside | PointRelation::Boundary
    )
}

/// Checks whether the **interiors** of 2 simple polygons intersect.
///
/// Touching strictly along a shared boundary edge or at shared vertices returns `None` so
/// neighboring regions can abut cleanly without gaps. Returns `Some(witness)` with a
/// representative face coordinate if the interiors overlap (crossing edges, partial
/// intrusion, full containment, or identical footprints).
pub fn polygons_overlap(poly_a: &[[i64; 2]], poly_b: &[[i64; 2]]) -> Option<[i64; 2]> {
    let ring_a = normalize_ring(poly_a);
    let ring_b = normalize_ring(poly_b);
    if ring_a.len() < 3 || ring_b.len() < 3 {
        return None;
    }
    if !aabb_strictly_overlaps(&ring_a, &ring_b) {
        return None;
    }

    // 1. Proper (strict) edge crossings between any edge of A and any edge of B.
    if let Some(witness) = find_proper_edge_crossing(&ring_a, &ring_b) {
        return Some(witness);
    }

    // 2. Any vertex or split-edge midpoint of A strictly inside B, or vice versa.
    if let Some(witness) = find_boundary_point_strictly_inside(&ring_a, &ring_b) {
        return Some(witness);
    }
    if let Some(witness) = find_boundary_point_strictly_inside(&ring_b, &ring_a) {
        return Some(witness);
    }

    // 3. Connectedness fallback: if no boundary point of either polygon enters the other's
    // interior, their interiors are either identical (or nested on shared boundaries) or
    // completely disjoint. Testing 1 guaranteed strictly-interior point of each decides it.
    if let Some((num_x, num_y, den)) = find_strictly_interior_point(&ring_a)
        && classify_rational_point(num_x, num_y, den, &ring_b) == PointRelation::Inside
    {
        return Some(round_rational_point(num_x, num_y, den));
    }
    if let Some((num_x, num_y, den)) = find_strictly_interior_point(&ring_b)
        && classify_rational_point(num_x, num_y, den, &ring_a) == PointRelation::Inside
    {
        return Some(round_rational_point(num_x, num_y, den));
    }

    None
}

/// Validates that no 2 authored region footprints on the same face have overlapping interiors.
///
/// # Errors
/// Returns `Err(overlaps)` listing every colliding region pair with a witness coordinate.
pub fn validate_non_overlapping(regions: &[RegionFootprint]) -> Result<(), Vec<RegionOverlap>> {
    let mut overlaps = Vec::new();
    for index_a in 0..regions.len() {
        let region_a = &regions[index_a];
        for region_b in &regions[(index_a + 1)..] {
            if region_a.face != region_b.face {
                continue;
            }
            if let Some(witness) = polygons_overlap(&region_a.polygon, &region_b.polygon) {
                overlaps.push(RegionOverlap {
                    region_a: region_a.id.clone(),
                    region_b: region_b.id.clone(),
                    face: region_a.face,
                    witness,
                });
            }
        }
    }

    if overlaps.is_empty() {
        Ok(())
    } else {
        Err(overlaps)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointRelation {
    Outside,
    Boundary,
    Inside,
}

/// Strips consecutive duplicate vertices, a closing repeat of vertex 0, and redundant
/// 180-degree collinear vertices along straight edges.
fn normalize_ring(poly: &[[i64; 2]]) -> Vec<[i64; 2]> {
    let mut deduped: Vec<[i64; 2]> = Vec::with_capacity(poly.len());
    for &vertex in poly {
        if deduped.last().copied() != Some(vertex) {
            deduped.push(vertex);
        }
    }
    if deduped.len() > 1 && deduped.first() == deduped.last() {
        deduped.pop();
    }
    if deduped.len() < 3 {
        return deduped;
    }

    // Remove redundant collinear vertices on straight edges so every remaining vertex is a
    // genuine corner; this keeps ear/interior-point construction non-degenerate.
    let mut simplified = deduped;
    loop {
        let count = simplified.len();
        if count < 3 {
            return simplified;
        }
        let mut next = Vec::with_capacity(count);
        for idx in 0..count {
            let prev = simplified[(idx + count - 1) % count];
            let curr = simplified[idx];
            let succ = simplified[(idx + 1) % count];
            if orient_i128(prev, curr, succ) == 0 && point_in_bbox_i64(curr, prev, succ) {
                continue;
            }
            next.push(curr);
        }
        if next.len() == count {
            return next;
        }
        simplified = next;
    }
}

fn aabb_strictly_overlaps(ring_a: &[[i64; 2]], ring_b: &[[i64; 2]]) -> bool {
    let first = ring_bbox(ring_a);
    let second = ring_bbox(ring_b);
    first.2 > second.0 && second.2 > first.0 && first.3 > second.1 && second.3 > first.1
}

fn ring_bbox(ring: &[[i64; 2]]) -> (i64, i64, i64, i64) {
    let mut min_x = i64::MAX;
    let mut min_y = i64::MAX;
    let mut max_x = i64::MIN;
    let mut max_y = i64::MIN;
    for &[coord_x, coord_y] in ring {
        min_x = min_x.min(coord_x);
        min_y = min_y.min(coord_y);
        max_x = max_x.max(coord_x);
        max_y = max_y.max(coord_y);
    }
    (min_x, min_y, max_x, max_y)
}

/// Classifies exact rational point `(num_x / den, num_y / den)` (with `den > 0`) against `ring`.
fn classify_rational_point(
    num_x: i128,
    num_y: i128,
    den: i128,
    ring: &[[i64; 2]],
) -> PointRelation {
    let mut inside = false;
    let count = ring.len();

    for idx in 0..count {
        let start = ring[idx];
        let end = ring[(idx + 1) % count];
        let start_x = i128::from(start[0]) * den;
        let start_y = i128::from(start[1]) * den;
        let end_x = i128::from(end[0]) * den;
        let end_y = i128::from(end[1]) * den;

        let cross = (end_x - start_x) * (num_y - start_y) - (end_y - start_y) * (num_x - start_x);
        if cross == 0
            && num_x >= start_x.min(end_x)
            && num_x <= start_x.max(end_x)
            && num_y >= start_y.min(end_y)
            && num_y <= start_y.max(end_y)
        {
            return PointRelation::Boundary;
        }

        if start_y <= num_y && end_y > num_y {
            if cross > 0 {
                inside = !inside;
            }
        } else if end_y <= num_y && start_y > num_y && cross < 0 {
            inside = !inside;
        }
    }

    if inside {
        PointRelation::Inside
    } else {
        PointRelation::Outside
    }
}

fn find_proper_edge_crossing(ring_a: &[[i64; 2]], ring_b: &[[i64; 2]]) -> Option<[i64; 2]> {
    let count_a = ring_a.len();
    let count_b = ring_b.len();

    for idx_a in 0..count_a {
        let first_start = ring_a[idx_a];
        let first_end = ring_a[(idx_a + 1) % count_a];

        for idx_b in 0..count_b {
            let other_start = ring_b[idx_b];
            let other_end = ring_b[(idx_b + 1) % count_b];

            let side_other_start = orient_i128(first_start, first_end, other_start);
            let side_other_end = orient_i128(first_start, first_end, other_end);
            if !opposite_nonzero_signs(side_other_start, side_other_end) {
                continue;
            }

            let side_first_start = orient_i128(other_start, other_end, first_start);
            let side_first_end = orient_i128(other_start, other_end, first_end);
            if !opposite_nonzero_signs(side_first_start, side_first_end) {
                continue;
            }

            // Compute exact crossing point along first_start -> first_end:
            // P = first_start + (first_end - first_start) * (side_first_start / (side_first_start - side_first_end))
            let denom = side_first_start - side_first_end;
            let delta_x = i128::from(first_end[0]) - i128::from(first_start[0]);
            let delta_y = i128::from(first_end[1]) - i128::from(first_start[1]);
            let hit_x = i128::from(first_start[0]) * denom + delta_x * side_first_start;
            let hit_y = i128::from(first_start[1]) * denom + delta_y * side_first_start;
            return Some(round_rational_point(hit_x, hit_y, denom));
        }
    }
    None
}

/// Checks vertices of `source` and midpoints of sub-segments formed by splitting `source`
/// edges at vertices of `target` lying on them.
fn find_boundary_point_strictly_inside(
    source: &[[i64; 2]],
    target: &[[i64; 2]],
) -> Option<[i64; 2]> {
    for &vertex in source {
        if classify_rational_point(i128::from(vertex[0]), i128::from(vertex[1]), 1, target)
            == PointRelation::Inside
        {
            return Some(vertex);
        }
    }

    let count = source.len();
    let mut split_points: Vec<[i64; 2]> = Vec::new();

    for idx in 0..count {
        let start = source[idx];
        let end = source[(idx + 1) % count];
        split_points.clear();
        split_points.push(start);
        split_points.push(end);

        for &candidate in target {
            if candidate != start
                && candidate != end
                && orient_i128(start, end, candidate) == 0
                && point_in_bbox_i64(candidate, start, end)
            {
                split_points.push(candidate);
            }
        }

        split_points.sort_by_key(|&pt| {
            (i128::from(pt[0]) - i128::from(start[0])).abs()
                + (i128::from(pt[1]) - i128::from(start[1])).abs()
        });
        split_points.dedup();

        for window in split_points.windows(2) {
            let sub_start = window[0];
            let sub_end = window[1];
            let mid_num_x = i128::from(sub_start[0]) + i128::from(sub_end[0]);
            let mid_num_y = i128::from(sub_start[1]) + i128::from(sub_end[1]);
            if classify_rational_point(mid_num_x, mid_num_y, 2, target) == PointRelation::Inside {
                return Some(round_rational_point(mid_num_x, mid_num_y, 2));
            }
        }
    }

    None
}

/// Finds a guaranteed strictly-interior rational point `(num_x, num_y, den)` of a simple
/// normalized polygon using the Two-Ears theorem construction at an extremal convex vertex.
fn find_strictly_interior_point(ring: &[[i64; 2]]) -> Option<(i128, i128, i128)> {
    let count = ring.len();
    if count < 3 {
        return None;
    }

    // Lexicographically smallest (x, then y) vertex is always strictly convex.
    let min_idx = (0..count).min_by_key(|&idx| (ring[idx][0], ring[idx][1]))?;
    let prev = ring[(min_idx + count - 1) % count];
    let apex = ring[min_idx];
    let next = ring[(min_idx + 1) % count];

    let tri_orient = orient_i128(prev, apex, next);
    if tri_orient == 0 {
        return None;
    }

    // Look for any other polygon vertex lying inside or on triangle (prev, apex, next).
    let mut best_reflex: Option<([i64; 2], i128)> = None;
    for (idx, &candidate) in ring.iter().enumerate() {
        if idx == min_idx || idx == (min_idx + count - 1) % count || idx == (min_idx + 1) % count {
            continue;
        }
        if point_in_triangle_inclusive(candidate, prev, apex, next, tri_orient) {
            // Distance from opposite base `prev -> next` is proportional to |orient(prev, next, candidate)|.
            let altitude_metric = orient_i128(prev, next, candidate).abs();
            let replace = match best_reflex {
                None => true,
                Some((_, best_metric)) => altitude_metric > best_metric,
            };
            if replace {
                best_reflex = Some((candidate, altitude_metric));
            }
        }
    }

    if let Some((reflex, _)) = best_reflex {
        // The open diagonal apex -> reflex lies strictly inside the polygon.
        Some((
            i128::from(apex[0]) + i128::from(reflex[0]),
            i128::from(apex[1]) + i128::from(reflex[1]),
            2,
        ))
    } else {
        // Triangle (prev, apex, next) is an ear; its centroid is strictly inside the polygon.
        Some((
            i128::from(prev[0]) + i128::from(apex[0]) + i128::from(next[0]),
            i128::from(prev[1]) + i128::from(apex[1]) + i128::from(next[1]),
            3,
        ))
    }
}

fn point_in_triangle_inclusive(
    point: [i64; 2],
    tri_a: [i64; 2],
    tri_b: [i64; 2],
    tri_c: [i64; 2],
    tri_orient: i128,
) -> bool {
    let side_ab = orient_i128(tri_a, tri_b, point);
    let side_bc = orient_i128(tri_b, tri_c, point);
    let side_ca = orient_i128(tri_c, tri_a, point);
    if tri_orient > 0 {
        side_ab >= 0 && side_bc >= 0 && side_ca >= 0
    } else {
        side_ab <= 0 && side_bc <= 0 && side_ca <= 0
    }
}

fn orient_i128(origin: [i64; 2], first: [i64; 2], second: [i64; 2]) -> i128 {
    let first_x = i128::from(first[0]) - i128::from(origin[0]);
    let first_y = i128::from(first[1]) - i128::from(origin[1]);
    let second_x = i128::from(second[0]) - i128::from(origin[0]);
    let second_y = i128::from(second[1]) - i128::from(origin[1]);
    first_x * second_y - first_y * second_x
}

const fn opposite_nonzero_signs(lhs: i128, rhs: i128) -> bool {
    (lhs > 0 && rhs < 0) || (lhs < 0 && rhs > 0)
}

fn point_in_bbox_i64(point: [i64; 2], start: [i64; 2], end: [i64; 2]) -> bool {
    point[0] >= start[0].min(end[0])
        && point[0] <= start[0].max(end[0])
        && point[1] >= start[1].min(end[1])
        && point[1] <= start[1].max(end[1])
}

fn round_rational_point(num_x: i128, num_y: i128, den: i128) -> [i64; 2] {
    [round_div_i128(num_x, den), round_div_i128(num_y, den)]
}

fn round_div_i128(numerator: i128, denominator: i128) -> i64 {
    let (num, den) = if denominator < 0 {
        (-numerator, -denominator)
    } else {
        (numerator, denominator)
    };
    let half = den / 2;
    let rounded = if num >= 0 {
        (num + half) / den
    } else {
        (num - half) / den
    };
    i64::try_from(rounded).unwrap_or(if rounded < 0 { i64::MIN } else { i64::MAX })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(min_x: i64, min_y: i64, max_x: i64, max_y: i64) -> Vec<[i64; 2]> {
        vec![
            [min_x, min_y],
            [max_x, min_y],
            [max_x, max_y],
            [min_x, max_y],
        ]
    }

    #[test]
    fn point_in_polygon_handles_convex_concave_and_closed_rings() {
        // Concave U-shape with an open courtyard in [3..7, 3..10].
        let u_shape = vec![
            [0, 0],
            [10, 0],
            [10, 10],
            [7, 10],
            [7, 3],
            [3, 3],
            [3, 10],
            [0, 10],
            [0, 0], // explicitly closed
        ];

        assert!(point_in_polygon([1, 5], &u_shape));
        assert!(point_in_polygon([8, 8], &u_shape));
        assert!(point_in_polygon([5, 1], &u_shape));
        // Boundary vertex and edge point.
        assert!(point_in_polygon([3, 3], &u_shape));
        assert!(point_in_polygon([5, 0], &u_shape));
        // Inside the concave bay -> outside the polygon.
        assert!(!point_in_polygon([5, 5], &u_shape));
        assert!(!point_in_polygon([5, 9], &u_shape));
        assert!(!point_in_polygon([-1, 5], &u_shape));
    }

    #[test]
    fn disjoint_and_shared_boundary_polygons_do_not_overlap() {
        let left = square(0, 0, 10, 10);
        let disjoint = square(20, 0, 30, 10);
        let shared_edge = square(10, 0, 20, 10);
        let shared_partial_edge = square(10, 2, 20, 8);
        let shared_corner = square(10, 10, 20, 20);

        assert_eq!(polygons_overlap(&left, &disjoint), None);
        assert_eq!(polygons_overlap(&left, &shared_edge), None);
        assert_eq!(polygons_overlap(&left, &shared_partial_edge), None);
        assert_eq!(polygons_overlap(&left, &shared_corner), None);

        // Concave polygon tightly hugging an inset neighbor along 3 shared edges.
        let u_shape = vec![
            [0, 0],
            [10, 0],
            [10, 10],
            [7, 10],
            [7, 3],
            [3, 3],
            [3, 10],
            [0, 10],
        ];
        let courtyard_fill = square(3, 3, 7, 10);
        assert_eq!(polygons_overlap(&u_shape, &courtyard_fill), None);
    }

    #[test]
    fn partial_overlap_crossing_edges_and_containment_are_rejected_with_witness() {
        let base = square(0, 0, 10, 10);

        // Partial overlap with crossing edges.
        let shifted = square(5, 5, 15, 15);
        let witness = polygons_overlap(&base, &shifted).expect("partial overlap must fail");
        assert!(point_in_polygon(witness, &base) && point_in_polygon(witness, &shifted));

        // Crossing "+" bars where NO vertex of either polygon is inside the other!
        let horizontal = square(0, 4, 10, 6);
        let vertical = square(4, 0, 6, 10);
        let cross_witness =
            polygons_overlap(&horizontal, &vertical).expect("crossing bars overlap");
        assert!(
            point_in_polygon(cross_witness, &horizontal)
                && point_in_polygon(cross_witness, &vertical)
        );

        // Full strict containment.
        let inner = square(2, 2, 8, 8);
        let inner_witness = polygons_overlap(&base, &inner).expect("containment must fail");
        assert!(point_in_polygon(inner_witness, &base) && point_in_polygon(inner_witness, &inner));

        // Identical polygons (all vertices on boundary, zero proper crossings).
        let identical_witness =
            polygons_overlap(&base, &base).expect("identical footprints overlap");
        assert!(point_in_polygon(identical_witness, &base));

        // Inscribed strip touching top and bottom boundaries (all vertices on boundary).
        let strip = square(3, 0, 7, 10);
        let strip_witness = polygons_overlap(&base, &strip).expect("inscribed strip overlaps");
        assert!(point_in_polygon(strip_witness, &base) && point_in_polygon(strip_witness, &strip));

        // Tiny 1-tile identical triangle with no integer interior points.
        let tiny = vec![[0, 0], [1, 0], [0, 1]];
        assert!(polygons_overlap(&tiny, &tiny).is_some());
    }

    #[test]
    fn concave_overlap_detected_even_when_overall_centroid_is_outside() {
        // Two identical thin C-shaped polygons whose bounding-box centroids lie outside the shape.
        let c_shape = vec![
            [0, 0],
            [100, 0],
            [100, 10],
            [10, 10],
            [10, 90],
            [100, 90],
            [100, 100],
            [0, 100],
        ];
        assert!(polygons_overlap(&c_shape, &c_shape).is_some());
    }

    #[test]
    fn validate_non_overlapping_checks_same_face_only_and_reports_all_pairs() {
        let regions = vec![
            RegionFootprint {
                id: "haven".to_owned(),
                face: CubeFace::PosZ,
                polygon: square(0, 0, 100, 100),
            },
            RegionFootprint {
                id: "east_fields".to_owned(),
                face: CubeFace::PosZ,
                polygon: square(100, 0, 200, 100), // shares x = 100 edge: valid
            },
            RegionFootprint {
                id: "other_face_same_coords".to_owned(),
                face: CubeFace::PosX,
                polygon: square(0, 0, 100, 100), // different face: valid
            },
        ];
        assert_eq!(validate_non_overlapping(&regions), Ok(()));

        let mut colliding = regions;
        colliding.push(RegionFootprint {
            id: "intruder".to_owned(),
            face: CubeFace::PosZ,
            polygon: square(80, 20, 120, 60), // overlaps both haven and east_fields
        });
        let err = validate_non_overlapping(&colliding).expect_err("must report overlaps");
        assert_eq!(err.len(), 2);
        assert_eq!(err[0].region_a, "haven");
        assert_eq!(err[0].region_b, "intruder");
        assert_eq!(err[0].face, CubeFace::PosZ);
        assert_eq!(err[1].region_a, "east_fields");
        assert_eq!(err[1].region_b, "intruder");
    }

    #[test]
    fn region_footprint_round_trips_json() {
        let region = RegionFootprint {
            id: "sunspire".to_owned(),
            face: CubeFace::NegY,
            polygon: square(10, 20, 30, 40),
        };
        let json = serde_json::to_string(&region).expect("serialize");
        let parsed: RegionFootprint = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, region);
    }
}
