"""Curate, clean, harmonize, and synthesize painterly tabletop battlemap terrain assets.

Subcommands:
    python tools/curate_terrain_assets.py build [--warehouse <dir>] [--stage-dir <dir>]
        [--manifest-out <path>] [--catalog-sheet <path>]
    python tools/curate_terrain_assets.py sheets [--warehouse <dir>] [--out-dir <dir>]

Every asset produced in `assets/terrain/clutter/` and `assets/terrain/palette/` is:
  - 90-degree overhead painterly tabletop battlemap art (`docs/design/visual_style_guide.md` §3)
  - Open-licensed / Game-Director-approved redistributable warehouse source or procedural synthesis
  - Alpha-cleaned and edge-decontaminated (premultiplied-safe RGB under alpha=0)
  - Explicitly attributed in `ATTRIBUTIONS.md`
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
import math
from pathlib import Path
import shutil
from typing import Callable

import numpy as np
from PIL import Image, ImageDraw
from scipy import ndimage

REPO_ROOT = Path(__file__).resolve().parent.parent
CLUTTER_DIR = REPO_ROOT / "assets" / "terrain" / "clutter"
PALETTE_DIR = REPO_ROOT / "assets" / "terrain" / "palette"
DEFAULT_WAREHOUSE = REPO_ROOT / "assets" / "library"
FALLBACK_WAREHOUSE = Path(r"C:\Users\olive\Documents\Zeta\assets\library")

PALETTE_SIZE = 512
INK_COLOR = np.array([27.0, 24.0, 20.0], dtype=np.float32)


# ---------------------------------------------------------------------------
# Deterministic Noise & Seamless Texture Primitives
# ---------------------------------------------------------------------------


def periodic_fbm(
    size: int,
    min_freq: int = 2,
    max_freq: int = 96,
    beta: float = 1.75,
    anisotropy: tuple[float, float] = (1.0, 1.0),
    angle_rad: float = 0.0,
    seed: int = 1,
) -> np.ndarray:
    """Exact 2D periodic (toroidal) fractal noise in [0, 1] via spectral synthesis."""
    rng = np.random.default_rng(seed)
    ky = np.fft.fftfreq(size) * size
    kx = np.fft.rfftfreq(size) * size
    gx, gy = np.meshgrid(kx, ky)

    ca, sa = math.cos(angle_rad), math.sin(angle_rad)
    rx = (gx * ca + gy * sa) / max(anisotropy[0], 1e-3)
    ry = (-gx * sa + gy * ca) / max(anisotropy[1], 1e-3)
    k = np.hypot(rx, ry)

    amp = np.zeros_like(k, dtype=np.float64)
    mask = (k >= min_freq) & (k <= max_freq)
    amp[mask] = np.power(k[mask], -beta)

    phases = rng.uniform(0.0, 2.0 * math.pi, size=k.shape)
    mag = rng.normal(0.0, 1.0, size=k.shape) * amp
    spectrum = mag * (np.cos(phases) + 1j * np.sin(phases))
    field = np.fft.irfft2(spectrum, s=(size, size)).astype(np.float32)
    lo, hi = float(field.min()), float(field.max())
    if hi - lo < 1e-6:
        return np.full((size, size), 0.5, dtype=np.float32)
    return (field - lo) / (hi - lo)


def warped_fbm(size: int, seed: int, min_freq: int = 2, max_freq: int = 64, warp_px: float = 18.0) -> np.ndarray:
    """Domain-warped periodic fBm for organic painterly variation."""
    base = periodic_fbm(size, min_freq=min_freq, max_freq=max_freq, beta=1.65, seed=seed)
    wx = (periodic_fbm(size, min_freq=2, max_freq=16, beta=1.9, seed=seed + 101) - 0.5) * 2.0 * warp_px
    wy = (periodic_fbm(size, min_freq=2, max_freq=16, beta=1.9, seed=seed + 202) - 0.5) * 2.0 * warp_px
    yy, xx = np.meshgrid(np.arange(size, dtype=np.float32), np.arange(size, dtype=np.float32), indexing="ij")
    coords = np.array([(yy + wy) % size, (xx + wx) % size])
    warped = ndimage.map_coordinates(base, coords, order=1, mode="wrap")
    lo, hi = float(warped.min()), float(warped.max())
    return (warped - lo) / max(hi - lo, 1e-6)


def make_toroidal_seamless(rgb: np.ndarray, flatten_sigma: float = 42.0) -> np.ndarray:
    """Convert any RGB image into an exact wrap-seamless 512x512 RGB array with flattened macro blotches."""
    h, w, _ = rgb.shape
    if (w, h) != (PALETTE_SIZE, PALETTE_SIZE):
        im = Image.fromarray(np.clip(rgb, 0, 255).astype(np.uint8), mode="RGB")
        rgb = np.asarray(im.resize((PALETTE_SIZE, PALETTE_SIZE), Image.Resampling.LANCZOS), dtype=np.float32)
    else:
        rgb = rgb.astype(np.float32)

    # Flatten low-frequency illumination lumps that cause visible 2x2 checker grids
    if flatten_sigma > 0:
        low = np.stack([ndimage.gaussian_filter(rgb[..., c], sigma=flatten_sigma, mode="reflect") for c in range(3)], axis=-1)
        mean_col = rgb.mean(axis=(0, 1), keepdims=True)
        rgb = rgb - low + mean_col

    # Dual-offset raised-cosine diamond/radial blend -> exact periodic wrap at x=0/512 and y=0/512
    n = PALETTE_SIZE
    u = np.arange(n, dtype=np.float32) / n
    wx = 0.5 - 0.5 * np.cos(2.0 * math.pi * u)
    wy = 0.5 - 0.5 * np.cos(2.0 * math.pi * u)
    weight = np.outer(wy, wx)[..., None]  # 0 on all 4 borders, 1 at center!
    # Normalize blend power so variance stays constant across center and seam
    shifted = np.roll(np.roll(rgb, n // 2, axis=0), n // 2, axis=1)
    mean_c = rgb.mean(axis=(0, 1), keepdims=True)
    d1 = rgb - mean_c
    d2 = shifted - mean_c
    denom = np.sqrt(weight * weight + (1.0 - weight) * (1.0 - weight))
    blended = mean_c + (weight * d1 + (1.0 - weight) * d2) / denom
    return np.clip(blended, 0.0, 255.0)


# ---------------------------------------------------------------------------
# Alpha Cleaning, Edge Decontamination & Painterly Color Harmonization
# ---------------------------------------------------------------------------


def decontaminate_rgba(rgba: np.ndarray, alpha_cutoff: int = 12) -> np.ndarray:
    """Strip edge halos and propagate clean interior RGB into semi-transparent and zero-alpha texels."""
    arr = rgba.astype(np.float32).copy()
    alpha = arr[..., 3]
    # Clean faint alpha dust
    alpha = np.where(alpha < alpha_cutoff, 0.0, alpha)
    # Soft ramp just above cutoff to avoid jagged 1-bit fringes
    ramp = np.clip((alpha - alpha_cutoff) / max(28.0 - alpha_cutoff, 1.0), 0.0, 1.0)
    alpha = np.where(alpha < 28.0, alpha * ramp, alpha)

    core = alpha >= 210.0
    eroded = ndimage.binary_erosion(core, iterations=2)
    if eroded.sum() < 16:
        eroded = alpha >= 128.0
    if eroded.sum() >= 4:
        _, indices = ndimage.distance_transform_edt(~eroded, return_indices=True)
        nearest_rgb = arr[..., :3][indices[0], indices[1]]
        # Replace RGB in all non-eroded pixels (semi-transparent border + alpha=0) with clean interior color
        blend = np.clip((alpha - 90.0) / 130.0, 0.0, 1.0)[..., None]
        blend = np.where(eroded[..., None], 1.0, blend * 0.65)
        arr[..., :3] = arr[..., :3] * blend + nearest_rgb * (1.0 - blend)

    arr[..., 3] = np.clip(alpha, 0.0, 255.0)
    return np.clip(arr, 0.0, 255.0).astype(np.uint8)


def trim_and_fit_rgba(rgba: np.ndarray, max_dim: int, pad_frac: float = 0.05) -> np.ndarray:
    """Trim excess transparent margin while preserving aspect ratio and sensible center."""
    alpha = rgba[..., 3]
    ys, xs = np.nonzero(alpha > 10)
    if len(xs) == 0:
        return rgba
    y0, y1 = int(ys.min()), int(ys.max()) + 1
    x0, x1 = int(xs.min()), int(xs.max()) + 1
    cropped = rgba[y0:y1, x0:x1]
    ch, cw = cropped.shape[:2]
    pad_x = max(4, int(cw * pad_frac))
    pad_y = max(4, int(ch * pad_frac))
    padded = np.zeros((ch + 2 * pad_y, cw + 2 * pad_y if False else cw + 2 * pad_x, 4), dtype=np.uint8)
    padded[pad_y : pad_y + ch, pad_x : pad_x + cw] = cropped
    # Propagate edge RGB into the new padding border so alpha=0 stays clean
    padded = decontaminate_rgba(padded, alpha_cutoff=10)

    ph, pw = padded.shape[:2]
    if max(ph, pw) > max_dim:
        scale = max_dim / float(max(ph, pw))
        nw, nh = max(16, int(round(pw * scale))), max(16, int(round(ph * scale)))
        # Premultiplied Lanczos downsample prevents dark/bright fringes!
        flt = padded.astype(np.float32)
        a01 = flt[..., 3:4] / 255.0
        pre = np.concatenate([flt[..., :3] * a01, flt[..., 3:4]], axis=-1)
        pre_im = [
            np.asarray(
                Image.fromarray(np.clip(pre[..., c], 0, 255).astype(np.uint8), mode="L").resize(
                    (nw, nh), Image.Resampling.LANCZOS
                ),
                dtype=np.float32,
            )
            for c in range(4)
        ]
        pre_s = np.stack(pre_im, axis=-1)
        a_out = np.clip(pre_s[..., 3:4], 0.0, 255.0)
        safe_denom = np.maximum(a_out / 255.0, 1.0 / 255.0)
        rgb_out = np.where(a_out > 1.0, pre_s[..., :3] / safe_denom, 0.0)
        out = np.concatenate([np.clip(rgb_out, 0, 255), a_out], axis=-1).astype(np.uint8)
        return decontaminate_rgba(out, alpha_cutoff=10)
    return padded


def harmonize_painterly_sprite(
    rgba: np.ndarray,
    sat_scale: float = 1.0,
    val_scale: float = 1.0,
    gamma: float = 1.0,
    warm_shift: tuple[float, float, float] = (1.0, 1.0, 1.0),
    ink_strength: float = 0.42,
    ao_rim: float = 0.18,
) -> np.ndarray:
    """Harmonize color/value to Zeta's tabletop palette and add subtle hand-inked contour cohesion."""
    arr = rgba.astype(np.float32)
    rgb = arr[..., :3] / 255.0
    alpha = arr[..., 3] / 255.0

    # Saturation adjustment in perceptual luma space
    luma = 0.299 * rgb[..., 0] + 0.587 * rgb[..., 1] + 0.114 * rgb[..., 2]
    chroma = rgb - luma[..., None]
    # Tame radioactive neon greens (where G vastly exceeds R and B)
    neon_green = np.clip((rgb[..., 1] - np.maximum(rgb[..., 0], rgb[..., 2]) - 0.18) * 2.2, 0.0, 0.45)
    rgb = luma[..., None] + chroma * sat_scale * (1.0 - 0.45 * neon_green[..., None])
    # Warm olive shift on overly electric greens
    rgb[..., 0] += neon_green * 0.08
    rgb[..., 2] -= neon_green * 0.05

    rgb = np.clip(rgb, 0.0, 1.0)
    if gamma != 1.0:
        rgb = np.power(rgb, gamma)
    rgb *= val_scale
    rgb[..., 0] *= warm_shift[0]
    rgb[..., 1] *= warm_shift[1]
    rgb[..., 2] *= warm_shift[2]

    # Subtle painterly micro-sharpen (unsharp mask on interior RGB)
    blur = np.stack([ndimage.gaussian_filter(rgb[..., c], sigma=1.2) for c in range(3)], axis=-1)
    rgb = np.clip(rgb + (rgb - blur) * 0.28, 0.0, 1.0)

    # Soft peripheral ambient occlusion + delicate dark walnut ink rim on opaque silhouette
    mask = alpha > 0.35
    if mask.sum() > 32 and (ink_strength > 0 or ao_rim > 0):
        edt = ndimage.distance_transform_edt(mask).astype(np.float32)
        # Directional SE shadow bias so SE edge gets slightly deeper ink/AO
        gy, gx = np.gradient(ndimage.gaussian_filter(alpha, sigma=1.5))
        se_facing = np.clip((-gx - gy) * 1.4, 0.0, 1.0)
        rim_ao = (1.0 - np.clip(edt / 7.0, 0.0, 1.0)) * ao_rim * (0.65 + 0.55 * se_facing)
        rgb *= 1.0 - rim_ao[..., None]

        ink_band = np.clip(1.65 + 0.85 * se_facing - edt, 0.0, 1.0) * (alpha > 0.25)
        ink_mix = np.clip(ink_band * ink_strength, 0.0, 0.82)[..., None]
        rgb = rgb * (1.0 - ink_mix) + (INK_COLOR / 255.0) * ink_mix

    out = np.concatenate([np.clip(rgb * 255.0, 0, 255), np.clip(alpha * 255.0, 0, 255)[..., None]], axis=-1)
    return decontaminate_rgba(out.astype(np.uint8), alpha_cutoff=10)


# ---------------------------------------------------------------------------
# Painterly Battlemap Hero Sprite Synthesizers
# ---------------------------------------------------------------------------


def shade_height_painterly(
    height: np.ndarray,
    mask: np.ndarray,
    base_rgb: np.ndarray,
    ink_creases: np.ndarray | None = None,
    bevel_strength: float = 0.42,
    outer_ink_px: float = 2.1,
    seed: int = 1,
) -> np.ndarray:
    """Light a heightfield from NW (-0.65, -0.65, 0.4) with warm sun, cool shadow, AO, and walnut ink."""
    h_size, w_size = height.shape
    grain = warped_fbm(max(h_size, w_size), seed=seed + 77, min_freq=6, max_freq=96, warp_px=8.0)[:h_size, :w_size]

    h_smooth = ndimage.gaussian_filter(height, sigma=1.35) + (grain - 0.5) * 0.06
    gy, gx = np.gradient(h_smooth)
    scale_n = 4.2
    nx = -gx * scale_n
    ny = -gy * scale_n
    nz = np.ones_like(height)
    norm = np.sqrt(nx * nx + ny * ny + nz * nz) + 1e-6
    nx, ny, nz = nx / norm, ny / norm, nz / norm

    # NW sunlight
    lx, ly, lz = -0.62, -0.62, 0.48
    ndotl = nx * lx + ny * ly + nz * lz
    lit = np.clip((ndotl - 0.48) * bevel_strength, -0.38, 0.36)

    rgb = base_rgb.copy().astype(np.float32)
    # Warm sunlit NW shift, cool deep shadow SE shift
    sun_pos = np.clip(lit, 0.0, 1.0)[..., None]
    shd_neg = np.clip(-lit, 0.0, 1.0)[..., None]
    rgb = rgb * (1.0 + sun_pos * np.array([1.08, 1.04, 0.86])) - rgb * shd_neg * np.array([0.82, 0.78, 0.68])
    # Painterly brushstroke value modulation
    stroke = periodic_fbm(max(h_size, w_size), min_freq=8, max_freq=80, beta=1.55, anisotropy=(2.2, 0.8), angle_rad=0.6, seed=seed + 313)[:h_size, :w_size]
    rgb *= (0.90 + 0.20 * stroke)[..., None]

    # Silhouette anti-aliased alpha & variable-width hand-inked outer contour
    edt_in = ndimage.distance_transform_edt(mask).astype(np.float32)
    edt_out = ndimage.distance_transform_edt(~mask).astype(np.float32)
    sdf = edt_in - edt_out
    alpha = np.clip(sdf + 0.65, 0.0, 1.0)

    se_rim = np.clip((gx + gy) * 1.5, 0.0, 1.0)
    wobble = (grain - 0.5) * 0.75
    ink_w = outer_ink_px + 0.95 * se_rim + wobble
    outer_ink = np.clip(ink_w - edt_in + 0.5, 0.0, 1.0) * (alpha > 0.05)

    total_ink = outer_ink
    if ink_creases is not None:
        total_ink = np.maximum(total_ink, np.clip(ink_creases, 0.0, 1.0))

    ink_a = np.clip(total_ink * 0.88, 0.0, 0.92)[..., None]
    rgb = rgb * (1.0 - ink_a) + INK_COLOR * ink_a

    rgba = np.concatenate([np.clip(rgb, 0, 255), (alpha * 255.0)[..., None]], axis=-1).astype(np.uint8)
    return decontaminate_rgba(rgba, alpha_cutoff=8)


def synth_moss_overlay(
    rgb: np.ndarray,
    height: np.ndarray,
    ink: np.ndarray,
    moss_mask: np.ndarray,
    seed: int,
    palette: str = "woodland",
) -> None:
    """Paint bumpy hand-inked moss cushions onto `rgb`, `height`, and `ink` in-place."""
    if moss_mask.sum() < 12:
        return
    sz = max(rgb.shape[0], rgb.shape[1])
    bumps = warped_fbm(sz, seed=seed + 501, min_freq=10, max_freq=72, warp_px=7.0)[: rgb.shape[0], : rgb.shape[1]]
    macro = periodic_fbm(sz, min_freq=3, max_freq=14, beta=1.8, seed=seed + 509)[: rgb.shape[0], : rgb.shape[1]]
    edt = ndimage.distance_transform_edt(moss_mask).astype(np.float32)
    blend = np.clip(edt / 2.2, 0.0, 1.0)

    if palette == "woodland":
        c_dark = np.array([48.0, 68.0, 28.0], dtype=np.float32)
        c_mid = np.array([86.0, 114.0, 42.0], dtype=np.float32)
        c_hi = np.array([134.0, 156.0, 56.0], dtype=np.float32)
    else:  # coastal sea-moss
        c_dark = np.array([38.0, 58.0, 34.0], dtype=np.float32)
        c_mid = np.array([68.0, 92.0, 46.0], dtype=np.float32)
        c_hi = np.array([104.0, 124.0, 58.0], dtype=np.float32)

    t = np.clip(0.55 * bumps + 0.45 * macro, 0.0, 1.0)[..., None]
    m_col = np.where(t < 0.5, c_dark + (c_mid - c_dark) * (t * 2.0), c_mid + (c_hi - c_mid) * ((t - 0.5) * 2.0))
    # Dark crevice speckles inside moss pillow
    speck = np.clip((0.28 - bumps) * 3.0, 0.0, 0.55)[..., None]
    m_col *= 1.0 - speck

    rgb[:] = rgb * (1.0 - blend[..., None]) + m_col * blend[..., None]
    height[:] = height + blend * (0.14 + 0.12 * bumps)
    moss_edge = np.clip(1.8 - np.abs(edt - 1.0), 0.0, 1.0) * moss_mask
    ink[:] = np.maximum(ink, moss_edge * 0.72)


