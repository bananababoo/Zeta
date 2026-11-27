#!/usr/bin/env python3
"""Generate the 6 cube-face climate maps from the 6 elevation faces.

Single source of truth for the planet climate layer:
  - Input:  assets/world/faces/{face}_elevation.png (1024x1024 16-bit, 32768 = sea level, 1 unit = 1 ft)
  - Output: assets/world/faces/{face}_climate.png   (same size, 8-bit RGBA, same face orientation)

Channel encoding (decoded by zeta_core::world::climate):
  R  mean annual air temperature, linear: byte 0 = -50 C, byte 255 = +40 C
  G  moisture index, linear 0..1 (0 = hyper-arid, 1 = saturated; ocean = 1)
  B  foliage density, linear 0..1 (0 = bare ground, 1 = closed canopy; ocean = 0)
  A  biome override id, 0 = none (nearest-sampled; no ids are defined yet, so always 0)

Inputs to the model are all functions of the 3D surface direction, so every
face edge matches its neighbor without seam fixing:
  - latitude (PosY = north) drives the base temperature and the moisture belts,
  - land elevation lowers temperature by the 6.5 C per km lapse rate,
  - great-circle distance to the nearest ocean dries continental interiors,
  - seeded 3D value noise adds low-frequency variation.

The output is deterministic: the same elevation faces always give byte-identical PNGs.
"""

from __future__ import annotations

import argparse
import math
from pathlib import Path

import numpy as np
from PIL import Image
from scipy.spatial import cKDTree

from build_planet_faces import FACES

SEA_LEVEL_U16 = 32768
TEMPERATURE_MIN_C = -50.0
TEMPERATURE_MAX_C = 40.0
PLANET_RADIUS_KM = 2034.0
LAPSE_RATE_C_PER_FT = 6.5 / 3280.84
OCEAN_TREE_RES = 256
NOISE_SEED = 0x5A_E7A_C11


def face_directions(fwd, right, down, res: int) -> np.ndarray:
    """Unit surface directions for every pixel center of 1 face, shape (res, res, 3)."""
    coords = (np.arange(res, dtype=np.float64) + 0.5) / res * 2.0 - 1.0
    tan_coords = np.tan(coords * (math.pi * 0.25))
    uu, vv = np.meshgrid(tan_coords, tan_coords)
    w = np.stack([fwd[i] + uu * right[i] + vv * down[i] for i in range(3)], axis=-1)
    return w / np.linalg.norm(w, axis=-1, keepdims=True)


def hash3(ix: np.ndarray, iy: np.ndarray, iz: np.ndarray, seed: int) -> np.ndarray:
    """Integer lattice hash to [0, 1) with wrapping uint64 arithmetic."""
    with np.errstate(over="ignore"):
        h = (ix.astype(np.uint64) * np.uint64(0x9E3779B97F4A7C15)
             ^ iy.astype(np.uint64) * np.uint64(0xC2B2AE3D27D4EB4F)
             ^ iz.astype(np.uint64) * np.uint64(0x165667B19E3779F9)
             ^ np.uint64(seed))
        h ^= h >> np.uint64(31)
        h *= np.uint64(0xBF58476D1CE4E5B9)
        h ^= h >> np.uint64(29)
        h *= np.uint64(0x94D049BB133111EB)
        h ^= h >> np.uint64(32)
    return (h >> np.uint64(11)).astype(np.float64) / float(1 << 53)


def value_noise(p: np.ndarray, seed: int) -> np.ndarray:
    """Quintic-faded 3D value noise in [-1, 1]; p has shape (..., 3)."""
    base = np.floor(p)
    f = p - base
    f = f * f * f * (f * (f * 6.0 - 15.0) + 10.0)
    b = base.astype(np.int64)
    out = np.zeros(p.shape[:-1])
    for dx in (0, 1):
        wx = f[..., 0] if dx else 1.0 - f[..., 0]
        for dy in (0, 1):
            wy = f[..., 1] if dy else 1.0 - f[..., 1]
            for dz in (0, 1):
                wz = f[..., 2] if dz else 1.0 - f[..., 2]
                out += wx * wy * wz * hash3(b[..., 0] + dx, b[..., 1] + dy, b[..., 2] + dz, seed)
    return out * 2.0 - 1.0


def fbm(dirs: np.ndarray, base_freq: float, octaves: int, seed: int) -> np.ndarray:
    """Fractal sum of value noise on the unit sphere, roughly in [-1, 1]."""
    total = np.zeros(dirs.shape[:-1])
    amp, norm, freq = 1.0, 0.0, base_freq
    for octave in range(octaves):
        total += amp * value_noise(dirs * freq, seed + octave * 7919)
        norm += amp
        amp *= 0.5
        freq *= 2.0
    return total / norm


def smoothstep(e0: float, e1: float, x: np.ndarray) -> np.ndarray:
    t = np.clip((x - e0) / (e1 - e0), 0.0, 1.0)
    return t * t * (3.0 - 2.0 * t)


def load_elevation_ft(faces_dir: Path, name: str) -> np.ndarray:
    img = np.array(Image.open(faces_dir / f"{name}_elevation.png"), dtype=np.float64)
    return img - SEA_LEVEL_U16


