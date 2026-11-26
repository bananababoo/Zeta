#!/usr/bin/env python3
"""Re-project Sera's equirectangular heightmap onto 6 S2-tangent cube-sphere faces.

Single source of truth for Layer 0 planetary base data:
  - Input:  assets/world/world_elevation.png (4096x2048 16-bit equirectangular)
  - Output: assets/world/faces/{pos_x,neg_x,pos_y,neg_y,pos_z,neg_z}_elevation.png
            (1024x1024 16-bit grayscale PNG, 32768 = exact sea level, 1 unit = 1 ft)

Cube-face basis convention (right-handed, matches zeta_core::world):
  Face   | Forward (+W) | Right (+U) | Down (+V)
  -------+--------------+------------+----------
  pos_x  | (+1,  0,  0) | ( 0,  0,-1) | ( 0,-1, 0)
  neg_x  | (-1,  0,  0) | ( 0,  0,+1) | ( 0,-1, 0)
  pos_y  | ( 0, +1,  0) | (+1,  0, 0) | ( 0, 0,+1)
  neg_y  | ( 0, -1,  0) | (+1,  0, 0) | ( 0, 0,-1)
  pos_z  | ( 0,  0, +1) | (+1,  0, 0) | ( 0,-1, 0)
  neg_z  | ( 0,  0, -1) | (-1,  0, 0) | ( 0,-1, 0)

With S2-style tangent warp: s = tan(pi/4 * u), t = tan(pi/4 * v),
where u, v in [-1, 1] across pixel centers of [0, FACE_RES).
"""

from __future__ import annotations

import argparse
import math
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw

FACE_RES = 1024
SEA_LEVEL_U16 = 32768
MAX_LAND_FT = 28000.0   # ~8,534 m highest peaks
MAX_DEPTH_FT = 30000.0  # ~9,144 m deepest ocean trenches

# (name, forward, right_u, down_v)
FACES: list[tuple[str, tuple[float, float, float], tuple[float, float, float], tuple[float, float, float]]] = [
    ("pos_x", (1.0, 0.0, 0.0), (0.0, 0.0, -1.0), (0.0, -1.0, 0.0)),
    ("neg_x", (-1.0, 0.0, 0.0), (0.0, 0.0, 1.0), (0.0, -1.0, 0.0)),
    ("pos_y", (0.0, 1.0, 0.0), (1.0, 0.0, 0.0), (0.0, 0.0, 1.0)),
    ("neg_y", (0.0, -1.0, 0.0), (1.0, 0.0, 0.0), (0.0, 0.0, -1.0)),
    ("pos_z", (0.0, 0.0, 1.0), (1.0, 0.0, 0.0), (0.0, -1.0, 0.0)),
    ("neg_z", (0.0, 0.0, -1.0), (-1.0, 0.0, 0.0), (0.0, -1.0, 0.0)),
]


def detect_source_sea_level(elev: np.ndarray, clean_map_path: Path | None) -> float:
    """Detect exact sea level threshold in source 16-bit equirectangular heightmap."""
    if clean_map_path is not None and clean_map_path.exists():
        # In world_map_clean.png, ocean pixels have B > R + 15 while land has green/brown/tan/white.
        rgb = np.array(Image.open(clean_map_path).convert("RGB"))
        if rgb.shape[:2] == elev.shape[:2]:
            is_ocean = (rgb[:, :, 2].astype(np.int16) - rgb[:, :, 0].astype(np.int16)) > 15
            ocean_frac = float(is_ocean.mean())
            sea_val = float(np.quantile(elev, ocean_frac))
            print(f"Calibrated sea level from {clean_map_path.name}: ocean={ocean_frac*100:.2f}%, threshold={sea_val:.1f}")
            return sea_val
    # Fallback: Sera.md specifies ~35% land (65% ocean)
    sea_val = float(np.quantile(elev, 0.65))
    print(f"Fallback sea level (65% ocean): threshold={sea_val:.1f}")
    return sea_val


