"""High-vertex-density GeoJSON + 16-bit heightmap exporter and 4K shaded-relief
physical-atlas renderer for Zeta's pre-humanoid world map.

Exports:
- export_world(elevation_m, rivers, boundary_lines, biome_id, biome_names, seed, out_dir) -> dict
- render_from_files(geojson_path, elevation_png_path, biome_rgb, out_png, crops_dir) -> dict

Coordinate convention (pixel-center equirectangular):
    Pixel (row, col) spans x in [col, col + 1], y in [row, row + 1] with
    pixel center at (col + 0.5, row + 0.5):
        lon = -180.0 + (x / W) * 360.0
        lat =   90.0 - (y / H) * 180.0
    Inverse:
        x = (lon + 180.0) / 360.0 * W
        y = ( 90.0 - lat) / 180.0 * H
        col = clip(floor(x), 0, W - 1)
        row = clip(floor(y), 0, H - 1)
"""

from __future__ import annotations

import json
import math
import time
from pathlib import Path
from typing import Any

import cv2
import numpy as np
from matplotlib import font_manager
from PIL import Image, ImageDraw, ImageFont
from scipy import ndimage
from skimage import measure

# Fixed linear 16-bit elevation encoding bounds (step = 17500 / 65535 = 0.2670 m,
# so maximum round-trip quantization error is 0.1335 m < 0.3 m).
ELEV_MIN_M: float = -9000.0
ELEV_MAX_M: float = 8500.0

# Export tuning constants
MIN_ISLAND_PIXELS: int = 5          # Drop only 1-4 px specks; preserve real archipelago islands
MIN_HOLE_PIXELS: int = 4            # Drop 1-3 px noise pinholes unless a river mouth lands inside
COASTLINE_SIMPLIFY_EPS_PX: float = 0.25
FAULT_SIMPLIFY_EPS_PX: float = 1.0
BIOME_SIMPLIFY_EPS_PX: float = 1.0
BIOME_MAX_W: int = 512
BIOME_MAX_H: int = 256

# Final 4K atlas dimensions and 1:1 crop dimensions
ATLAS_WIDTH: int = 3840
ATLAS_HEIGHT: int = 1920
CROP_WIDTH: int = 800
CROP_HEIGHT: int = 600

# Earth-like spherical circumference scale for physically scaled gradients
PLANET_RADIUS_M: float = 6_371_000.0


# ---------------------------------------------------------------------------
# Coordinate & 16-bit Heightmap Helpers
# ---------------------------------------------------------------------------

def px_to_lonlat(x: np.ndarray, y: np.ndarray, width: int, height: int) -> tuple[np.ndarray, np.ndarray]:
    """Convert pixel-corner coordinates x in [0, W], y in [0, H] to RFC 7946 lon/lat."""
    lon = np.clip(-180.0 + (x / float(width)) * 360.0, -180.0, 180.0)
    lat = np.clip(90.0 - (y / float(height)) * 180.0, -90.0, 90.0)
    return lon, lat


def lonlat_to_px(lon: np.ndarray | float, lat: np.ndarray | float, width: int, height: int) -> tuple[Any, Any]:
    """Convert lon/lat to continuous pixel-corner coordinates x in [0, W], y in [0, H]."""
    x = (np.asarray(lon, dtype=np.float64) + 180.0) / 360.0 * float(width)
    y = (90.0 - np.asarray(lat, dtype=np.float64)) / 180.0 * float(height)
    return x, y


def lonlat_to_rowcol(lon: float, lat: float, width: int, height: int) -> tuple[int, int]:
    """Map (lon, lat) to the pixel (row, col) containing it under pixel-center convention."""
    x = (float(lon) + 180.0) / 360.0 * float(width)
    y = (90.0 - float(lat)) / 180.0 * float(height)
    col = int(np.clip(math.floor(x), 0, width - 1))
    row = int(np.clip(math.floor(y), 0, height - 1))
    return row, col


def encode_elevation_u16(
    elevation_m: np.ndarray,
    elev_min_m: float = ELEV_MIN_M,
    elev_max_m: float = ELEV_MAX_M,
) -> np.ndarray:
    """Linearly encode elevation in meters to lossless uint16 [0..65535]."""
    span = float(elev_max_m) - float(elev_min_m)
    norm = np.clip((elevation_m.astype(np.float64) - float(elev_min_m)) / span, 0.0, 1.0)
    return np.rint(norm * 65535.0).astype(np.uint16)


def decode_elevation_u16(
    u16: np.ndarray,
    elev_min_m: float = ELEV_MIN_M,
    elev_max_m: float = ELEV_MAX_M,
) -> np.ndarray:
    """Decode uint16 heightmap back to float32 elevation in meters."""
    span = float(elev_max_m) - float(elev_min_m)
    return ((u16.astype(np.float64) / 65535.0) * span + float(elev_min_m)).astype(np.float32)


def _signed_area(pts: np.ndarray) -> float:
    """Shoelace signed area for an open ring array of shape (N, 2)."""
    if len(pts) < 3:
        return 0.0
    x = pts[:, 0]
    y = pts[:, 1]
    return float(0.5 * np.sum(x * np.roll(y, -1) - np.roll(x, -1) * y))


def _ring_px_to_geojson(
    pts_px: np.ndarray,
    width: int,
    height: int,
    is_exterior: bool,
    decimals: int = 5,
) -> list[list[float]] | None:
    """Convert an open (N, 2) pixel-corner ring into a closed RFC 7946 lon/lat ring.

    Enforces RFC 7946 Section 3.1.6 winding:
      - exterior rings: counterclockwise (signed area > 0 in lon/lat)
      - interior holes: clockwise (signed area < 0 in lon/lat)
    """
    if len(pts_px) < 3:
        return None
    lon, lat = px_to_lonlat(pts_px[:, 0], pts_px[:, 1], width, height)
    lon_r = np.round(lon, decimals)
    lat_r = np.round(lat, decimals)

    # Deduplicate consecutive identical rounded vertices
    coords: list[list[float]] = []
    for lo, la in zip(lon_r.tolist(), lat_r.tolist()):
        if not coords or coords[-1][0] != lo or coords[-1][1] != la:
            coords.append([lo, la])
    if len(coords) >= 2 and coords[0] == coords[-1]:
        coords.pop()
    if len(coords) < 3:
        return None

    # Enforce RFC 7946 winding in (lon, lat) space
    arr = np.asarray(coords, dtype=np.float64)
    area_ll = _signed_area(arr)
    if abs(area_ll) < 1e-14:
        return None
    if is_exterior and area_ll < 0.0:
        coords.reverse()
    elif (not is_exterior) and area_ll > 0.0:
        coords.reverse()

    coords.append([coords[0][0], coords[0][1]])
    return coords