def build_ocean_tree(faces_dir: Path) -> cKDTree:
    """KD tree over unit directions of ocean pixels, subsampled to OCEAN_TREE_RES per face."""
    points = []
    for name, fwd, right, down in FACES:
        elev = load_elevation_ft(faces_dir, name)
        step = elev.shape[0] // OCEAN_TREE_RES
        sub = elev[step // 2::step, step // 2::step]
        dirs = face_directions(fwd, right, down, elev.shape[0])[step // 2::step, step // 2::step]
        points.append(dirs[sub < 0.0])
    return cKDTree(np.concatenate(points))


def climate_face(elev_ft: np.ndarray, dirs: np.ndarray, ocean_tree: cKDTree) -> np.ndarray:
    """Compute the RGBA8 climate texels for 1 face."""
    res = elev_ft.shape[0]
    ocean = elev_ft < 0.0
    land_ft = np.maximum(elev_ft, 0.0)
    abs_lat_deg = np.degrees(np.arcsin(np.clip(np.abs(dirs[..., 1]), 0.0, 1.0)))

    temperature = 27.0 - 52.0 * (abs_lat_deg / 90.0) ** 1.7
    temperature -= land_ft * LAPSE_RATE_C_PER_FT
    temperature += 1.5 * fbm(dirs, 4.0, 3, NOISE_SEED)

    ocean_km = np.zeros((res, res))
    land_idx = np.nonzero(~ocean)
    if land_idx[0].size:
        chord, _ = ocean_tree.query(dirs[land_idx])
        ocean_km[land_idx] = 2.0 * np.arcsin(np.clip(chord / 2.0, 0.0, 1.0)) * PLANET_RADIUS_KM
    belts = np.interp(abs_lat_deg, [0, 10, 25, 35, 50, 60, 75, 90],
                      [1.0, 0.9, 0.15, 0.25, 0.7, 0.65, 0.4, 0.3])
    moisture = belts * (0.25 + 0.75 * np.exp(-ocean_km / 400.0))
    moisture += 0.12 * fbm(dirs, 6.0, 4, NOISE_SEED + 1)
    moisture = np.where(ocean, 1.0, np.clip(moisture, 0.0, 1.0))

    foliage = (smoothstep(-5.0, 8.0, temperature)
               * (1.0 - 0.3 * smoothstep(30.0, 38.0, temperature))
               * smoothstep(0.15, 0.85, moisture)
               * (1.0 - smoothstep(9000.0, 12000.0, land_ft)))
    foliage += 0.1 * fbm(dirs, 12.0, 3, NOISE_SEED + 2)
    foliage = np.where(ocean, 0.0, np.clip(foliage, 0.0, 1.0))

    t_unit = (np.clip(temperature, TEMPERATURE_MIN_C, TEMPERATURE_MAX_C) - TEMPERATURE_MIN_C) / (
        TEMPERATURE_MAX_C - TEMPERATURE_MIN_C)
    rgba = np.zeros((res, res, 4), dtype=np.uint8)
    rgba[..., 0] = np.round(t_unit * 255.0).astype(np.uint8)
    rgba[..., 1] = np.round(moisture * 255.0).astype(np.uint8)
    rgba[..., 2] = np.round(foliage * 255.0).astype(np.uint8)
    return rgba


NET_LAYOUT = {"pos_y": (1, 0), "neg_x": (0, 1), "pos_z": (1, 1), "pos_x": (2, 1), "neg_z": (3, 1), "neg_y": (1, 2)}


def write_preview(faces: dict[str, np.ndarray], path: Path, cell: int = 128) -> None:
    """Write a 2x2 grid of cube nets: temperature, moisture, foliage, RGB composite."""
    def net(channel_img) -> Image.Image:
        canvas = Image.new("RGB", (cell * 4, cell * 3), (40, 40, 40))
        for name, (cx, cy) in NET_LAYOUT.items():
            canvas.paste(channel_img(faces[name]).resize((cell, cell), Image.Resampling.BOX), (cx * cell, cy * cell))
        return canvas

    def ramp(ch: int, lo, hi):
        def make(rgba: np.ndarray) -> Image.Image:
            t = rgba[..., ch].astype(np.float64)[..., None] / 255.0
            return Image.fromarray((np.array(lo) * (1 - t) + np.array(hi) * t).astype(np.uint8))
        return make

    nets = [
        net(ramp(0, (40, 60, 230), (230, 60, 30))),
        net(ramp(1, (210, 180, 120), (30, 90, 200))),
        net(ramp(2, (200, 190, 160), (20, 110, 30))),
        net(lambda rgba: Image.fromarray(rgba[..., :3])),
    ]
    out = Image.new("RGB", (cell * 8 + 8, cell * 6 + 8), (0, 0, 0))
    for i, n in enumerate(nets):
        out.paste(n, ((i % 2) * (cell * 4 + 8), (i // 2) * (cell * 3 + 8)))
    path.parent.mkdir(parents=True, exist_ok=True)
    out.save(path)


def main() -> None:
    root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--faces-dir", type=Path, default=root / "assets" / "world" / "faces")
    parser.add_argument("--preview", type=Path, help="optional PNG path for a cube-net preview")
    args = parser.parse_args()

    ocean_tree = build_ocean_tree(args.faces_dir)
    faces: dict[str, np.ndarray] = {}
    for name, fwd, right, down in FACES:
        elev = load_elevation_ft(args.faces_dir, name)
        rgba = climate_face(elev, face_directions(fwd, right, down, elev.shape[0]), ocean_tree)
        Image.fromarray(rgba).save(args.faces_dir / f"{name}_climate.png", optimize=True)
        faces[name] = rgba
        print(f"{name}_climate.png: {rgba.shape[1]}x{rgba.shape[0]} RGBA8")
    if args.preview:
        write_preview(faces, args.preview)
        print(f"preview: {args.preview}")


if __name__ == "__main__":
    main()