def bilinear_sample_equirect(src: np.ndarray, lon: np.ndarray, lat: np.ndarray) -> np.ndarray:
    """Bilinear sample equirectangular grid where lon in [-pi, pi], lat in [-pi/2, pi/2]."""
    h, w = src.shape
    fx = ((lon + math.pi) / (2.0 * math.pi)) * w - 0.5
    fy = ((math.pi * 0.5 - lat) / math.pi) * h - 0.5

    x0 = np.floor(fx).astype(np.int64)
    y0 = np.floor(fy).astype(np.int64)
    dx = fx - x0
    dy = fy - y0

    x0_w = x0 % w
    x1_w = (x0 + 1) % w
    y0_c = np.clip(y0, 0, h - 1)
    y1_c = np.clip(y0 + 1, 0, h - 1)

    c00 = src[y0_c, x0_w]
    c10 = src[y0_c, x1_w]
    c01 = src[y1_c, x0_w]
    c11 = src[y1_c, x1_w]

    top = c00 * (1.0 - dx) + c10 * dx
    bot = c01 * (1.0 - dx) + c11 * dx
    return top * (1.0 - dy) + bot * dy


def project_face(
    src_ft: np.ndarray,
    fwd: tuple[float, float, float],
    right: tuple[float, float, float],
    down: tuple[float, float, float],
    res: int,
) -> np.ndarray:
    """Sample source elevation (in feet, 0 = sea level) onto 1 tangent-warped cube face."""
    # Pixel centers in [-1, 1]
    coords = (np.arange(res, dtype=np.float64) + 0.5) / res * 2.0 - 1.0
    tan_coords = np.tan(coords * (math.pi * 0.25))
    uu, vv = np.meshgrid(tan_coords, tan_coords)  # shape (res, res): [y, x]

    wx = fwd[0] + uu * right[0] + vv * down[0]
    wy = fwd[1] + uu * right[1] + vv * down[1]
    wz = fwd[2] + uu * right[2] + vv * down[2]

    inv_len = 1.0 / np.sqrt(wx * wx + wy * wy + wz * wz)
    nx = wx * inv_len
    ny = wy * inv_len
    nz = wz * inv_len

    lat = np.arcsin(np.clip(ny, -1.0, 1.0))
    lon = np.arctan2(nx, nz)  # matches standard equirectangular center at +Z

    return bilinear_sample_equirect(src_ft, lon, lat)


def render_shaded_face(elev_u16: np.ndarray, land_only: bool = False) -> Image.Image:
    """Render a quick visual preview of a face (land/ocean mask or shaded heightmap)."""
    ft = elev_u16.astype(np.float64) - SEA_LEVEL_U16
    is_land = elev_u16 >= SEA_LEVEL_U16
    h, w = elev_u16.shape
    rgb = np.zeros((h, w, 3), dtype=np.uint8)

    if land_only:
        rgb[~is_land] = (28, 58, 92)
        rgb[is_land] = (176, 194, 134)
        return Image.fromarray(rgb, mode="RGB")

    # Ocean bathymetry gradient
    depth = np.clip(-ft / MAX_DEPTH_FT, 0.0, 1.0)
    rgb[~is_land, 0] = (18 + (1.0 - depth[~is_land]) * 38).astype(np.uint8)
    rgb[~is_land, 1] = (42 + (1.0 - depth[~is_land]) * 78).astype(np.uint8)
    rgb[~is_land, 2] = (78 + (1.0 - depth[~is_land]) * 86).astype(np.uint8)

    # Land hypsometric tint + simple hillshade
    land_norm = np.clip(ft / MAX_LAND_FT, 0.0, 1.0)
    gy, gx = np.gradient(ft / 600.0)
    shade = np.clip(0.82 - gx * 0.35 - gy * 0.35, 0.45, 1.2)

    # Lowland green -> highland ochre -> alpine rock -> snow
    r = np.interp(land_norm, [0.0, 0.18, 0.45, 0.72, 1.0], [92, 162, 168, 142, 238])
    g = np.interp(land_norm, [0.0, 0.18, 0.45, 0.72, 1.0], [136, 166, 132, 130, 240])
    b = np.interp(land_norm, [0.0, 0.18, 0.45, 0.72, 1.0], [74, 102, 92, 122, 244])

    rgb[is_land, 0] = np.clip(r[is_land] * shade[is_land], 0, 255).astype(np.uint8)
    rgb[is_land, 1] = np.clip(g[is_land] * shade[is_land], 0, 255).astype(np.uint8)
    rgb[is_land, 2] = np.clip(b[is_land] * shade[is_land], 0, 255).astype(np.uint8)
    return Image.fromarray(rgb, mode="RGB")