def synth_gnarled_root_trunk(seed: int, n_roots: int = 9, size: int = 512) -> np.ndarray:
    """Synthesize iconic top-down spreading buttress-root forest trunk (matching reference battlemap)."""
    rng = np.random.default_rng(seed)
    yy, xx = np.meshgrid(np.arange(size, dtype=np.float32), np.arange(size, dtype=np.float32), indexing="ij")
    cx, cy = size * 0.5 + rng.uniform(-10, 10), size * 0.5 + rng.uniform(-10, 10)

    height = np.zeros((size, size), dtype=np.float32)
    wood_mask = np.zeros((size, size), dtype=bool)
    ink = np.zeros((size, size), dtype=np.float32)
    bark_grain = np.zeros((size, size), dtype=np.float32)

    base_angles = np.linspace(0, 2 * math.pi, n_roots, endpoint=False) + rng.uniform(0, 2 * math.pi)
    base_angles += rng.uniform(-0.22, 0.22, size=n_roots)

    # Collect main buttress roots + secondary forked rootlets
    branches: list[tuple[np.ndarray, np.ndarray, np.ndarray]] = []
    for idx, ang0 in enumerate(base_angles):
        reach = rng.uniform(size * 0.37, size * 0.45)
        w0 = rng.uniform(22.0, 31.0)
        bend1 = rng.uniform(-0.38, 0.38)
        bend2 = rng.uniform(-0.26, 0.26)
        ts = np.linspace(0.0, 1.0, 110)
        r = ts * reach
        ang = ang0 + bend1 * np.sin(ts * math.pi * 1.15) + bend2 * np.sin(ts * math.pi * 2.3 + 0.7)
        px = cx + np.cos(ang) * r
        py = cy + np.sin(ang) * r
        pw = w0 * np.power(1.0 - ts * 0.90, 1.18) + 1.8
        branches.append((px, py, pw))

        # Secondary forked rootlet splitting off at t ~ 0.4..0.62
        n_forks = 2 if idx % 2 == 0 else 1
        for f_i in range(n_forks):
            t_split = rng.uniform(0.36, 0.64)
            idx_s = int(t_split * (len(ts) - 1))
            fork_sign = 1.0 if (idx + f_i) % 2 == 0 else -1.0
            fork_ang0 = ang[idx_s] + fork_sign * rng.uniform(0.38, 0.68)
            sub_len = reach * rng.uniform(0.32, 0.48)
            sub_ts = np.linspace(0.0, 1.0, 60)
            sub_ang = fork_ang0 + fork_sign * rng.uniform(-0.25, 0.28) * np.sin(sub_ts * math.pi)
            spx = px[idx_s] + np.cos(sub_ang) * (sub_ts * sub_len)
            spy = py[idx_s] + np.sin(sub_ang) * (sub_ts * sub_len)
            spw = pw[idx_s] * 0.72 * np.power(1.0 - sub_ts * 0.92, 1.25) + 1.4
            branches.append((spx, spy, spw))

    noise_w = warped_fbm(size, seed=seed + 19, min_freq=4, max_freq=48, warp_px=12.0)

    # Rasterize each root spline with cylindrical cross-section + longitudinal warped bark grain
    grain_ang = warped_fbm(size, seed=seed + 63, min_freq=8, max_freq=88, warp_px=9.0)
    for b_idx, (px, py, pw) in enumerate(branches):
        x_min = max(0, int(px.min() - pw.max() - 6))
        x_max = min(size, int(px.max() + pw.max() + 6))
        y_min = max(0, int(py.min() - pw.max() - 6))
        y_max = min(size, int(py.max() + pw.max() + 6))
        if x_max <= x_min or y_max <= y_min:
            continue
        sub_x = xx[y_min:y_max, x_min:x_max][..., None]
        sub_y = yy[y_min:y_max, x_min:x_max][..., None]
        sx = px[::2][None, None, :]
        sy = py[::2][None, None, :]
        sw = pw[::2][None, None, :]
        dist = np.hypot(sub_x - sx, sub_y - sy)
        rel = dist / np.maximum(sw, 1.0)
        best = np.argmin(rel, axis=-1)
        min_rel = np.take_along_axis(rel, best[..., None], axis=-1)[..., 0]
        local_n = noise_w[y_min:y_max, x_min:x_max]
        local_g = grain_ang[y_min:y_max, x_min:x_max]
        eff_rel = min_rel + (local_n - 0.5) * 0.16
        inside = eff_rel < 1.0
        if not inside.any():
            continue
        cyl_h = np.sqrt(np.clip(1.0 - eff_rel * eff_rel, 0.0, 1.0)) * (0.52 + 0.36 * (sw[0, 0, best] / 30.0))
        # Subtle organic bark fiber ridges (no bullseye stripes!)
        ridges = 0.55 * local_g + 0.45 * (0.5 + 0.5 * np.sin(min_rel * 7.5 + local_n * 5.0 + b_idx))

        old_h = height[y_min:y_max, x_min:x_max]
        overlap = inside & wood_mask[y_min:y_max, x_min:x_max] & (eff_rel > 0.74) & (cyl_h > old_h - 0.06)
        ink[y_min:y_max, x_min:x_max] = np.where(overlap, np.maximum(ink[y_min:y_max, x_min:x_max], 0.68), ink[y_min:y_max, x_min:x_max])

        take = inside & (cyl_h >= old_h)
        height[y_min:y_max, x_min:x_max] = np.where(take, cyl_h, old_h)
        bark_grain[y_min:y_max, x_min:x_max] = np.where(take, ridges, bark_grain[y_min:y_max, x_min:x_max])
        wood_mask[y_min:y_max, x_min:x_max] |= inside

    # Broad weathered central trunk crown blending smoothly into radial root buttresses
    dx, dy = xx - cx, yy - cy
    rad = np.hypot(dx, dy)
    ang_grid = np.arctan2(dy, dx)
    flute = 1.0 + 0.18 * np.cos(ang_grid * n_roots + 0.4) + 0.14 * (noise_w - 0.5)
    core_r = size * 0.125 * flute
    core_mask = rad < core_r
    core_blend = np.clip(1.0 - rad / np.maximum(core_r * 1.35, 1.0), 0.0, 1.0)
    core_h = 0.74 + 0.24 * np.clip(1.0 - (rad / np.maximum(core_r, 1.0)) ** 2, 0.0, 1.0) + 0.07 * noise_w
    wood_mask |= core_mask
    height = np.maximum(height, np.where(core_mask, core_h, 0.0))
    # Radial bark flow on central heartwood (eliminates inner bullseye stripes!)
    radial_bark = 0.5 + 0.5 * np.sin(ang_grid * (n_roots * 2.0) + noise_w * 4.5)
    bark_grain = bark_grain * (1.0 - core_blend) + (0.6 * grain_ang + 0.4 * radial_bark) * core_blend

    # Tucked woodland moss cushions ONLY in root V-crotches (no black puddle background!)
    dist_to_wood = ndimage.distance_transform_edt(~wood_mask).astype(np.float32)
    moss_field = warped_fbm(size, seed=seed + 131, min_freq=4, max_freq=22, warp_px=10.0)
    crotch_moss = (~wood_mask) & (rad > size * 0.08) & (rad < size * 0.24) & (dist_to_wood < 11.0) & (moss_field > 0.56)
    crotch_moss = ndimage.binary_opening(ndimage.binary_closing(crotch_moss, iterations=2), iterations=1)
    full_mask = wood_mask | crotch_moss

    # Warm weathered oak-brown bark palette matching reference battlemap trunks
    fine = periodic_fbm(size, min_freq=12, max_freq=120, beta=1.5, seed=seed + 88)
    c_bark_dark = np.array([50.0, 36.0, 25.0], dtype=np.float32)
    c_bark_mid = np.array([106.0, 82.0, 56.0], dtype=np.float32)
    c_bark_top = np.array([150.0, 122.0, 88.0], dtype=np.float32)

    t_wood = np.clip(0.42 * height + 0.38 * bark_grain + 0.20 * fine, 0.0, 1.0)[..., None]
    rgb = np.where(
        t_wood < 0.52,
        c_bark_dark + (c_bark_mid - c_bark_dark) * (t_wood / 0.52),
        c_bark_mid + (c_bark_top - c_bark_mid) * ((t_wood - 0.52) / 0.48),
    )

    wood_edt = ndimage.distance_transform_edt(wood_mask).astype(np.float32)
    # Keep dark bark ink fissures along radiating roots while keeping central trunk heart smooth
    outer_root_zone = np.clip((rad - core_r * 0.85) / 18.0, 0.0, 1.0)
    fissure = np.clip((0.23 - bark_grain) * 4.2, 0.0, 0.58) * wood_mask * (wood_edt > 2.2) * outer_root_zone
    ink = np.maximum(ink * outer_root_zone, fissure)
    synth_moss_overlay(rgb, height, ink, crotch_moss, seed=seed + 17)

    out = shade_height_painterly(height, full_mask, rgb, ink_creases=ink, bevel_strength=0.52, outer_ink_px=2.2, seed=seed)
    return trim_and_fit_rgba(out, max_dim=size, pad_frac=0.04)


def synth_billowing_canopy(
    seed: int,
    style: str = "temperate_olive",
    size: int = 512,
) -> np.ndarray:
    """Synthesize hand-inked multi-tier scalloped battlemap tree canopy."""
    rng = np.random.default_rng(seed)
    yy, xx = np.meshgrid(np.arange(size, dtype=np.float32), np.arange(size, dtype=np.float32), indexing="ij")
    cx, cy = size * 0.5, size * 0.5

    palettes = {
        "temperate_olive": (
            np.array([26.0, 42.0, 20.0]),
            np.array([66.0, 96.0, 38.0]),
            np.array([134.0, 162.0, 62.0]),
        ),
        "temperate_emerald": (
            np.array([22.0, 42.0, 24.0]),
            np.array([54.0, 92.0, 42.0]),
            np.array([118.0, 152.0, 64.0]),
        ),
        "jungle_emergent": (
            np.array([18.0, 44.0, 22.0]),
            np.array([50.0, 104.0, 42.0]),
            np.array([126.0, 166.0, 56.0]),
        ),
        "swamp_willow": (
            np.array([28.0, 40.0, 22.0]),
            np.array([66.0, 84.0, 40.0]),
            np.array([114.0, 128.0, 58.0]),
        ),
        "savanna_acacia": (
            np.array([46.0, 50.0, 22.0]),
            np.array([96.0, 102.0, 40.0]),
            np.array([154.0, 148.0, 62.0]),
        ),
    }
    c_deep, c_mid, c_sun = [x.astype(np.float32) for x in palettes[style]]

    height = np.zeros((size, size), dtype=np.float32)
    mask = np.zeros((size, size), dtype=bool)
    rgb = np.zeros((size, size, 3), dtype=np.float32)
    ink = np.zeros((size, size), dtype=np.float32)

    warp_a = warped_fbm(size, seed=seed + 11, min_freq=5, max_freq=56, warp_px=14.0)
    leaf_detail = periodic_fbm(size, min_freq=14, max_freq=110, beta=1.45, seed=seed + 23)

    # Optional radiating tropical palm under-fronds for jungle_emergent
    if style == "jungle_emergent":
        n_fronds = 11
        for fi in range(n_fronds):
            ang = fi * (2 * math.pi / n_fronds) + rng.uniform(-0.18, 0.18)
            flen = rng.uniform(size * 0.38, size * 0.46)
            ca, sa = math.cos(ang), math.sin(ang)
            ux = (xx - cx) * ca + (yy - cy) * sa
            uy = -(xx - cx) * sa + (yy - cy) * ca
            t_f = ux / flen
            in_len = (t_f > 0.12) & (t_f < 1.0)
            half_w = flen * 0.16 * np.sin(np.clip(t_f, 0.0, 1.0) * math.pi)
            # Serrated palm pinnae notches
            serr = 0.72 + 0.28 * np.cos(t_f * 42.0)
            fmask = in_len & (np.abs(uy) < half_w * serr)
            if fmask.any():
                fh = 0.25 + 0.12 * (1.0 - np.abs(uy) / np.maximum(half_w, 1.0))
                rgb = np.where(fmask[..., None], c_deep * 0.85 + (c_mid - c_deep) * (0.4 + 0.4 * leaf_detail[..., None]), rgb)
                height = np.maximum(height, np.where(fmask, fh, 0.0))
                fedt = ndimage.distance_transform_edt(fmask).astype(np.float32)
                ink = np.maximum(ink, np.clip(1.8 - fedt, 0.0, 1.0) * fmask * 0.75)
                mask |= fmask

    # Optional gnarled inner branches peeking through for swamp_willow / open woodland bough
    if style in ("swamp_willow", "savanna_acacia"):
        for bi in range(6):
            ang = bi * (math.pi / 3.0) + rng.uniform(-0.3, 0.3)
            ts = np.linspace(0.0, 1.0, 50)
            blen = size * 0.30
            bx = cx + np.cos(ang + 0.25 * np.sin(ts * 3.0)) * (ts * blen)
            by = cy + np.sin(ang + 0.25 * np.sin(ts * 3.0)) * (ts * blen)
            bw = 11.0 * (1.0 - ts * 0.75)
            dist = np.hypot(xx[..., None] - bx[None, None, ::2], yy[..., None] - by[None, None, ::2])
            rel = (dist / bw[None, None, ::2]).min(axis=-1)
            bmask = rel < 1.0
            rgb = np.where(bmask[..., None], np.array([78.0, 62.0, 44.0], dtype=np.float32), rgb)
            height = np.maximum(height, np.where(bmask, 0.28, 0.0))
            mask |= bmask

    # 3 stacked tiers of scalloped leaf-crown clusters (outer dark boughs -> mid boughs -> sunlit crown)
    tiers = [
        (10, (size * 0.19, size * 0.27), (size * 0.13, size * 0.17), 0.36, 0.18),
        (9, (size * 0.10, size * 0.21), (size * 0.11, size * 0.15), 0.62, 0.52),
        (6, (0.0, size * 0.13), (size * 0.09, size * 0.13), 0.88, 0.82),
    ]
    if style == "temperate_emerald":
        # Open ring bough around slightly shaded central hollow for variety
        tiers[2] = (7, (size * 0.08, size * 0.16), (size * 0.085, size * 0.12), 0.88, 0.80)

    for n_clumps, r_range, rad_range, base_h, sun_mix in tiers:
        ang_step = 2.0 * math.pi / n_clumps
        ang_off = rng.uniform(0, 2 * math.pi)
        for ci in range(n_clumps):
            ang = ang_off + ci * ang_step + rng.uniform(-0.28, 0.28)
            dist_c = rng.uniform(*r_range)
            bx = cx + math.cos(ang) * dist_c
            by = cy + math.sin(ang) * dist_c
            brad = rng.uniform(*rad_range)

            dx, dy = xx - bx, yy - by
            d = np.hypot(dx, dy)
            theta = np.arctan2(dy, dx)
            # Scalloped leaf-cluster lobes (7 + 13 lobes + organic warp)
            lobes = (
                1.0
                + 0.13 * np.cos(theta * 7.0 + ci * 1.3)
                + 0.08 * np.cos(theta * 13.0 - ci * 2.1)
                + 0.18 * (warp_a - 0.5)
            )
            eff_r = brad * lobes
            cmask = d < eff_r
            if not cmask.any():
                continue

            # Drop shadow AO onto lower foliage underneath this bough!
            sh_mask = (np.hypot(dx - 6.0, dy - 7.0) < eff_r * 1.03) & mask & (~cmask)
            rgb = np.where(sh_mask[..., None], rgb * 0.74, rgb)

            dome = np.sqrt(np.clip(1.0 - (d / np.maximum(eff_r, 1.0)) ** 2, 0.0, 1.0))
            ch = base_h + 0.22 * dome + 0.08 * leaf_detail

            # Sunlit bias on NW side of each individual bough cluster + overall crown NW side
            local_nw = np.clip((-dx - dy) / (brad * 1.8) * 0.5 + 0.5, 0.0, 1.0)
            global_nw = np.clip((-(xx - cx) - (yy - cy)) / (size * 0.75) * 0.5 + 0.5, 0.0, 1.0)
            tone = np.clip(0.35 * sun_mix + 0.35 * local_nw + 0.18 * global_nw + 0.24 * (leaf_detail - 0.3), 0.0, 1.0)[..., None]

            b_col = np.where(
                tone < 0.5,
                c_deep + (c_mid - c_deep) * (tone * 2.0),
                c_mid + (c_sun - c_mid) * ((tone - 0.5) * 2.0),
            )
            # Subtle dark leaf-tuft separations inside the bough
            tuft_sep = np.clip((0.24 - leaf_detail) * 2.6, 0.0, 0.42)[..., None]
            b_col *= 1.0 - tuft_sep

            cedt = ndimage.distance_transform_edt(cmask).astype(np.float32)
            # Crisp hand-inked bough separation contour where overlapping lower tiers!
            bough_ink = np.clip(1.95 - cedt, 0.0, 1.0) * cmask * mask * 0.78

            rgb = np.where(cmask[..., None], b_col, rgb)
            height = np.where(cmask, ch, height)
            ink = np.where(cmask, bough_ink, ink)
            mask |= cmask

    out = shade_height_painterly(height, mask, rgb, ink_creases=ink, bevel_strength=0.34, outer_ink_px=2.2, seed=seed)
    return trim_and_fit_rgba(out, max_dim=size, pad_frac=0.04)


def synth_stepped_boulder(
    seed: int,
    style: str = "slate_mossy",
    size: int = 480,
    n_steps: int = 5,
    base_grain_rgb: np.ndarray | None = None,
) -> np.ndarray:
    """Synthesize terraced/stepped angular battlemap cliff boulder (matching reference lower-right boulder)."""
    rng = np.random.default_rng(seed)
    yy, xx = np.meshgrid(np.arange(size, dtype=np.float32), np.arange(size, dtype=np.float32), indexing="ij")
    cx, cy = size * 0.5, size * 0.5

    palettes = {
        "slate_mossy": (np.array([58.0, 60.0, 58.0]), np.array([106.0, 108.0, 104.0]), np.array([162.0, 162.0, 154.0])),
        "forest_mossy": (np.array([56.0, 60.0, 52.0]), np.array([98.0, 102.0, 92.0]), np.array([144.0, 146.0, 134.0])),
        "sandstone": (np.array([104.0, 56.0, 36.0]), np.array([166.0, 98.0, 58.0]), np.array([214.0, 148.0, 92.0])),
        "sandstone_crag": (np.array([96.0, 58.0, 38.0]), np.array([154.0, 96.0, 58.0]), np.array([198.0, 138.0, 86.0])),
        "snow_crag": (np.array([56.0, 62.0, 68.0]), np.array([98.0, 106.0, 114.0]), np.array([146.0, 152.0, 160.0])),
        "coastal_wet": (np.array([36.0, 42.0, 44.0]), np.array([68.0, 76.0, 78.0]), np.array([112.0, 118.0, 118.0])),
    }
    c_base, c_mid, c_cap = [x.astype(np.float32) for x in palettes[style]]

    warp = warped_fbm(size, seed=seed + 31, min_freq=3, max_freq=36, warp_px=16.0)
    fine = periodic_fbm(size, min_freq=10, max_freq=110, beta=1.55, seed=seed + 53)

    # Angular faceted rock field (max of half-planes -> chiseled polygonal rock facets + warp!)
    n_facets = rng.integers(7, 10)
    facet_angs = np.sort(rng.uniform(0, 2 * math.pi, size=n_facets))
    # Ensure no gap > pi*0.65 so polygon stays closed and compact
    facet_angs = np.linspace(0, 2 * math.pi, n_facets, endpoint=False) + rng.uniform(-0.22, 0.22, size=n_facets)
    aspect_x = rng.uniform(0.84, 1.15)
    aspect_y = rng.uniform(0.78, 1.08)

    height = np.zeros((size, size), dtype=np.float32)
    mask = np.zeros((size, size), dtype=bool)
    rgb = np.zeros((size, size, 3), dtype=np.float32)
    ink = np.zeros((size, size), dtype=np.float32)

    # Off-center summit drift so SE or NW faces have wider terraced steps like the reference boulder!
    drift_x = rng.uniform(-24.0, 26.0)
    drift_y = rng.uniform(-22.0, 22.0)

    for s in range(n_steps):
        frac = s / max(n_steps - 1, 1)
        scx = cx + drift_x * frac
        scy = cy + drift_y * frac
        rad = size * (0.40 - 0.23 * frac)
        # Faceted distance
        dx = (xx - scx) / aspect_x + (warp - 0.5) * 34.0
        dy = (yy - scy) / aspect_y + (periodic_fbm(size, 3, 24, seed=seed + 70 + s) - 0.5) * 30.0

        poly_d = np.zeros_like(xx)
        for fi, fa in enumerate(facet_angs):
            proj = dx * math.cos(fa) + dy * math.sin(fa)
            f_rad = rad * rng.uniform(0.88, 1.08)
            poly_d = np.maximum(poly_d, proj / max(f_rad, 1.0))

        # Blend 72% faceted chiseled planes + 28% radial so corners are crisp rock angles, not spikes
        radial_d = np.hypot(dx, dy) / max(rad, 1.0)
        d_norm = 0.72 * poly_d + 0.28 * radial_d
        smask = d_norm < 1.0
        if not smask.any():
            continue

        sedt = ndimage.distance_transform_edt(smask).astype(np.float32)
        # Drop AO onto lower ledge right below this step!
        ao_band = (d_norm >= 1.0) & (d_norm < 1.12) & mask
        rgb = np.where(ao_band[..., None], rgb * 0.76, rgb)

        step_h = 0.18 + 0.74 * frac + 0.05 * np.clip(sedt / 14.0, 0.0, 1.0) + 0.04 * fine
        t_col = np.clip(frac * 0.85 + 0.22 * (fine - 0.4), 0.0, 1.0)[..., None]
        scol = np.where(
            t_col < 0.5,
            c_base + (c_mid - c_base) * (t_col * 2.0),
            c_mid + (c_cap - c_mid) * ((t_col - 0.5) * 2.0),
        )
        if base_grain_rgb is not None:
            g_crop = base_grain_rgb[:size, :size].astype(np.float32)
            g_lum = (g_crop.mean(axis=-1, keepdims=True) / max(float(g_crop.mean()), 1.0))
            scol = np.clip(scol * (0.72 + 0.28 * g_lum), 0.0, 255.0)

        # Pale chiseled lip highlight along NW step edge
        nw_lip = np.clip(3.2 - sedt, 0.0, 1.0) * smask * np.clip((-(xx - scx) - (yy - scy)) / max(rad, 1.0), 0.0, 1.0)
        scol += nw_lip[..., None] * 24.0

        # Crisp dark ink contour separating each terrace step from the step below
        if s > 0:
            step_ink = np.clip(2.2 - sedt, 0.0, 1.0) * smask * 0.86
        else:
            step_ink = np.zeros_like(sedt)

        rgb = np.where(smask[..., None], scol, rgb)
        height = np.where(smask, step_h, height)
        ink = np.where(smask, np.maximum(ink * 0.25, step_ink), ink)
        mask |= smask

    # Fine rock fracture cracks across ledges
    crack1 = np.abs(warped_fbm(size, seed=seed + 211, min_freq=3, max_freq=18, warp_px=22.0) - 0.5)
    crack_ink = np.clip((0.016 - crack1) * 55.0, 0.0, 0.65) * mask
    ink = np.maximum(ink, crack_ink)

    # Biome surface treatments: moss cushions, snow cap, or coastal wet sheen + barnacles
    if style in ("slate_mossy", "forest_mossy"):
        m_noise = periodic_fbm(size, min_freq=4, max_freq=18, seed=seed + 303)
        thresh = 0.63 if style == "slate_mossy" else 0.52
        moss = mask & (height < 0.86) & (m_noise > thresh)
        moss = ndimage.binary_opening(ndimage.binary_closing(moss, iterations=2), iterations=1)
        synth_moss_overlay(rgb, height, ink, moss, seed=seed + 309, palette="woodland")
    elif style == "snow_crag":
        s_noise = warped_fbm(size, seed=seed + 401, min_freq=4, max_freq=24, warp_px=12.0)
        nw_bias = np.clip((-(xx - cx) - (yy - cy)) / (size * 0.6) * 0.35 + 0.5, 0.0, 1.0)
        snow_mask = mask & ((height > 0.72) | ((height > 0.42) & (s_noise * 0.6 + nw_bias * 0.4 > 0.60)))
        snow_mask = ndimage.binary_closing(snow_mask, iterations=2)
        sedt = ndimage.distance_transform_edt(snow_mask).astype(np.float32)
        s_blend = np.clip(sedt / 2.5, 0.0, 1.0)[..., None]
        snow_col = np.array([228.0, 236.0, 246.0], dtype=np.float32) + (fine[..., None] - 0.5) * 18.0
        rgb = rgb * (1.0 - s_blend) + snow_col * s_blend
        height += s_blend[..., 0] * 0.08
        ink = np.maximum(ink, np.clip(1.6 - np.abs(sedt - 1.0), 0.0, 1.0) * snow_mask * 0.55)
    elif style == "coastal_wet":
        # Dark wet water-line rim + sea-moss + pale barnacles
        medt = ndimage.distance_transform_edt(mask).astype(np.float32)
        wet = np.clip(1.0 - medt / 28.0, 0.0, 1.0)[..., None]
        rgb *= 1.0 - 0.32 * wet
        m_noise = periodic_fbm(size, min_freq=5, max_freq=22, seed=seed + 455)
        sea_moss = mask & (medt < 36.0) & (medt > 6.0) & (m_noise > 0.60)
        synth_moss_overlay(rgb, height, ink, sea_moss, seed=seed + 460, palette="coastal")

    out = shade_height_painterly(height, mask, rgb, ink_creases=ink, bevel_strength=0.46, outer_ink_px=2.3, seed=seed)
    return trim_and_fit_rgba(out, max_dim=size, pad_frac=0.04)


