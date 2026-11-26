"""Reprojects the equirectangular 4K world atlas into an Equal Earth true-area projection.

Equal Earth (Savric, Patterson & Jenny, 2018) is strictly equal-area: equal areas on the
sphere map to equal pixel areas everywhere from the equator to the poles.
"""

from __future__ import annotations

import json
import math
from pathlib import Path

import cv2
import numpy as np
from PIL import Image, ImageDraw, ImageFont

# Equal Earth polynomial constants (Savric et al. 2018)
A1 = 1.340264
A2 = -0.081106
A3 = 0.000893
A4 = 0.003796
M_CONST = math.sqrt(3.0) / 2.0

# Exact Equal Earth projected bounds: x in [-X_MAX, +X_MAX], y in [-Y_MAX, +Y_MAX]
THETA_MAX = math.asin(M_CONST)  # pi / 3
Y_MAX = THETA_MAX * (A1 + A2 * THETA_MAX**2 + THETA_MAX**6 * (A3 + A4 * THETA_MAX**2))
X_MAX = (2.0 * math.sqrt(3.0) * math.pi) / (3.0 * A1)  # at theta = 0, lambda = pi


def equal_earth_forward(lon_rad: np.ndarray | float, lat_rad: np.ndarray | float):
    """Forward Equal Earth projection: (lon, lat) in radians -> (x, y)."""
    theta = np.arcsin(np.clip(M_CONST * np.sin(lat_rad), -1.0, 1.0))
    t2 = theta * theta
    t6 = t2 * t2 * t2
    poly = A1 + A2 * t2 + t6 * (A3 + A4 * t2)
    dpoly = A1 + 3.0 * A2 * t2 + t6 * (7.0 * A3 + 9.0 * A4 * t2)
    x = (2.0 * math.sqrt(3.0) * lon_rad * np.cos(theta)) / (3.0 * dpoly)
    y = theta * poly
    return x, y


def equal_earth_inverse_grid(out_w: int, out_h: int, pad: float = 0.045):
    """Computes inverse Equal Earth mapping (out_y, out_x) -> (lon_deg, lat_deg, inside_mask)."""
    # Preserve exact 1:1 Equal Earth aspect ratio (X_MAX / Y_MAX ~ 2.0545)
    aspect = X_MAX / Y_MAX
    inner_w = out_w * (1.0 - 2.0 * pad)
    inner_h = out_h * (1.0 - 2.0 * pad)
    if inner_w / inner_h > aspect:
        scale_y = Y_MAX / (inner_h * 0.5)
        scale_x = scale_y
    else:
        scale_x = X_MAX / (inner_w * 0.5)
        scale_y = scale_x

    yy, xx = np.mgrid[0:out_h, 0:out_w].astype(np.float64)
    px = (xx - (out_w - 1) * 0.5) * scale_x
    py = ((out_h - 1) * 0.5 - yy) * scale_y  # north = +y

    # Solve y = theta * (A1 + A2*t^2 + t^6*(A3 + A4*t^2)) via Newton-Raphson (7 iters -> 1e-13)
    valid_y = np.abs(py) <= Y_MAX
    y_clamped = np.clip(py, -Y_MAX, Y_MAX)
    theta = y_clamped / A1
    for _ in range(8):
        t2 = theta * theta
        t6 = t2 * t2 * t2
        f = theta * (A1 + A2 * t2 + t6 * (A3 + A4 * t2)) - y_clamped
        df = A1 + 3.0 * A2 * t2 + t6 * (7.0 * A3 + 9.0 * A4 * t2)
        theta = theta - f / df

    t2 = theta * theta
    t6 = t2 * t2 * t2
    dpoly = A1 + 3.0 * A2 * t2 + t6 * (7.0 * A3 + 9.0 * A4 * t2)
    lon_rad = (3.0 * px * dpoly) / (2.0 * math.sqrt(3.0) * np.maximum(np.cos(theta), 1e-9))
    sin_lat = np.sin(theta) / M_CONST
    inside = valid_y & (np.abs(lon_rad) <= math.pi) & (np.abs(sin_lat) <= 1.0)
    lat_rad = np.arcsin(np.clip(sin_lat, -1.0, 1.0))

    lon_deg = np.degrees(lon_rad)
    lat_deg = np.degrees(lat_rad)
    return lon_deg, lat_deg, inside, scale_x, scale_y


def project_point(lon_deg: float, lat_deg: float, out_w: int, out_h: int, scale_x: float, scale_y: float):
    x, y = equal_earth_forward(math.radians(lon_deg), math.radians(lat_deg))
    px = (out_w - 1) * 0.5 + float(x) / scale_x
    py = (out_h - 1) * 0.5 - float(y) / scale_y
    return px, py