def build_cube_net_preview(faces: dict[str, np.ndarray], out_path: Path, land_only: bool) -> None:
    """Arrange the 6 cube faces in a standard unfolded cube cross net (4x3 grid)."""
    tile = 384
    canvas = Image.new("RGB", (tile * 4, tile * 3), (18, 20, 24))
    draw = ImageDraw.Draw(canvas)
    layout = {
        "pos_y": (1, 0),
        "neg_x": (0, 1),
        "pos_z": (1, 1),
        "pos_x": (2, 1),
        "neg_z": (3, 1),
        "neg_y": (1, 2),
    }
    for name, (gx, gy) in layout.items():
        img = render_shaded_face(faces[name], land_only=land_only).resize((tile, tile), Image.Resampling.BILINEAR)
        canvas.paste(img, (gx * tile, gy * tile))
        draw.rectangle([gx * tile, gy * tile, (gx + 1) * tile - 1, (gy + 1) * tile - 1], outline=(220, 210, 180), width=1)
        draw.text((gx * tile + 8, gy * tile + 6), name, fill=(255, 248, 220))
    out_path.parent.mkdir(parents=True, exist_ok=True)
    canvas.save(out_path)


def main() -> None:
    parser = argparse.ArgumentParser(description="Build 6 cube-sphere 16-bit elevation faces for Sera.")
    parser.add_argument("--src", type=Path, default=Path("assets/world/world_elevation.png"))
    parser.add_argument("--clean-map", type=Path, default=Path("assets/world/world_map_clean.png"))
    parser.add_argument("--out-dir", type=Path, default=Path("assets/world/faces"))
    parser.add_argument("--preview-dir", type=Path, default=None)
    parser.add_argument("--res", type=int, default=FACE_RES)
    args = parser.parse_args()

    src_u16 = np.array(Image.open(args.src), dtype=np.float64)
    sea_raw = detect_source_sea_level(src_u16, args.clean_map)
    max_raw = float(src_u16.max())
    min_raw = float(src_u16.min())

    # Convert raw heightmap into feet relative to sea level (0.0 ft = sea level)
    src_ft = np.where(
        src_u16 >= sea_raw,
        (src_u16 - sea_raw) / max(1.0, max_raw - sea_raw) * MAX_LAND_FT,
        -(sea_raw - src_u16) / max(1.0, sea_raw - min_raw) * MAX_DEPTH_FT,
    )

    args.out_dir.mkdir(parents=True, exist_ok=True)
    faces_out: dict[str, np.ndarray] = {}
    total_land = 0
    total_px = 0

    for name, fwd, right, down in FACES:
        face_ft = project_face(src_ft, fwd, right, down, args.res)
        face_u16 = np.clip(np.rint(face_ft + SEA_LEVEL_U16), 0, 65535).astype(np.uint16)
        faces_out[name] = face_u16
        out_file = args.out_dir / f"{name}_elevation.png"
        Image.fromarray(face_u16, mode="I;16").save(out_file)
        land_px = int((face_u16 >= SEA_LEVEL_U16).sum())
        total_land += land_px
        total_px += face_u16.size
        print(
            f"Wrote {out_file} ({args.res}x{args.res}): "
            f"land={land_px / face_u16.size * 100:.1f}%, "
            f"min={int(face_u16.min()) - SEA_LEVEL_U16}ft, max={int(face_u16.max()) - SEA_LEVEL_U16}ft"
        )

    print(f"Total planetary land share across 6 cube faces: {total_land / total_px * 100:.2f}%")

    if args.preview_dir is not None:
        build_cube_net_preview(faces_out, args.preview_dir / "cube_net_land_ocean.png", land_only=True)
        build_cube_net_preview(faces_out, args.preview_dir / "cube_net_elevation.png", land_only=False)
        print(f"Wrote cube-net previews to {args.preview_dir}")


if __name__ == "__main__":
    main()