def synth_mossy_fallen_log(seed: int, hollow: bool = False) -> np.ndarray:
    """Synthesize splintered-end fallen woodland log with organic bark fissures and lush moss blanket."""
    w, h = 512, 256
    rng = np.random.default_rng(seed)
    yy, xx = np.meshgrid(np.arange(h, dtype=np.float32), np.arange(w, dtype=np.float32), indexing="ij")

    height = np.zeros((h, w), dtype=np.float32)
    mask = np.zeros((h, w), dtype=bool)
    rgb = np.zeros((h, w, 3), dtype=np.float32)
    ink = np.zeros((h, w), dtype=np.float32)

    noise = warped_fbm(w, seed=seed + 13, min_freq=4, max_freq=56, warp_px=12.0)[:h, :w]
    wood_fiber = periodic_fbm(w, min_freq=8, max_freq=110, beta=1.45, anisotropy=(5.5, 0.65), angle_rad=-0.10, seed=seed + 29)[:h, :w]

    # Naturally curved, slightly angled woodland trunk spine
    ts = np.linspace(0.0, 1.0, 180)
    sx = 34.0 + ts * (w - 68.0)
    sy = h * 0.56 - 22.0 * (ts - 0.5) + 13.0 * np.sin(ts * math.pi * 1.15)
    sr = (38.0 - 8.5 * ts) * (1.0 + 0.08 * np.sin(ts * 7.0 + 0.5))

    dx = xx[..., None] - sx[None, None, ::2]
    dy = yy[..., None] - sy[None, None, ::2]
    rr = sr[None, None, ::2]
    dist = np.hypot(dx, dy)
    idx = np.argmin(dist / rr, axis=-1)
    best_t = ts[::2][idx]
    best_y = dy[np.arange(h)[:, None], np.arange(w)[None, :], idx]
    best_r = rr[0, 0, idx]
    cross = best_y / np.maximum(best_r, 1.0)

    # Asymmetric shattered wood splinters at ends (3-4 coarse uneven wedges, NOT comb teeth!)
    spl_l = (
        0.055
        + 0.055 * np.clip(np.sin(cross * 3.4 + 0.6), -0.6, 1.0)
        + 0.035 * np.sin(cross * 7.1 - 1.1)
        + 0.045 * (noise - 0.5)
    )
    spl_r = (
        0.945
        - 0.060 * np.clip(np.cos(cross * 3.1 - 0.4), -0.6, 1.0)
        - 0.035 * np.cos(cross * 6.7 + 0.9)
        - 0.045 * (noise - 0.5)
    )
    in_trunk = (np.abs(cross) < 1.0 + (noise - 0.5) * 0.16) & (best_t > spl_l) & (best_t < spl_r)

    # 3 organic broken branch stubs
    for b_t, b_sign, b_ang, b_len in [(0.26, -1.0, -0.95, 52.0), (0.68, 1.0, 0.68, 48.0), (0.82, -1.0, -0.58, 34.0)]:
        bx0 = float(np.interp(b_t, ts, sx))
        by0 = float(np.interp(b_t, ts, sy)) + b_sign * 16.0
        bts = np.linspace(0.0, 1.0, 30)
        bsx = bx0 + np.cos(b_ang) * bts * b_len
        bsy = by0 + np.sin(b_ang + 0.18 * bts) * bts * b_len
        bsw = 13.5 * np.power(1.0 - bts * 0.72, 1.15)
        bdist = np.hypot(xx[..., None] - bsx[None, None, :], yy[..., None] - bsy[None, None, :])
        brel = (bdist / bsw[None, None, :]).min(axis=-1) + (noise - 0.5) * 0.14
        in_trunk |= brel < 1.0

    mask |= in_trunk
    cyl = np.sqrt(np.clip(1.0 - cross * cross, 0.0, 1.0))
    height = np.where(in_trunk, 0.34 + 0.54 * cyl + 0.08 * wood_fiber, 0.0)

    heartwood = in_trunk & (
        (best_t < spl_l + 0.065 * (1.0 - 0.35 * np.abs(cross)) + 0.03 * (wood_fiber - 0.5))
        | (best_t > spl_r - 0.065 * (1.0 - 0.35 * np.abs(cross)) + 0.03 * (wood_fiber - 0.5))
    )
    bark_furrow = 0.55 * wood_fiber + 0.45 * (0.5 + 0.5 * np.cos(cross * 11.0 + noise * 5.5))

    c_bark_d = np.array([52.0, 38.0, 26.0], dtype=np.float32)
    c_bark_l = np.array([106.0, 80.0, 54.0], dtype=np.float32)
    c_heart = np.array([156.0, 124.0, 82.0], dtype=np.float32)

    bark_col = c_bark_d + (c_bark_l - c_bark_d) * np.clip(bark_furrow, 0.0, 1.0)[..., None]
    wood_col = c_heart * (0.78 + 0.36 * wood_fiber[..., None])
    rgb = np.where(heartwood[..., None], wood_col, bark_col)

    ink_furrow = np.clip((0.22 - bark_furrow) * 3.8, 0.0, 0.62) * in_trunk * (~heartwood)
    ink = np.maximum(ink, ink_furrow)

    # Organic tapered rotted split cavity for hollow log (warm dark rotted wood interior, not flat black box!)
    cav = np.zeros_like(in_trunk)
    if hollow:
        t_mid = (best_t - 0.50) / 0.21
        cav_half_w = 0.44 * np.clip(1.0 - t_mid * t_mid, 0.0, 1.0) + 0.12 * (noise - 0.5)
        cav = in_trunk & (cav_half_w > 0.06) & (np.abs(cross - 0.04) < cav_half_w)
        if cav.any():
            cedt = ndimage.distance_transform_edt(cav).astype(np.float32)
            depth_shade = np.clip(1.0 - cedt / 12.0, 0.28, 1.0)[..., None]
            rot_col = np.array([62.0, 42.0, 26.0], dtype=np.float32) * depth_shade * (0.75 + 0.45 * wood_fiber[..., None])
            rgb = np.where(cav[..., None], rot_col, rgb)
            height = np.where(cav, 0.16 + 0.06 * wood_fiber, height)
            ink = np.maximum(ink, np.clip(2.2 - cedt, 0.0, 1.0) * cav * 0.88)

    # Velvety bumpy woodland moss patches draped naturally along the log!
    moss_n = warped_fbm(w, seed=seed + 99, min_freq=3, max_freq=18, warp_px=14.0)[:h, :w]
    moss = in_trunk & (~heartwood) & (~cav) & (cross < 0.52) & (cross > -0.88) & (moss_n > 0.43)
    moss = ndimage.binary_opening(ndimage.binary_closing(moss, iterations=2), iterations=1)
    synth_moss_overlay(rgb, height, ink, moss, seed=seed + 105, palette="woodland")

    out = shade_height_painterly(height, mask, rgb, ink_creases=ink, bevel_strength=0.48, outer_ink_px=2.1, seed=seed)
    return trim_and_fit_rgba(out, max_dim=480, pad_frac=0.05)


def synth_fern_or_bracken(seed: int, multi_cluster: bool = False, size: int = 384) -> np.ndarray:
    """Synthesize crisp hand-inked asymmetric pinnate woodland fern / bracken cluster."""
    rng = np.random.default_rng(seed)
    yy, xx = np.meshgrid(np.arange(size, dtype=np.float32), np.arange(size, dtype=np.float32), indexing="ij")

    height = np.zeros((size, size), dtype=np.float32)
    mask = np.zeros((size, size), dtype=bool)
    rgb = np.zeros((size, size, 3), dtype=np.float32)
    ink = np.zeros((size, size), dtype=np.float32)
    fine = warped_fbm(size, seed=seed + 17, min_freq=8, max_freq=72, warp_px=7.0)

    centers = (
        [
            (size * 0.36, size * 0.42, 0.33),
            (size * 0.65, size * 0.52, 0.30),
            (size * 0.47, size * 0.66, 0.26),
        ]
        if multi_cluster
        else [(size * 0.50, size * 0.50, 0.42)]
    )

    for cx, cy, max_r_frac in centers:
        for tier in (0, 1):
            n_fronds = (9 if multi_cluster else 11) if tier == 0 else (6 if multi_cluster else 8)
            ang_base = rng.uniform(0, 2 * math.pi)
            for fi in range(n_fronds):
                # Organic asymmetric spacing & varied frond lengths (breaks spoke-wheel look!)
                ang = ang_base + fi * (2 * math.pi / n_fronds) + rng.uniform(-0.28, 0.28)
                flen = size * max_r_frac * rng.uniform(0.62, 1.06) * (0.82 if tier == 1 else 1.0)
                curve = rng.uniform(-0.32, 0.32)
                ca, sa = math.cos(ang), math.sin(ang)
                ux = (xx - cx) * ca + (yy - cy) * sa
                uy = -(xx - cx) * sa + (yy - cy) * ca
                t_raw = ux / max(flen, 1.0)
                uy_c = uy - curve * flen * (np.clip(t_raw, 0.0, 1.2) ** 1.8)
                # Forward-swept rounded oblong pinnae leaflets!
                t_swept = t_raw - 0.16 * (np.abs(uy_c) / max(flen * 0.2, 1.0))

                in_t = (t_raw > 0.03) & (t_raw < 1.0)
                env = flen * 0.23 * np.sin(np.clip(t_raw, 0.0, 1.0) * math.pi) * (1.0 - 0.22 * np.clip(t_raw, 0.0, 1.0))
                n_pinnae = rng.uniform(9.5, 12.5)
                # Rounded dome leaflet lobes (`sqrt(max(sin, 0))` -> plump rounded leaflets, not sharp triangles!)
                lobe = np.sqrt(np.clip(0.5 + 0.5 * np.cos(t_swept * 2.0 * math.pi * n_pinnae), 0.0, 1.0))
                pinna_reach = env * (0.20 + 0.80 * lobe) * (0.92 + 0.16 * fine)
                stem = in_t & (np.abs(uy_c) < 1.8)
                fmask = (in_t & (np.abs(uy_c) < pinna_reach)) | stem
                if not fmask.any():
                    continue

                # Cast soft AO onto lower fronds
                sh_ux = (xx - cx - 4.0) * ca + (yy - cy - 5.0) * sa
                sh_uy = -(xx - cx - 4.0) * sa + (yy - cy - 5.0) * ca
                sh = mask & (~fmask) & (sh_ux > 0) & (sh_ux < flen) & (np.abs(sh_uy) < env * 1.05)
                rgb = np.where(sh[..., None], rgb * 0.76, rgb)

                hue_jit = rng.uniform(-10.0, 12.0)
                c_dark = np.array([26.0, 50.0, 22.0], dtype=np.float32)
                c_mid = np.array([62.0 + hue_jit * 0.6, 102.0 + hue_jit * 0.4, 36.0], dtype=np.float32)
                c_tip = np.array([118.0 + hue_jit, 150.0 + hue_jit * 0.5, 52.0], dtype=np.float32)

                # Deep forest shadow at rosette center, sunlit olive-emerald toward tips
                center_ao = np.clip(t_raw / 0.35, 0.25, 1.0)
                tone = np.clip(((0.22 if tier == 0 else 0.48) + 0.38 * np.clip(t_raw, 0.0, 1.0) + 0.22 * (fine - 0.4)) * center_ao, 0.0, 1.0)[..., None]
                fcol = np.where(
                    tone < 0.5,
                    c_dark + (c_mid - c_dark) * (tone * 2.0),
                    c_mid + (c_tip - c_mid) * ((tone - 0.5) * 2.0),
                )
                fcol = np.where(stem[..., None], fcol * 1.14, fcol)

                fedt = ndimage.distance_transform_edt(fmask).astype(np.float32)
                f_ink = np.clip(1.65 - fedt, 0.0, 1.0) * fmask * 0.84
                fh = (0.30 if tier == 0 else 0.58) + 0.16 * np.clip(fedt / 5.5, 0.0, 1.0)

                rgb = np.where(fmask[..., None], fcol, rgb)
                height = np.where(fmask, fh, height)
                ink = np.where(fmask, f_ink, ink)
                mask |= fmask

    if multi_cluster:
        for _ in range(6):
            bx = rng.uniform(size * 0.28, size * 0.72)
            by = rng.uniform(size * 0.30, size * 0.72)
            if not mask[int(by), int(bx)]:
                continue
            for ox, oy in [(0.0, 0.0), (5.0, 2.5), (-3.5, 4.5)]:
                bd = np.hypot(xx - (bx + ox), yy - (by + oy))
                bmask = bd < 4.0
                bcol = np.array([178.0, 50.0, 34.0], dtype=np.float32) + np.clip(3.0 - bd, 0.0, 3.0)[..., None] * 14.0
                rgb = np.where(bmask[..., None], bcol, rgb)
                height = np.where(bmask, 0.84, height)
                ink = np.where((bd >= 2.9) & (bd < 4.4), 0.85, ink)

    out = shade_height_painterly(height, mask, rgb, ink_creases=ink, bevel_strength=0.34, outer_ink_px=1.7, seed=seed)
    return trim_and_fit_rgba(out, max_dim=size, pad_frac=0.05)


def synth_desert_cactus_barrel(seed: int, size: int = 320) -> np.ndarray:
    """Synthesize organic 90-degree overhead desert barrel cactus cluster with irregular ribs & ruffled bloom."""
    rng = np.random.default_rng(seed)
    yy, xx = np.meshgrid(np.arange(size, dtype=np.float32), np.arange(size, dtype=np.float32), indexing="ij")
    height = np.zeros((size, size), dtype=np.float32)
    mask = np.zeros((size, size), dtype=bool)
    rgb = np.zeros((size, size, 3), dtype=np.float32)
    ink = np.zeros((size, size), dtype=np.float32)
    warp = warped_fbm(size, seed=seed + 19, min_freq=5, max_freq=56, warp_px=9.0)

    globes = [
        (size * 0.45, size * 0.47, size * 0.28, 11, True),
        (size * 0.70, size * 0.61, size * 0.16, 9, False),
        (size * 0.31, size * 0.69, size * 0.13, 8, False),
    ]
    for gx, gy, grad, n_ribs, has_bloom in globes:
        dx, dy = xx - gx + (warp - 0.5) * 6.5, yy - gy + (warp - 0.5) * 6.5
        d = np.hypot(dx, dy)
        ang = np.arctan2(dy, dx) + 0.08 * np.sin(d * 0.12)
        rib_wave = 0.5 + 0.5 * np.cos(ang * n_ribs)
        eff_r = grad * (0.88 + 0.12 * rib_wave + 0.06 * (warp - 0.5))
        gmask = d < eff_r
        # Cast AO onto earlier globes
        sh = (np.hypot(xx - gx - 5.0, yy - gy - 6.0) < eff_r * 1.04) & mask & (~gmask)
        rgb = np.where(sh[..., None], rgb * 0.72, rgb)

        dome = np.sqrt(np.clip(1.0 - (d / np.maximum(eff_r, 1.0)) ** 2, 0.0, 1.0))
        gh = 0.30 + 0.52 * dome + 0.10 * rib_wave

        c_val = np.array([48.0, 74.0, 44.0], dtype=np.float32)
        c_rib = np.array([94.0, 126.0, 68.0], dtype=np.float32)
        gcol = c_val + (c_rib - c_val) * np.clip(0.60 * rib_wave + 0.32 * dome + 0.22 * (warp - 0.4), 0.0, 1.0)[..., None]

        # Fine warm spine tufts jittered along rib crests
        for ri in range(n_ribs):
            ra = ri * (2.0 * math.pi / n_ribs)
            for rf in (0.48, 0.74, 0.92):
                j_ang = ra + rng.uniform(-0.04, 0.04)
                sx = gx + math.cos(j_ang) * grad * rf * rng.uniform(0.94, 1.03)
                sy = gy + math.sin(j_ang) * grad * rf * rng.uniform(0.94, 1.03)
                # Tiny star spine lines
                sdx, sdy = xx - sx, yy - sy
                sd = np.hypot(sdx, sdy)
                sang = np.arctan2(sdy, sdx)
                sp = (sd < 4.5 * (0.45 + 0.55 * np.abs(np.cos(sang * 3.0 + ri)))) & gmask
                gcol = np.where(sp[..., None], gcol * 0.45 + np.array([204.0, 182.0, 120.0], dtype=np.float32) * 0.55, gcol)

        if has_bloom:
            bd = np.hypot(xx - gx, yy - gy)
            bang = np.arctan2(yy - gy, xx - gx)
            petal1 = grad * 0.25 * (0.78 + 0.22 * np.cos(bang * 7.0 + 0.3) + 0.14 * (warp - 0.5))
            petal2 = grad * 0.16 * (0.80 + 0.20 * np.cos(bang * 6.0 + 1.1))
            bm1, bm2 = bd < petal1, bd < petal2
            gcol = np.where(bm1[..., None], np.array([206.0, 96.0, 52.0], dtype=np.float32) * (0.85 + 0.25 * warp[..., None]), gcol)
            gcol = np.where(bm2[..., None], np.array([228.0, 168.0, 58.0], dtype=np.float32), gcol)
            gh = np.where(bm1, gh + 0.14, gh)

        gedt = ndimage.distance_transform_edt(gmask).astype(np.float32)
        rib_ink = np.clip((0.14 - rib_wave) * 4.2, 0.0, 0.52) * gmask * (d > grad * 0.25)
        g_ink = np.maximum(np.clip(2.0 - gedt, 0.0, 1.0) * gmask * 0.85, rib_ink)

        rgb = np.where(gmask[..., None], gcol, rgb)
        height = np.where(gmask, gh, height)
        ink = np.where(gmask, g_ink, ink)
        mask |= gmask

    out = shade_height_painterly(height, mask, rgb, ink_creases=ink, bevel_strength=0.44, outer_ink_px=2.0, seed=seed)
    return trim_and_fit_rgba(out, max_dim=size, pad_frac=0.05)