def measure_major_landmasses(geojson_path: Path) -> list[dict]:
    """Measures true spherical area (Earth-radius km^2 and % of total land) of major land polygons."""
    gj = json.loads(geojson_path.read_text(encoding="utf-8"))
    w = int(gj["properties"]["width"])
    h = int(gj["properties"]["height"])
    earth_r_km = 6371.0
    earth_area_km2 = 4.0 * math.pi * earth_r_km**2

    # Spherical area weight per row in pixel units (sums over whole globe to earth_area_km2)
    rows = np.arange(h, dtype=np.float64)
    lat_top = np.radians(90.0 - (rows / h) * 180.0)
    lat_bot = np.radians(90.0 - ((rows + 1.0) / h) * 180.0)
    row_cell_km2 = (earth_area_km2 / (2.0 * w)) * np.abs(np.sin(lat_top) - np.sin(lat_bot))

    total_land_km2 = 0.0
    landmasses = []
    for idx, feat in enumerate(gj["features"]):
        if feat["properties"].get("layer") != "land":
            continue
        rings = feat["geometry"]["coordinates"]
        outer = np.array(rings[0], dtype=np.float64)
        min_lon, max_lon = outer[:, 0].min(), outer[:, 0].max()
        min_lat, max_lat = outer[:, 1].min(), outer[:, 1].max()

        # Rasterize into tight local bbox for exact spherical area
        c0 = max(0, int(math.floor((min_lon + 180.0) / 360.0 * w)) - 1)
        c1 = min(w, int(math.ceil((max_lon + 180.0) / 360.0 * w)) + 2)
        r0 = max(0, int(math.floor((90.0 - max_lat) / 180.0 * h)) - 1)
        r1 = min(h, int(math.ceil((90.0 - min_lat) / 180.0 * h)) + 2)
        bw, bh = max(1, c1 - c0), max(1, r1 - r0)
        mask = np.zeros((bh, bw), dtype=np.uint8)

        def to_local(ring):
            arr = np.asarray(ring, dtype=np.float64)
            lx = (arr[:, 0] + 180.0) / 360.0 * w - c0
            ly = (90.0 - arr[:, 1]) / 180.0 * h - r0
            return np.round(np.stack([lx, ly], axis=1)).astype(np.int32)

        cv2.fillPoly(mask, [to_local(rings[0])], 255)
        for hole in rings[1:]:
            cv2.fillPoly(mask, [to_local(hole)], 0)

        row_counts = (mask > 0).sum(axis=1).astype(np.float64)
        area_km2 = float(np.sum(row_counts * row_cell_km2[r0:r1]))
        total_land_km2 += area_km2

        ys, xs = np.nonzero(mask)
        if len(xs) > 0:
            # Area-weighted centroid inside the polygon
            weights = row_cell_km2[r0 + ys]
            clon = float(-180.0 + (np.average(c0 + xs + 0.5, weights=weights) / w) * 360.0)
            clat = float(90.0 - (np.average(r0 + ys + 0.5, weights=weights) / h) * 180.0)
        else:
            clon, clat = float(outer[:, 0].mean()), float(outer[:, 1].mean())

        landmasses.append({
            "id": feat.get("id") or feat["properties"].get("id") or f"land_{idx}",
            "area_km2": round(area_km2, 0),
            "centroid_lon": round(clon, 2),
            "centroid_lat": round(clat, 2),
            "holes": len(rings) - 1,
        })

    landmasses.sort(key=lambda d: d["area_km2"], reverse=True)
    for item in landmasses:
        item["pct_of_land"] = round(100.0 * item["area_km2"] / max(1.0, total_land_km2), 2)
        item["pct_of_globe"] = round(100.0 * item["area_km2"] / earth_area_km2, 2)
    return landmasses