def _find_contour_component_label(
    pts_px: np.ndarray,
    labels: np.ndarray,
    height: int,
    width: int,
) -> int:
    """Find the 4-connected foreground component label adjacent to a marching-squares contour."""
    n = len(pts_px)
    if n < 2:
        return 0
    step = max(1, n // 16)
    indices = list(range(0, n, step))
    if len(indices) < min(n, 16):
        indices = list(range(n))

    # First pass: probe along left normal (-dy, dx), which points into foreground
    # for skimage.measure.find_contours(..., positive_orientation='low').
    for sign in (1.0, -1.0):
        for idx in indices:
            p0 = pts_px[idx]
            p1 = pts_px[(idx + 1) % n]
            dx = float(p1[0] - p0[0])
            dy = float(p1[1] - p0[1])
            nrm = math.hypot(dx, dy)
            if nrm < 1e-9:
                continue
            mx = 0.5 * (float(p0[0]) + float(p1[0]))
            my = 0.5 * (float(p0[1]) + float(p1[1]))
            nx = (-dy / nrm) * sign
            ny = (dx / nrm) * sign
            cx = mx + 0.25 * nx
            cy = my + 0.25 * ny
            col = int(math.floor(cx))
            row = int(math.floor(cy))
            if 0 <= row < height and 0 <= col < width:
                lbl = int(labels[row, col])
                if lbl > 0:
                    return lbl
    return 0


def _extract_polygons_from_binary(
    mask_u8: np.ndarray,
    width_full: int,
    height_full: int,
    simplify_eps_px: float,
    min_component_px: int = 1,
    min_hole_px: int = 1,
    protected_water_pixels: set[tuple[int, int]] | None = None,
    decimals: int = 5,
) -> tuple[list[dict[str, Any]], np.ndarray]:
    """Extract non-overlapping Polygon rings (exterior + interior holes) from a uint8 mask.

    Uses 4-connected components + marching squares (fully_connected='low',
    positive_orientation='low') so every connected component has exactly one
    exterior ring and zero or more interior hole rings, with zero overlap and
    100% land-pixel-center preservation under simplify_eps_px <= 0.25.
    """
    h, w = mask_u8.shape
    scale_x = float(width_full) / float(w)
    scale_y = float(height_full) / float(h)

    # 1) Drop tiny foreground specks
    n_lbl, labels, stats, centroids = cv2.connectedComponentsWithStats(mask_u8, connectivity=4)
    keep = np.zeros(n_lbl, dtype=np.uint8)
    if n_lbl > 1:
        keep[1:] = (stats[1:, cv2.CC_STAT_AREA] >= min_component_px).astype(np.uint8)
    clean = keep[labels]

    # 2) Optionally fill tiny interior water pinholes (never filling ocean or river mouths)
    if min_hole_px > 1:
        inv = (clean == 0).astype(np.uint8)
        nw, w_labels, w_stats, _ = cv2.connectedComponentsWithStats(inv, connectivity=8)
        if nw > 1:
            # Identify border-touching water components (ocean / border bays)
            border_labels = set(np.unique(w_labels[0, :]).tolist())
            border_labels.update(np.unique(w_labels[-1, :]).tolist())
            border_labels.update(np.unique(w_labels[:, 0]).tolist())
            border_labels.update(np.unique(w_labels[:, -1]).tolist())
            protected_labels = set(border_labels)
            if protected_water_pixels:
                for pr, pc in protected_water_pixels:
                    if 0 <= pr < h and 0 <= pc < w:
                        protected_labels.add(int(w_labels[pr, pc]))
            fill_lut = np.zeros(nw, dtype=np.uint8)
            for wl in range(1, nw):
                if wl not in protected_labels and int(w_stats[wl, cv2.CC_STAT_AREA]) < min_hole_px:
                    fill_lut[wl] = 1
            if np.any(fill_lut):
                clean = np.where(fill_lut[w_labels] > 0, np.uint8(1), clean)

    # Re-label after pinhole cleanup so labels match exact 4-connected components
    n_lbl, labels, stats, centroids = cv2.connectedComponentsWithStats(clean, connectivity=4)
    if n_lbl <= 1:
        return [], clean

    # Pad with 0 on all 4 sides so border-touching polygons close cleanly along [-180..180, -90..90]
    padded = np.pad(clean, 1, mode="constant", constant_values=0)
    raw_contours = measure.find_contours(
        padded,
        level=0.5,
        fully_connected="low",
        positive_orientation="low",
    )

    exteriors: dict[int, list[list[float]]] = {}
    holes_by_comp: dict[int, list[list[list[float]]]] = {}

    for contour in raw_contours:
        if len(contour) < 4:
            continue
        # Remove closing duplicate point from skimage contour; convert padded (r_p, c_p) -> (x, y)
        pts = np.column_stack([contour[:-1, 1] - 0.5, contour[:-1, 0] - 0.5]).astype(np.float64)
        pts[:, 0] = np.clip(pts[:, 0], 0.0, float(w))
        pts[:, 1] = np.clip(pts[:, 1], 0.0, float(h))

        raw_area = _signed_area(pts)
        if abs(raw_area) < 1e-9:
            continue
        is_exterior = raw_area > 0.0

        comp_id = _find_contour_component_label(pts, labels, h, w)
        if comp_id <= 0:
            continue

        # Simplify with Douglas-Peucker; fall back gracefully if ring collapses
        simp = pts
        if simplify_eps_px > 0.0:
            cand = cv2.approxPolyDP(
                pts.astype(np.float32).reshape(-1, 1, 2),
                float(simplify_eps_px),
                True,
            ).reshape(-1, 2).astype(np.float64)
            cand_area = _signed_area(cand)
            if len(cand) >= 3 and (cand_area * raw_area) > 0.0 and abs(cand_area) > 1e-6:
                simp = cand
            else:
                cand2 = cv2.approxPolyDP(
                    pts.astype(np.float32).reshape(-1, 1, 2),
                    float(simplify_eps_px) * 0.4,
                    True,
                ).reshape(-1, 2).astype(np.float64)
                if len(cand2) >= 3 and (_signed_area(cand2) * raw_area) > 0.0:
                    simp = cand2

        # Scale from (w, h) to full grid (width_full, height_full)
        if scale_x != 1.0 or scale_y != 1.0:
            simp_full = np.column_stack([simp[:, 0] * scale_x, simp[:, 1] * scale_y])
        else:
            simp_full = simp

        ring = _ring_px_to_geojson(
            simp_full,
            width_full,
            height_full,
            is_exterior=is_exterior,
            decimals=decimals,
        )
        if ring is None:
            # Fallback to unsimplified ring
            pts_full = np.column_stack([pts[:, 0] * scale_x, pts[:, 1] * scale_y])
            ring = _ring_px_to_geojson(
                pts_full,
                width_full,
                height_full,
                is_exterior=is_exterior,
                decimals=decimals,
            )
        if ring is None:
            continue

        if is_exterior:
            # Each 4-connected component has a single exterior ring
            if comp_id not in exteriors or len(ring) > len(exteriors[comp_id]):
                exteriors[comp_id] = ring
        else:
            holes_by_comp.setdefault(comp_id, []).append(ring)

    polygons: list[dict[str, Any]] = []
    for comp_id, ext_ring in exteriors.items():
        area_px = int(stats[comp_id, cv2.CC_STAT_AREA])
        cx, cy = float(centroids[comp_id, 0]) + 0.5, float(centroids[comp_id, 1]) + 0.5
        c_lon = round(float(np.clip(-180.0 + (cx * scale_x / width_full) * 360.0, -180.0, 180.0)), 4)
        c_lat = round(float(np.clip(90.0 - (cy * scale_y / height_full) * 180.0, -90.0, 90.0)), 4)
        holes = holes_by_comp.get(comp_id, [])
        polygons.append(
            {
                "comp_id": comp_id,
                "rings": [ext_ring, *holes],
                "area_px": area_px,
                "hole_count": len(holes),
                "centroid_lonlat": [c_lon, c_lat],
            }
        )

    polygons.sort(key=lambda p: p["area_px"], reverse=True)
    return polygons, clean


def _split_antimeridian_line(
    coords_lonlat: list[list[float]],
    decimals: int = 5,
) -> list[list[list[float]]]:
    """Clamp, deduplicate, and split a lon/lat polyline across antimeridian jumps."""
    if not coords_lonlat or len(coords_lonlat) < 2:
        return []

    segments: list[list[list[float]]] = []
    cur: list[list[float]] = []

    for pt in coords_lonlat:
        lo = round(float(np.clip(pt[0], -180.0, 180.0)), decimals)
        la = round(float(np.clip(pt[1], -90.0, 90.0)), decimals)
        if not cur:
            cur.append([lo, la])
            continue
        prev_lo, prev_la = cur[-1]
        if lo == prev_lo and la == prev_la:
            continue
        if abs(lo - prev_lo) > 180.0:
            if len(cur) >= 2:
                segments.append(cur)
            cur = [[lo, la]]
        else:
            cur.append([lo, la])

    if len(cur) >= 2:
        segments.append(cur)
    return segments


def _simplify_lonlat_line(
    coords: list[list[float]],
    width: int,
    height: int,
    eps_px: float,
    decimals: int = 4,
) -> list[list[float]]:
    """Mildly simplify a polyline in pixel space while keeping endpoints fixed."""
    if len(coords) <= 2 or eps_px <= 0.0:
        return coords
    arr = np.asarray(coords, dtype=np.float64)
    x, y = lonlat_to_px(arr[:, 0], arr[:, 1], width, height)
    pts = np.column_stack([x, y]).astype(np.float32)
    simp = cv2.approxPolyDP(pts.reshape(-1, 1, 2), float(eps_px), False).reshape(-1, 2)
    if len(simp) < 2:
        return coords
    lon, lat = px_to_lonlat(simp[:, 0], simp[:, 1], width, height)
    out: list[list[float]] = []
    for lo, la in zip(np.round(lon, decimals).tolist(), np.round(lat, decimals).tolist()):
        if not out or out[-1][0] != lo or out[-1][1] != la:
            out.append([lo, la])
    return out if len(out) >= 2 else coords


# ---------------------------------------------------------------------------
# 1. Public API: export_world
# ---------------------------------------------------------------------------

def export_world(
    elevation_m: np.ndarray,
    rivers: list[dict],
    boundary_lines: list[dict],
    biome_id: np.ndarray,
    biome_names: dict[int, str],
    seed: int,
    out_dir: Path,
) -> dict:
    """Export high-vertex-density GeoJSON + lossless 16-bit elevation PNG.

    Saves:
      - out_dir / "world_map.geojson"
      - out_dir / "world_elevation.png"
    """
    out_path = Path(out_dir)
    out_path.mkdir(parents=True, exist_ok=True)

    elev = np.asarray(elevation_m, dtype=np.float32)
    if elev.ndim != 2:
        raise ValueError(f"elevation_m must be 2D (H, W), got shape {elev.shape}")
    height, width = elev.shape

    # Collect river mouth water pixels so pinhole cleanup never fills a river's terminal lake
    mouth_water_pixels: set[tuple[int, int]] = set()
    for rv in (rivers or []):
        coords = rv.get("coords_lonlat") or []
        if coords:
            mr, mc = lonlat_to_rowcol(coords[-1][0], coords[-1][1], width, height)
            mouth_water_pixels.add((mr, mc))

    # 1) High-definition coastline polygons with interior lake/inland-sea holes
    raw_land = (elev > 0.0).astype(np.uint8)
    land_polys, land_clean = _extract_polygons_from_binary(
        raw_land,
        width_full=width,
        height_full=height,
        simplify_eps_px=COASTLINE_SIMPLIFY_EPS_PX,
        min_component_px=MIN_ISLAND_PIXELS,
        min_hole_px=MIN_HOLE_PIXELS,
        protected_water_pixels=mouth_water_pixels,
        decimals=5,
    )

    # Precompute max elevation per land component cleanly using connected component labels
    n_lbl, comp_labels = cv2.connectedComponents(land_clean, connectivity=4)
    comp_max_elev = np.zeros(n_lbl, dtype=np.float32)
    if n_lbl > 1:
        comp_max_elev = ndimage.maximum(elev, labels=comp_labels, index=np.arange(n_lbl)).astype(np.float32)

    features: list[dict[str, Any]] = []
    coastline_vertices = 0
    total_holes = 0

    for idx, poly in enumerate(land_polys):
        rings = poly["rings"]
        verts = sum(len(r) for r in rings)
        coastline_vertices += verts
        total_holes += poly["hole_count"]
        cid = poly["comp_id"]
        max_el = float(comp_max_elev[cid]) if cid < len(comp_max_elev) else 0.0
        features.append(
            {
                "type": "Feature",
                "properties": {
                    "layer": "land",
                    "id": f"land_{idx}",
                    "area_px": poly["area_px"],
                    "hole_count": poly["hole_count"],
                    "max_elev_m": round(max_el, 1),
                    "centroid_lonlat": poly["centroid_lonlat"],
                },
                "geometry": {
                    "type": "Polygon",
                    "coordinates": rings,
                },
            }
        )

    # 2) Branching river LineStrings
    river_features_count = 0
    river_vertices = 0
    for r_idx, rv in enumerate(rivers or []):
        raw_coords = rv.get("coords_lonlat") or []
        segs = _split_antimeridian_line(raw_coords, decimals=5)
        if not segs:
            continue
        rid = str(rv.get("id", f"river_{r_idx}"))
        stream_order = int(rv.get("stream_order", 1))
        max_flow = round(float(rv.get("max_flow", 0.0)), 2)
        if len(segs) == 1:
            geom: dict[str, Any] = {"type": "LineString", "coordinates": segs[0]}
            river_vertices += len(segs[0])
        else:
            geom = {"type": "MultiLineString", "coordinates": segs}
            river_vertices += sum(len(s) for s in segs)
        features.append(
            {
                "type": "Feature",
                "properties": {
                    "layer": "river",
                    "id": rid,
                    "stream_order": stream_order,
                    "max_flow": max_flow,
                },
                "geometry": geom,
            }
        )
        river_features_count += 1

    # 3) Tectonic boundary fault LineStrings
    boundary_features_count = 0
    boundary_vertices = 0
    for b_idx, bl in enumerate(boundary_lines or []):
        raw_coords = bl.get("coords_lonlat") or []
        segs = _split_antimeridian_line(raw_coords, decimals=4)
        if not segs:
            continue
        simp_segs = [
            _simplify_lonlat_line(seg, width, height, FAULT_SIMPLIFY_EPS_PX, decimals=4)
            for seg in segs
        ]
        bid = str(bl.get("id", f"fault_{b_idx}"))
        btype = str(bl.get("boundary_type", "unknown"))
        if len(simp_segs) == 1:
            geom = {"type": "LineString", "coordinates": simp_segs[0]}
            boundary_vertices += len(simp_segs[0])
        else:
            geom = {"type": "MultiLineString", "coordinates": simp_segs}
            boundary_vertices += sum(len(s) for s in simp_segs)
        features.append(
            {
                "type": "Feature",
                "properties": {
                    "layer": "tectonic_boundary",
                    "id": bid,
                    "boundary_type": btype,
                },
                "geometry": geom,
            }
        )
        boundary_features_count += 1

    # 4) Simplified biome MultiPolygon features (compact, land-only)
    biome_features_count = 0
    biome_vertices = 0
    if biome_id is not None:
        b_arr = np.asarray(biome_id)
        if b_arr.shape != (height, width):
            b_arr = cv2.resize(b_arr.astype(np.uint8), (width, height), interpolation=cv2.INTER_NEAREST)
        bw = min(width, BIOME_MAX_W)
        bh = min(height, BIOME_MAX_H)
        b_small = cv2.resize(b_arr.astype(np.uint8), (bw, bh), interpolation=cv2.INTER_NEAREST)
        l_small = cv2.resize(land_clean, (bw, bh), interpolation=cv2.INTER_NEAREST)

        name_lookup = {int(k): str(v) for k, v in (biome_names or {}).items()}
        unique_biomes = np.unique(b_small[l_small > 0]).tolist()
        for bid_val in unique_biomes:
            bid_int = int(bid_val)
            b_mask = ((b_small == bid_int) & (l_small > 0)).astype(np.uint8)
            b_polys, _ = _extract_polygons_from_binary(
                b_mask,
                width_full=width,
                height_full=height,
                simplify_eps_px=BIOME_SIMPLIFY_EPS_PX,
                min_component_px=4,
                min_hole_px=4,
                decimals=4,
            )
            if not b_polys:
                continue
            mp_coords = [p["rings"] for p in b_polys]
            biome_vertices += sum(len(r) for poly_rings in mp_coords for r in poly_rings)
            b_name = name_lookup.get(bid_int, f"biome_{bid_int}")
            if len(mp_coords) == 1:
                b_geom: dict[str, Any] = {"type": "Polygon", "coordinates": mp_coords[0]}
            else:
                b_geom = {"type": "MultiPolygon", "coordinates": mp_coords}
            features.append(
                {
                    "type": "Feature",
                    "properties": {
                        "layer": "biome",
                        "biome_id": bid_int,
                        "biome_name": b_name,
                        "polygon_count": len(mp_coords),
                    },
                    "geometry": b_geom,
                }
            )
            biome_features_count += 1

    # Top-level GeoJSON document with exact elevation encoding metadata
    biome_names_str = {str(k): str(v) for k, v in (biome_names or {}).items()}
    geojson_doc = {
        "type": "FeatureCollection",
        "properties": {
            "seed": int(seed),
            "width": int(width),
            "height": int(height),
            "elev_min_m": float(ELEV_MIN_M),
            "elev_max_m": float(ELEV_MAX_M),
            "crs": "OGC:CRS84",
            "projection": "equirectangular_pixel_center",
            "biome_names": biome_names_str,
        },
        "features": features,
    }

    geojson_path = out_path / "world_map.geojson"
    geojson_text = json.dumps(geojson_doc, separators=(",", ":"), ensure_ascii=False)
    geojson_path.write_text(geojson_text, encoding="utf-8", newline="\n")

    # Save lossless 16-bit grayscale heightmap PNG
    elev_u16 = encode_elevation_u16(elev, ELEV_MIN_M, ELEV_MAX_M)
    elevation_png_path = out_path / "world_elevation.png"
    ok = cv2.imwrite(str(elevation_png_path), elev_u16, [cv2.IMWRITE_PNG_COMPRESSION, 6])
    if not ok:
        raise RuntimeError(f"Failed to write 16-bit heightmap PNG to {elevation_png_path}")

    total_vertices = coastline_vertices + river_vertices + boundary_vertices + biome_vertices
    return {
        "feature_counts": {
            "land": len(land_polys),
            "river": river_features_count,
            "tectonic_boundary": boundary_features_count,
            "biome": biome_features_count,
            "total": len(features),
        },
        "total_vertices": int(total_vertices),
        "coastline_vertices": int(coastline_vertices),
        "river_vertices": int(river_vertices),
        "boundary_vertices": int(boundary_vertices),
        "biome_vertices": int(biome_vertices),
        "land_polygons": int(len(land_polys)),
        "holes": int(total_holes),
        "river_count": int(river_features_count),
        "boundary_count": int(boundary_features_count),
        "biome_features": int(biome_features_count),
        "geojson_bytes": int(geojson_path.stat().st_size),
        "elevation_png_bytes": int(elevation_png_path.stat().st_size),
        "geojson_path": str(geojson_path),
        "elevation_png_path": str(elevation_png_path),
    }


# ---------------------------------------------------------------------------
# GeoJSON Rasterization & Strict Automated Validation
# ---------------------------------------------------------------------------

def _iter_polygon_rings(geometry: dict[str, Any]):
    """Yield (polygon_rings_list) for Polygon or MultiPolygon geometry."""
    gtype = geometry.get("type")
    coords = geometry.get("coordinates") or []
    if gtype == "Polygon":
        yield coords
    elif gtype == "MultiPolygon":
        for poly in coords:
            yield poly


def _validate_and_rasterize_geojson(
    geojson_doc: dict[str, Any],
    width: int,
    height: int,
) -> tuple[np.ndarray, np.ndarray, list[dict[str, Any]], list[dict[str, Any]], dict[str, Any]]:
    """Validate all GeoJSON rings & inland river vertices, and rasterize exact land masks.

    Returns:
      - land_mask: (H, W) bool (exact pixel-center occupancy from GeoJSON polygons)
      - land_alpha: (H, W) float32 in [0, 1] (2x subpixel anti-aliased land coverage)
      - land_features: list of land GeoJSON features
      - river_features: list of river GeoJSON features
      - validation: detailed validation report dict
    """
    features = geojson_doc.get("features") or []
    land_features: list[dict[str, Any]] = []
    river_features: list[dict[str, Any]] = []

    total_rings = 0
    unclosed_rings = 0
    short_rings = 0
    out_of_bounds_coords = 0

    # 2x supersampled mask for exact pixel-center sampling AND crisp anti-aliased coastlines
    mask2 = np.zeros((height * 2, width * 2), dtype=np.uint8)
    sub_shift = 2
    sub_mult = float(2 * (1 << sub_shift))  # 2x grid * 4 subpixel shift = 8x scale

    for feat in features:
        props = feat.get("properties") or {}
        layer = props.get("layer", "")
        geom = feat.get("geometry") or {}
        gtype = geom.get("type", "")

        if gtype in ("Polygon", "MultiPolygon"):
            is_land = layer == "land"
            if is_land:
                land_features.append(feat)
            for poly_rings in _iter_polygon_rings(geom):
                cv_rings: list[np.ndarray] = []
                for ring in poly_rings:
                    total_rings += 1
                    if len(ring) < 4:
                        short_rings += 1
                    if not ring or ring[0][0] != ring[-1][0] or ring[0][1] != ring[-1][1]:
                        unclosed_rings += 1
                    if ring:
                        arr = np.asarray(ring, dtype=np.float64)
                        if (
                            np.any(arr[:, 0] < -180.0 - 1e-9)
                            or np.any(arr[:, 0] > 180.0 + 1e-9)
                            or np.any(arr[:, 1] < -90.0 - 1e-9)
                            or np.any(arr[:, 1] > 90.0 + 1e-9)
                        ):
                            out_of_bounds_coords += 1
                        if is_land and len(ring) >= 4:
                            x, y = lonlat_to_px(arr[:, 0], arr[:, 1], width, height)
                            pts_i = np.rint(np.column_stack([x * sub_mult, y * sub_mult])).astype(np.int32)
                            cv_rings.append(pts_i)
                if is_land and cv_rings:
                    # Single cv2.fillPoly call with [exterior, hole1, hole2, ...] fills
                    # exterior minus its holes via even-odd rule without disturbing islands in lakes.
                    cv2.fillPoly(mask2, cv_rings, 1, shift=sub_shift)

        elif layer == "river" and gtype in ("LineString", "MultiLineString"):
            river_features.append(feat)

    # Pixel (row, col) center (col + 0.5, row + 0.5) maps to (2*col + 1, 2*row + 1) on mask2
    land_mask = mask2[1::2, 1::2] > 0
    # 2x2 box average gives smooth 5-level subpixel coastline anti-aliasing
    land_alpha = (
        mask2[0::2, 0::2].astype(np.float32)
        + mask2[0::2, 1::2].astype(np.float32)
        + mask2[1::2, 0::2].astype(np.float32)
        + mask2[1::2, 1::2].astype(np.float32)
    ) * 0.25

    # Validate EVERY inland river vertex (all vertices except the final mouth) against land_mask
    river_inland_vertices_checked = 0
    river_ocean_violations = 0
    violation_samples: list[dict[str, Any]] = []

    for feat in river_features:
        props = feat.get("properties") or {}
        rid = str(props.get("id", "unknown"))
        geom = feat.get("geometry") or {}
        gtype = geom.get("type")
        if gtype == "LineString":
            all_pts = geom.get("coordinates") or []
        else:
            # Flatten MultiLineString in upstream-to-mouth order
            all_pts = [pt for seg in (geom.get("coordinates") or []) for pt in seg]
        if len(all_pts) < 2:
            continue
        inland_pts = all_pts[:-1]  # Every point except the final mouth vertex
        for v_idx, pt in enumerate(inland_pts):
            lo, la = float(pt[0]), float(pt[1])
            if lo < -180.0 - 1e-9 or lo > 180.0 + 1e-9 or la < -90.0 - 1e-9 or la > 90.0 + 1e-9:
                out_of_bounds_coords += 1
            r, c = lonlat_to_rowcol(lo, la, width, height)
            river_inland_vertices_checked += 1
            if not land_mask[r, c]:
                river_ocean_violations += 1
                if len(violation_samples) < 10:
                    violation_samples.append(
                        {"river_id": rid, "vertex_idx": v_idx, "lon": lo, "lat": la, "row": r, "col": c}
                    )

    passed = (
        unclosed_rings == 0
        and short_rings == 0
        and out_of_bounds_coords == 0
        and river_ocean_violations == 0
    )
    validation = {
        "passed": bool(passed),
        "total_rings_checked": int(total_rings),
        "unclosed_rings": int(unclosed_rings),
        "short_rings": int(short_rings),
        "out_of_bounds_coords": int(out_of_bounds_coords),
        "river_inland_vertices_checked": int(river_inland_vertices_checked),
        "river_ocean_violations": int(river_ocean_violations),
        "violation_samples": violation_samples,
    }
    return land_mask, land_alpha, land_features, river_features, validation


# ---------------------------------------------------------------------------
# Physical Atlas Shading, Bathymetry, Rivers, Graticule & Cartouche
# ---------------------------------------------------------------------------

def _spherical_gradients(
    elev_m: np.ndarray,
    z_factor: float,
    smooth_sigma: float = 0.0,
) -> tuple[np.ndarray, np.ndarray]:
    """Compute spherically scaled (dz/dx, dz/dy) in dimensionless slope units."""
    h, w = elev_m.shape
    src = cv2.GaussianBlur(elev_m, (0, 0), smooth_sigma) if smooth_sigma > 0.0 else elev_m

    # Scharr 3x3 gives crisp, rotationally isotropic derivatives (normalized by 32)
    gx = cv2.Scharr(src, cv2.CV_32F, 1, 0) * (1.0 / 32.0)
    gy = cv2.Scharr(src, cv2.CV_32F, 0, 1) * (1.0 / 32.0)

    dy_m = (math.pi * PLANET_RADIUS_M) / float(h)
    lat_rad = np.deg2rad(90.0 - (np.arange(h, dtype=np.float32) + 0.5) / float(h) * 180.0)
    cos_lat = np.maximum(np.cos(lat_rad), 0.18)[:, None]
    dx_m = ((2.0 * math.pi * PLANET_RADIUS_M) / float(w)) * cos_lat

    dz_dx = (gx / dx_m) * float(z_factor)
    dz_dy = (gy / dy_m) * float(z_factor)
    return dz_dx, dz_dy


def _directional_light(
    dz_dx: np.ndarray,
    dz_dy: np.ndarray,
    azimuth_deg: float,
    altitude_deg: float,
) -> np.ndarray:
    """Lambertian hillshade for compass azimuth (0=N, 90=E, 315=NW) and sun altitude."""
    az = math.radians(azimuth_deg)
    alt = math.radians(altitude_deg)
    # Image x points East (+sin(az)), image y points South (-cos(az))
    lx = math.cos(alt) * math.sin(az)
    ly = -math.cos(alt) * math.cos(az)
    lz = math.sin(alt)

    # Surface normal proportional to (-dz/dx, -dz/dy, 1)
    inv_len = 1.0 / np.sqrt(dz_dx * dz_dx + dz_dy * dz_dy + 1.0)
    dot = (-dz_dx * lx - dz_dy * ly + lz) * inv_len
    return np.clip(dot, 0.0, 1.0)


def _shade_land(
    elevation_m: np.ndarray,
    land_mask: np.ndarray,
    biome_rgb: np.ndarray,
) -> np.ndarray:
    """Produce crisp Swiss/Imhof multi-directional hillshaded alpine & physical land RGB float32 [0..255]."""
    h, w = elevation_m.shape
    elev_land = np.where(land_mask, np.maximum(elevation_m, 0.0), 0.0).astype(np.float32)

    # Multi-scale spherical gradients: crisp micro-ridges + eroded spurs + macro massifs
    dx_fine, dy_fine = _spherical_gradients(elev_land, z_factor=14.5, smooth_sigma=0.0)
    dx_med, dy_med = _spherical_gradients(elev_land, z_factor=8.5, smooth_sigma=1.8)
    dx_macro, dy_macro = _spherical_gradients(elev_land, z_factor=4.8, smooth_sigma=4.5)
    dz_dx = 0.56 * dx_fine + 0.30 * dx_med + 0.14 * dx_macro
    dz_dy = 0.56 * dy_fine + 0.30 * dy_med + 0.14 * dy_macro
    slope_mag = np.sqrt(dz_dx * dz_dx + dz_dy * dz_dy)

    # Multi-directional Imhof lighting:
    # - Primary warm NW sun at low 30 deg altitude for sculpted 3D ridges & deep shadows
    # - Secondary W cross-light so NW-SE trending cordilleras stay crisp and sculptural
    # - Subtle cool NE sky fill so shadowed valleys hold blue-slate atmospheric depth
    l_nw = _directional_light(dz_dx, dz_dy, azimuth_deg=315.0, altitude_deg=30.0)
    l_w = _directional_light(dz_dx, dz_dy, azimuth_deg=260.0, altitude_deg=36.0)
    l_ne = _directional_light(dz_dx, dz_dy, azimuth_deg=35.0, altitude_deg=50.0)

    # Sky-view / ambient occlusion: darken incised valleys & gorges, lift sharp ridge crests
    blur_near = cv2.GaussianBlur(elev_land, (0, 0), 2.5)
    blur_mid = cv2.GaussianBlur(elev_land, (0, 0), 9.0)
    blur_far = cv2.GaussianBlur(elev_land, (0, 0), 22.0)
    local_relief = (
        0.45 * (elev_land - blur_near)
        + 0.35 * (elev_land - blur_mid)
        + 0.20 * (elev_land - blur_far)
    )
    # Stronger occlusion in negative (valley) curvature than lift on crests
    ao_raw = np.where(
        local_relief < 0.0,
        1.0 + local_relief / 820.0,
        1.0 + local_relief / 1350.0,
    )
    ao = np.clip(ao_raw, 0.62, 1.15)
    relief_weight = np.clip(elev_land / 450.0 + slope_mag * 0.55, 0.0, 1.0)
    ao = 1.0 + (ao - 1.0) * relief_weight

    # Prepare base biome albedo (preserving climate_erosion's smooth physical palette)
    base = biome_rgb.astype(np.float32)
    if base.max() <= 1.05:
        base = base * 255.0

    # Subtle weathered granite/slate rock exposure on steep high escarpments
    lat_abs = np.abs(90.0 - (np.arange(h, dtype=np.float32) + 0.5) / float(h) * 180.0)[:, None]
    rock_mask = np.clip((elev_land - 1500.0) / 2200.0, 0.0, 1.0) * np.clip((slope_mag - 0.50) / 1.15, 0.0, 1.0)
    rock_rgb = np.array([116.0, 110.0, 104.0], dtype=np.float32)
    base = base * (1.0 - 0.36 * rock_mask[..., None]) + rock_rgb * (0.36 * rock_mask[..., None])

    # Conservative alpine glacier accent only on highest peaks (never bleaching out biome_rgb)
    snowline_m = 5300.0 - 40.0 * lat_abs
    snow_elev = np.clip((elev_land - snowline_m) / 1300.0, 0.0, 1.0)
    cliff_shed = 1.0 - 0.55 * np.clip((slope_mag - 0.95) / 1.35, 0.0, 1.0)
    snow_mask = snow_elev * cliff_shed
    snow_rgb = np.array([232.0, 237.0, 244.0], dtype=np.float32)
    base = base * (1.0 - 0.42 * snow_mask[..., None]) + snow_rgb * (0.42 * snow_mask[..., None])

    # Imhof warm-sun / cool-indigo-shadow chromatic shading centered on flat-plane luminance
    flat_nw = math.sin(math.radians(30.0))
    flat_w = math.sin(math.radians(36.0))
    flat_ne = math.sin(math.radians(50.0))

    delta_nw = (l_nw - flat_nw) * 1.12
    delta_w = (l_w - flat_w) * 0.32
    delta_ne = (l_ne - flat_ne) * 0.22

    # Warm sunlight on NW-facing mountain slopes; crisp cool violet-slate shadow on SE faces
    key_sum = delta_nw + 0.72 * delta_w
    sun_push = np.maximum(key_sum, 0.0)
    shadow_pull = np.maximum(-key_sum, 0.0)

    r_gain = (1.0 + sun_push * 0.58 - shadow_pull * 0.78 + delta_ne * 0.10) * ao
    g_gain = (1.0 + sun_push * 0.51 - shadow_pull * 0.70 + delta_ne * 0.18) * ao
    b_gain = (1.0 + sun_push * 0.36 - shadow_pull * 0.52 + delta_ne * 0.34) * ao

    shaded = np.empty_like(base)
    shaded[..., 0] = base[..., 0] * np.clip(r_gain, 0.22, 1.46)
    shaded[..., 1] = base[..., 1] * np.clip(g_gain, 0.26, 1.42)
    shaded[..., 2] = base[..., 2] * np.clip(b_gain, 0.34, 1.36)

    # Delicate cool atmospheric shadow fill + crisp warm ridge highlight kiss
    valley_shadow = np.clip(shadow_pull * 0.85 + np.maximum(1.0 - ao, 0.0) * 1.2, 0.0, 1.0)
    shaded[..., 2] += valley_shadow * 8.0

    crest_glow = np.clip((local_relief - 140.0) / 850.0, 0.0, 1.0) * np.clip(l_nw - 0.50, 0.0, 0.50)
    shaded[..., 0] += crest_glow * 30.0
    shaded[..., 1] += crest_glow * 25.0
    shaded[..., 2] += crest_glow * 14.0

    return np.clip(shaded, 0.0, 255.0)


def _shade_water(
    elevation_m: np.ndarray,
    land_mask: np.ndarray,
) -> np.ndarray:
    """Render rich GEBCO/Blue-Marble bathymetry + continental shelves + inland lakes."""
    h, w = elevation_m.shape
    water_u8 = (~land_mask).astype(np.uint8)
    depth_m = np.where(~land_mask, np.maximum(-elevation_m, 0.0), 0.0).astype(np.float32)

    # Distinguish open ocean from enclosed inland lakes/seas
    n_w, w_lbl, w_stats, _ = cv2.connectedComponentsWithStats(water_u8, connectivity=4)
    inland_lake_mask = np.zeros((h, w), dtype=bool)
    if n_w > 1:
        # Components smaller than 1.5% of map area and not touching top/bottom ocean borders
        ocean_threshold = int(0.015 * h * w)
        is_lake_lut = np.zeros(n_w, dtype=bool)
        for wl in range(1, n_w):
            if int(w_stats[wl, cv2.CC_STAT_AREA]) < ocean_threshold:
                is_lake_lut[wl] = True
        inland_lake_mask = is_lake_lut[w_lbl]

    # Distance from coast in pixels for luminous turquoise continental shelf halo
    dist_px = cv2.distanceTransform(water_u8, cv2.DIST_L2, 5)
    # Normalize shelf distance relative to image resolution (approx 22 px at 4096x2048)
    shelf_scale = max(6.0, float(w) / 185.0)
    coastal_halo = np.exp(-dist_px / shelf_scale)
    shallow_depth = np.exp(-depth_m / 260.0)
    shelf_factor = np.clip(0.55 * shallow_depth + 0.45 * coastal_halo * np.exp(-depth_m / 950.0), 0.0, 1.0)

    # Multi-stop bathymetry color lookup table (depth in meters -> RGB)
    stops_m = np.array([0.0, 45.0, 220.0, 1100.0, 2800.0, 4600.0, 6500.0, 9000.0], dtype=np.float32)
    stops_r = np.array([104.0, 66.0, 40.0, 25.0, 17.0, 12.0, 8.0, 5.0], dtype=np.float32)
    stops_g = np.array([184.0, 150.0, 108.0, 74.0, 52.0, 38.0, 25.0, 16.0], dtype=np.float32)
    stops_b = np.array([196.0, 178.0, 152.0, 122.0, 96.0, 78.0, 58.0, 42.0], dtype=np.float32)

    ocean_rgb = np.empty((h, w, 3), dtype=np.float32)
    ocean_rgb[..., 0] = np.interp(depth_m, stops_m, stops_r)
    ocean_rgb[..., 1] = np.interp(depth_m, stops_m, stops_g)
    ocean_rgb[..., 2] = np.interp(depth_m, stops_m, stops_b)

    # Lift shallow turquoise shelves around continents and archipelagos
    shelf_tint = np.array([98.0, 182.0, 194.0], dtype=np.float32)
    ocean_rgb = ocean_rgb * (1.0 - 0.36 * shelf_factor[..., None]) + shelf_tint * (0.36 * shelf_factor[..., None])

    # Submarine hillshading for mid-ocean ridges, seamounts & subduction trenches
    elev_ocean = np.where(~land_mask, np.minimum(elevation_m, 0.0), 0.0).astype(np.float32)
    odx, ody = _spherical_gradients(elev_ocean, z_factor=2.8, smooth_sigma=1.0)
    o_light = _directional_light(odx, ody, azimuth_deg=315.0, altitude_deg=45.0)
    flat_o = math.sin(math.radians(45.0))
    # Fade submarine shading right against coastline so coast drop doesn't create harsh seam
    offshore_fade = np.clip((dist_px - 2.0) / 8.0, 0.0, 1.0)
    o_gain = 1.0 + (o_light - flat_o) * 0.58 * offshore_fade
    ocean_rgb *= np.clip(o_gain, 0.65, 1.32)[..., None]

    # Inland lakes & enclosed seas get vibrant freshwater cerulean-teal palette
    if np.any(inland_lake_mask):
        lake_shallow = np.exp(-dist_px / max(3.0, shelf_scale * 0.45))
        lake_r = 42.0 + 38.0 * lake_shallow
        lake_g = 108.0 + 52.0 * lake_shallow
        lake_b = 148.0 + 34.0 * lake_shallow
        ocean_rgb[inland_lake_mask, 0] = lake_r[inland_lake_mask]
        ocean_rgb[inland_lake_mask, 1] = lake_g[inland_lake_mask]
        ocean_rgb[inland_lake_mask, 2] = lake_b[inland_lake_mask]

    return np.clip(ocean_rgb, 0.0, 255.0)


def _composite_rivers(
    canvas_rgb: np.ndarray,
    river_features: list[dict[str, Any]],
    land_alpha: np.ndarray,
    elevation_m: np.ndarray,
) -> np.ndarray:
    """Draw tapered anti-aliased branching rivers strictly clipped to the GeoJSON land mask."""
    if not river_features:
        return canvas_rgb

    h, w, _ = canvas_rgb.shape
    res_scale = float(w) / 4096.0

    # Render river alpha and bank shadow onto float32 accumulators via 2x supersampling
    # for crisp sub-pixel tapered widths without jaggies.
    river_mask = np.zeros((h, w), dtype=np.float32)
    bank_mask = np.zeros((h, w), dtype=np.float32)

    # Sort smallest tributaries first so major mainstem trunks paint cleanly on top
    ordered = sorted(
        river_features,
        key=lambda f: (
            int((f.get("properties") or {}).get("stream_order", 1)),
            float((f.get("properties") or {}).get("max_flow", 0.0)),
        ),
    )

    shift = 4
    s_mult = float(1 << shift)

    # Draw directly onto uint8 scratch masks in buckets of width for high speed + AA
    tmp_river = np.zeros((h, w), dtype=np.uint8)
    tmp_bank = np.zeros((h, w), dtype=np.uint8)

    for feat in ordered:
        props = feat.get("properties") or {}
        geom = feat.get("geometry") or {}
        gtype = geom.get("type")
        segs = [geom["coordinates"]] if gtype == "LineString" else list(geom.get("coordinates") or [])
        order = max(1, int(props.get("stream_order", 1)))
        max_flow = max(0.0, float(props.get("max_flow", 1.0)))

        # Taper width from headwater to mouth scaled by stream order and log flow
        flow_boost = min(2.4, 0.35 * math.log1p(max_flow))
        w_start = max(0.75, (0.65 + 0.35 * (order - 1)) * res_scale)
        w_end = max(w_start + 0.4, (1.15 + 0.65 * (order - 1) + flow_boost) * res_scale)

        for seg in segs:
            if len(seg) < 2:
                continue
            arr = np.asarray(seg, dtype=np.float64)
            xs, ys = lonlat_to_px(arr[:, 0], arr[:, 1], w, h)
            # Pixel centers sit at (col + 0.5, row + 0.5); OpenCV lines sample at (col, row)
            pts = np.rint(np.column_stack([(xs - 0.5) * s_mult, (ys - 0.5) * s_mult])).astype(np.int32)

            # Chunk polyline into ~8 taper bands so width grows smoothly downstream
            n_pts = len(pts)
            n_bands = min(8, n_pts - 1)
            for b in range(n_bands):
                i0 = (b * (n_pts - 1)) // n_bands
                i1 = ((b + 1) * (n_pts - 1)) // n_bands
                if i1 <= i0:
                    continue
                t = (b + 0.5) / float(n_bands)
                width_px = w_start + (w_end - w_start) * (t ** 0.85)
                thick = max(1, int(round(width_px)))
                intensity = int(np.clip(round(255.0 * min(1.0, width_px / max(1.0, float(thick)) * 0.95)), 110, 255))
                sub = pts[i0 : i1 + 1]
                cv2.polylines(tmp_bank, [sub], False, 180, thickness=thick + 2, lineType=cv2.LINE_AA, shift=shift)
                cv2.polylines(tmp_river, [sub], False, intensity, thickness=thick, lineType=cv2.LINE_AA, shift=shift)

    # Strictly clip every river pixel to the GeoJSON land mask (land_alpha)
    river_mask = (tmp_river.astype(np.float32) / 255.0) * land_alpha
    bank_mask = (tmp_bank.astype(np.float32) / 255.0) * land_alpha * (1.0 - river_mask)

    # Natural elevation-aware river water tint (mountain azure -> lowland deep cerulean-teal)
    elev_norm = np.clip(elevation_m / 2800.0, 0.0, 1.0)[..., None]
    river_low = np.array([42.0, 106.0, 146.0], dtype=np.float32)
    river_high = np.array([78.0, 144.0, 184.0], dtype=np.float32)
    river_col = river_low * (1.0 - elev_norm) + river_high * elev_norm

    out = canvas_rgb.copy()
    # Subtle bank darkening (22%) keeps thin rivers crisp over arid & alpine biomes
    out *= (1.0 - 0.22 * bank_mask[..., None])
    alpha_r = np.clip(river_mask * 0.92, 0.0, 1.0)[..., None]
    out = out * (1.0 - alpha_r) + river_col * alpha_r
    return np.clip(out, 0.0, 255.0)


def _draw_graticule(
    img_rgb: np.ndarray,
    land_mask_resized: np.ndarray,
    step_deg: int = 30,
) -> np.ndarray:
    """Overlay an airy, low-contrast 30-degree graticule."""
    h, w, _ = img_rgb.shape
    out = img_rgb.astype(np.float32)
    grat = np.zeros((h, w), dtype=np.float32)

    for lon in range(-180 + step_deg, 180, step_deg):
        x = int(round((lon + 180.0) / 360.0 * w))
        if 0 <= x < w:
            grat[:, x] = 1.0
    for lat in range(-90 + step_deg, 90, step_deg):
        y = int(round((90.0 - lat) / 180.0 * h))
        if 0 <= y < h:
            # Make equator slightly clearer than other parallels
            grat[y, :] = 1.25 if lat == 0 else 1.0

    # Subtle light tint over dark ocean, subtle cool slate tint over land
    ocean_blend = np.clip(grat * 0.14, 0.0, 0.20) * (~land_mask_resized)
    land_blend = np.clip(grat * 0.11, 0.0, 0.16) * land_mask_resized

    ocean_line = np.array([195.0, 222.0, 238.0], dtype=np.float32)
    land_line = np.array([40.0, 52.0, 62.0], dtype=np.float32)

    out = out * (1.0 - ocean_blend[..., None]) + ocean_line * ocean_blend[..., None]
    out = out * (1.0 - land_blend[..., None]) + land_line * land_blend[..., None]
    return np.clip(out, 0.0, 255.0).astype(np.uint8)


def _load_font(family: str, size: int, bold: bool = False) -> ImageFont.FreeTypeFont | ImageFont.ImageFont:
    """Load a crisp TrueType font bundled with matplotlib (fallback to PIL default)."""
    try:
        prop = font_manager.FontProperties(
            family=family,
            weight="bold" if bold else "normal",
        )
        font_path = font_manager.findfont(prop, fallback_to_default=True)
        return ImageFont.truetype(font_path, size=size)
    except Exception:
        return ImageFont.load_default()


def _add_minimal_cartouche_and_frame(
    img_rgb: np.ndarray,
    seed: int,
    elev_min_m: float,
    elev_max_m: float,
) -> np.ndarray:
    """Add a refined, uncluttered cartographic frame, edge degree ticks, corner cartouche,
    elevation/bathymetry scale bar, and small compass rose (zero labels over islands)."""
    pil_img = Image.fromarray(img_rgb, mode="RGB").convert("RGBA")
    w, h = pil_img.size
    overlay = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    draw = ImageDraw.Draw(overlay)

    scale = w / 3840.0
    pad = max(6, int(round(12 * scale)))

    # 1) Delicate double hairline frame inset near the map margin
    draw.rectangle(
        [pad, pad, w - 1 - pad, h - 1 - pad],
        outline=(225, 215, 190, 145),
        width=max(1, int(round(2 * scale))),
    )
    draw.rectangle(
        [pad + 4, pad + 4, w - 1 - pad - 4, h - 1 - pad - 4],
        outline=(20, 30, 42, 110),
        width=1,
    )

    # 2) Subtle lat/lon degree ticks along edges only (never on landmasses)
    f_tick = _load_font("DejaVu Sans", max(11, int(round(18 * scale))), bold=False)

    def _draw_shadow_text(xy: tuple[int, int], text: str, font: Any, fill: tuple[int, int, int, int], anchor: str = "la"):
        x0, y0 = xy
        draw.text((x0 + 1, y0 + 1), text, font=font, fill=(8, 14, 24, 190), anchor=anchor)
        draw.text((x0, y0), text, font=font, fill=fill, anchor=anchor)

    tick_col = (232, 236, 240, 200)
    for lat in (-60, -30, 0, 30, 60):
        y = int(round((90.0 - lat) / 180.0 * h))
        lbl = "0°" if lat == 0 else (f"{lat}°N" if lat > 0 else f"{-lat}°S")
        _draw_shadow_text((pad + int(12 * scale), y), lbl, f_tick, tick_col, anchor="lm")
        _draw_shadow_text((w - pad - int(12 * scale), y), lbl, f_tick, tick_col, anchor="rm")

    for lon in (-120, -60, 0, 60, 120):
        x = int(round((lon + 180.0) / 360.0 * w))
        lbl = "0°" if lon == 0 else (f"{lon}°E" if lon > 0 else f"{-lon}°W")
        _draw_shadow_text((x, pad + int(10 * scale)), lbl, f_tick, tick_col, anchor="ma")

    # 3) Minimal corner title cartouche (bottom-left margin, dynamically sized to fit text!)
    f_title = _load_font("DejaVu Serif", max(13, int(round(25 * scale))), bold=True)
    f_sub = _load_font("DejaVu Sans", max(10, int(round(16 * scale))), bold=False)
    title_text = "ZETA  ·  PRE-HUMANOID WORLD"
    sub_text = f"Physical Relief & Bathymetry  ·  Equirectangular  ·  Seed {seed}"
    tb_title = draw.textbbox((0, 0), title_text, font=f_title)
    tb_sub = draw.textbbox((0, 0), sub_text, font=f_sub)
    title_w, title_h = tb_title[2] - tb_title[0], tb_title[3] - tb_title[1]
    sub_w, sub_h = tb_sub[2] - tb_sub[0], tb_sub[3] - tb_sub[1]

    inner_x = int(round(22 * scale))
    inner_y = int(round(18 * scale))
    line_gap = int(round(12 * scale))
    box_w = max(title_w, sub_w) + 2 * inner_x
    box_h = title_h + sub_h + line_gap + 2 * inner_y
    bx0 = pad + int(round(22 * scale))
    by0 = h - pad - int(round(22 * scale)) - box_h
    draw.rounded_rectangle(
        [bx0, by0, bx0 + box_w, by0 + box_h],
        radius=max(4, int(round(8 * scale))),
        fill=(11, 20, 34, 188),
        outline=(214, 198, 162, 175),
        width=max(1, int(round(1.5 * scale))),
    )
    _draw_shadow_text(
        (bx0 + inner_x, by0 + inner_y - tb_title[1]),
        title_text,
        f_title,
        (244, 238, 222, 245),
        anchor="la",
    )
    _draw_shadow_text(
        (bx0 + inner_x, by0 + inner_y + title_h + line_gap - tb_sub[1]),
        sub_text,
        f_sub,
        (188, 206, 222, 230),
        anchor="la",
    )

    # 4) Compact elevation & bathymetry scale legend (bottom-right margin, text-measured)
    f_leg_title = _load_font("DejaVu Sans", max(9, int(round(14 * scale))), bold=True)
    leg_title = "BATHYMETRY & HYPSOMETRIC ELEVATION (m)"
    tb_lt = draw.textbbox((0, 0), leg_title, font=f_leg_title)
    lt_w = tb_lt[2] - tb_lt[0]
    leg_pad_x = int(round(22 * scale))
    leg_w = max(int(round(460 * scale)), lt_w + 2 * leg_pad_x)
    leg_h = int(round(98 * scale))
    lx0 = w - pad - int(round(22 * scale)) - leg_w
    ly0 = h - pad - int(round(22 * scale)) - leg_h
    draw.rounded_rectangle(
        [lx0, ly0, lx0 + leg_w, ly0 + leg_h],
        radius=max(4, int(round(8 * scale))),
        fill=(11, 20, 34, 188),
        outline=(214, 198, 162, 175),
        width=max(1, int(round(1.5 * scale))),
    )
    bar_x0 = lx0 + leg_pad_x
    bar_x1 = lx0 + leg_w - leg_pad_x
    bar_y0 = ly0 + int(38 * scale)
    bar_y1 = ly0 + int(58 * scale)
    _draw_shadow_text(
        (bar_x0, ly0 + int(13 * scale)),
        leg_title,
        f_leg_title,
        (228, 224, 210, 235),
        anchor="la",
    )

    # Smooth horizontal gradient ramp centered at sea level (0 m -> 0.50)
    ramp_stops = [
        (0.00, (7, 22, 50)),
        (0.22, (18, 56, 102)),
        (0.42, (56, 140, 174)),
        (0.50, (104, 184, 196)),
        (0.505, (86, 134, 78)),
        (0.68, (152, 162, 102)),
        (0.84, (142, 128, 114)),
        (1.00, (234, 238, 245)),
    ]
    bar_span = max(1, bar_x1 - bar_x0)
    for px in range(bar_x0, bar_x1):
        t = (px - bar_x0) / float(bar_span)
        c = ramp_stops[-1][1]
        for k in range(len(ramp_stops) - 1):
            t0, c0 = ramp_stops[k]
            t1, c1 = ramp_stops[k + 1]
            if t0 <= t <= t1:
                u = (t - t0) / max(1e-6, t1 - t0)
                c = (
                    int(round(c0[0] * (1.0 - u) + c1[0] * u)),
                    int(round(c0[1] * (1.0 - u) + c1[1] * u)),
                    int(round(c0[2] * (1.0 - u) + c1[2] * u)),
                )
                break
        draw.line([(px, bar_y0), (px, bar_y1)], fill=(c[0], c[1], c[2], 235), width=1)
    draw.rectangle([bar_x0, bar_y0, bar_x1, bar_y1], outline=(220, 215, 200, 165), width=1)

    f_leg = _load_font("DejaVu Sans", max(9, int(round(13 * scale))), bold=False)
    for frac, txt in [
        (0.0, "-8k"),
        (0.25, "-4k"),
        (0.50, "0"),
        (0.75, "+4k"),
        (1.0, "+8k"),
    ]:
        tx = int(round(bar_x0 + frac * bar_span))
        draw.line([(tx, bar_y1), (tx, bar_y1 + int(4 * scale))], fill=(214, 224, 234, 180), width=1)
        _draw_shadow_text((tx, bar_y1 + int(9 * scale)), txt, f_leg, (214, 224, 234, 225), anchor="ma")

    # 5) Small crisp compass rose above bottom-right legend
    cr = int(round(28 * scale))
    cx = lx0 + leg_w - cr - int(round(14 * scale))
    cy = ly0 - cr - int(round(18 * scale))
    if cy > pad + cr * 2:
        draw.ellipse(
            [cx - cr, cy - cr, cx + cr, cy + cr],
            fill=(11, 20, 34, 150),
            outline=(214, 198, 162, 165),
            width=max(1, int(round(1.5 * scale))),
        )
        r_out = cr * 0.78
        r_in = cr * 0.22
        # North arrow (gold) and South/East/West arms (ivory)
        draw.polygon(
            [(cx, cy - r_out), (cx + r_in, cy), (cx, cy)],
            fill=(236, 206, 134, 235),
        )
        draw.polygon(
            [(cx, cy - r_out), (cx - r_in, cy), (cx, cy)],
            fill=(196, 164, 96, 225),
        )
        draw.polygon(
            [(cx, cy + r_out), (cx + r_in, cy), (cx - r_in, cy)],
            fill=(206, 214, 224, 180),
        )
        draw.polygon(
            [(cx + r_out * 0.85, cy), (cx, cy - r_in), (cx, cy + r_in)],
            fill=(206, 214, 224, 175),
        )
        draw.polygon(
            [(cx - r_out * 0.85, cy), (cx, cy - r_in), (cx, cy + r_in)],
            fill=(160, 172, 186, 175),
        )
        _draw_shadow_text(
            (cx, cy - cr - int(6 * scale)),
            "N",
            _load_font("DejaVu Serif", max(10, int(round(16 * scale))), bold=True),
            (242, 228, 186, 235),
            anchor="mb",
        )

    composited = Image.alpha_composite(pil_img, overlay).convert("RGB")
    return np.asarray(composited, dtype=np.uint8)


# ---------------------------------------------------------------------------
# Automatic 1:1 Crop Selection (4 Signature Regions)
# ---------------------------------------------------------------------------

def _clamp_crop_box(cx: int, cy: int, crop_w: int, crop_h: int, width: int, height: int) -> tuple[int, int, int, int]:
    """Return clamped (x0, y0, x1, y1) inside [0, width] x [0, height]."""
    cw = min(crop_w, width)
    ch = min(crop_h, height)
    x0 = int(np.clip(cx - cw // 2, 0, max(0, width - cw)))
    y0 = int(np.clip(cy - ch // 2, 0, max(0, height - ch)))
    return x0, y0, x0 + cw, y0 + ch


def _box_iou(a: tuple[int, int, int, int], b: tuple[int, int, int, int]) -> float:
    ix0, iy0 = max(a[0], b[0]), max(a[1], b[1])
    ix1, iy1 = min(a[2], b[2]), min(a[3], b[3])
    inter = max(0, ix1 - ix0) * max(0, iy1 - iy0)
    if inter == 0:
        return 0.0
    area_a = max(1, (a[2] - a[0]) * (a[3] - a[1]))
    area_b = max(1, (b[2] - b[0]) * (b[3] - b[1]))
    return inter / float(area_a + area_b - inter)


def _select_and_save_crops(
    full_rgb_u8: np.ndarray,
    elevation_m: np.ndarray,
    land_mask: np.ndarray,
    land_features: list[dict[str, Any]],
    river_features: list[dict[str, Any]],
    crops_dir: Path,
) -> list[dict[str, Any]]:
    """Select and save 4 zoomed 1:1 full-resolution crops (800x600 each)."""
    crops_dir.mkdir(parents=True, exist_ok=True)
    h, w, _ = full_rgb_u8.shape
    cw = min(CROP_WIDTH, w)
    ch = min(CROP_HEIGHT, h)

    chosen_boxes: list[tuple[int, int, int, int]] = []
    crop_meta: list[dict[str, Any]] = []

    # --- Crop 1: Highest mountain range ---
    elev_land = np.where(land_mask, np.maximum(elevation_m, 0.0), 0.0).astype(np.float32)
    # Smooth window highlights the core of the highest alpine range rather than a lone spike
    mountain_score = cv2.GaussianBlur(elev_land, (0, 0), max(3.0, w / 256.0))
    m_idx = int(np.argmax(mountain_score))
    my, mx = divmod(m_idx, w)
    box1 = _clamp_crop_box(mx, my, cw, ch, w, h)
    peak_m = float(elev_land[box1[1] : box1[3], box1[0] : box1[2]].max())
    chosen_boxes.append(box1)

    # --- Crop 2: Densest archipelago ---
    # Score sliding windows by count of distinct small/medium island polygons
    island_pts: list[tuple[float, float, float]] = []
    max_island_area = max(400, int(0.0025 * w * h))
    for feat in land_features:
        props = feat.get("properties") or {}
        area = int(props.get("area_px", 0))
        if MIN_ISLAND_PIXELS <= area <= max_island_area:
            c_ll = props.get("centroid_lonlat")
            if not c_ll:
                ring = (feat.get("geometry") or {}).get("coordinates", [[]])[0]
                if ring:
                    arr = np.asarray(ring, dtype=np.float64)
                    c_ll = [float(arr[:, 0].mean()), float(arr[:, 1].mean())]
            if c_ll:
                ix, iy = lonlat_to_px(c_ll[0], c_ll[1], w, h)
                # Slightly favor medium islands over tiny specks while counting diversity
                wt = 1.0 + 0.3 * math.log1p(area / 10.0)
                island_pts.append((float(ix), float(iy), wt))

    # Build coarse heatmap of archipelago density (16 px blocks)
    step = max(8, w // 256)
    gh, gw = max(1, h // step), max(1, w // step)
    island_grid = np.zeros((gh, gw), dtype=np.float32)
    for ix, iy, wt in island_pts:
        gx = int(np.clip(ix / step, 0, gw - 1))
        gy = int(np.clip(iy / step, 0, gh - 1))
        island_grid[gy, gx] += wt

    win_gw = max(1, cw // step)
    win_gh = max(1, ch // step)
    arch_density = cv2.boxFilter(island_grid, cv2.CV_32F, (win_gw, win_gh), normalize=False, borderType=cv2.BORDER_CONSTANT)

    # Pick densest archipelago window that doesn't heavily overlap crop 1
    best_arch = None
    best_arch_score = -1.0
    flat_order = np.argsort(arch_density.ravel())[::-1]
    for fidx in flat_order[:250]:
        gy, gx = divmod(int(fidx), gw)
        sc = float(arch_density[gy, gx])
        cand = _clamp_crop_box(int((gx + 0.5) * step), int((gy + 0.5) * step), cw, ch, w, h)
        if best_arch is None:
            best_arch = (cand, sc)
        if all(_box_iou(cand, b) < 0.25 for b in chosen_boxes):
            best_arch = (cand, sc)
            best_arch_score = sc
            break
    if best_arch is None:
        box2 = _clamp_crop_box(w // 4, h // 2, cw, ch, w, h)
        arch_count = 0
    else:
        box2 = best_arch[0]
        arch_count = sum(
            1
            for ix, iy, _ in island_pts
            if box2[0] <= ix <= box2[2] and box2[1] <= iy <= box2[3]
        )
    chosen_boxes.append(box2)

    # --- Crop 3: Rugged coastline / fjords ---
    # Coastline perimeter pixels weighted by nearby topographic relief
    land_u8 = land_mask.astype(np.uint8)
    coast_edge = cv2.morphologyEx(land_u8, cv2.MORPH_GRADIENT, np.ones((3, 3), dtype=np.uint8)).astype(np.float32)
    local_mean = cv2.GaussianBlur(elev_land, (0, 0), 7.0)
    local_sq = cv2.GaussianBlur(elev_land * elev_land, (0, 0), 7.0)
    local_std = np.sqrt(np.maximum(local_sq - local_mean * local_mean, 0.0))
    rugged_field = coast_edge * (1.0 + local_std / 350.0)
    rugged_coarse = cv2.resize(rugged_field, (gw, gh), interpolation=cv2.INTER_AREA)
    rugged_win = cv2.boxFilter(rugged_coarse, cv2.CV_32F, (win_gw, win_gh), normalize=False, borderType=cv2.BORDER_CONSTANT)

    best_fjord = None
    for fidx in np.argsort(rugged_win.ravel())[::-1][:400]:
        gy, gx = divmod(int(fidx), gw)
        sc = float(rugged_win[gy, gx])
        cand = _clamp_crop_box(int((gx + 0.5) * step), int((gy + 0.5) * step), cw, ch, w, h)
        if best_fjord is None:
            best_fjord = (cand, sc)
        if all(_box_iou(cand, b) < 0.25 for b in chosen_boxes):
            best_fjord = (cand, sc)
            break
    box3 = best_fjord[0] if best_fjord is not None else _clamp_crop_box(3 * w // 4, h // 3, cw, ch, w, h)
    fjord_metric = float(best_fjord[1]) if best_fjord is not None else 0.0
    chosen_boxes.append(box3)

    # --- Crop 4: Largest river delta ---
    if river_features:
        best_river = max(
            river_features,
            key=lambda f: (
                float((f.get("properties") or {}).get("max_flow", 0.0)),
                int((f.get("properties") or {}).get("stream_order", 1)),
                len((f.get("geometry") or {}).get("coordinates") or []),
            ),
        )
        r_props = best_river.get("properties") or {}
        r_geom = best_river.get("geometry") or {}
        if r_geom.get("type") == "LineString":
            r_pts = r_geom.get("coordinates") or []
        else:
            r_pts = [pt for seg in (r_geom.get("coordinates") or []) for pt in seg]
        if r_pts:
            mouth_lon, mouth_lat = float(r_pts[-1][0]), float(r_pts[-1][1])
            mx_r, my_r = lonlat_to_px(mouth_lon, mouth_lat, w, h)
            # Shift moderately upstream so delta + tributaries + coastline are all framed nicely
            up_idx = max(0, int(len(r_pts) * 0.55))
            ux_r, uy_r = lonlat_to_px(float(r_pts[up_idx][0]), float(r_pts[up_idx][1]), w, h)
            cx4 = int(round(0.55 * float(mx_r) + 0.45 * float(ux_r)))
            cy4 = int(round(0.55 * float(my_r) + 0.45 * float(uy_r)))
            box4 = _clamp_crop_box(cx4, cy4, cw, ch, w, h)
            delta_metric = float(r_props.get("max_flow", 0.0))
        else:
            box4 = _clamp_crop_box(w // 2, h // 2, cw, ch, w, h)
            delta_metric = 0.0
    else:
        box4 = _clamp_crop_box(w // 2, h // 2, cw, ch, w, h)
        delta_metric = 0.0

    crop_specs = [
        ("crop_1.png", "highest_mountain_range", box1, f"peak_elev_m={peak_m:.1f}"),
        ("crop_2.png", "densest_archipelago", box2, f"island_count={arch_count}"),
        ("crop_3.png", "rugged_coastline_fjords", box3, f"rugged_score={fjord_metric:.1f}"),
        ("crop_4.png", "largest_river_delta", box4, f"max_flow={delta_metric:.1f}"),
    ]

    for fname, theme, (x0, y0, x1, y1), metric in crop_specs:
        crop_img = full_rgb_u8[y0:y1, x0:x1]
        out_file = crops_dir / fname
        Image.fromarray(crop_img, mode="RGB").save(out_file, format="PNG", compress_level=6)
        c_lon, c_lat = px_to_lonlat(np.array([(x0 + x1) * 0.5]), np.array([(y0 + y1) * 0.5]), w, h)
        crop_meta.append(
            {
                "name": theme,
                "path": str(out_file),
                "bbox_px": [int(x0), int(y0), int(x1), int(y1)],
                "size_px": [int(x1 - x0), int(y1 - y0)],
                "center_lonlat": [round(float(c_lon[0]), 3), round(float(c_lat[0]), 3)],
                "metric": metric,
            }
        )

    return crop_meta


# ---------------------------------------------------------------------------
# 2. Public API: render_from_files
# ---------------------------------------------------------------------------

def render_from_files(
    geojson_path: Path,
    elevation_png_path: Path,
    biome_rgb: np.ndarray,
    out_png: Path,
    crops_dir: Path,
) -> dict:
    """Render 4K physical-atlas PNG(s) and 4 zoomed 1:1 crops STRICTLY from disk files.

    Single-source-of-truth guarantee:
      - Coastline land/water mask is rasterized exclusively from `geojson_path`.
      - All terrain & bathymetry elevations are decoded exclusively from 16-bit `elevation_png_path`.
      - Rivers are read exclusively from `geojson_path` and strictly clipped to the GeoJSON land mask.
    """
    t_start = time.perf_counter()
    gj_path = Path(geojson_path)
    elev_path = Path(elevation_png_path)
    out_path = Path(out_png)
    crops_path = Path(crops_dir)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    crops_path.mkdir(parents=True, exist_ok=True)

    # 1) Read GeoJSON & 16-bit elevation PNG from disk
    geojson_doc = json.loads(gj_path.read_text(encoding="utf-8"))
    top_props = geojson_doc.get("properties") or {}
    elev_min_m = float(top_props.get("elev_min_m", ELEV_MIN_M))
    elev_max_m = float(top_props.get("elev_max_m", ELEV_MAX_M))
    seed = int(top_props.get("seed", 0))

    u16 = cv2.imread(str(elev_path), cv2.IMREAD_UNCHANGED)
    if u16 is None:
        raise FileNotFoundError(f"Could not read 16-bit elevation PNG: {elev_path}")
    if u16.dtype != np.uint16 or u16.ndim != 2:
        raise ValueError(f"Expected 2D uint16 grayscale PNG, got shape={u16.shape} dtype={u16.dtype}")
    height, width = u16.shape
    elevation_m = decode_elevation_u16(u16, elev_min_m=elev_min_m, elev_max_m=elev_max_m)

    # Prepare biome RGB array matching (height, width, 3)
    if biome_rgb is None:
        b_rgb = np.full((height, width, 3), np.array([122, 146, 98], dtype=np.uint8), dtype=np.uint8)
    else:
        b_rgb = np.asarray(biome_rgb)
        if b_rgb.ndim != 3 or b_rgb.shape[2] != 3:
            raise ValueError(f"biome_rgb must have shape (H, W, 3), got {b_rgb.shape}")
        if b_rgb.shape[:2] != (height, width):
            b_rgb = cv2.resize(b_rgb, (width, height), interpolation=cv2.INTER_LINEAR)

    # 2) Strictly rasterize GeoJSON polygons & validate closed rings + inland river vertices
    land_mask, land_alpha, land_features, river_features, validation = _validate_and_rasterize_geojson(
        geojson_doc, width=width, height=height
    )
    if not validation["passed"]:
        print(
            "[export_render WARNING] Validation flagged issues: "
            f"unclosed={validation['unclosed_rings']}, "
            f"short={validation['short_rings']}, "
            f"oob={validation['out_of_bounds_coords']}, "
            f"river_ocean_violations={validation['river_ocean_violations']}"
        )

    # 3) Render full-resolution (4096x2048) physical relief, bathymetry, coastline edge & rivers
    land_rgb = _shade_land(elevation_m, land_mask, b_rgb)
    water_rgb = _shade_water(elevation_m, land_mask)

    alpha3 = land_alpha[..., None]
    composite = land_rgb * alpha3 + water_rgb * (1.0 - alpha3)

    # Crisp whisper-thin shoreline stroke right along the GeoJSON coastline transition
    coast_band = (land_alpha > 0.05) & (land_alpha < 0.95)
    land_u8 = land_mask.astype(np.uint8)
    coast_outline = (
        cv2.morphologyEx(land_u8, cv2.MORPH_GRADIENT, np.ones((3, 3), dtype=np.uint8)) > 0
    ) & land_mask
    shore_weight = np.where(coast_band, 0.34, np.where(coast_outline, 0.22, 0.0)).astype(np.float32)
    shore_col = np.array([22.0, 44.0, 58.0], dtype=np.float32)
    composite = composite * (1.0 - shore_weight[..., None]) + shore_col * shore_weight[..., None]

    # Tapered anti-aliased rivers strictly clipped to GeoJSON land mask
    composite = _composite_rivers(composite, river_features, land_alpha, elevation_m)
    full_rgb_u8 = np.clip(composite, 0.0, 255.0).astype(np.uint8)

    # 4) Save 4 zoomed 1:1 full-resolution crops directly from native canvas
    crops_meta = _select_and_save_crops(
        full_rgb_u8=full_rgb_u8,
        elevation_m=elevation_m,
        land_mask=land_mask,
        land_features=land_features,
        river_features=river_features,
        crops_dir=crops_path,
    )

    # 5) Supersample down to 4K (3840 x 1920) with area filtering for ultra-crisp anti-aliasing
    if (width, height) != (ATLAS_WIDTH, ATLAS_HEIGHT):
        interp = cv2.INTER_AREA if width >= ATLAS_WIDTH else cv2.INTER_CUBIC
        atlas_base = cv2.resize(full_rgb_u8, (ATLAS_WIDTH, ATLAS_HEIGHT), interpolation=interp)
        land_4k = cv2.resize(land_u8, (ATLAS_WIDTH, ATLAS_HEIGHT), interpolation=cv2.INTER_NEAREST) > 0
    else:
        atlas_base = full_rgb_u8
        land_4k = land_mask

    # Subtle 30-degree graticule
    atlas_clean = _draw_graticule(atlas_base, land_4k, step_deg=30)

    # Refined minimal-cartouche version (no cluttered text over islands!)
    atlas_cartouche = _add_minimal_cartouche_and_frame(
        atlas_clean,
        seed=seed,
        elev_min_m=elev_min_m,
        elev_max_m=elev_max_m,
    )

    # Save both clean unlabelled atlas AND lightly cartouched atlas
    clean_path = out_path.parent / "world_map_clean.png"
    labelled_path = out_path.parent / "world_map_pre_humanoid.png"

    if out_path.name == "world_map_clean.png":
        Image.fromarray(atlas_clean, mode="RGB").save(out_path, format="PNG", compress_level=6)
        Image.fromarray(atlas_cartouche, mode="RGB").save(labelled_path, format="PNG", compress_level=6)
    elif out_path.name == "world_map_pre_humanoid.png":
        Image.fromarray(atlas_cartouche, mode="RGB").save(out_path, format="PNG", compress_level=6)
        Image.fromarray(atlas_clean, mode="RGB").save(clean_path, format="PNG", compress_level=6)
    else:
        Image.fromarray(atlas_cartouche, mode="RGB").save(out_path, format="PNG", compress_level=6)
        Image.fromarray(atlas_clean, mode="RGB").save(clean_path, format="PNG", compress_level=6)
        Image.fromarray(atlas_cartouche, mode="RGB").save(labelled_path, format="PNG", compress_level=6)

    elapsed_s = time.perf_counter() - t_start
    return {
        "out_png": str(out_path),
        "clean_png": str(clean_path),
        "labelled_png": str(labelled_path),
        "atlas_size": [ATLAS_WIDTH, ATLAS_HEIGHT],
        "source_size": [width, height],
        "land_fraction_geojson": round(float(land_mask.mean()), 5),
        "validation": validation,
        "validation_passed": validation["passed"],
        "river_ocean_violations": validation["river_ocean_violations"],
        "crops": crops_meta,
        "elapsed_s": round(elapsed_s, 3),
    }


# ---------------------------------------------------------------------------
# Self-Test Suite
# ---------------------------------------------------------------------------

def _build_synthetic_world(width: int = 4096, height: int = 2048, seed: int = 777) -> dict[str, Any]:
    """Generate a rich synthetic world (continents, alpine cordillera, inland sea,
    archipelago, subduction trench, mid-ocean ridge, branching rivers, smooth biomes)
    for thorough end-to-end self-testing."""
    rng = np.random.default_rng(seed)

    # Multi-scale fractal heightfield
    base = np.zeros((height, width), dtype=np.float32)
    for sy, amp in [(6, 1.0), (12, 0.58), (24, 0.34), (48, 0.21), (96, 0.13), (192, 0.08), (384, 0.048), (768, 0.026)]:
        g = rng.standard_normal((sy, sy * 2)).astype(np.float32)
        base += cv2.resize(g, (width, height), interpolation=cv2.INTER_CUBIC) * amp

    # Ridged multifractal alpine detail
    ridged = np.zeros((height, width), dtype=np.float32)
    for sy, amp in [(24, 0.55), (64, 0.35), (160, 0.20), (400, 0.11)]:
        g = rng.standard_normal((sy, sy * 2)).astype(np.float32)
        n = cv2.resize(g, (width, height), interpolation=cv2.INTER_CUBIC)
        ridged += (1.0 - np.abs(np.tanh(n))) ** 2 * amp

    # Shift to ~35% land fraction
    sea_thresh = float(np.quantile(base, 0.65))
    rel = base - sea_thresh

    # Add a dramatic subduction trench and mid-ocean ridge in oceans
    yy, xx = np.mgrid[0:height, 0:width].astype(np.float32)
    u = xx / float(width)
    v = yy / float(height)
    trench = -3800.0 * np.exp(-(((u - 0.18 - 0.04 * np.sin(v * 9.0)) / 0.012) ** 2))
    ridge = 1400.0 * np.exp(-(((u - 0.52 + 0.05 * np.cos(v * 7.0)) / 0.022) ** 2))

    # Scale land smoothly to 0..+7200 m (no flat plateau clipping) and ocean to -8200..0 m
    pos_rel = np.maximum(rel, 0.0)
    pos_max = float(pos_rel.max()) + 1e-6
    land_raw = (pos_rel / pos_max) * 2900.0 + ((pos_rel / pos_max) ** 1.15) * ridged * 4200.0
    land_max = float(land_raw.max()) + 1e-6
    land_h = (land_raw / land_max) * 7100.0
    ocean_h = np.where(rel <= 0.0, rel * 3600.0 + trench + ridge * 0.65, 0.0)
    elevation_m = np.where(rel > 0.0, np.clip(land_h + 12.0, 1.0, 7400.0), np.clip(ocean_h - 15.0, -8600.0, -1.0)).astype(
        np.float32
    )

    # Carve one guaranteed crisp inland sea + island inside it on the largest continent
    n_lbl, labels, stats, centroids = cv2.connectedComponentsWithStats((elevation_m > 0.0).astype(np.uint8), connectivity=4)
    if n_lbl > 1:
        biggest = 1 + int(np.argmax(stats[1:, cv2.CC_STAT_AREA]))
        # Find deepest interior point of biggest continent
        dist_in = cv2.distanceTransform((labels == biggest).astype(np.uint8), cv2.DIST_L2, 5)
        cy_i, cx_i = divmod(int(np.argmax(dist_in)), width)
        rad = max(12.0, float(dist_in[cy_i, cx_i]) * 0.28)
        d_lake = np.hypot((xx - cx_i) * 0.85, yy - cy_i)
        lake_ring = (d_lake < rad) & (d_lake > rad * 0.24)
        elevation_m = np.where(lake_ring, -320.0 * (1.0 - d_lake / rad) - 25.0, elevation_m)

    # Clean land mask (matches export_world speck filtering) to trace guaranteed valid rivers
    n_lbl, labels, stats, _ = cv2.connectedComponentsWithStats((elevation_m > 0.0).astype(np.uint8), connectivity=4)
    keep = np.zeros(n_lbl, dtype=np.uint8)
    keep[1:] = (stats[1:, cv2.CC_STAT_AREA] >= MIN_ISLAND_PIXELS).astype(np.uint8)
    land_clean = keep[labels] > 0

    # Generate realistic branching rivers strictly on land_clean ending at the exact ocean pixel
    dist_coast = cv2.distanceTransform(land_clean.astype(np.uint8), cv2.DIST_L2, 5)
    rivers: list[dict[str, Any]] = []
    # Pick high-interior headwaters on big landmasses and walk strictly downhill toward coast
    grad_y, grad_x = np.gradient(dist_coast)
    candidates = np.argwhere(dist_coast > max(20.0, width / 90.0))
    if len(candidates) > 0:
        pick_idx = rng.choice(len(candidates), size=min(24, len(candidates)), replace=False)
        for r_i, idx in enumerate(pick_idx):
            r, c = int(candidates[idx, 0]), int(candidates[idx, 1])
            pts_ll: list[list[float]] = []
            visited: set[tuple[int, int]] = set()
            for _step in range(900):
                if not land_clean[r, c]:
                    # Mouth vertex reached on exact water pixel
                    lo = -180.0 + (c + 0.5) / width * 360.0
                    la = 90.0 - (r + 0.5) / height * 180.0
                    pts_ll.append([lo, la])
                    break
                lo = -180.0 + (c + 0.5) / width * 360.0
                la = 90.0 - (r + 0.5) / height * 180.0
                pts_ll.append([lo, la])
                visited.add((r, c))
                # Step to neighbor minimizing dist_coast (with mild meander from elevation)
                best_nb = None
                best_val = 1e18
                for dr in (-1, 0, 1):
                    for dc in (-1, 0, 1):
                        if dr == 0 and dc == 0:
                            continue
                        nr, nc = r + dr, c + dc
                        if 0 <= nr < height and 0 <= nc < width and (nr, nc) not in visited:
                            val = float(dist_coast[nr, nc]) + 0.00015 * float(elevation_m[nr, nc])
                            if val < best_val:
                                best_val = val
                                best_nb = (nr, nc)
                if best_nb is None:
                    break
                r, c = best_nb
            if len(pts_ll) >= 12 and not land_clean[r, c]:
                order = min(5, max(1, len(pts_ll) // 55 + 1))
                rivers.append(
                    {
                        "id": f"river_{r_i}",
                        "stream_order": order,
                        "max_flow": round(float(len(pts_ll) * order * 12.5), 1),
                        "coords_lonlat": pts_ll,
                    }
                )

    # Synthetic tectonic boundary lines
    boundary_lines = [
        {
            "id": "fault_subduction_west",
            "boundary_type": "subduction",
            "coords_lonlat": [
                [-180.0 + (0.18 + 0.04 * math.sin(t * 9.0)) * 360.0, 90.0 - t * 180.0]
                for t in np.linspace(0.08, 0.92, 90)
            ],
        },
        {
            "id": "fault_ridge_mid",
            "boundary_type": "divergent",
            "coords_lonlat": [
                [-180.0 + (0.52 - 0.05 * math.cos(t * 7.0)) * 360.0, 90.0 - t * 180.0]
                for t in np.linspace(0.05, 0.95, 90)
            ],
        },
    ]

    # Smooth physical biome_rgb + categorical biome_id
    lat_deg = np.abs(90.0 - (np.arange(height, dtype=np.float32) + 0.5) / height * 180.0)[:, None]
    temp = np.clip((30.0 - 0.55 * lat_deg - np.maximum(elevation_m, 0.0) * 0.0058 + 10.0) / 45.0, 0.0, 1.0)
    moist = np.clip(0.5 + 0.35 * np.sin(u * 9.0 + v * 5.0) - 0.25 * (np.abs(lat_deg - 26.0) < 12.0), 0.0, 1.0)

    # Smooth natural satellite-atlas palette
    r_ch = (194.0 * (1.0 - moist) * temp + 58.0 * moist * temp + 186.0 * (1.0 - temp)).astype(np.float32)
    g_ch = (176.0 * (1.0 - moist) * temp + 106.0 * moist * temp + 194.0 * (1.0 - temp)).astype(np.float32)
    b_ch = (128.0 * (1.0 - moist) * temp + 64.0 * moist * temp + 188.0 * (1.0 - temp)).astype(np.float32)
    biome_rgb = np.clip(np.stack([r_ch, g_ch, b_ch], axis=-1), 0.0, 255.0).astype(np.uint8)

    biome_id = np.zeros((height, width), dtype=np.uint8)
    biome_id[(elevation_m > 0.0) & (temp < 0.25)] = 1
    biome_id[(elevation_m > 0.0) & (temp >= 0.25) & (moist < 0.35)] = 2
    biome_id[(elevation_m > 0.0) & (temp >= 0.25) & (moist >= 0.35) & (moist < 0.68)] = 3
    biome_id[(elevation_m > 0.0) & (temp >= 0.25) & (moist >= 0.68)] = 4
    biome_names = {0: "ocean", 1: "tundra_alpine", 2: "arid_steppe", 3: "temperate_woodland", 4: "lush_forest"}

    return {
        "elevation_m": elevation_m,
        "rivers": rivers,
        "boundary_lines": boundary_lines,
        "biome_id": biome_id,
        "biome_names": biome_names,
        "biome_rgb": biome_rgb,
        "seed": seed,
    }


def _run_self_test() -> None:
    print("=== export_render.py self-test ===")
    test_dir = Path(__file__).resolve().parent / "_selftest_export_render"
    crops_dir = test_dir / "crops"
    test_dir.mkdir(parents=True, exist_ok=True)

    # 1) Exact 16-bit elevation round-trip test across full [-9000, +8500] range
    ramp = np.linspace(ELEV_MIN_M, ELEV_MAX_M, 100_000, dtype=np.float32).reshape(200, 500)
    u16 = encode_elevation_u16(ramp)
    probe_png = test_dir / "_probe_u16.png"
    assert cv2.imwrite(str(probe_png), u16), "Failed to write probe 16-bit PNG"
    u16_disk = cv2.imread(str(probe_png), cv2.IMREAD_UNCHANGED)
    assert u16_disk.dtype == np.uint16, f"Expected uint16, got {u16_disk.dtype}"
    assert np.array_equal(u16, u16_disk), "16-bit PNG round-trip was not lossless!"
    decoded = decode_elevation_u16(u16_disk)
    max_err_m = float(np.max(np.abs(decoded - ramp)))
    assert max_err_m < 0.3, f"Round-trip 16-bit elevation error {max_err_m:.4f} m >= 0.3 m"
    probe_png.unlink(missing_ok=True)
    print(f"[PASS] 16-bit PNG lossless round-trip max error: {max_err_m:.4f} m (< 0.3 m)")

    # 2) Full 4096x2048 end-to-end export + strict single-source-of-truth render
    t0 = time.perf_counter()
    world = _build_synthetic_world(width=4096, height=2048, seed=777)
    t_gen = time.perf_counter() - t0
    print(f"[INFO] Synthetic 4096x2048 world built in {t_gen:.2f}s (rivers={len(world['rivers'])})")

    t1 = time.perf_counter()
    export_stats = export_world(
        elevation_m=world["elevation_m"],
        rivers=world["rivers"],
        boundary_lines=world["boundary_lines"],
        biome_id=world["biome_id"],
        biome_names=world["biome_names"],
        seed=world["seed"],
        out_dir=test_dir,
    )
    t_exp = time.perf_counter() - t1
    print(
        f"[PASS] export_world ({t_exp:.2f}s): "
        f"land_polygons={export_stats['land_polygons']}, holes={export_stats['holes']}, "
        f"coastline_vertices={export_stats['coastline_vertices']}, total_vertices={export_stats['total_vertices']}, "
        f"rivers={export_stats['river_count']}, "
        f"geojson={export_stats['geojson_bytes'] / (1024 * 1024):.2f} MB, "
        f"elev_png={export_stats['elevation_png_bytes'] / (1024 * 1024):.2f} MB"
    )

    # Verify GeoJSON schema & RFC 7946 invariants directly from disk
    gj_path = Path(export_stats["geojson_path"])
    elev_path = Path(export_stats["elevation_png_path"])
    doc = json.loads(gj_path.read_text(encoding="utf-8"))
    assert doc["type"] == "FeatureCollection"
    props = doc["properties"]
    assert props["width"] == 4096 and props["height"] == 2048
    assert props["elev_min_m"] == ELEV_MIN_M and props["elev_max_m"] == ELEV_MAX_M
    assert export_stats["land_polygons"] >= 10
    assert export_stats["holes"] >= 1
    assert export_stats["coastline_vertices"] >= 40_000, (
        f"Expected >= 40,000 coastline vertices at 4096x2048, got {export_stats['coastline_vertices']}"
    )

    # Verify every ring in GeoJSON is closed, >= 4 pts, valid bounds, RFC 7946 winding
    checked_rings = 0
    for feat in doc["features"]:
        geom = feat["geometry"]
        if geom["type"] in ("Polygon", "MultiPolygon"):
            for poly in _iter_polygon_rings(geom):
                for r_idx, ring in enumerate(poly):
                    assert len(ring) >= 4, f"Ring has < 4 vertices: {len(ring)}"
                    assert ring[0] == ring[-1], "Unclosed ring found"
                    arr = np.asarray(ring[:-1], dtype=np.float64)
                    assert np.all((arr[:, 0] >= -180.0) & (arr[:, 0] <= 180.0)), "Lon out of RFC 7946 range"
                    assert np.all((arr[:, 1] >= -90.0) & (arr[:, 1] <= 90.0)), "Lat out of RFC 7946 range"
                    area = _signed_area(arr)
                    if r_idx == 0:
                        assert area > 0.0, f"Exterior ring not CCW in lon/lat (area={area})"
                    else:
                        assert area < 0.0, f"Hole ring not CW in lon/lat (area={area})"
                    checked_rings += 1
    print(f"[PASS] RFC 7946 GeoJSON schema & winding verified on {checked_rings} polygon rings")

    # 3) Strict single-source-of-truth 4K render + 4 zoomed 1:1 crops
    out_png = test_dir / "world_map_pre_humanoid.png"
    render_stats = render_from_files(
        geojson_path=gj_path,
        elevation_png_path=elev_path,
        biome_rgb=world["biome_rgb"],
        out_png=out_png,
        crops_dir=crops_dir,
    )
    val = render_stats["validation"]
    assert val["passed"], f"render_from_files validation failed: {val}"
    assert val["unclosed_rings"] == 0
    assert val["river_ocean_violations"] == 0
    assert val["river_inland_vertices_checked"] > 0

    for key in ("out_png", "clean_png", "labelled_png"):
        p = Path(render_stats[key])
        assert p.exists() and p.stat().st_size > 100_000, f"Missing or tiny output PNG: {p}"
        with Image.open(p) as im:
            assert im.size == (ATLAS_WIDTH, ATLAS_HEIGHT), f"Expected 4K 3840x1920, got {im.size}"

    assert len(render_stats["crops"]) == 4
    for cm in render_stats["crops"]:
        cp = Path(cm["path"])
        assert cp.exists() and cp.stat().st_size > 10_000, f"Missing or tiny crop: {cp}"
        with Image.open(cp) as im:
            assert im.size == (CROP_WIDTH, CROP_HEIGHT), f"Expected 800x600 crop, got {im.size}"

    print(
        f"[PASS] render_from_files ({render_stats['elapsed_s']:.2f}s): "
        f"atlas={render_stats['atlas_size']}, land_frac={render_stats['land_fraction_geojson']:.4f}, "
        f"river_inland_checked={val['river_inland_vertices_checked']}, "
        f"river_ocean_violations={val['river_ocean_violations']}, "
        f"crops={len(render_stats['crops'])}"
    )
    for cm in render_stats["crops"]:
        print(f"  - {cm['name']}: {cm['path']} center={cm['center_lonlat']} ({cm['metric']})")

    # 4) Negative validator test: verify an ocean-crossing inland river vertex IS flagged
    bad_doc = json.loads(gj_path.read_text(encoding="utf-8"))
    ocean_px = np.argwhere(world["elevation_m"] < -1500.0)
    assert len(ocean_px) > 0
    orow, ocol = int(ocean_px[0, 0]), int(ocean_px[0, 1])
    olon = -180.0 + (ocol + 0.5) / 4096.0 * 360.0
    olat = 90.0 - (orow + 0.5) / 2048.0 * 180.0
    bad_doc["features"].append(
        {
            "type": "Feature",
            "properties": {"layer": "river", "id": "bad_ocean_river", "stream_order": 2, "max_flow": 50.0},
            "geometry": {"type": "LineString", "coordinates": [[olon, olat], [olon + 0.1, olat], [olon + 0.2, olat]]},
        }
    )
    _, _, _, _, bad_val = _validate_and_rasterize_geojson(bad_doc, width=4096, height=2048)
    assert not bad_val["passed"] and bad_val["river_ocean_violations"] >= 1, (
        f"Validator failed to detect injected ocean river vertex: {bad_val}"
    )
    print(f"[PASS] Negative river-ocean validator caught {bad_val['river_ocean_violations']} injected violation(s)")
    print("=== ALL SELF-TESTS PASSED ===")


if __name__ == "__main__":
    _run_self_test()