def synth_prickly_pear(seed: int, size: int = 320) -> np.ndarray:
    """Synthesize organic top-down prickly-pear cactus with warped pads, woody base & blooms."""
    rng = np.random.default_rng(seed)
    yy, xx = np.meshgrid(np.arange(size, dtype=np.float32), np.arange(size, dtype=np.float32), indexing="ij")
    height = np.zeros((size, size), dtype=np.float32)
    mask = np.zeros((size, size), dtype=bool)
    rgb = np.zeros((size, size, 3), dtype=np.float32)
    ink = np.zeros((size, size), dtype=np.float32)
    warp = warped_fbm(size, seed=seed + 43, min_freq=5, max_freq=56, warp_px=10.0)

    cx, cy = size * 0.5, size * 0.5
    pads = [(cx, cy, 0.2, 34.0, 25.0, 0.32)]
    for i in range(6):
        ang = i * (2 * math.pi / 6.0) + rng.uniform(-0.28, 0.28)
        px = cx + math.cos(ang) * rng.uniform(34.0, 46.0)
        py = cy + math.sin(ang) * rng.uniform(34.0, 46.0)
        pads.append((px, py, ang, rng.uniform(30.0, 39.0), rng.uniform(19.0, 25.0), 0.54))
        if i % 2 == 0:
            ang2 = ang + rng.uniform(-0.5, 0.5)
            px2 = px + math.cos(ang2) * 36.0
            py2 = py + math.sin(ang2) * 36.0
            pads.append((px2, py2, ang2, 27.0, 18.0, 0.76))

    for p_idx, (px, py, ang, rx, ry, base_h) in enumerate(pads):
        ca, sa = math.cos(ang), math.sin(ang)
        ux = (xx - px) * ca + (yy - py) * sa + (warp - 0.5) * 6.0
        uy = -(xx - px) * sa + (yy - py) * ca + (warp - 0.5) * 5.0
        # Pear-shaped pad (wider near outer end `ux > 0`)
        taper = 1.0 + 0.18 * np.clip(ux / rx, -0.8, 0.8)
        d2 = (ux / rx) ** 2 + (uy / (ry * taper)) ** 2
        pmask = d2 < 1.0
        sh = (np.hypot(xx - px - 4.0, yy - py - 5.0) < rx) & mask & (~pmask)
        rgb = np.where(sh[..., None], rgb * 0.76, rgb)

        pedt = ndimage.distance_transform_edt(pmask).astype(np.float32)
        ph = base_h + 0.22 * np.sqrt(np.clip(1.0 - d2, 0.0, 1.0))
        c_base = np.array([96.0, 102.0, 62.0] if p_idx == 0 else [76.0, 114.0, 60.0], dtype=np.float32)
        pcol = c_base + (warp[..., None] - 0.5) * 30.0

        # Irregular scattered spine areoles (NOT a regular grid!)
        for _ in range(9):
            ax = px + rng.uniform(-0.65, 0.65) * rx
            ay = py + rng.uniform(-0.65, 0.65) * ry
            ad = np.hypot(xx - ax, yy - ay)
            sp = (ad < 2.1) & pmask & (pedt > 3.5)
            pcol = np.where(sp[..., None], np.array([196.0, 180.0, 128.0], dtype=np.float32), pcol)

        pink = np.clip(1.95 - pedt, 0.0, 1.0) * pmask * 0.84
        rgb = np.where(pmask[..., None], pcol, rgb)
        height = np.where(pmask, ph, height)
        ink = np.where(pmask, pink, ink)
        mask |= pmask

    for px, py, ang, rx, _, _ in pads[-3:]:
        fx = px + math.cos(ang) * (rx * 0.70)
        fy = py + math.sin(ang) * (rx * 0.70)
        fd = np.hypot(xx - fx, yy - fy) + (warp - 0.5) * 2.5
        fmask = fd < 8.5
        fcol = np.where(
            (fd < 3.6)[..., None],
            np.array([228.0, 180.0, 56.0], dtype=np.float32),
            np.array([210.0, 96.0, 50.0], dtype=np.float32),
        )
        rgb = np.where(fmask[..., None], fcol, rgb)
        height = np.where(fmask, 0.90, height)
        ink = np.where((fd >= 6.8) & (fd < 8.8), 0.85, ink)
        mask |= fmask

    out = shade_height_painterly(height, mask, rgb, ink_creases=ink, bevel_strength=0.40, outer_ink_px=1.9, seed=seed)
    return trim_and_fit_rgba(out, max_dim=size, pad_frac=0.05)


def synth_swamp_cattails(seed: int, size: int = 320) -> np.ndarray:
    """Synthesize top-down wetland sedge blades and velvety brown cattail seed spikes."""
    rng = np.random.default_rng(seed)
    yy, xx = np.meshgrid(np.arange(size, dtype=np.float32), np.arange(size, dtype=np.float32), indexing="ij")
    cx, cy = size * 0.5, size * 0.5
    height = np.zeros((size, size), dtype=np.float32)
    mask = np.zeros((size, size), dtype=bool)
    rgb = np.zeros((size, size, 3), dtype=np.float32)
    ink = np.zeros((size, size), dtype=np.float32)

    for bi in range(26):
        ang = bi * (2 * math.pi / 26.0) + rng.uniform(-0.14, 0.14)
        blen = rng.uniform(size * 0.28, size * 0.44)
        curve = rng.uniform(-0.24, 0.24)
        ca, sa = math.cos(ang), math.sin(ang)
        ux = (xx - cx) * ca + (yy - cy) * sa
        uy = -(xx - cx) * sa + (yy - cy) * ca - curve * blen * ((ux / blen) ** 2)
        t = ux / blen
        bw = 5.4 * (1.0 - np.clip(t, 0.0, 1.0) * 0.88)
        bmask = (t > 0.0) & (t < 1.0) & (np.abs(uy) < bw)
        if not bmask.any():
            continue
        bcol = np.array([62.0, 90.0, 38.0], dtype=np.float32) + np.clip(t, 0.0, 1.0)[..., None] * np.array([46.0, 42.0, 16.0])
        bedt = ndimage.distance_transform_edt(bmask).astype(np.float32)
        rgb = np.where(bmask[..., None], bcol, rgb)
        height = np.where(bmask, 0.35 + 0.25 * np.clip(t, 0.0, 1.0), height)
        ink = np.where(bmask, np.clip(1.3 - bedt, 0.0, 1.0) * 0.72, ink)
        mask |= bmask

    # 5 dark-brown cylindrical cattail spikes
    for si in range(5):
        ang = si * (2 * math.pi / 5.0) + rng.uniform(-0.3, 0.3)
        r0 = rng.uniform(22.0, 52.0)
        sx0, sy0 = cx + math.cos(ang) * r0, cy + math.sin(ang) * r0
        ca, sa = math.cos(ang), math.sin(ang)
        ux = (xx - sx0) * ca + (yy - sy0) * sa
        uy = -(xx - sx0) * sa + (yy - sy0) * ca
        smask = (np.abs(ux) < 16.0) & (np.abs(uy) < 6.8) & (((ux / 16.0) ** 2 + (uy / 6.8) ** 2) < 1.15)
        sedt = ndimage.distance_transform_edt(smask).astype(np.float32)
        scol = np.array([82.0, 50.0, 28.0], dtype=np.float32) + np.clip(sedt / 5.0, 0.0, 1.0)[..., None] * 28.0
        rgb = np.where(smask[..., None], scol, rgb)
        height = np.where(smask, 0.85, height)
        ink = np.where(smask, np.clip(1.8 - sedt, 0.0, 1.0) * 0.88, ink)
        mask |= smask

    out = shade_height_painterly(height, mask, rgb, ink_creases=ink, bevel_strength=0.34, outer_ink_px=1.6, seed=seed)
    return trim_and_fit_rgba(out, max_dim=size, pad_frac=0.05)


def synth_coastal_driftwood(seed: int) -> np.ndarray:
    """Synthesize sun-bleached silver-tan weathered coastal driftwood branch with longitudinal wood grain."""
    w, h = 384, 224
    yy, xx = np.meshgrid(np.arange(h, dtype=np.float32), np.arange(w, dtype=np.float32), indexing="ij")
    height = np.zeros((h, w), dtype=np.float32)
    mask = np.zeros((h, w), dtype=bool)
    warp = warped_fbm(w, seed=seed + 7, min_freq=4, max_freq=48, warp_px=10.0)[:h, :w]
    wood_fiber = periodic_fbm(w, min_freq=8, max_freq=96, beta=1.45, anisotropy=(6.0, 0.6), angle_rad=0.08, seed=seed + 19)[:h, :w]

    ts = np.linspace(0.0, 1.0, 120)
    branches = [
        (36 + ts * 308, h * 0.56 + 18 * np.sin(ts * math.pi * 1.25), 18.5 * np.power(1.0 - 0.62 * ts, 0.9) + 2.5),
        (168 + ts * 124, h * 0.53 - ts * 58.0 + 6 * np.sin(ts * 4.0), 11.5 * np.power(1.0 - 0.80 * ts, 1.1) + 1.8),
        (108 + ts * 88, h * 0.59 + ts * 46.0, 9.5 * np.power(1.0 - 0.82 * ts, 1.1) + 1.6),
    ]
    for px, py, pw in branches:
        dist = np.hypot(xx[..., None] - px[None, None, ::2], yy[..., None] - py[None, None, ::2])
        rel = dist / pw[None, None, ::2]
        min_rel = rel.min(axis=-1) + (warp - 0.5) * 0.20
        inside = min_rel < 1.0
        ch = np.sqrt(np.clip(1.0 - min_rel * min_rel, 0.0, 1.0)) * 0.72 + 0.08 * wood_fiber
        height = np.maximum(height, np.where(inside, ch, 0.0))
        mask |= inside

    c_d = np.array([112.0, 102.0, 86.0], dtype=np.float32)
    c_l = np.array([186.0, 174.0, 152.0], dtype=np.float32)
    rgb = c_d + (c_l - c_d) * np.clip(0.65 * wood_fiber + 0.35 * warp, 0.0, 1.0)[..., None]
    medt = ndimage.distance_transform_edt(mask).astype(np.float32)
    ink = np.clip((0.19 - wood_fiber) * 3.8, 0.0, 0.48) * mask * (medt > 2.5)
    out = shade_height_painterly(height, mask, rgb, ink_creases=ink, bevel_strength=0.48, outer_ink_px=2.0, seed=seed)
    return trim_and_fit_rgba(out, max_dim=360, pad_frac=0.05)


def synth_tundra_heather(seed: int, size: int = 300) -> np.ndarray:
    """Synthesize low wind-swept subarctic heather and russet dwarf-birch scrub cushion."""
    rng = np.random.default_rng(seed)
    yy, xx = np.meshgrid(np.arange(size, dtype=np.float32), np.arange(size, dtype=np.float32), indexing="ij")
    cx, cy = size * 0.5, size * 0.5
    height = np.zeros((size, size), dtype=np.float32)
    mask = np.zeros((size, size), dtype=bool)
    rgb = np.zeros((size, size, 3), dtype=np.float32)
    ink = np.zeros((size, size), dtype=np.float32)
    warp = warped_fbm(size, seed=seed + 61, min_freq=6, max_freq=60, warp_px=10.0)

    clumps = [
        (cx - 26, cy - 10, 56.0, np.array([92.0, 84.0, 44.0]), np.array([132.0, 114.0, 58.0])),
        (cx + 28, cy + 12, 50.0, np.array([104.0, 66.0, 54.0]), np.array([146.0, 96.0, 74.0])),
        (cx - 4, cy + 24, 44.0, np.array([76.0, 88.0, 52.0]), np.array([118.0, 126.0, 72.0])),
    ]
    for bx, by, br, cd, cl in clumps:
        dx, dy = xx - bx, yy - by
        d = np.hypot(dx, dy)
        ang = np.arctan2(dy, dx)
        eff = br * (0.86 + 0.14 * np.cos(ang * 9.0) + 0.22 * (warp - 0.5))
        cm = d < eff
        cedt = ndimage.distance_transform_edt(cm).astype(np.float32)
        ch = 0.35 + 0.35 * np.clip(cedt / 24.0, 0.0, 1.0) + 0.12 * warp
        ccol = cd.astype(np.float32) + (cl.astype(np.float32) - cd.astype(np.float32)) * warp[..., None]
        rgb = np.where(cm[..., None], ccol, rgb)
        height = np.where(cm, ch, height)
        ink = np.where(cm, np.clip(1.8 - cedt, 0.0, 1.0) * 0.78, ink)
        mask |= cm

    out = shade_height_painterly(height, mask, rgb, ink_creases=ink, bevel_strength=0.38, outer_ink_px=1.9, seed=seed)
    return trim_and_fit_rgba(out, max_dim=size, pad_frac=0.05)


def synth_moss_pillow_decal(seed: int, size: int = 256) -> np.ndarray:
    """Synthesize lumpy woodland moss pillow cluster with small grey stones (matching reference path edge)."""
    rng = np.random.default_rng(seed)
    yy, xx = np.meshgrid(np.arange(size, dtype=np.float32), np.arange(size, dtype=np.float32), indexing="ij")
    height = np.zeros((size, size), dtype=np.float32)
    mask = np.zeros((size, size), dtype=bool)
    rgb = np.zeros((size, size, 3), dtype=np.float32)
    ink = np.zeros((size, size), dtype=np.float32)
    warp = warped_fbm(size, seed=seed + 3, min_freq=5, max_freq=48, warp_px=9.0)

    # 2 small tucked slate stones first
    for sx, sy, sr in [(size * 0.34, size * 0.36, 22.0), (size * 0.68, size * 0.62, 18.0)]:
        d = np.hypot(xx - sx, yy - sy) + (warp - 0.5) * 8.0
        sm = d < sr
        sedt = ndimage.distance_transform_edt(sm).astype(np.float32)
        rgb = np.where(sm[..., None], np.array([112.0, 114.0, 108.0], dtype=np.float32) * (0.85 + 0.3 * warp[..., None]), rgb)
        height = np.where(sm, 0.36 + 0.15 * np.clip(sedt / 8.0, 0.0, 1.0), height)
        ink = np.where(sm, np.clip(1.8 - sedt, 0.0, 1.0) * 0.85, ink)
        mask |= sm

    # 5 plump moss pillows
    pillows = [
        (size * 0.46, size * 0.52, 42.0),
        (size * 0.64, size * 0.42, 32.0),
        (size * 0.30, size * 0.58, 29.0),
        (size * 0.54, size * 0.68, 26.0),
        (size * 0.40, size * 0.32, 22.0),
    ]
    for px, py, pr in pillows:
        dx, dy = xx - px, yy - py
        ang = np.arctan2(dy, dx)
        eff = pr * (0.88 + 0.12 * np.cos(ang * 6.0 + px) + 0.22 * (warp - 0.5))
        pm = np.hypot(dx, dy) < eff
        synth_moss_overlay(rgb, height, ink, pm, seed=seed + int(px), palette="woodland")
        mask |= pm

    out = shade_height_painterly(height, mask, rgb, ink_creases=ink, bevel_strength=0.38, outer_ink_px=1.8, seed=seed)
    return trim_and_fit_rgba(out, max_dim=size, pad_frac=0.05)


def synth_forgetmenot_decal(seed: int, size: int = 224) -> np.ndarray:
    """Synthesize subtle woodland forget-me-not blue wildflower cluster (matching reference bottom-right)."""
    rng = np.random.default_rng(seed)
    yy, xx = np.meshgrid(np.arange(size, dtype=np.float32), np.arange(size, dtype=np.float32), indexing="ij")
    height = np.zeros((size, size), dtype=np.float32)
    mask = np.zeros((size, size), dtype=bool)
    rgb = np.zeros((size, size, 3), dtype=np.float32)
    ink = np.zeros((size, size), dtype=np.float32)

    # Dark olive basal leaves
    for _ in range(14):
        ang = rng.uniform(0, 2 * math.pi)
        dist = rng.uniform(10.0, 56.0)
        lx = size * 0.5 + math.cos(ang) * dist
        ly = size * 0.5 + math.sin(ang) * dist
        ca, sa = math.cos(ang), math.sin(ang)
        ux = (xx - lx) * ca + (yy - ly) * sa
        uy = -(xx - lx) * sa + (yy - ly) * ca
        lm = ((ux / 18.0) ** 2 + (uy / 9.0) ** 2) < 1.0
        ledt = ndimage.distance_transform_edt(lm).astype(np.float32)
        rgb = np.where(lm[..., None], np.array([54.0, 82.0, 38.0], dtype=np.float32), rgb)
        height = np.where(lm, 0.28, height)
        ink = np.where(lm, np.clip(1.4 - ledt, 0.0, 1.0) * 0.72, ink)
        mask |= lm

    # 9 small sky-blue 5-petal blossoms
    for _ in range(9):
        fx = size * 0.5 + rng.uniform(-48.0, 48.0)
        fy = size * 0.5 + rng.uniform(-48.0, 48.0)
        dx, dy = xx - fx, yy - fy
        fd = np.hypot(dx, dy)
        ang = np.arctan2(dy, dx) + rng.uniform(0, 2 * math.pi)
        pr = 7.8 * (0.72 + 0.30 * np.cos(ang * 5.0))
        fm = fd < pr
        fcol = np.where(
            (fd < 2.6)[..., None],
            np.array([234.0, 202.0, 84.0], dtype=np.float32),
            np.array([108.0, 152.0, 204.0], dtype=np.float32),
        )
        fedt = ndimage.distance_transform_edt(fm).astype(np.float32)
        rgb = np.where(fm[..., None], fcol, rgb)
        height = np.where(fm, 0.62, height)
        ink = np.where(fm, np.clip(1.3 - fedt, 0.0, 1.0) * 0.78, ink)
        mask |= fm

    out = shade_height_painterly(height, mask, rgb, ink_creases=ink, bevel_strength=0.28, outer_ink_px=1.4, seed=seed)
    return trim_and_fit_rgba(out, max_dim=size, pad_frac=0.06)


def synth_mushroom_cluster(seed: int, red_caps: bool = True, size: int = 240) -> np.ndarray:
    """Synthesize top-down woodland mushroom / toadstool cluster nestled in moss."""
    rng = np.random.default_rng(seed)
    yy, xx = np.meshgrid(np.arange(size, dtype=np.float32), np.arange(size, dtype=np.float32), indexing="ij")
    height = np.zeros((size, size), dtype=np.float32)
    mask = np.zeros((size, size), dtype=bool)
    rgb = np.zeros((size, size, 3), dtype=np.float32)
    ink = np.zeros((size, size), dtype=np.float32)
    fine = periodic_fbm(size, min_freq=6, max_freq=48, seed=seed + 15)

    # Basal moss cushion
    bm = np.hypot(xx - size * 0.5, yy - size * 0.5) < 46.0 + (fine - 0.5) * 18.0
    synth_moss_overlay(rgb, height, ink, bm, seed=seed + 2, palette="woodland")
    mask |= bm

    caps = [
        (size * 0.44, size * 0.46, 28.0),
        (size * 0.64, size * 0.54, 21.0),
        (size * 0.34, size * 0.64, 16.0),
        (size * 0.56, size * 0.32, 14.0),
        (size * 0.48, size * 0.68, 11.0),
    ]
    for cx, cy, cr in caps:
        dx, dy = xx - cx, yy - cy
        d = np.hypot(dx, dy) + (fine - 0.5) * 3.0
        cm = d < cr
        cedt = ndimage.distance_transform_edt(cm).astype(np.float32)
        dome = np.sqrt(np.clip(1.0 - (d / cr) ** 2, 0.0, 1.0))
        if red_caps:
            ccol = np.array([138.0, 36.0, 26.0], dtype=np.float32) + dome[..., None] * np.array([62.0, 32.0, 18.0])
            # Irregular organic cream wart flecks (varied sizes & jittered positions)
            n_warts = max(3, int(cr * 0.28))
            for _ in range(n_warts):
                wa = rng.uniform(0, 2 * math.pi)
                wr = rng.uniform(0.12, 0.74) * cr
                wx, wy = cx + math.cos(wa) * wr, cy + math.sin(wa) * wr
                wrad = rng.uniform(1.5, max(2.2, cr * 0.14))
                sp = (np.hypot(xx - wx, yy - wy) + (fine - 0.5) * 1.4) < wrad
                ccol = np.where((sp & cm & (cedt > 2.2))[..., None], np.array([226.0, 212.0, 182.0], dtype=np.float32), ccol)
        else:
            ccol = np.array([88.0, 60.0, 38.0], dtype=np.float32) + dome[..., None] * np.array([66.0, 50.0, 30.0]) + (fine[..., None] - 0.5) * 18.0

        rgb = np.where(cm[..., None], ccol, rgb)
        height = np.where(cm, 0.45 + 0.42 * dome, height)
        ink = np.where(cm, np.clip(1.8 - cedt, 0.0, 1.0) * 0.85, ink)
        mask |= cm

    out = shade_height_painterly(height, mask, rgb, ink_creases=ink, bevel_strength=0.42, outer_ink_px=1.7, seed=seed)
    return trim_and_fit_rgba(out, max_dim=size, pad_frac=0.06)