def render_equal_earth(
    src_clean_png: Path,
    geojson_path: Path,
    out_clean_png: Path,
    out_numbered_png: Path,
) -> list[dict]:
    src = np.asarray(Image.open(src_clean_png).convert("RGB"))
    sh, sw = src.shape[:2]

    out_w, out_h = 3840, 1920
    lon_deg, lat_deg, inside, scale_x, scale_y = equal_earth_inverse_grid(out_w, out_h, pad=0.055)

    # Map (lon, lat) -> source pixel coordinates with cubic interpolation
    map_x = ((lon_deg + 180.0) / 360.0 * sw - 0.5).astype(np.float32)
    map_y = ((90.0 - lat_deg) / 180.0 * sh - 0.5).astype(np.float32)
    remapped = cv2.remap(
        src,
        map_x,
        map_y,
        interpolation=cv2.INTER_CUBIC,
        borderMode=cv2.BORDER_WRAP,
    )

    # Deep neutral slate background outside the Equal Earth globe oval
    bg = np.array([15, 20, 28], dtype=np.uint8)
    canvas = np.where(inside[..., None], remapped, bg)

    img = Image.fromarray(canvas, mode="RGB").convert("RGBA")
    overlay = Image.new("RGBA", (out_w, out_h), (0, 0, 0, 0))
    draw = ImageDraw.Draw(overlay)

    # Curved Equal Earth graticule (every 30 deg lon, 30 deg lat)
    for lat in (-60, -30, 0, 30, 60):
        pts = [project_point(lon, lat, out_w, out_h, scale_x, scale_y) for lon in np.linspace(-180, 180, 361)]
        alpha = 68 if lat == 0 else 32
        draw.line(pts, fill=(235, 244, 252, alpha), width=2 if lat == 0 else 1)

    for lon in range(-180, 181, 30):
        pts = [project_point(lon, lat, out_w, out_h, scale_x, scale_y) for lat in np.linspace(-90, 90, 181)]
        is_rim = abs(lon) == 180
        draw.line(pts, fill=(210, 224, 238, 170 if is_rim else 30), width=3 if is_rim else 1)

    # Top and bottom polar boundary lines
    for lat in (-90, 90):
        pts = [project_point(lon, lat, out_w, out_h, scale_x, scale_y) for lon in np.linspace(-180, 180, 181)]
        draw.line(pts, fill=(210, 224, 238, 170), width=3)

    try:
        font_title = ImageFont.truetype("arialbd.ttf", 34)
        font_sub = ImageFont.truetype("arial.ttf", 20)
        font_tick = ImageFont.truetype("arial.ttf", 18)
        font_badge = ImageFont.truetype("arialbd.ttf", 30)
    except OSError:
        font_title = font_sub = font_tick = font_badge = ImageFont.load_default()

    # Latitude labels along left/right rim
    for lat in (-60, -30, 0, 30, 60):
        lbl = "EQ" if lat == 0 else f"{abs(lat)}°{'N' if lat > 0 else 'S'}"
        lx, ly = project_point(-180, lat, out_w, out_h, scale_x, scale_y)
        rx, ry = project_point(180, lat, out_w, out_h, scale_x, scale_y)
        draw.text((lx - 14, ly), lbl, font=font_tick, fill=(190, 206, 222, 220), anchor="rm")
        draw.text((rx + 14, ry), lbl, font=font_tick, fill=(190, 206, 222, 220), anchor="lm")

    # Title cartouche (bottom-left)
    draw.rectangle([(48, out_h - 140), (920, out_h - 42)], fill=(12, 18, 26, 220), outline=(180, 176, 162, 165), width=2)
    draw.text((70, out_h - 122), "ZETA  ·  EQUAL EARTH TRUE-AREA PROJECTION", font=font_title, fill=(244, 238, 224, 250))
    draw.text((70, out_h - 76), "Equal pixel area = equal surface area everywhere (35.0% spherical landmass, seed 3)", font=font_sub, fill=(194, 208, 220, 235))

    clean_out = Image.alpha_composite(img, overlay).convert("RGB")
    clean_out.save(out_clean_png, quality=96)

    # Numbered overlay showing the top 8 largest continents/landmasses with true area rank
    landmasses = measure_major_landmasses(geojson_path)
    num_overlay = Image.new("RGBA", (out_w, out_h), (0, 0, 0, 0))
    ndraw = ImageDraw.Draw(num_overlay)

    top_n = landmasses[:8]
    for rank, item in enumerate(top_n, start=1):
        px, py = project_point(item["centroid_lon"], item["centroid_lat"], out_w, out_h, scale_x, scale_y)
        r = 26
        ndraw.ellipse([(px - r, py - r), (px + r, py + r)], fill=(16, 24, 34, 225), outline=(248, 236, 198, 245), width=3)
        ndraw.text((px, py), str(rank), font=font_badge, fill=(252, 244, 214, 255), anchor="mm")

    numbered_out = Image.alpha_composite(clean_out.convert("RGBA"), num_overlay).convert("RGB")
    numbered_out.save(out_numbered_png, quality=96)
    return top_n


if __name__ == "__main__":
    repo = Path(r"c:\Users\olive\Documents\Zeta")
    art = Path(r"C:\Users\olive\.gemini\antigravity\brain\10d9cbe5-489a-4c39-93e5-0f025910730d")
    top = render_equal_earth(
        src_clean_png=repo / "assets/world/world_map_clean.png",
        geojson_path=repo / "assets/world/world_map.geojson",
        out_clean_png=repo / "assets/world/world_map_equal_earth.png",
        out_numbered_png=art / "world_map_equal_earth_numbered.png",
    )
    # Also copy clean Equal Earth PNG to artifacts directory
    Image.open(repo / "assets/world/world_map_equal_earth.png").save(art / "world_map_equal_earth.png")
    print("Top 8 landmasses by true spherical area (Earth-sized planet baseline):")
    for idx, item in enumerate(top, start=1):
        print(
            f"  #{idx} ({item['id']}): {item['area_km2']:,.0f} km^2 | "
            f"{item['pct_of_land']:.1f}% of land | centroid=({item['centroid_lon']:.1f}, {item['centroid_lat']:.1f}) | holes={item['holes']}"
        )