def synth_scatter_decal(seed: int, kind: str, size: int = 256) -> np.ndarray:
    """Synthesize leaf_litter, pine_cones, coastal_shells, or snowdrift decals."""
    rng = np.random.default_rng(seed)
    yy, xx = np.meshgrid(np.arange(size, dtype=np.float32), np.arange(size, dtype=np.float32), indexing="ij")
    height = np.zeros((size, size), dtype=np.float32)
    mask = np.zeros((size, size), dtype=bool)
    rgb = np.zeros((size, size, 3), dtype=np.float32)
    ink = np.zeros((size, size), dtype=np.float32)
    fine = periodic_fbm(size, min_freq=6, max_freq=64, seed=seed + 71)

    if kind == "leaf_litter":
        leaf_cols = [
            np.array([128.0, 78.0, 38.0]),
            np.array([154.0, 104.0, 44.0]),
            np.array([98.0, 62.0, 34.0]),
            np.array([112.0, 96.0, 42.0]),
        ]
        for i in range(26):
            lx = size * 0.5 + rng.uniform(-76.0, 76.0)
            ly = size * 0.5 + rng.uniform(-76.0, 76.0)
            if math.hypot(lx - size * 0.5, ly - size * 0.5) > 84.0:
                continue
            ang = rng.uniform(0, 2 * math.pi)
            ca, sa = math.cos(ang), math.sin(ang)
            ux = (xx - lx) * ca + (yy - ly) * sa
            uy = -(xx - lx) * sa + (yy - ly) * ca
            lr = rng.uniform(11.0, 18.0)
            # Pointed oak/beech leaf silhouette
            t = ux / lr
            lw = lr * 0.48 * np.cos(np.clip(t, -1.0, 1.0) * math.pi * 0.5) * (0.85 + 0.2 * np.cos(t * 9.0))
            lm = (np.abs(t) < 1.0) & (np.abs(uy) < lw)
            ledt = ndimage.distance_transform_edt(lm).astype(np.float32)
            col = leaf_cols[i % len(leaf_cols)].astype(np.float32) * (0.85 + 0.3 * fine[..., None])
            rgb = np.where(lm[..., None], col, rgb)
            height = np.where(lm, 0.25 + 0.01 * i, height)
            ink = np.where(lm, np.clip(1.3 - ledt, 0.0, 1.0) * 0.72, ink)
            mask |= lm

    elif kind == "pine_cones":
        # Soft bed of fine fallen russet pine needle clusters + small twigs
        needle_pad = warped_fbm(size, seed=seed + 12, min_freq=5, max_freq=36, warp_px=12.0)
        rad0 = np.hypot(xx - size * 0.5, yy - size * 0.5)
        nbed = (rad0 < 58.0 + (needle_pad - 0.5) * 26.0) & (needle_pad > 0.42)
        nbed = ndimage.binary_closing(nbed, iterations=1)
        needle_tex = periodic_fbm(size, min_freq=18, max_freq=110, anisotropy=(3.0, 0.8), angle_rad=0.7, seed=seed + 18)
        rgb = np.where(nbed[..., None], np.array([86.0, 58.0, 34.0], dtype=np.float32) + needle_tex[..., None] * np.array([42.0, 28.0, 14.0]), rgb)
        height = np.where(nbed, 0.18 + 0.08 * needle_tex, height)
        mask |= nbed

        # 5 oval pinecones built from overlapping woody diamond bracts (no pinwheel spirals!)
        cones = [
            (108.0, 114.0, 19.0, 14.0, 0.4),
            (148.0, 128.0, 17.0, 12.5, -0.7),
            (122.0, 150.0, 16.0, 12.0, 1.2),
            (94.0, 140.0, 14.5, 11.0, 2.1),
            (140.0, 98.0, 14.0, 10.5, -1.5),
        ]
        for cx, cy, rx, ry, cang in cones:
            ca, sa = math.cos(cang), math.sin(cang)
            ux = (xx - cx) * ca + (yy - cy) * sa
            uy = -(xx - cx) * sa + (yy - cy) * ca
            d2 = (ux / rx) ** 2 + (uy / ry) ** 2
            # Overlapping bract diamond grid along cone axis
            bract = np.abs(np.sin(ux * 0.42) * np.cos(uy * 0.48))
            cm = d2 < (0.92 + 0.14 * bract)
            cedt = ndimage.distance_transform_edt(cm).astype(np.float32)
            dome = np.sqrt(np.clip(1.0 - d2, 0.0, 1.0))
            ccol = np.array([64.0, 40.0, 24.0], dtype=np.float32) + np.clip(0.55 * bract + 0.45 * dome, 0.0, 1.0)[..., None] * np.array([72.0, 48.0, 26.0])
            rgb = np.where(cm[..., None], ccol, rgb)
            height = np.where(cm, 0.44 + 0.32 * dome + 0.08 * bract, height)
            ink = np.where(cm, np.maximum(np.clip(1.6 - cedt, 0.0, 1.0) * 0.86, np.clip((0.18 - bract) * 3.5, 0.0, 0.55)), ink)
            mask |= cm

    elif kind == "coastal_shells":
        items_s = [
            (104, 112, 18.0, "scallop", 0.5, np.array([218.0, 198.0, 170.0])),
            (146, 122, 14.5, "scallop", -1.1, np.array([204.0, 172.0, 146.0])),
            (122, 148, 17.0, "star", 0.3, np.array([186.0, 112.0, 68.0])),
            (134, 94, 11.5, "pebble", 0.0, np.array([118.0, 122.0, 120.0])),
            (96, 140, 12.5, "pebble", 0.0, np.array([148.0, 140.0, 124.0])),
            (118, 124, 8.5, "pebble", 0.0, np.array([94.0, 98.0, 98.0])),
        ]
        for sx, sy, sr, stype, sang, scol in items_s:
            dx, dy = xx - sx + (fine - 0.5) * 2.2, yy - sy + (fine - 0.5) * 2.2
            d = np.hypot(dx, dy)
            ang = np.arctan2(dy, dx) - sang
            if stype == "scallop":
                ribs = 0.5 + 0.5 * np.cos(ang * 9.0)
                fan = sr * (0.72 + 0.28 * np.cos(ang) + 0.06 * ribs)
                sm = d < fan
                col = scol.astype(np.float32) * (0.80 + 0.24 * ribs[..., None])
            elif stype == "star":
                # Gently curved organic sea-star arms
                ang_c = ang + 0.18 * (d / sr)
                arm = sr * (0.38 + 0.62 * np.power(0.5 + 0.5 * np.cos(ang_c * 5.0), 1.8))
                sm = d < arm
                col = scol.astype(np.float32) * (0.84 + 0.24 * fine[..., None])
            else:
                sm = d < sr * (0.88 + 0.20 * fine)
                col = scol.astype(np.float32) * (0.84 + 0.26 * fine[..., None])
            sedt = ndimage.distance_transform_edt(sm).astype(np.float32)
            rgb = np.where(sm[..., None], col, rgb)
            height = np.where(sm, 0.40 + 0.24 * np.clip(sedt / 6.0, 0.0, 1.0), height)
            ink = np.where(sm, np.clip(1.5 - sedt, 0.0, 1.0) * 0.80, ink)
            mask |= sm

    elif kind == "snowdrift":
        warp = warped_fbm(size, seed=seed + 9, min_freq=3, max_freq=24, warp_px=14.0)
        dx = (xx - size * 0.5) / 86.0
        dy = (yy - size * 0.5) / 56.0
        d = np.hypot(dx, dy) + (warp - 0.5) * 0.35
        mask = d < 1.0
        edt = ndimage.distance_transform_edt(mask).astype(np.float32)
        ridge = np.exp(-((dy + 0.18 * np.sin(dx * 3.0)) ** 2) / 0.18)
        height = np.where(mask, 0.25 + 0.45 * ridge * np.clip(edt / 22.0, 0.0, 1.0), 0.0)
        rgb = np.array([224.0, 234.0, 246.0], dtype=np.float32) + (fine[..., None] - 0.5) * 14.0
        out = shade_height_painterly(height, mask, rgb, ink_creases=None, bevel_strength=0.28, outer_ink_px=0.9, seed=seed)
        # Soft feathered edges on snowdrift so it blends into snow/tundra seamlessly
        soft_a = np.clip(edt / 10.0, 0.0, 1.0)
        out[..., 3] = (out[..., 3].astype(np.float32) * soft_a).astype(np.uint8)
        return trim_and_fit_rgba(out, max_dim=size, pad_frac=0.05)

    out = shade_height_painterly(height, mask, rgb, ink_creases=ink, bevel_strength=0.36, outer_ink_px=1.5, seed=seed)
    return trim_and_fit_rgba(out, max_dim=size, pad_frac=0.06)


# ---------------------------------------------------------------------------
# Seamless Painterly Ground Palette Builder (`512x512` RGBA8)
# ---------------------------------------------------------------------------


def build_seamless_palette_texture(
    kind: str,
    seed: int,
    base_tex_a: np.ndarray | None = None,
    base_tex_b: np.ndarray | None = None,
) -> Image.Image:
    """Build a rich, 100% wrap-seamless 512x512 RGBA8 ground texture with zero grid artifacts."""
    n = PALETTE_SIZE
    f_macro = warped_fbm(n, seed=seed + 1, min_freq=2, max_freq=10, warp_px=24.0)
    f_med = warped_fbm(n, seed=seed + 2, min_freq=6, max_freq=28, warp_px=12.0)
    f_fine = periodic_fbm(n, min_freq=16, max_freq=128, beta=1.45, seed=seed + 3)
    f_blade = periodic_fbm(n, min_freq=18, max_freq=140, beta=1.35, anisotropy=(2.4, 0.7), angle_rad=0.55, seed=seed + 4)

    # Extract normalized zero-mean periodic micro-detail from real warehouse base texture(s)
    detail_rgb = np.zeros((n, n, 3), dtype=np.float32)
    if base_tex_a is not None:
        sa = make_toroidal_seamless(base_tex_a, flatten_sigma=28.0)
        sa_dev = sa - sa.mean(axis=(0, 1), keepdims=True)
        # Clamp extreme bright/dark outlier pixels (eliminates baked white flower dots or specular flecks!)
        sa_dev = np.clip(sa_dev, -36.0, 36.0)
        if base_tex_b is not None:
            sb = make_toroidal_seamless(base_tex_b, flatten_sigma=28.0)
            sb_dev = np.clip(sb - sb.mean(axis=(0, 1), keepdims=True), -36.0, 36.0)
            mix = f_macro[..., None]
            detail_rgb = sa_dev * mix + sb_dev * (1.0 - mix)
        else:
            detail_rgb = sa_dev

    # Color palettes & surface micro-relief tuned to match `media_1795821203623_e3cd2937.jpg`
    presets: dict[str, tuple[tuple[int, int, int], tuple[int, int, int], tuple[int, int, int], float]] = {
        # (shadow_rgb, mid_rgb, highlight_rgb, detail_gain)
        "grass_lush": ((46, 72, 30), (74, 104, 42), (108, 134, 54), 0.72),
        "grass_clover": ((42, 68, 28), (68, 98, 40), (104, 132, 52), 0.70),
        "grass_meadow": ((54, 78, 34), (84, 110, 46), (118, 138, 60), 0.72),
        "grass_forest": ((46, 70, 30), (74, 100, 41), (106, 128, 53), 0.70),
        "grass_dry": ((92, 88, 46), (126, 116, 62), (156, 142, 78), 0.68),
        "grass_savanna": ((98, 84, 38), (134, 114, 52), (166, 140, 68), 0.70),
        "grass_cold": ((76, 82, 52), (106, 110, 72), (138, 136, 94), 0.65),
        "tundra": ((78, 76, 56), (108, 104, 78), (142, 138, 112), 0.68),
        "tundra_lichen": ((72, 76, 52), (104, 106, 74), (152, 154, 126), 0.68),
        "forest_floor": ((44, 38, 24), (72, 62, 36), (98, 88, 46), 0.75),
        "forest_floor_moss": ((36, 48, 24), (62, 76, 34), (94, 110, 44), 0.72),
        "forest_floor_pine": ((52, 38, 24), (84, 60, 36), (114, 84, 48), 0.72),
        "swamp": ((38, 46, 22), (62, 70, 32), (88, 94, 42), 0.68),
        "mud": ((50, 38, 26), (72, 56, 38), (94, 74, 52), 0.62),
        "dirt": ((78, 58, 38), (112, 84, 56), (142, 108, 74), 0.74),
        "dirt_dry": ((104, 82, 54), (138, 110, 74), (168, 136, 92), 0.70),
        "rock_gray": ((72, 74, 72), (104, 106, 102), (138, 138, 132), 0.82),
        "rock_slate": ((54, 58, 60), (84, 88, 90), (118, 122, 122), 0.82),
        "rock_brown": ((66, 54, 42), (96, 80, 62), (128, 108, 84), 0.80),
        "rock_sandstone": ((112, 64, 40), (156, 94, 58), (194, 128, 80), 0.78),
        "rock_mountain": ((82, 84, 86), (116, 118, 118), (150, 152, 148), 0.82),
        "rock_snow": ((168, 182, 202), (208, 220, 236), (240, 245, 252), 0.62),
        "snow": ((184, 200, 222), (222, 232, 245), (247, 250, 254), 0.52),
        "ice": ((114, 148, 168), (148, 180, 198), (188, 212, 226), 0.65),
        "sand_beach": ((166, 148, 116), (194, 176, 142), (218, 202, 168), 0.55),
        "beach_wet": ((128, 112, 86), (156, 138, 108), (182, 164, 132), 0.58),
        "sand_desert": ((168, 128, 88), (200, 158, 112), (226, 186, 138), 0.65),
        "sand_dunes": ((172, 132, 86), (206, 164, 112), (232, 194, 140), 0.62),
        "water_ocean": ((20, 52, 88), (32, 76, 118), (50, 102, 144), 0.52),
        "water_shallow": ((26, 82, 92), (42, 112, 118), (68, 138, 140), 0.52),
    }
    c0, c1, c2, gain = presets[kind]
    p0 = np.array(c0, dtype=np.float32)
    p1 = np.array(c1, dtype=np.float32)
    p2 = np.array(c2, dtype=np.float32)

    # Compose smooth organic painterly tone field
    if "sand" in kind or "beach" in kind or kind == "snow":
        ripples = periodic_fbm(n, min_freq=6, max_freq=32, beta=1.8, anisotropy=(3.2, 0.7), angle_rad=0.42, seed=seed + 19)
        t = np.clip(0.38 * f_macro + 0.38 * ripples + 0.24 * f_fine, 0.0, 1.0)[..., None]
    elif "grass" in kind:
        t = np.clip(0.36 * f_macro + 0.30 * f_med + 0.34 * f_blade, 0.0, 1.0)[..., None]
    elif "rock" in kind:
        strata = periodic_fbm(n, min_freq=4, max_freq=42, beta=1.55, anisotropy=(2.2, 0.85), angle_rad=-0.45, seed=seed + 23)
        t = np.clip(0.34 * f_macro + 0.40 * strata + 0.26 * f_fine, 0.0, 1.0)[..., None]
    else:
        t = np.clip(0.40 * f_macro + 0.34 * f_med + 0.26 * f_fine, 0.0, 1.0)[..., None]

    rgb = np.where(t < 0.5, p0 + (p1 - p0) * (t * 2.0), p1 + (p2 - p1) * ((t - 0.5) * 2.0))
    rgb += detail_rgb * gain

    # Surface-specific painterly accents (strictly periodic!)
    if kind in ("forest_floor", "forest_floor_moss"):
        # Blend rich woodland moss pockets + warm fallen leaf flecks into dark humus
        moss_p = np.clip((f_macro - 0.52) * 2.8, 0.0, 1.0)[..., None]
        moss_c = np.array([58.0, 82.0, 32.0], dtype=np.float32) + (f_blade[..., None] - 0.5) * 26.0
        rgb = rgb * (1.0 - 0.55 * moss_p) + moss_c * (0.55 * moss_p)
        leaf_fleck = (f_fine > 0.84) & (f_med > 0.45)
        rgb = np.where(leaf_fleck[..., None], rgb * 0.65 + np.array([128.0, 86.0, 44.0], dtype=np.float32) * 0.35, rgb)
    elif kind in ("tundra", "tundra_lichen", "grass_cold"):
        # Subarctic olive-russet moss + pale lichen patches
        lich = np.clip((f_med - 0.62) * 2.6, 0.0, 0.65)[..., None]
        rgb = rgb * (1.0 - lich) + np.array([152.0, 156.0, 128.0], dtype=np.float32) * lich
    elif "rock" in kind and kind != "rock_snow":
        # Subtle dark periodic rock fissures (wrap-safe gradient!)
        fiss = np.abs(f_med - 0.5)
        crack = np.clip((0.022 - fiss) * 32.0, 0.0, 0.38)[..., None]
        rgb *= 1.0 - crack
    elif kind == "ice":
        fiss = np.abs(f_med - 0.5)
        frost_line = np.clip((0.020 - fiss) * 34.0, 0.0, 0.45)[..., None]
        rgb = rgb * (1.0 - frost_line) + np.array([228.0, 242.0, 250.0], dtype=np.float32) * frost_line

    # Subtle periodic NW micro-relief bump so ground feels crisp and tactile at gameplay zoom
    bump = 0.42 * f_med + 0.58 * f_fine
    gx = np.roll(bump, -1, axis=1) - np.roll(bump, 1, axis=1)
    gy = np.roll(bump, -1, axis=0) - np.roll(bump, 1, axis=0)
    relief_scale = 20.0 if kind != "snow" else 16.0
    rgb += (-gx - gy)[..., None] * relief_scale
    rgb += detail_rgb * (gain * 0.24)

    rgba = np.zeros((n, n, 4), dtype=np.uint8)
    rgba[..., :3] = np.clip(rgb, 0.0, 255.0).astype(np.uint8)
    rgba[..., 3] = 255
    return Image.fromarray(rgba, mode="RGBA")


# ---------------------------------------------------------------------------
# Curated Asset Roster & Metadata
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class SpriteSpec:
    filename: str
    tier: str  # decal | understory | prop | canopy
    min_size_tiles: float
    max_size_tiles: float
    rotatable: bool
    casts_shadow: bool
    biomes: list[str]
    source_kind: str  # existing | warehouse | synthesized
    warehouse_rel: str  # relative to warehouse root if warehouse/existing
    attribution: str
    # Processing options for warehouse sprites or generator callback for synthesized sprites
    max_dim: int = 384
    sat_scale: float = 0.96
    val_scale: float = 0.96
    gamma: float = 1.03
    warm_shift: tuple[float, float, float] = (1.0, 1.0, 0.97)
    ink_strength: float = 0.40
    recolor_deadwood: bool = False
    generator: Callable[[], np.ndarray] | None = None


def build_sprite_specs(rock_grain: np.ndarray | None) -> list[SpriteSpec]:
    return [
        # --- 14 Existing Sprites (preserved at exact filenames) ---
        SpriteSpec("grassclump_a.png", "decal", 0.40, 0.75, False, False, ["lush_meadow", "temperate_forest", "savanna", "tundra"], "existing", "nature/plants/temperate/brush/grassclump_a.png", "Maptool Backgrounds, Enviroment/Plants/Temperate/Brush"),
        SpriteSpec("grassclump_c.png", "decal", 0.40, 0.75, False, False, ["lush_meadow", "temperate_forest", "savanna", "tundra"], "existing", "nature/plants/temperate/brush/grassclump_c.png", "Maptool Backgrounds, Enviroment/Plants/Temperate/Brush"),
        SpriteSpec("grassclump_g.png", "decal", 0.40, 0.75, False, False, ["lush_meadow", "temperate_forest", "savanna", "tundra"], "existing", "nature/plants/temperate/brush/grassclump_g.png", "Maptool Backgrounds, Enviroment/Plants/Temperate/Brush"),
        SpriteSpec("grass_patch5_blu.png", "decal", 0.45, 0.85, True, False, ["lush_meadow", "temperate_forest", "swamp"], "existing", "nature/plants/temperate/brush/grass_patch5_blu.png", "Maptool Backgrounds, Enviroment/Plants/Temperate/Brush"),
        SpriteSpec("dense_clover_patch_plant.png", "decal", 0.40, 0.75, True, False, ["lush_meadow", "temperate_forest", "swamp"], "existing", "nature/plants/dense_clover_patch_plant.png", "Maptool Objects, N Bushes plants stuff/Clovers 01"),
        SpriteSpec("white_aa_a.png", "decal", 0.35, 0.55, False, False, ["lush_meadow", "alpine"], "existing", "nature/plants/flowers/white_aa_a.png", "Maptool Backgrounds, Enviroment/Plants/Flowers"),
        SpriteSpec("wasurenagusa_blu.png", "decal", 0.35, 0.55, True, False, ["lush_meadow", "temperate_forest"], "existing", "nature/plants/flowers/wasurenagusa_blu.png", "Maptool Backgrounds, Enviroment/Plants/Flowers"),
        SpriteSpec("red_flower_jhd_c.png", "decal", 0.30, 0.45, True, False, ["lush_meadow", "jungle"], "existing", "nature/plants/flowers/red_flower_jhd_c.png", "Maptool Backgrounds, Enviroment/Plants/Flowers"),
        SpriteSpec("zinnia_gold_small_b.png", "decal", 0.35, 0.50, True, False, ["lush_meadow", "savanna"], "existing", "nature/plants/flowers/zinnia_gold_small_b.png", "Maptool Backgrounds, Enviroment/Plants/Flowers"),
        SpriteSpec("zinnia_purple_small_b.png", "decal", 0.35, 0.50, True, False, ["lush_meadow", "tundra"], "existing", "nature/plants/flowers/zinnia_purple_small_b.png", "Maptool Backgrounds, Enviroment/Plants/Flowers"),
        SpriteSpec("rock1sm.png", "decal", 0.30, 0.55, True, False, ["alpine", "tundra", "temperate_forest", "coast"], "existing", "nature/rocks/generic/rock1sm.png", "Maptool Backgrounds, Enviroment/Rocky/Generic"),
        SpriteSpec("rock3s.png", "decal", 0.30, 0.55, True, False, ["alpine", "tundra", "temperate_forest", "coast"], "existing", "nature/rocks/generic/rock3s.png", "Maptool Backgrounds, Enviroment/Rocky/Generic"),
        SpriteSpec("stones_dotw_tan_gt_lrk.png", "decal", 0.45, 0.85, True, False, ["desert", "savanna", "coast", "lush_meadow"], "existing", "nature/rocks/cluster/stones_dotw_tan_gt_lrk.png", "Maptool Backgrounds, Enviroment/Rocky/Cluster"),
        SpriteSpec("random_stones03.png", "decal", 0.45, 0.85, True, False, ["alpine", "tundra", "coast", "desert"], "existing", "nature/rocks/cluster/random_stones03.png", "Maptool Backgrounds, Enviroment/Rocky/Cluster"),

        # --- Tier 4: Canopies & Spreading Root Trunks (`canopy`, 2.5 - 5.5 tiles) ---
        SpriteSpec("canopy_gnarled_roots_a.png", "canopy", 3.2, 5.2, True, True, ["temperate_forest", "jungle", "swamp"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=512, generator=lambda: synth_gnarled_root_trunk(seed=101, n_roots=10, size=512)),
        SpriteSpec("canopy_gnarled_roots_b.png", "canopy", 3.0, 4.8, True, True, ["temperate_forest", "jungle", "swamp"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=512, generator=lambda: synth_gnarled_root_trunk(seed=205, n_roots=8, size=512)),
        SpriteSpec("canopy_temperate_billow_a.png", "canopy", 3.2, 5.4, True, True, ["temperate_forest", "lush_meadow"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=512, generator=lambda: synth_billowing_canopy(seed=301, style="temperate_olive", size=512)),
        SpriteSpec("canopy_temperate_billow_b.png", "canopy", 3.0, 5.0, True, True, ["temperate_forest", "lush_meadow"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=512, generator=lambda: synth_billowing_canopy(seed=309, style="temperate_emerald", size=512)),
        SpriteSpec("canopy_oak_crown_a.png", "canopy", 3.0, 5.2, True, True, ["temperate_forest", "lush_meadow"], "warehouse", "nature/plants/temperate/trees/oak/oak_aa3.png", "Maptool Backgrounds, Enviroment/Plants/Temperate/Trees/Oak", max_dim=512, sat_scale=0.92, val_scale=0.95, gamma=1.06, warm_shift=(1.02, 1.0, 0.92), ink_strength=0.46),
        SpriteSpec("canopy_oak_crown_b.png", "canopy", 3.0, 5.0, True, True, ["temperate_forest", "lush_meadow"], "warehouse", "nature/plants/temperate/trees/cluster/trees03.png", "Maptool Backgrounds, Enviroment/Plants/Temperate/Trees/Cluster", max_dim=512, sat_scale=1.08, val_scale=0.94, gamma=1.08, warm_shift=(0.98, 1.03, 0.90), ink_strength=0.48),
        SpriteSpec("canopy_broadleaf_crown_a.png", "canopy", 3.2, 5.4, True, True, ["temperate_forest", "jungle", "savanna"], "warehouse", "nature/plants/warm/trees/baobab/baobab.png", "Maptool Backgrounds, Enviroment/Plants/Warm/Trees/Baobab", max_dim=512, sat_scale=1.02, val_scale=0.96, gamma=1.05, warm_shift=(1.0, 1.02, 0.92), ink_strength=0.46),
        SpriteSpec("canopy_jungle_emergent.png", "canopy", 3.4, 5.5, True, True, ["jungle"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=512, generator=lambda: synth_billowing_canopy(seed=407, style="jungle_emergent", size=512)),
        SpriteSpec("canopy_palm_fan_a.png", "canopy", 2.6, 4.4, True, True, ["jungle", "coast"], "warehouse", "nature/plants/warm/trees/palm/evermotion_plants001.png", "Maptool Backgrounds, Enviroment/Plants/Warm/Trees/Palm", max_dim=448, sat_scale=0.94, val_scale=0.90, gamma=1.10, warm_shift=(0.94, 1.02, 0.88), ink_strength=0.48),
        SpriteSpec("canopy_palm_cluster_b.png", "canopy", 2.8, 4.8, True, True, ["jungle", "coast"], "warehouse", "nature/plants/warm/trees/palm/cocopalm_aa5.png", "Maptool Backgrounds, Enviroment/Plants/Warm/Trees/Palm", max_dim=512, sat_scale=0.96, val_scale=0.98, gamma=1.02, warm_shift=(1.02, 1.0, 0.94), ink_strength=0.44),
        SpriteSpec("canopy_pine_boreal_a.png", "canopy", 2.6, 4.4, True, True, ["taiga", "alpine", "temperate_forest"], "warehouse", "nature/plants/temperate/trees/fir/fir_aa5.png", "Maptool Backgrounds, Enviroment/Plants/Temperate/Trees/Fir", max_dim=448, sat_scale=0.98, val_scale=0.98, gamma=1.02, warm_shift=(1.0, 1.02, 0.95), ink_strength=0.42),
        SpriteSpec("canopy_pine_boreal_b.png", "canopy", 2.6, 4.4, True, True, ["taiga", "alpine"], "warehouse", "nature/plants/temperate/trees/pine/evermotion_plants022.png", "Maptool Backgrounds, Enviroment/Plants/Temperate/Trees/Pine", max_dim=448, sat_scale=1.04, val_scale=0.90, gamma=1.08, warm_shift=(0.96, 1.02, 0.92), ink_strength=0.46),
        SpriteSpec("canopy_spruce_boreal_c.png", "canopy", 2.5, 4.2, True, True, ["taiga", "alpine", "tundra"], "warehouse", "nature/plants/temperate/trees/spruce/evermotion_plants057.png", "Maptool Backgrounds, Enviroment/Plants/Temperate/Trees/Spruce", max_dim=448, sat_scale=1.08, val_scale=0.86, gamma=1.12, warm_shift=(0.90, 1.02, 0.94), ink_strength=0.48),
        SpriteSpec("canopy_pine_snow_a.png", "canopy", 2.6, 4.4, True, True, ["taiga", "alpine", "snowfield", "tundra"], "warehouse", "nature/plants/cold/trees/snowy_pine_c.png", "Maptool Backgrounds, Enviroment/Plants/Cold/Trees", max_dim=448, sat_scale=0.96, val_scale=0.96, gamma=1.02, warm_shift=(0.98, 1.0, 1.02), ink_strength=0.42),
        SpriteSpec("canopy_pine_snow_b.png", "canopy", 2.5, 4.0, True, True, ["taiga", "alpine", "snowfield", "tundra"], "warehouse", "nature/plants/cold/trees/snowy_pine_b.png", "Maptool Backgrounds, Enviroment/Plants/Cold/Trees", max_dim=416, sat_scale=0.96, val_scale=0.96, gamma=1.02, warm_shift=(0.98, 1.0, 1.02), ink_strength=0.42),
        SpriteSpec("canopy_swamp_willow.png", "canopy", 2.8, 4.8, True, True, ["swamp"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=512, generator=lambda: synth_billowing_canopy(seed=511, style="swamp_willow", size=512)),
        SpriteSpec("canopy_deadwood_snag_a.png", "canopy", 2.5, 4.2, True, True, ["swamp", "savanna", "tundra", "desert"], "warehouse", "nature/plants/cold/trees/treewhite.png", "Maptool Backgrounds, Enviroment/Plants/Cold/Trees", max_dim=448, recolor_deadwood=True, ink_strength=0.52),
        SpriteSpec("canopy_deadwood_snag_b.png", "canopy", 2.6, 4.4, True, True, ["swamp", "savanna", "taiga"], "warehouse", "nature/plants/dead/trees/tree4_700_pix.png", "Maptool Backgrounds, Enviroment/Plants/Dead/Trees", max_dim=448, sat_scale=0.92, val_scale=0.92, gamma=1.06, warm_shift=(1.04, 0.98, 0.90), ink_strength=0.44),
        SpriteSpec("canopy_savanna_acacia.png", "canopy", 3.0, 5.0, True, True, ["savanna"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=512, generator=lambda: synth_billowing_canopy(seed=613, style="savanna_acacia", size=512)),
        SpriteSpec("canopy_savanna_scrub_a.png", "canopy", 2.5, 4.0, True, True, ["savanna", "lush_meadow"], "warehouse", "nature/plants/warm/trees/tree109.png", "Maptool Backgrounds, Enviroment/Plants/Warm/Trees", max_dim=448, sat_scale=0.92, val_scale=0.92, gamma=1.06, warm_shift=(1.06, 1.0, 0.86), ink_strength=0.44),

        # --- Tier 3: Mid-Scale Props (`prop`, 1.2 - 3.8 tiles) ---
        SpriteSpec("prop_log_mossy_fallen_a.png", "prop", 2.2, 3.8, True, True, ["temperate_forest", "jungle", "taiga", "swamp"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=480, generator=lambda: synth_mossy_fallen_log(seed=701, hollow=False)),
        SpriteSpec("prop_log_hollow_mossy_b.png", "prop", 2.0, 3.5, True, True, ["temperate_forest", "swamp", "jungle"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=480, generator=lambda: synth_mossy_fallen_log(seed=719, hollow=True)),
        SpriteSpec("prop_log_mossy_forked.png", "prop", 1.8, 3.2, True, True, ["temperate_forest", "swamp", "jungle"], "warehouse", "nature/plants/dead/fallen/decaying_log2.png", "Maptool Backgrounds, Enviroment/Plants/Dead/Fallen", max_dim=448, sat_scale=0.95, val_scale=0.94, gamma=1.06, ink_strength=0.46),
        SpriteSpec("prop_log_mossy_bark.png", "prop", 1.8, 3.2, True, True, ["temperate_forest", "taiga", "swamp"], "warehouse", "nature/plants/dead/fallen/decaying_log4.png", "Maptool Backgrounds, Enviroment/Plants/Dead/Fallen", max_dim=448, sat_scale=0.92, val_scale=0.96, gamma=1.04, ink_strength=0.44),
        SpriteSpec("prop_log_deadwood.png", "prop", 1.6, 2.8, True, True, ["taiga", "savanna", "tundra", "coast"], "warehouse", "nature/plants/dead/fallen/log_4.png", "Maptool Backgrounds, Enviroment/Plants/Dead/Fallen", max_dim=416, sat_scale=0.95, val_scale=0.92, gamma=1.06, warm_shift=(1.04, 0.99, 0.92), ink_strength=0.46),
        SpriteSpec("prop_stump_cut_wood.png", "prop", 1.2, 1.8, True, True, ["temperate_forest", "taiga", "lush_meadow"], "warehouse", "nature/plants/dead/stumps/stump.png", "Maptool Backgrounds, Enviroment/Plants/Dead/Stumps", max_dim=280, sat_scale=0.95, val_scale=0.94, gamma=1.05, ink_strength=0.44),
        SpriteSpec("prop_stump_mossy_a.png", "prop", 1.3, 2.1, True, True, ["temperate_forest", "jungle", "swamp"], "warehouse", "nature/plants/dead/stumps/smoothcut_surf2.png", "Maptool Backgrounds, Enviroment/Plants/Dead/Stumps", max_dim=320, sat_scale=0.94, val_scale=0.94, gamma=1.06, ink_strength=0.44),
        SpriteSpec("prop_stump_hollow_roots.png", "prop", 1.5, 2.4, True, True, ["temperate_forest", "swamp", "taiga"], "warehouse", "nature/plants/dead/stumps/hollow_surf2.png", "Maptool Backgrounds, Enviroment/Plants/Dead/Stumps", max_dim=352, sat_scale=1.05, val_scale=0.88, gamma=1.08, warm_shift=(1.06, 0.98, 0.88), ink_strength=0.48),
        SpriteSpec("prop_boulder_stepped_slate_a.png", "prop", 2.2, 3.8, True, True, ["temperate_forest", "alpine", "taiga", "tundra"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=480, generator=lambda: synth_stepped_boulder(seed=801, style="slate_mossy", size=480, n_steps=5, base_grain_rgb=rock_grain)),
        SpriteSpec("prop_boulder_stepped_mossy_b.png", "prop", 1.6, 2.8, True, True, ["temperate_forest", "jungle", "swamp", "lush_meadow"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=416, generator=lambda: synth_stepped_boulder(seed=817, style="forest_mossy", size=416, n_steps=4, base_grain_rgb=rock_grain)),
        SpriteSpec("prop_boulder_mossy_oval.png", "prop", 1.4, 2.4, True, True, ["temperate_forest", "lush_meadow", "swamp"], "warehouse", "nature/rocks/rounded_mossy_boulder_rock.png", "Maptool Objects, Caves/Big rocks", max_dim=352, sat_scale=1.05, val_scale=0.96, gamma=1.04, ink_strength=0.46),
        SpriteSpec("prop_boulder_mossy_round.png", "prop", 1.3, 2.2, True, True, ["temperate_forest", "taiga", "tundra"], "warehouse", "nature/rocks/round_mossy_stone_boulder.png", "Maptool Objects, Caves/Big rocks", max_dim=352, sat_scale=1.05, val_scale=0.96, gamma=1.04, ink_strength=0.46),
        SpriteSpec("prop_boulder_granite_grey.png", "prop", 1.6, 3.0, True, True, ["alpine", "tundra", "taiga", "coast"], "warehouse", "nature/rocks/boulders/boulder_big_e.png", "Maptool Backgrounds, Enviroment/Rocky/Boulders", max_dim=384, sat_scale=0.95, val_scale=0.88, gamma=1.10, ink_strength=0.50),
        SpriteSpec("prop_boulder_crag_warm.png", "prop", 1.5, 2.8, True, True, ["savanna", "desert", "alpine"], "warehouse", "nature/rocks/boulders/boulder_big_p.png", "Maptool Backgrounds, Enviroment/Rocky/Boulders", max_dim=384, sat_scale=0.98, val_scale=0.94, gamma=1.05, ink_strength=0.46),
        SpriteSpec("prop_boulder_slate_dark.png", "prop", 1.4, 2.5, True, True, ["alpine", "tundra", "coast"], "warehouse", "nature/rocks/large/rock_large1_lrk_b.png", "Maptool Backgrounds, Enviroment/Rocky/Large", max_dim=352, sat_scale=0.90, val_scale=1.04, gamma=1.0, warm_shift=(1.02, 1.0, 0.94), ink_strength=0.46),
        SpriteSpec("prop_boulder_sandstone_mesa_a.png", "prop", 1.8, 3.4, True, True, ["desert", "savanna"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=448, generator=lambda: synth_stepped_boulder(seed=833, style="sandstone", size=448, n_steps=5, base_grain_rgb=rock_grain)),
        SpriteSpec("prop_boulder_sandstone_crag_b.png", "prop", 1.3, 2.3, True, True, ["desert", "savanna"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=352, generator=lambda: synth_stepped_boulder(seed=849, style="sandstone_crag", size=352, n_steps=4, base_grain_rgb=rock_grain)),
        SpriteSpec("prop_boulder_snow_crag_a.png", "prop", 1.8, 3.2, True, True, ["alpine", "snowfield", "tundra"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=448, generator=lambda: synth_stepped_boulder(seed=865, style="snow_crag", size=448, n_steps=5, base_grain_rgb=rock_grain)),
        SpriteSpec("prop_boulder_snow_cluster.png", "prop", 1.5, 2.6, True, True, ["alpine", "snowfield", "tundra"], "warehouse", "nature/rocks/snow/snowy_rocks_f.png", "Maptool Backgrounds, Enviroment/Rocky/Snow", max_dim=384, sat_scale=0.92, val_scale=0.95, gamma=1.04, ink_strength=0.44),
        SpriteSpec("prop_boulder_coastal_wet_a.png", "prop", 1.5, 2.8, True, True, ["coast"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=416, generator=lambda: synth_stepped_boulder(seed=881, style="coastal_wet", size=416, n_steps=4, base_grain_rgb=rock_grain)),
        SpriteSpec("prop_stones_mossy_cluster.png", "prop", 1.2, 2.0, True, True, ["temperate_forest", "swamp", "lush_meadow", "coast"], "warehouse", "nature/rocks/cluster/random_stones26.png", "Maptool Backgrounds, Enviroment/Rocky/Cluster", max_dim=352, sat_scale=0.98, val_scale=0.94, gamma=1.05, ink_strength=0.44),
        SpriteSpec("prop_stones_crag_cluster.png", "prop", 1.2, 2.0, True, True, ["alpine", "tundra", "taiga"], "warehouse", "nature/rocks/cluster/random_stones02.png", "Maptool Backgrounds, Enviroment/Rocky/Cluster", max_dim=352, sat_scale=0.96, val_scale=0.94, gamma=1.05, ink_strength=0.44),
        SpriteSpec("prop_stones_cairn_pile.png", "prop", 1.2, 1.9, True, True, ["alpine", "tundra", "desert", "savanna"], "warehouse", "nature/rocks/cluster/rocks_dotw_gt.png", "Maptool Backgrounds, Enviroment/Rocky/Cluster", max_dim=320, sat_scale=0.94, val_scale=0.95, gamma=1.04, ink_strength=0.42),

        # --- Tier 2: Understory & Shrubs (`understory`, 0.8 - 2.2 tiles) ---
        SpriteSpec("understory_fern_fronds_a.png", "understory", 1.0, 1.8, True, True, ["temperate_forest", "jungle", "taiga", "swamp"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=352, generator=lambda: synth_fern_or_bracken(seed=901, multi_cluster=False, size=352)),
        SpriteSpec("understory_bracken_bed_b.png", "understory", 1.3, 2.2, True, True, ["temperate_forest", "jungle", "taiga"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=384, generator=lambda: synth_fern_or_bracken(seed=919, multi_cluster=True, size=384)),
        SpriteSpec("understory_fern_wild_a.png", "understory", 0.9, 1.6, True, True, ["temperate_forest", "taiga", "swamp"], "warehouse", "nature/plants/large_wild_fern.png", "Maptool Objects, N Bushes plants stuff/Fern 02", max_dim=320, sat_scale=1.06, val_scale=0.94, gamma=1.06, warm_shift=(0.98, 1.04, 0.88), ink_strength=0.46),
        SpriteSpec("understory_fern_lush_b.png", "understory", 0.9, 1.6, True, True, ["temperate_forest", "jungle", "swamp"], "warehouse", "nature/plants/lush_forest_fern.png", "Maptool Objects, N Bushes plants stuff/Fern 02", max_dim=320, sat_scale=1.06, val_scale=0.94, gamma=1.06, warm_shift=(0.98, 1.04, 0.88), ink_strength=0.46),
        SpriteSpec("understory_fern_spread_c.png", "understory", 1.0, 1.9, True, True, ["jungle", "temperate_forest"], "warehouse", "nature/plants/fern_big_02.png", "Maptool Objects, N Bushes plants stuff/Fern big 02", max_dim=320, sat_scale=0.96, val_scale=0.94, gamma=1.05, ink_strength=0.44),
        SpriteSpec("understory_jungle_broadleaf_a.png", "understory", 1.0, 1.9, True, True, ["jungle", "swamp"], "warehouse", "nature/plants/large_leafy_jungle_plant.png", "Maptool Objects, N Bushes plants stuff/Elephant ear 01", max_dim=300, sat_scale=1.04, val_scale=0.92, gamma=1.06, warm_shift=(0.96, 1.04, 0.88), ink_strength=0.46),
        SpriteSpec("understory_jungle_broadleaf_b.png", "understory", 1.0, 1.8, True, True, ["jungle"], "warehouse", "nature/plants/broadleaf_forest_plant.png", "Maptool Objects, N Bushes plants stuff/Elephant ear 01", max_dim=300, sat_scale=1.04, val_scale=0.94, gamma=1.05, warm_shift=(0.96, 1.04, 0.88), ink_strength=0.46),
        SpriteSpec("understory_shrub_woodland_a.png", "understory", 1.1, 2.0, True, True, ["temperate_forest", "lush_meadow"], "warehouse", "nature/plants/top_down_dense_bush_cluster.png", "Maptool Objects, N Trees/Baobab 01", max_dim=320, sat_scale=0.94, val_scale=0.94, gamma=1.06, warm_shift=(1.0, 1.0, 0.90), ink_strength=0.46),
        SpriteSpec("understory_shrub_woodland_b.png", "understory", 1.0, 1.9, True, True, ["temperate_forest", "lush_meadow", "taiga"], "warehouse", "nature/plants/temperate/brush/brush_nit1.png", "Maptool Backgrounds, Enviroment/Plants/Temperate/Brush", max_dim=320, sat_scale=0.98, val_scale=0.98, gamma=1.02, ink_strength=0.40),
        SpriteSpec("understory_shrub_woodland_c.png", "understory", 0.9, 1.7, True, True, ["temperate_forest", "lush_meadow", "jungle"], "warehouse", "nature/plants/bush4.png", "Maptool Objects, N Bushes plants stuff/bush4", max_dim=300, sat_scale=0.84, val_scale=0.88, gamma=1.12, warm_shift=(0.96, 1.0, 0.88), ink_strength=0.48),
        SpriteSpec("understory_shrub_berry_flower.png", "understory", 0.85, 1.5, True, True, ["lush_meadow", "temperate_forest"], "warehouse", "nature/plants/flowers/flowerpatch01_c.png", "Maptool Backgrounds, Enviroment/Plants/Flowers", max_dim=280, sat_scale=0.90, val_scale=0.94, gamma=1.05, ink_strength=0.42),
        SpriteSpec("understory_scrub_dry_a.png", "understory", 0.85, 1.6, True, True, ["savanna", "desert", "tundra"], "warehouse", "nature/plants/warm/brush/dried_grassclump_b.png", "Maptool Backgrounds, Enviroment/Plants/Warm/Brush", max_dim=280, sat_scale=0.92, val_scale=0.92, gamma=1.06, ink_strength=0.40),
        SpriteSpec("understory_tundra_heather.png", "understory", 0.85, 1.6, True, True, ["tundra", "alpine"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=300, generator=lambda: synth_tundra_heather(seed=941, size=300)),
        SpriteSpec("understory_desert_cactus_barrel.png", "understory", 0.85, 1.6, True, True, ["desert"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=320, generator=lambda: synth_desert_cactus_barrel(seed=953, size=320)),
        SpriteSpec("understory_desert_prickly_pear.png", "understory", 0.95, 1.8, True, True, ["desert", "savanna"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=320, generator=lambda: synth_prickly_pear(seed=967, size=320)),
        SpriteSpec("understory_agave_spiky.png", "understory", 0.85, 1.6, True, True, ["desert", "savanna", "coast"], "warehouse", "nature/plants/plant_14.png", "Maptool Objects, N Bushes plants stuff/Plant 14", max_dim=280, sat_scale=1.08, val_scale=0.88, gamma=1.10, warm_shift=(0.94, 1.04, 0.90), ink_strength=0.46),
        SpriteSpec("understory_cattail_sedge_a.png", "understory", 0.9, 1.7, True, True, ["swamp", "coast"], "warehouse", "nature/plants/water/plants/cattail_b.png", "Maptool Backgrounds, Enviroment/Plants/Water/Plants", max_dim=300, sat_scale=1.0, val_scale=0.98, gamma=1.02, ink_strength=0.38),
        SpriteSpec("understory_swamp_cattails_b.png", "understory", 1.0, 1.8, True, True, ["swamp"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=320, generator=lambda: synth_swamp_cattails(seed=977, size=320)),
        SpriteSpec("understory_dune_grass_a.png", "understory", 0.8, 1.5, True, False, ["coast", "savanna"], "warehouse", "nature/plants/temperate/brush/grasslong01_c.png", "Maptool Backgrounds, Enviroment/Plants/Temperate/Brush", max_dim=280, sat_scale=0.94, val_scale=0.92, gamma=1.06, warm_shift=(1.04, 1.0, 0.88), ink_strength=0.36),
        SpriteSpec("understory_coastal_driftwood.png", "understory", 1.0, 2.0, True, True, ["coast", "swamp"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=360, generator=lambda: synth_coastal_driftwood(seed=991)),

        # --- Tier 1: Ground Decals & Micro-Clutter (`decal`, 0.35 - 1.2 tiles) ---
        SpriteSpec("decal_moss_pillow_cluster.png", "decal", 0.55, 1.15, True, False, ["temperate_forest", "jungle", "swamp", "taiga"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=256, generator=lambda: synth_moss_pillow_decal(seed=1009, size=256)),
        SpriteSpec("decal_moss_patch_a.png", "decal", 0.50, 1.05, True, False, ["temperate_forest", "taiga", "swamp", "tundra"], "warehouse", "overlays/ground/mossy/moss_sc1.png", "Maptool Backgrounds, Enviroment/Overlays/Mossy", max_dim=256, sat_scale=0.94, val_scale=0.92, gamma=1.06, ink_strength=0.32),
        SpriteSpec("decal_moss_patch_b.png", "decal", 0.50, 1.05, True, False, ["temperate_forest", "jungle", "swamp"], "warehouse", "overlays/ground/mossy/moss_jungle_grass.png", "Maptool Backgrounds, Enviroment/Overlays/Mossy", max_dim=256, sat_scale=0.98, val_scale=0.94, gamma=1.05, ink_strength=0.32),
        SpriteSpec("decal_clover_lush_a.png", "decal", 0.40, 0.75, True, False, ["lush_meadow", "temperate_forest"], "warehouse", "overlays/ground/clover/clovers_fl_kpl01_c.png", "Maptool Backgrounds, Enviroment/Overlays/Clover", max_dim=180, sat_scale=0.95, val_scale=0.96, gamma=1.02, ink_strength=0.25),
        SpriteSpec("decal_wildflowers_forgetmenot.png", "decal", 0.40, 0.75, True, False, ["temperate_forest", "lush_meadow", "alpine"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=224, generator=lambda: synth_forgetmenot_decal(seed=1021, size=224)),
        SpriteSpec("decal_mushrooms_toadstools.png", "decal", 0.35, 0.65, True, False, ["temperate_forest", "taiga", "swamp"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=240, generator=lambda: synth_mushroom_cluster(seed=1033, red_caps=True, size=240)),
        SpriteSpec("decal_mushrooms_boletes.png", "decal", 0.35, 0.65, True, False, ["temperate_forest", "taiga", "jungle", "swamp"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=240, generator=lambda: synth_mushroom_cluster(seed=1049, red_caps=False, size=240)),
        SpriteSpec("decal_leaf_litter_humus.png", "decal", 0.55, 1.10, True, False, ["temperate_forest", "jungle"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=256, generator=lambda: synth_scatter_decal(seed=1061, kind="leaf_litter", size=256)),
        SpriteSpec("decal_leaves_autumn_a.png", "decal", 0.50, 0.95, True, False, ["temperate_forest"], "warehouse", "overlays/ground/leaves/leaves02_kpl_b.png", "Maptool Backgrounds, Enviroment/Overlays/Leaves", max_dim=256, sat_scale=0.88, val_scale=0.92, gamma=1.06, ink_strength=0.30),
        SpriteSpec("decal_leaves_russet_b.png", "decal", 0.50, 0.95, True, False, ["temperate_forest", "taiga"], "warehouse", "overlays/ground/leaves/leaves03_kpl_b.png", "Maptool Backgrounds, Enviroment/Overlays/Leaves", max_dim=256, sat_scale=0.78, val_scale=0.88, gamma=1.08, warm_shift=(0.96, 1.04, 0.92), ink_strength=0.30),
        SpriteSpec("decal_pine_needles_cones.png", "decal", 0.45, 0.90, True, False, ["taiga", "alpine", "temperate_forest"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=256, generator=lambda: synth_scatter_decal(seed=1087, kind="pine_cones", size=256)),
        SpriteSpec("decal_pebbles_grey.png", "decal", 0.45, 0.85, True, False, ["alpine", "tundra", "coast", "temperate_forest"], "warehouse", "nature/rocks/cluster/stones_dotw_grey2_gt_lrk.png", "Maptool Backgrounds, Enviroment/Rocky/Cluster", max_dim=256, sat_scale=0.35, val_scale=0.95, gamma=1.04, warm_shift=(1.0, 1.02, 0.98), ink_strength=0.34),
        SpriteSpec("decal_coastal_shells.png", "decal", 0.40, 0.75, True, False, ["coast"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=256, generator=lambda: synth_scatter_decal(seed=1103, kind="coastal_shells", size=256)),
        SpriteSpec("decal_lilypads_blossom.png", "decal", 0.50, 0.95, True, False, ["swamp"], "warehouse", "nature/plants/water/plants/lily_pads_jcd.png", "Maptool Backgrounds, Enviroment/Plants/Water/Plants", max_dim=224, sat_scale=0.96, val_scale=0.96, gamma=1.02, ink_strength=0.30),
        SpriteSpec("decal_lilypads_cluster.png", "decal", 0.45, 0.85, True, False, ["swamp"], "warehouse", "nature/plants/water/plants/lilly_pads2_tpg.png", "Maptool Backgrounds, Enviroment/Plants/Water/Plants", max_dim=200, sat_scale=0.94, val_scale=0.92, gamma=1.06, ink_strength=0.36),
        SpriteSpec("decal_swamp_duckweed.png", "decal", 0.50, 1.00, True, False, ["swamp"], "warehouse", "nature/plants/water/plants/pond_skumb.png", "Maptool Backgrounds, Enviroment/Plants/Water/Plants", max_dim=200, sat_scale=0.94, val_scale=0.92, gamma=1.05, ink_strength=0.25),
        SpriteSpec("decal_snowdrift_wind.png", "decal", 0.60, 1.20, True, False, ["snowfield", "alpine", "tundra", "taiga"], "synthesized", "", "Procedural painterly synthesis (`tools/curate_terrain_assets.py`), CC0 1.0", max_dim=256, generator=lambda: synth_scatter_decal(seed=1117, kind="snowdrift", size=256)),
        SpriteSpec("decal_ice_fractured.png", "decal", 0.50, 1.05, True, False, ["snowfield", "alpine", "tundra"], "warehouse", "overlays/ground/ice/ice_cover_2.png", "Maptool Backgrounds, Enviroment/Overlays/Ice", max_dim=256, sat_scale=0.92, val_scale=0.98, gamma=1.02, ink_strength=0.32),
    ]


@dataclass(frozen=True)
class PaletteSpec:
    id: str
    filename: str
    ground_cover: str | None
    recommended_tiles_per_repeat: int
    clutter: bool
    biomes: list[str]
    is_existing: bool
    seed: int
    tex_a_rel: str
    tex_b_rel: str
    attribution: str


PALETTE_SPECS: list[PaletteSpec] = [
    # 22 Existing entries in `assets/terrain/palette.json` (upgraded seamlessly in-place!)
    PaletteSpec("grass_lush", "grass_lush.png", "lush_grass", 4, True, ["lush_meadow", "temperate_forest", "jungle", "coast"], True, 2001, "textures/terrain/grass/dark_green/floor_grass3andsoil.jpg", "textures/terrain/grass/grass_mixed_medium_base_torque3d_mit.png", "Composite from Maptool Textures (Natural/Grass/Dark Green, Torque3D MIT) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("grass_meadow", "grass_meadow.png", "grass", 4, True, ["lush_meadow", "temperate_forest", "coast"], True, 2002, "textures/terrain/grass/grass_mixed_medium_base_torque3d_mit.png", "textures/terrain/grass/dark_green/floor_grass3andsoil.jpg", "Composite from Maptool Textures (Torque3D MIT, Natural/Grass) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("grass_forest", "grass_forest.png", "grass", 4, True, ["temperate_forest", "taiga", "jungle"], True, 2003, "textures/terrain/grass/dark_green/jungle_floor.jpg", "textures/terrain/grass/dark_green/floor_grass3andsoil.jpg", "Composite from Maptool Textures (Natural/Grass/Dark Green) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("grass_dry", "grass_dry.png", "dry_grass", 4, True, ["savanna", "desert"], True, 2004, "textures/terrain/grass/grass_mixed_medium_base_torque3d_mit.png", "textures/terrain/dirt/light/floor_soilandgravel4.jpg", "Composite from Maptool Textures (Torque3D MIT, Natural/Dirt/Light) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("grass_savanna", "grass_savanna.png", "dry_grass", 4, True, ["savanna"], True, 2005, "textures/terrain/grass/grass_mixed_medium_base_torque3d_mit.png", "textures/terrain/dirt/light/floor_soilandgravel5.jpg", "Composite from Maptool Textures (Torque3D MIT, Natural/Dirt/Light) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("grass_cold", "grass_cold.png", "tundra", 6, True, ["tundra", "taiga", "alpine"], True, 2006, "textures/terrain/snow/wintry_tundra.jpg", "textures/terrain/grass/grass_mixed_medium_base_torque3d_mit.png", "Composite from Maptool Textures (Wintry Tundra, Torque3D MIT) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("tundra", "tundra.png", "tundra", 5, True, ["tundra", "alpine"], True, 2007, "textures/terrain/snow/wintry_tundra.jpg", "textures/terrain/rock/gray/vuklek.png", "Composite from Maptool Textures (Wintry Tundra, Natural/Rock/Gray) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("forest_floor", "forest_floor.png", "forest_floor", 4, True, ["temperate_forest", "jungle", "taiga"], True, 2008, "textures/terrain/dirt/dark/floor_soilandgravel7.jpg", "textures/terrain/grass/dark_green/jungle_floor.jpg", "Composite from Maptool Textures (Natural/Dirt/Dark, Natural/Grass/Dark Green) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("swamp", "swamp.png", "swamp", 6, True, ["swamp"], True, 2009, "textures/terrain/swamp/swamptilelg_sc_dgw.png", "textures/terrain/swamp/swamptilelgbrown_sc_dgw.png", "Composite from Maptool Textures (Natural/Swamp) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("mud", "mud.png", "swamp", 4, False, ["swamp", "coast"], True, 2010, "textures/terrain/dirt/mud.jpg", "textures/terrain/dirt/dark/floor_soilandgravel7.jpg", "Composite from Maptool Textures (mud, Natural/Dirt/Dark) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("dirt", "dirt.png", "dirt", 4, True, ["temperate_forest", "lush_meadow", "savanna"], True, 2011, "textures/terrain/dirt/dark/floor_soilandgravel7.jpg", "textures/terrain/dirt/light/floor_soilandgravel3.jpg", "Composite from Maptool Textures (Natural/Dirt) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("dirt_dry", "dirt_dry.png", "dirt", 4, True, ["savanna", "desert"], True, 2012, "textures/terrain/dirt/light/floor_soilandgravel4.jpg", "textures/terrain/dirt/light/floor_soilandgravel5.jpg", "Composite from Maptool Textures (Natural/Dirt/Light) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("rock_gray", "rock_gray.png", "rock", 6, True, ["alpine", "temperate_forest", "tundra", "coast"], True, 2013, "textures/terrain/rock/gray/vuklek.png", "textures/terrain/rock/slate_rough.jpg", "Composite from Maptool Textures (Natural/Rock/Gray, slate rough) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("rock_brown", "rock_brown.png", "rock", 6, True, ["desert", "savanna", "temperate_forest"], True, 2014, "textures/terrain/rock/brown/wet_cavern_floor2_kpl_hrc.png", "textures/terrain/rock/andesite_floor.png", "Composite from Maptool Textures (Natural/Rock/Brown, Andesite Floor) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("rock_mountain", "rock_mountain.png", "rock", 6, True, ["alpine", "tundra", "taiga"], True, 2015, "textures/terrain/rock/slate_rough.jpg", "textures/terrain/rock/gray/vuklek.png", "Composite from Maptool Textures (Natural/Rock) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("rock_snow", "rock_snow.png", "snow", 8, True, ["alpine", "snowfield", "tundra"], True, 2016, "textures/terrain/rock/gray/vuklek.png", "textures/terrain/snow/sno_007.jpg", "Composite from Maptool Textures (Natural/Rock/Gray, Natural/Snow) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("snow", "snow.png", "snow", 6, True, ["snowfield", "alpine", "taiga"], True, 2017, "textures/terrain/snow/windshield_7_am.jpg", "textures/terrain/snow/sno_007.jpg", "Composite from Maptool Textures (Natural/Snow) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("ice", "ice.png", None, 6, False, ["snowfield", "alpine", "tundra"], True, 2018, "textures/terrain/snow/windshield_7_am.jpg", "textures/terrain/rock/slate_rough.jpg", "Composite from Maptool Textures (Natural/Snow, Natural/Rock) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("sand_beach", "sand_beach.png", "beach", 4, True, ["coast"], True, 2019, "textures/terrain/sand/brown/sands.jpg", "textures/terrain/dirt/light/floor_soilandgravel3.jpg", "Composite from Maptool Textures (Natural/Sand/Brown, Natural/Dirt/Light) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("sand_desert", "sand_desert.png", "desert_sand", 5, True, ["desert"], True, 2020, "textures/terrain/sand/brown/sands.jpg", "textures/terrain/sand/brown/sahara.jpg", "Composite from Maptool Textures (Natural/Sand/Brown) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("water_ocean", "water_ocean.png", "deep_water", 8, False, ["coast"], True, 2021, "textures/terrain/water/ocean/choppy_sea_blue_kpl_hrc.png", "textures/terrain/water/ocean/water1alg_dgw.png", "Composite from Maptool Textures (Natural/Water/Ocean) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("water_shallow", "water_shallow.png", "shallow_water", 6, False, ["coast", "swamp"], True, 2022, "textures/terrain/water/ocean/choppy_sea_green_kpl_hrc.png", "textures/terrain/water/ocean/choppy_sea_blue_kpl_hrc.png", "Composite from Maptool Textures (Natural/Water/Ocean) + periodic synthesis (`tools/curate_terrain_assets.py`)"),

    # 8 New High-Craft Seamless Biome Variants (keeps total <= MAX_PALETTE_ENTRIES = 32!)
    PaletteSpec("forest_floor_moss", "forest_floor_moss.png", "forest_floor", 5, True, ["temperate_forest", "jungle", "swamp"], False, 2023, "textures/terrain/grass/dark_green/jungle_floor.jpg", "textures/terrain/swamp/swamptilelg_sc_dgw.png", "Composite from Maptool Textures (Natural/Grass/Dark Green, Natural/Swamp) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("forest_floor_pine", "forest_floor_pine.png", "forest_floor", 4, True, ["taiga", "temperate_forest", "alpine"], False, 2024, "textures/terrain/dirt/dark/floor_soilandgravel7.jpg", "textures/terrain/dirt/light/floor_soilandgravel5.jpg", "Composite from Maptool Textures (Natural/Dirt) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("grass_clover", "grass_clover.png", "lush_grass", 5, True, ["lush_meadow", "temperate_forest"], False, 2025, "textures/terrain/grass/dark_green/floor_grass3andsoil.jpg", "textures/terrain/grass/dark_green/jungle_floor.jpg", "Composite from Maptool Textures (Natural/Grass/Dark Green) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("tundra_lichen", "tundra_lichen.png", "tundra", 5, True, ["tundra", "alpine"], False, 2026, "textures/terrain/snow/wintry_tundra.jpg", "textures/terrain/rock/slate_rough.jpg", "Composite from Maptool Textures (Wintry Tundra, Natural/Rock) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("rock_slate", "rock_slate.png", "rock", 6, True, ["temperate_forest", "alpine", "coast"], False, 2027, "textures/terrain/rock/slate_rough.jpg", "textures/terrain/rock/brown/wet_cavern_floor1_kpl_hrc.png", "Composite from Maptool Textures (Natural/Rock) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("rock_sandstone", "rock_sandstone.png", "rock", 6, True, ["desert", "savanna"], False, 2028, "textures/terrain/rock/andesite_floor.png", "textures/terrain/rock/brown/wet_cavern_floor2_kpl_hrc.png", "Composite from Maptool Textures (Natural/Rock) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("sand_dunes", "sand_dunes.png", "desert_sand", 6, True, ["desert"], False, 2029, "textures/terrain/sand/brown/sahara.jpg", "textures/terrain/sand/brown/sands.jpg", "Composite from Maptool Textures (Natural/Sand/Brown) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
    PaletteSpec("beach_wet", "beach_wet.png", "beach", 5, True, ["coast"], False, 2030, "textures/terrain/sand/brown/sands.jpg", "textures/terrain/dirt/dark/floor_soilandgravel7.jpg", "Composite from Maptool Textures (Natural/Sand/Brown, Natural/Dirt/Dark) + periodic synthesis (`tools/curate_terrain_assets.py`)"),
]


# ---------------------------------------------------------------------------
# Source Resolution, Build Pipeline, Attributions & Visual Catalog Sheet
# ---------------------------------------------------------------------------


class WarehouseResolver:
    """Resolve source images from `--warehouse` or `--stage-dir`."""

    def __init__(self, warehouse_dir: Path | None, stage_dir: Path | None) -> None:
        self.warehouse_dir = warehouse_dir
        self.stage_index: dict[str, Path] = {}
        if stage_dir and stage_dir.exists():
            # Index staged files by stem suffix after code prefix (`T41_oak_aa3.png` -> `oak_aa3`)
            for p in stage_dir.rglob("*"):
                if p.is_file() and "_" in p.name:
                    after = p.name.split("_", 1)[1].lower()
                    self.stage_index[after] = p
                    self.stage_index[Path(after).stem] = p

    def load_rgba(self, warehouse_rel: str) -> np.ndarray:
        if self.warehouse_dir is not None:
            cand = self.warehouse_dir / warehouse_rel
            try:
                if cand.exists():
                    return np.asarray(Image.open(cand).convert("RGBA"), dtype=np.uint8)
            except PermissionError:
                pass
        name_low = Path(warehouse_rel).name.lower()
        stem_low = Path(warehouse_rel).stem.lower()
        for key in (name_low, stem_low):
            if key in self.stage_index:
                return np.asarray(Image.open(self.stage_index[key]).convert("RGBA"), dtype=np.uint8)
        raise FileNotFoundError(f"Could not resolve warehouse asset: {warehouse_rel}")

    def try_load_rgb(self, warehouse_rel: str) -> np.ndarray | None:
        try:
            return self.load_rgba(warehouse_rel)[..., :3]
        except Exception:
            return None


def recolor_white_tree_to_deadwood(rgba: np.ndarray) -> np.ndarray:
    """Convert stark white/grey bare branch sprite (`treewhite.png`) into warm weathered bogwood/deadwood."""
    arr = rgba.astype(np.float32)
    luma = (0.299 * arr[..., 0] + 0.587 * arr[..., 1] + 0.114 * arr[..., 2]) / 255.0
    c_dark = np.array([58.0, 46.0, 34.0], dtype=np.float32)
    c_light = np.array([138.0, 116.0, 90.0], dtype=np.float32)
    arr[..., :3] = c_dark + (c_light - c_dark) * np.power(np.clip(luma, 0.0, 1.0), 0.9)[..., None]
    return arr.astype(np.uint8)


def write_attributions(sprite_specs: list[SpriteSpec], palette_specs: list[PaletteSpec]) -> None:
    clut_lines = [
        "# Ground Clutter Asset Attributions",
        "",
        "Every ground clutter, understory, prop, and canopy sprite in `assets/terrain/clutter/` comes either from Zeta's redistributable local asset warehouse (`assets/library/`, confirmed redistributable by the Game Director on 2026-11-26) or from deterministic procedural synthesis in `tools/curate_terrain_assets.py` (released under CC0 1.0).",
        "",
        "| File | Tier | Source / License Attribution |",
        "|---|---|---|",
    ]
    for s in sorted(sprite_specs, key=lambda x: x.filename):
        clut_lines.append(f"| `{s.filename}` | `{s.tier}` | {s.attribution} |")
    (CLUTTER_DIR / "ATTRIBUTIONS.md").write_text("\n".join(clut_lines) + "\n", encoding="utf-8", newline="\n")

    pal_lines = [
        "# Terrain Palette Texture Attributions",
        "",
        "Every seamless `512x512` `RGBA8` ground texture in `assets/terrain/palette/` is built deterministically by `tools/curate_terrain_assets.py` from Zeta's redistributable local asset warehouse (`assets/library/textures/`, confirmed redistributable by the Game Director on 2026-11-26) combined with periodic procedural synthesis (CC0 1.0).",
        "",
        "| File | Ground Cover | Source / License Attribution |",
        "|---|---|---|",
    ]
    for p in sorted(palette_specs, key=lambda x: x.filename):
        gc = f"`{p.ground_cover}`" if p.ground_cover else "none (`clutter: false`)"
        pal_lines.append(f"| `{p.filename}` | {gc} | {p.attribution} |")
    (PALETTE_DIR / "ATTRIBUTIONS.md").write_text("\n".join(pal_lines) + "\n", encoding="utf-8", newline="\n")


def render_catalog_sheets(
    sprite_entries: list[dict],
    palette_entries: list[dict],
    catalog_out: Path,
) -> None:
    """Render comprehensive labeled visual catalog sheet PNG (plus high-zoom inspection sheets)."""
    catalog_out.parent.mkdir(parents=True, exist_ok=True)
    tier_order = [("canopy", "Canopies & Gnarled Root Trunks"), ("prop", "Mid-Scale Props: Boulders, Logs & Stumps"), ("understory", "Understory: Ferns, Shrubs, Cacti & Reeds"), ("decal", "Ground Decals & Micro-Clutter")]

    cols = 8
    cell_w, cell_h = 200, 212
    total_w = cols * cell_w + 32

    # Compute total height across 4 sprite tier sections + 1 palette 2x2-tiled section
    y_cursor = 48
    sec_layouts = []
    for tier_key, tier_title in tier_order:
        grp = [e for e in sprite_entries if e["tier"] == tier_key]
        rows = max(1, (len(grp) + cols - 1) // cols)
        sec_layouts.append((tier_title, grp, y_cursor, rows))
        y_cursor += 34 + rows * cell_h + 14

    pal_rows = (len(palette_entries) + cols - 1) // cols
    pal_y0 = y_cursor
    total_h = pal_y0 + 36 + pal_rows * cell_h + 24

    sheet = Image.new("RGB", (total_w, total_h), (28, 30, 26))
    draw = ImageDraw.Draw(sheet)
    draw.text(
        (18, 14),
        f"Zeta Curated Painterly Battlemap Asset Catalog — {len(sprite_entries)} Clutter Sprites (4 Tiers) + {len(palette_entries)} Seamless 512x512 Ground Textures",
        fill=(248, 232, 196),
    )

    for tier_title, grp, sy, _ in sec_layouts:
        draw.text((18, sy + 6), f"=== {tier_title} ({len(grp)} sprites) ===", fill=(242, 206, 122))
        for idx, item in enumerate(grp):
            r, c = divmod(idx, cols)
            x0 = 16 + c * cell_w
            y0 = sy + 32 + r * cell_h
            # Split olive / warm-soil background to verify clean alpha edges
            draw.rectangle([x0 + 6, y0 + 28, x0 + 100, y0 + 202], fill=(60, 74, 42))
            draw.rectangle([x0 + 100, y0 + 28, x0 + 194, y0 + 202], fill=(122, 102, 78))

            p = REPO_ROOT / "assets" / item["texture"]
            with Image.open(p) as im:
                rgba = im.convert("RGBA")
                w, h = rgba.size
                scale = min(166 / max(w, 1), 166 / max(h, 1))
                nw, nh = max(1, int(w * scale)), max(1, int(h * scale))
                thumb = rgba.resize((nw, nh), Image.Resampling.LANCZOS)
                sheet.paste(thumb, (x0 + 100 - nw // 2, y0 + 115 - nh // 2), thumb)

            stem = Path(item["texture"]).stem
            draw.text((x0 + 6, y0 + 2), stem[:25], fill=(238, 234, 224))
            draw.text(
                (x0 + 6, y0 + 14),
                f"{w}x{h} | {item['min_size_tiles']:.1f}-{item['max_size_tiles']:.1f}t",
                fill=(186, 198, 168),
            )

    draw.text(
        (18, pal_y0 + 6),
        f"=== Seamless 512x512 Ground Palette Textures — 2x2 Tiled Preview ({len(palette_entries)} textures) ===",
        fill=(242, 206, 122),
    )
    for idx, item in enumerate(palette_entries):
        r, c = divmod(idx, cols)
        x0 = 16 + c * cell_w
        y0 = pal_y0 + 32 + r * cell_h
        p = REPO_ROOT / "assets" / item["texture"]
        with Image.open(p) as im:
            tile86 = im.convert("RGB").resize((86, 86), Image.Resampling.LANCZOS)
            tiled = Image.new("RGB", (172, 172))
            tiled.paste(tile86, (0, 0))
            tiled.paste(tile86, (86, 0))
            tiled.paste(tile86, (0, 86))
            tiled.paste(tile86, (86, 86))
            sheet.paste(tiled, (x0 + 14, y0 + 30))
        draw.text((x0 + 12, y0 + 2), item["id"][:24], fill=(238, 234, 224))
        draw.text((x0 + 12, y0 + 14), f"{item['ground_cover'] or 'none'} r={item['recommended_tiles_per_repeat']}", fill=(186, 198, 168))

    sheet.save(catalog_out)

    # Also save 2 high-zoom inspection sheets in the same folder (`hero_100pct_zoom.png` & `palette_2x2_zoom.png`)
    hero_names = [
        "canopy_gnarled_roots_a.png",
        "canopy_gnarled_roots_b.png",
        "canopy_temperate_billow_a.png",
        "canopy_temperate_billow_b.png",
        "canopy_jungle_emergent.png",
        "canopy_swamp_willow.png",
        "canopy_savanna_acacia.png",
        "prop_boulder_stepped_slate_a.png",
        "prop_boulder_stepped_mossy_b.png",
        "prop_boulder_sandstone_mesa_a.png",
        "prop_boulder_snow_crag_a.png",
        "prop_boulder_coastal_wet_a.png",
        "prop_log_mossy_fallen_a.png",
        "prop_log_hollow_mossy_b.png",
        "understory_fern_fronds_a.png",
        "understory_bracken_bed_b.png",
        "understory_desert_cactus_barrel.png",
        "understory_desert_prickly_pear.png",
        "understory_swamp_cattails_b.png",
        "understory_coastal_driftwood.png",
        "decal_moss_pillow_cluster.png",
        "decal_wildflowers_forgetmenot.png",
        "decal_mushrooms_toadstools.png",
        "decal_pine_needles_cones.png",
    ]
    # Composite hero sprites directly over our upgraded `forest_floor.png` + `dirt.png` at high resolution!
    ff_im = Image.open(PALETTE_DIR / "forest_floor.png").convert("RGB")
    dirt_im = Image.open(PALETTE_DIR / "dirt.png").convert("RGB")
    hcols = 6
    hrows = (len(hero_names) + hcols - 1) // hcols
    hcw, hch = 340, 360
    hsheet = Image.new("RGB", (hcols * hcw + 20, hrows * hch + 20), (32, 34, 28))
    hdraw = ImageDraw.Draw(hsheet)
    bg_ff = ff_im.resize((320, 320), Image.Resampling.LANCZOS)
    bg_dt = dirt_im.resize((320, 320), Image.Resampling.LANCZOS)
    for idx, fn in enumerate(hero_names):
        r, c = divmod(idx, hcols)
        x0, y0 = 10 + c * hcw, 10 + r * hch
        # Left half real forest_floor texture, right half real dirt path texture!
        hsheet.paste(bg_ff.crop((0, 0, 160, 320)), (x0 + 10, y0 + 30))
        hsheet.paste(bg_dt.crop((160, 0, 320, 320)), (x0 + 170, y0 + 30))
        with Image.open(CLUTTER_DIR / fn) as sp:
            rgba = sp.convert("RGBA")
            w, h = rgba.size
            scale = min(304 / max(w, 1), 304 / max(h, 1), 1.0)
            nw, nh = int(w * scale), int(h * scale)
            sp_r = rgba.resize((nw, nh), Image.Resampling.LANCZOS) if scale < 1.0 else rgba
            hsheet.paste(sp_r, (x0 + 170 - nw // 2, y0 + 190 - nh // 2), sp_r)
        hdraw.text((x0 + 10, y0 + 8), f"{fn} ({w}x{h})", fill=(248, 234, 200))
    hsheet.save(catalog_out.with_name("hero_sprites_100pct_inspection.png"))

    # Palette 2x2 zoom sheet
    pcols = 6
    prows = (len(palette_entries) + pcols - 1) // pcols
    pcw, pch = 276, 296
    psheet = Image.new("RGB", (pcols * pcw + 20, prows * pch + 20), (24, 24, 22))
    pdraw = ImageDraw.Draw(psheet)
    for idx, item in enumerate(palette_entries):
        r, c = divmod(idx, pcols)
        x0, y0 = 10 + c * pcw, 10 + r * pch
        p = REPO_ROOT / "assets" / item["texture"]
        with Image.open(p) as im:
            t128 = im.convert("RGB").resize((128, 128), Image.Resampling.LANCZOS)
            tiled = Image.new("RGB", (256, 256))
            tiled.paste(t128, (0, 0))
            tiled.paste(t128, (128, 0))
            tiled.paste(t128, (0, 128))
            tiled.paste(t128, (128, 128))
            psheet.paste(tiled, (x0 + 10, y0 + 30))
        pdraw.line([(x0 + 10 + 128, y0 + 26), (x0 + 10 + 128, y0 + 30)], fill=(230, 90, 70), width=2)
        pdraw.line([(x0 + 6, y0 + 30 + 128), (x0 + 10, y0 + 30 + 128)], fill=(230, 90, 70), width=2)
        pdraw.text((x0 + 10, y0 + 8), f"{item['id']} ({item['ground_cover'] or 'none'})", fill=(245, 232, 200))
    psheet.save(catalog_out.with_name("palette_2x2_seamless_inspection.png"))


def cmd_build(args: argparse.Namespace) -> int:
    CLUTTER_DIR.mkdir(parents=True, exist_ok=True)
    PALETTE_DIR.mkdir(parents=True, exist_ok=True)

    wh = Path(args.warehouse) if args.warehouse else DEFAULT_WAREHOUSE
    try:
        wh_ok = (wh / "manifest.json").exists()
    except PermissionError:
        wh_ok = False
    if not wh_ok:
        try:
            if (FALLBACK_WAREHOUSE / "manifest.json").exists():
                wh = FALLBACK_WAREHOUSE
                wh_ok = True
        except PermissionError:
            wh_ok = False

    stage_dir = Path(args.stage_dir) if args.stage_dir else None
    resolver = WarehouseResolver(wh if wh_ok else None, stage_dir)

    rock_grain = resolver.try_load_rgb("textures/terrain/rock/gray/vuklek.png")
    sprite_specs = build_sprite_specs(rock_grain)

    print(f"[1/4] Building & cleaning {len(sprite_specs)} battlemap clutter sprites...", flush=True)
    sprite_manifest: list[dict] = []
    for spec in sprite_specs:
        dst = CLUTTER_DIR / spec.filename
        if args.reuse_built and dst.exists():
            with Image.open(dst) as im:
                w, h = im.size
        elif spec.source_kind == "existing":
            # Clean alpha/zero-alpha RGB bleed in-place while preserving exact pixel dimensions!
            rgba = np.asarray(Image.open(dst).convert("RGBA"), dtype=np.uint8)
            cleaned = decontaminate_rgba(rgba, alpha_cutoff=8)
            Image.fromarray(cleaned, mode="RGBA").save(dst)
            h, w = cleaned.shape[:2]
        elif spec.source_kind == "warehouse":
            raw = resolver.load_rgba(spec.warehouse_rel)
            if spec.recolor_deadwood:
                raw = recolor_white_tree_to_deadwood(raw)
            trimmed = trim_and_fit_rgba(raw, max_dim=spec.max_dim, pad_frac=0.05)
            styled = harmonize_painterly_sprite(
                trimmed,
                sat_scale=spec.sat_scale,
                val_scale=spec.val_scale,
                gamma=spec.gamma,
                warm_shift=spec.warm_shift,
                ink_strength=spec.ink_strength,
            )
            Image.fromarray(styled, mode="RGBA").save(dst)
            h, w = styled.shape[:2]
        else:
            assert spec.generator is not None
            gen = spec.generator()
            Image.fromarray(gen, mode="RGBA").save(dst)
            h, w = gen.shape[:2]

        sprite_manifest.append(
            {
                "id": Path(spec.filename).stem,
                "path": f"assets/terrain/clutter/{spec.filename}",
                "texture": f"terrain/clutter/{spec.filename}",
                "pixel_size": [int(w), int(h)],
                "tier": spec.tier,
                "min_size_tiles": spec.min_size_tiles,
                "max_size_tiles": spec.max_size_tiles,
                "rotatable": spec.rotatable,
                "casts_shadow": spec.casts_shadow,
                "biomes": spec.biomes,
                "source_kind": spec.source_kind,
                "preserved_existing": spec.source_kind == "existing",
            }
        )

    print(f"[2/4] Building {len(PALETTE_SPECS)} seamless 512x512 RGBA8 ground palette textures...", flush=True)
    palette_manifest: list[dict] = []
    max_seam_ratio = 0.0
    for pspec in PALETTE_SPECS:
        dst = PALETTE_DIR / pspec.filename
        if args.reuse_built and dst.exists():
            im = Image.open(dst).convert("RGBA")
        else:
            ta = resolver.try_load_rgb(pspec.tex_a_rel)
            tb = resolver.try_load_rgb(pspec.tex_b_rel)
            im = build_seamless_palette_texture(pspec.id, seed=pspec.seed, base_tex_a=ta, base_tex_b=tb)
            assert im.size == (PALETTE_SIZE, PALETTE_SIZE) and im.mode == "RGBA"
            im.save(dst)

        arr = np.asarray(im, dtype=np.float32)[..., :3]
        lr = float(np.abs(arr[:, 0, :] - arr[:, -1, :]).mean())
        tb_s = float(np.abs(arr[0, :, :] - arr[-1, :, :]).mean())
        adj = float((np.abs(arr[:, 1:, :] - arr[:, :-1, :]).mean() + np.abs(arr[1:, :, :] - arr[:-1, :, :]).mean()) * 0.5)
        sratio = max(lr, tb_s) / (adj + 1e-4)
        max_seam_ratio = max(max_seam_ratio, sratio)

        palette_manifest.append(
            {
                "id": pspec.id,
                "path": f"assets/terrain/palette/{pspec.filename}",
                "texture": f"terrain/palette/{pspec.filename}",
                "pixel_size": [PALETTE_SIZE, PALETTE_SIZE],
                "format": "RGBA8",
                "ground_cover": pspec.ground_cover,
                "recommended_tiles_per_repeat": pspec.recommended_tiles_per_repeat,
                "clutter": pspec.clutter,
                "biomes": pspec.biomes,
                "in_current_palette_json": pspec.is_existing,
                "seam_ratio": round(sratio, 3),
            }
        )
    print(f"  -> Max palette seam_ratio across all {len(PALETTE_SPECS)} textures: {max_seam_ratio:.3f}", flush=True)

    print("[3/4] Writing ATTRIBUTIONS.md and curated_assets_manifest.json...", flush=True)
    write_attributions(sprite_specs, PALETTE_SPECS)

    if args.manifest_out:
        mpath = Path(args.manifest_out)
        mpath.parent.mkdir(parents=True, exist_ok=True)
        payload = {
            "schema_version": 1,
            "summary": {
                "total_sprites": len(sprite_manifest),
                "by_tier": {
                    t: sum(1 for s in sprite_manifest if s["tier"] == t)
                    for t in ("decal", "understory", "prop", "canopy")
                },
                "total_palette_textures": len(palette_manifest),
                "existing_palette_upgraded": sum(1 for p in palette_manifest if p["in_current_palette_json"]),
                "new_palette_variants": sum(1 for p in palette_manifest if not p["in_current_palette_json"]),
                "biomes_covered": sorted({b for s in sprite_manifest for b in s["biomes"]}),
            },
            "sprites": sprite_manifest,
            "palette_textures": palette_manifest,
        }
        mpath.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8", newline="\n")

    if args.catalog_sheet:
        print("[4/4] Rendering visual catalog & 100%-zoom inspection sheets...", flush=True)
        render_catalog_sheets(sprite_manifest, palette_manifest, Path(args.catalog_sheet))

    print("Done!", flush=True)
    return 0


def cmd_sheets(args: argparse.Namespace) -> int:
    """Render candidate contact sheets directly from `--warehouse`."""
    wh = Path(args.warehouse) if args.warehouse else DEFAULT_WAREHOUSE
    if not (wh / "manifest.json").exists() and (FALLBACK_WAREHOUSE / "manifest.json").exists():
        wh = FALLBACK_WAREHOUSE
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    manifest = json.loads((wh / "manifest.json").read_text(encoding="utf-8"))
    nature = [a for a in manifest["assets"] if a.get("category") == "nature" and a.get("ext", "").lower() == ".png"]
    nature.sort(key=lambda a: (a.get("subcategory", ""), -a.get("size_bytes", 0)))

    cols = 8
    sample = nature[:64]
    rows = max(1, (len(sample) + cols - 1) // cols)
    cw, ch = 200, 212
    sheet = Image.new("RGB", (cols * cw + 20, rows * ch + 40), (28, 30, 26))
    draw = ImageDraw.Draw(sheet)
    draw.text((12, 10), f"Warehouse Nature Candidates ({len(sample)} shown of {len(nature)})", fill=(245, 232, 196))
    for i, a in enumerate(sample):
        r, c = divmod(i, cols)
        x0, y0 = 10 + c * cw, 36 + r * ch
        draw.rectangle([x0 + 6, y0 + 26, x0 + 100, y0 + 198], fill=(58, 68, 44))
        draw.rectangle([x0 + 100, y0 + 26, x0 + 194, y0 + 198], fill=(152, 148, 140))
        rel = a["path"][len("library/") :] if a["path"].startswith("library/") else a["path"]
        p = wh / rel
        if p.exists():
            with Image.open(p) as im:
                rgba = im.convert("RGBA")
                w, h = rgba.size
                sc = min(164 / max(w, 1), 164 / max(h, 1))
                thumb = rgba.resize((max(1, int(w * sc)), max(1, int(h * sc))), Image.Resampling.LANCZOS)
                sheet.paste(thumb, (x0 + 100 - thumb.width // 2, y0 + 112 - thumb.height // 2), thumb)
        draw.text((x0 + 6, y0 + 6), Path(a["path"]).stem[:24], fill=(235, 232, 224))
    sheet.save(out_dir / "warehouse_candidates_sheet.png")
    print(f"Saved {out_dir / 'warehouse_candidates_sheet.png'}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Curate and synthesize Zeta terrain battlemap assets.")
    sub = parser.add_subparsers(dest="cmd")

    p_build = sub.add_parser("build", help="Build curated clutter sprites, seamless palette textures, and manifest.")
    p_build.add_argument("--warehouse", type=str, default="", help="Path to assets/library")
    p_build.add_argument("--stage-dir", type=str, default="", help="Optional staged candidate cache dir")
    p_build.add_argument("--manifest-out", type=str, default="", help="Output path for curated_assets_manifest.json")
    p_build.add_argument("--catalog-sheet", type=str, default="", help="Output path for full visual catalog sheet PNG")
    p_build.add_argument("--reuse-built", action="store_true", help="Reuse already-built PNGs on disk when writing manifest and catalog sheets")

    p_sheets = sub.add_parser("sheets", help="Render candidate contact sheets from warehouse.")
    p_sheets.add_argument("--warehouse", type=str, default="", help="Path to assets/library")
    p_sheets.add_argument("--out-dir", type=str, required=True, help="Output directory for contact sheets")

    args = parser.parse_args()
    if args.cmd == "sheets":
        return cmd_sheets(args)
    if args.cmd is None:
        args = parser.parse_args(["build"])
    return cmd_build(args)


if __name__ == "__main__":
    raise SystemExit(main())
