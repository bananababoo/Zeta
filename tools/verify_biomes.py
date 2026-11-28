#!/usr/bin/env python3
"""Deterministic multi-biome screenshot, visual quality, and terrain asset verification tool.

Checks:
1. Scene integrity: verifies expected PNGs in <shots_dir> exist, decode cleanly at 1280x720,
   and contain zero corrupt, missing-texture, or blank/unstreamed regions.
2. Quantitative visual quality metrics per scene (compared against optional --reference
   battlemap and built-in painterly tabletop thresholds):
   - Luminance dynamic range (p95 - p05, shadow fraction < 0.18, highlight fraction > 0.72)
   - Micro-detail sharpness (Sobel gradient RMS & Laplacian RMS)
   - Anti-repetition & anti-banding (2D FFT radial-normalized periodic peak & off-axis peak
     ratios across 4..260 px wavelengths — catches wallpaper tiling grids and 5-ft contour
     striping like pre-upgrade planet_coast.png)
   - Color richness (HSV saturation mean/p90/IQR and RGB channel balance)
3. Asset & attribution audits:
   - assets/terrain/palette.json (valid entries, textures exist, valid PNG, uniform dimensions)
   - assets/terrain/clutter.json (valid palette refs, clutter:true invariant, all legacy and
     multi-tier layer textures exist & decode cleanly)
   - assets/terrain/clutter/ATTRIBUTIONS.md (and palette/ATTRIBUTIONS.md if present: zero
     unattributed PNG files on disk and zero missing attributed filenames)
4. Visual montage (--montage <out.png>): renders all 11 biome scenes + river_scene +
   planet_coast (+ optional reference battlemap) at 640x360 per panel with metric headers.
"""

from __future__ import annotations

import argparse
import json
import math
import re
import sys
from pathlib import Path
from typing import Any

import numpy as np
from PIL import Image, ImageDraw, ImageFont
from scipy.ndimage import laplace, sobel

EXPECTED_WIDTH = 1280
EXPECTED_HEIGHT = 720

EXISTING_SCENES: list[str] = [
    "inventory_open",
    "inventory_hover",
    "inventory_menu",
    "river_scene",
    "combat_hud",
    "inspect_bridge",
    "planet_coast",
    "planet_seam",
    "river_edge",
]

BIOME_SCENES: list[str] = [
    "biome_temperate_forest",
    "biome_jungle",
    "biome_taiga",
    "biome_lush_meadow",
    "biome_savanna",
    "biome_desert",
    "biome_swamp",
    "biome_tundra",
    "biome_alpine",
    "biome_snowfield",
    "biome_coast",
]

ALL_SCENES: list[str] = EXISTING_SCENES + BIOME_SCENES
MONTAGE_SCENES: list[str] = BIOME_SCENES + ["river_scene", "planet_coast"]

# Pre-measured metrics from the reference tabletop battlemap (`media_1795821203623_e3cd2937.jpg`),
# shown in the comparison table whenever `--reference` is omitted (e.g. inside sandboxed runs).
DEFAULT_REFERENCE_METRICS: dict[str, Any] = {
    "lum_p05": 0.0884,
    "lum_p50": 0.2330,
    "lum_p95": 0.4912,
    "lum_range": 0.4028,
    "shadow_frac": 0.3364,
    "highlight_frac": 0.0001,
    "clip_frac": 0.0,
    "sobel_rms": 0.0540,
    "laplacian_rms": 0.1309,
    "fft_peak": 5.21,
    "fft_offaxis": 5.21,
    "sat_mean": 0.5193,
    "sat_p90": 0.7280,
    "sat_iqr": 0.2510,
    "rgb_mean": [0.236, 0.273, 0.146],
}
BRIGHT_BIOMES: set[str] = {"biome_snowfield", "biome_desert", "biome_alpine", "biome_tundra"}
LOW_SAT_BIOMES: set[str] = {"biome_snowfield", "biome_alpine", "biome_tundra"}

THRESHOLDS = {
    "lum_range_min": 0.22,
    "lum_range_min_bright": 0.14,
    "shadow_frac_min_foliage": 0.02,
    "sobel_rms_min": 0.045,
    "laplacian_rms_min": 0.025,
    "fft_offaxis_max": 7.5,
    "fft_peak_max": 9.5,
    "sat_mean_min": 0.14,
    "sat_mean_min_cold": 0.03,
}


def terrain_roi(scene_name: str, rgb: np.ndarray) -> np.ndarray:
    """Return unobstructed terrain pixels so HUD panels do not skew map-quality metrics."""
    if scene_name.startswith("biome_"):
        return rgb
    if scene_name in ("planet_coast", "planet_seam"):
        return rgb[56:, :, :]
    if scene_name in ("river_scene", "river_edge"):
        return rgb[56:644, :900, :]
    return rgb


def check_image_integrity(path: Path, scene_name: str) -> tuple[np.ndarray | None, list[str]]:
    """Verify PNG exists, decodes cleanly at 1280x720, and has zero corrupt/blank regions."""
    errors: list[str] = []
    if not path.is_file():
        return None, [f"{scene_name}: missing file {path}"]
    if path.stat().st_size == 0:
        return None, [f"{scene_name}: empty 0-byte file {path}"]

    try:
        with Image.open(path) as probe:
            probe.verify()
        with Image.open(path) as img:
            img.load()
            w, h = img.size
            rgba = np.array(img.convert("RGBA"), dtype=np.uint8)
    except Exception as exc:  # noqa: BLE001
        return None, [f"{scene_name}: corrupt/unreadable PNG ({exc})"]

    if (w, h) != (EXPECTED_WIDTH, EXPECTED_HEIGHT):
        errors.append(f"{scene_name}: expected {EXPECTED_WIDTH}x{EXPECTED_HEIGHT}, got {w}x{h}")
        return None, errors

    alpha = rgba[..., 3]
    if int(alpha.min()) == 0:
        zero_frac = float((alpha == 0).mean())
        errors.append(f"{scene_name}: contains transparent pixels (alpha=0 fraction {zero_frac:.4f})")

    rgb = rgba[..., :3]
    if float(rgb.std()) < 2.0:
        errors.append(f"{scene_name}: blank/uniform image (RGB std={rgb.std():.3f})")

    # Check for Bevy/WGPU magenta fallback texture (255, 0, 255)
    magenta = (rgb[..., 0] > 245) & (rgb[..., 1] < 10) & (rgb[..., 2] > 245)
    if float(magenta.mean()) > 0.001:
        errors.append(f"{scene_name}: missing-texture magenta detected ({magenta.mean() * 100:.2f}%)")

    # On unobstructed biome/planet scenes, verify no 160x144 grid patch is an unstreamed flat hole.
    if scene_name.startswith("biome_") or scene_name in ("planet_coast", "planet_seam"):
        start_y = 0 if scene_name.startswith("biome_") else 72
        sub = rgb[start_y:, :, :]
        gh, gw = 4, 8
        ph = sub.shape[0] // gh
        pw = sub.shape[1] // gw
        blank_patches = 0
        for gy in range(gh):
            for gx in range(gw):
                patch = sub[gy * ph : (gy + 1) * ph, gx * pw : (gx + 1) * pw]
                if float(patch.std()) < 0.15:
                    blank_patches += 1
        if blank_patches > 0:
            errors.append(f"{scene_name}: {blank_patches}/{gh * gw} patches are completely flat/unrendered")

    return rgb, errors


def compute_fft_ratios(lum: np.ndarray) -> tuple[float, float]:
    """Compute 2D FFT periodic peak ratio and off-axis striping ratio across 4..260 px wavelengths.

    Normalizes each spatial frequency against the median magnitude of its concentric radial
    annulus, so natural isotropic 1/f^a painterly detail stays near ~2.5..5.5 while periodic
    tiling grids or parallel 5-ft contour stripes produce sharp narrowband spikes (>8..30+).
    """
    h, w = lum.shape
    zm = lum - float(lum.mean())
    window = np.outer(np.hanning(h), np.hanning(w))
    mag = np.abs(np.fft.fftshift(np.fft.fft2(zm * window)))

    cy, cx = h // 2, w // 2
    yy = (np.arange(h, dtype=np.float64) - cy) / float(h)
    xx = (np.arange(w, dtype=np.float64) - cx) / float(w)
    dx, dy = np.meshgrid(xx, yy)
    freq = np.hypot(dx, dy)

    f_min = 1.0 / 260.0
    f_max = 0.25  # 4 px wavelength
    band = (freq >= f_min) & (freq <= f_max)
    if not np.any(band):
        return 1.0, 1.0

    # Log-spaced radial bins for smooth azimuthally invariant baseline
    n_bins = 48
    edges = np.geomspace(f_min, f_max, n_bins + 1)
    bin_idx = np.searchsorted(edges, freq, side="right") - 1
    norm = np.ones_like(mag)
    for b in range(n_bins):
        m = band & (bin_idx == b)
        if int(m.sum()) >= 16:
            med = float(np.median(mag[m]))
            if med > 1e-12:
                norm[m] = mag[m] / med

    peak_all = float(norm[band].max())
    # Off-axis excludes within +-8 degrees of horizontal/vertical axes to isolate diagonal
    # contour striping (like planet_coast) and diagonal lattice artifacts.
    angle_tan = math.tan(math.radians(8.0))
    off_axis = band & (np.abs(dx) > angle_tan * np.abs(dy)) & (np.abs(dy) > angle_tan * np.abs(dx))
    peak_offaxis = float(norm[off_axis].max()) if np.any(off_axis) else peak_all
    return peak_all, peak_offaxis


def compute_metrics(rgb: np.ndarray) -> dict[str, Any]:
    """Compute perceptual luminance, sharpness, anti-repetition/anti-banding, and color metrics."""
    rgb_f = rgb.astype(np.float64) / 255.0
    lum = 0.2126 * rgb_f[..., 0] + 0.7152 * rgb_f[..., 1] + 0.0722 * rgb_f[..., 2]

    p05, p50, p95 = (float(x) for x in np.percentile(lum, [5.0, 50.0, 95.0]))
    lum_range = p95 - p05
    shadow_frac = float((lum < 0.18).mean())
    highlight_frac = float((lum > 0.72).mean())
    clip_frac = float((lum > 0.98).mean())

    gx = sobel(lum, axis=1, mode="nearest") / 8.0
    gy = sobel(lum, axis=0, mode="nearest") / 8.0
    sobel_rms = float(np.sqrt(np.mean(gx * gx + gy * gy)))
    lap = laplace(lum, mode="nearest")
    laplacian_rms = float(np.sqrt(np.mean(lap * lap)))

    fft_peak, fft_offaxis = compute_fft_ratios(lum)

    c_max = rgb_f.max(axis=-1)
    c_min = rgb_f.min(axis=-1)
    chroma = c_max - c_min
    sat = np.where(c_max > 0.02, chroma / np.maximum(c_max, 1e-6), 0.0)
    sat_p25, sat_p75, sat_p90 = (float(x) for x in np.percentile(sat, [25.0, 75.0, 90.0]))
    sat_mean = float(sat.mean())
    sat_iqr = sat_p75 - sat_p25

    mean_r = float(rgb_f[..., 0].mean())
    mean_g = float(rgb_f[..., 1].mean())
    mean_b = float(rgb_f[..., 2].mean())

    return {
        "lum_p05": round(p05, 4),
        "lum_p50": round(p50, 4),
        "lum_p95": round(p95, 4),
        "lum_range": round(lum_range, 4),
        "shadow_frac": round(shadow_frac, 4),
        "highlight_frac": round(highlight_frac, 4),
        "clip_frac": round(clip_frac, 4),
        "sobel_rms": round(sobel_rms, 4),
        "laplacian_rms": round(laplacian_rms, 4),
        "fft_peak": round(fft_peak, 2),
        "fft_offaxis": round(fft_offaxis, 2),
        "sat_mean": round(sat_mean, 4),
        "sat_p90": round(sat_p90, 4),
        "sat_iqr": round(sat_iqr, 4),
        "rgb_mean": [round(mean_r, 3), round(mean_g, 3), round(mean_b, 3)],
    }


def evaluate_quality_flags(scene_name: str, m: dict[str, Any]) -> list[str]:
    """Check metrics against painterly battlemap thresholds and return human-readable warnings."""
    flags: list[str] = []
    min_dr = THRESHOLDS["lum_range_min_bright"] if scene_name in BRIGHT_BIOMES else THRESHOLDS["lum_range_min"]
    if m["lum_range"] < min_dr:
        flags.append(f"flat_contrast(DR={m['lum_range']:.2f}<{min_dr:.2f})")

    if scene_name in ("biome_temperate_forest", "biome_jungle", "biome_taiga", "biome_swamp"):
        if m["shadow_frac"] < THRESHOLDS["shadow_frac_min_foliage"]:
            flags.append(f"shallow_shadows(shad={m['shadow_frac'] * 100:.1f}%<2.0%)")

    if m["sobel_rms"] < THRESHOLDS["sobel_rms_min"] or m["laplacian_rms"] < THRESHOLDS["laplacian_rms_min"]:
        flags.append(f"low_detail(sob={m['sobel_rms']:.3f},lap={m['laplacian_rms']:.3f})")

    if m["fft_offaxis"] > THRESHOLDS["fft_offaxis_max"]:
        flags.append(f"periodic_banding(fft_off={m['fft_offaxis']:.1f}>{THRESHOLDS['fft_offaxis_max']})")
    elif m["fft_peak"] > THRESHOLDS["fft_peak_max"]:
        flags.append(f"periodic_tiling(fft={m['fft_peak']:.1f}>{THRESHOLDS['fft_peak_max']})")

    min_sat = THRESHOLDS["sat_mean_min_cold"] if scene_name in LOW_SAT_BIOMES else THRESHOLDS["sat_mean_min"]
    if m["sat_mean"] < min_sat:
        flags.append(f"washed_color(sat={m['sat_mean']:.2f}<{min_sat:.2f})")

    return flags


def parse_attributed_files(md_path: Path) -> set[str]:
    """Extract backticked .png filenames or relative paths from an ATTRIBUTIONS.md file."""
    text = md_path.read_text(encoding="utf-8")
    tokens = re.findall(r"`([^`]+\.png)`", text, flags=re.IGNORECASE)
    normalized: set[str] = set()
    for tok in tokens:
        clean = tok.strip().replace("\\", "/")
        normalized.add(clean)
        normalized.add(Path(clean).name)
    return normalized


def audit_terrain_assets(repo_root: Path) -> tuple[dict[str, Any], list[str]]:
    """Audit palette.json, clutter.json, and ATTRIBUTIONS.md invariants."""
    errors: list[str] = []
    assets_dir = repo_root / "assets"
    palette_path = assets_dir / "terrain" / "palette.json"
    clutter_path = assets_dir / "terrain" / "clutter.json"
    clutter_dir = assets_dir / "terrain" / "clutter"
    clutter_attr = clutter_dir / "ATTRIBUTIONS.md"
    palette_dir = assets_dir / "terrain" / "palette"
    palette_attr = palette_dir / "ATTRIBUTIONS.md"

    summary: dict[str, Any] = {
        "palette_entries": 0,
        "palette_size": None,
        "clutter_rules": 0,
        "clutter_referenced_textures": 0,
        "clutter_disk_pngs": 0,
    }

    if not palette_path.is_file():
        return summary, [f"missing {palette_path}"]
    try:
        palette_data = json.loads(palette_path.read_text(encoding="utf-8"))
    except Exception as exc:  # noqa: BLE001
        return summary, [f"invalid JSON in {palette_path}: {exc}"]

    entries = palette_data.get("entries", [])
    if not isinstance(entries, list) or not (1 <= len(entries) <= 32):
        errors.append(f"palette.json entries count {len(entries)} outside 1..=32")
    summary["palette_entries"] = len(entries)

    palette_by_id: dict[str, dict[str, Any]] = {}
    uniform_size: tuple[int, int] | None = None
    for entry in entries:
        pid = entry.get("id", "")
        if not pid or pid in palette_by_id:
            errors.append(f"palette.json duplicate or empty id {pid!r}")
            continue
        palette_by_id[pid] = entry
        tex_rel = entry.get("texture", "")
        tex_path = assets_dir / tex_rel
        if not tex_path.is_file():
            errors.append(f"palette entry {pid!r} missing texture {tex_rel}")
            continue
        try:
            with Image.open(tex_path) as img:
                img.load()
                sz = img.size
        except Exception as exc:  # noqa: BLE001
            errors.append(f"palette entry {pid!r} corrupt PNG {tex_rel}: {exc}")
            continue
        if uniform_size is None:
            uniform_size = sz
            summary["palette_size"] = list(sz)
        elif sz != uniform_size:
            errors.append(f"palette texture {tex_rel} size {sz} != uniform {uniform_size}")

    if not clutter_path.is_file():
        errors.append(f"missing {clutter_path}")
        return summary, errors
    try:
        clutter_data = json.loads(clutter_path.read_text(encoding="utf-8"))
    except Exception as exc:  # noqa: BLE001
        errors.append(f"invalid JSON in {clutter_path}: {exc}")
        return summary, errors

    palette_rules = clutter_data.get("palette", {})
    summary["clutter_rules"] = len(palette_rules)
    referenced_clutter: set[str] = set()
    for pid, rule in palette_rules.items():
        if pid not in palette_by_id:
            errors.append(f"clutter.json references unknown palette id {pid!r}")
        elif not palette_by_id[pid].get("clutter", False):
            errors.append(f"clutter.json assigns clutter to {pid!r} which has clutter: false in palette.json")

        sprite_lists: list[list[dict[str, Any]]] = []
        if "sprites" in rule and isinstance(rule["sprites"], list):
            sprite_lists.append(rule["sprites"])
        for layer in rule.get("layers", []):
            if isinstance(layer, dict) and isinstance(layer.get("sprites"), list):
                sprite_lists.append(layer["sprites"])
        if not sprite_lists:
            errors.append(f"clutter.json rule {pid!r} defines neither sprites nor layers")
        for sp_list in sprite_lists:
            for sp in sp_list:
                tex = sp.get("texture", "")
                if not tex:
                    errors.append(f"clutter.json rule {pid!r} has empty sprite texture")
                else:
                    referenced_clutter.add(tex)

    summary["clutter_referenced_textures"] = len(referenced_clutter)
    for tex_rel in sorted(referenced_clutter):
        tex_path = assets_dir / tex_rel
        if not tex_path.is_file():
            errors.append(f"clutter.json missing texture on disk: {tex_rel}")
            continue
        try:
            with Image.open(tex_path) as img:
                img.load()
                if img.size[0] <= 0 or img.size[1] <= 0:
                    errors.append(f"clutter texture {tex_rel} has zero dimension {img.size}")
        except Exception as exc:  # noqa: BLE001
            errors.append(f"clutter texture {tex_rel} failed PNG decode: {exc}")

    # Attribution check for assets/terrain/clutter/**/*.png
    if not clutter_attr.is_file():
        errors.append(f"missing {clutter_attr}")
    else:
        attributed = parse_attributed_files(clutter_attr)
        disk_pngs = sorted(clutter_dir.rglob("*.png"))
        summary["clutter_disk_pngs"] = len(disk_pngs)
        for png in disk_pngs:
            rel_clutter = png.relative_to(clutter_dir).as_posix()
            if rel_clutter not in attributed and png.name not in attributed:
                errors.append(f"unattributed clutter sprite on disk: assets/terrain/clutter/{rel_clutter}")
        # Also verify every backticked filename in clutter/ATTRIBUTIONS.md exists
        disk_keys = {p.relative_to(clutter_dir).as_posix() for p in disk_pngs} | {p.name for p in disk_pngs}
        raw_tokens = re.findall(r"`([^`]+\.png)`", clutter_attr.read_text(encoding="utf-8"), flags=re.IGNORECASE)
        for tok in raw_tokens:
            clean = tok.strip().replace("\\", "/")
            if clean not in disk_keys and Path(clean).name not in disk_keys:
                errors.append(f"clutter/ATTRIBUTIONS.md lists missing file {tok!r}")

    if palette_attr.is_file():
        pal_attributed = parse_attributed_files(palette_attr)
        for png in sorted(palette_dir.rglob("*.png")):
            rel_pal = png.relative_to(palette_dir).as_posix()
            if rel_pal not in pal_attributed and png.name not in pal_attributed:
                errors.append(f"unattributed palette texture on disk: assets/terrain/palette/{rel_pal}")

    return summary, errors


def build_montage(
    out_path: Path,
    images: dict[str, np.ndarray],
    metrics: dict[str, dict[str, Any]],
    flags_by_scene: dict[str, list[str]],
    ref_rgb: np.ndarray | None,
    ref_metrics: dict[str, Any] | None,
) -> None:
    """Render a high-legibility 3-column visual montage of all 11 biomes + river_scene + planet_coast."""
    panel_w = 640
    panel_h = 360
    header_h = 30
    pad = 12
    cols = 3

    items: list[tuple[str, np.ndarray, dict[str, Any], list[str]]] = []
    if ref_rgb is not None and ref_metrics is not None:
        items.append(("REFERENCE BATTLEMAP", ref_rgb, ref_metrics, []))
    for name in MONTAGE_SCENES:
        if name in images and name in metrics:
            items.append((name, images[name], metrics[name], flags_by_scene.get(name, [])))

    rows = math.ceil(len(items) / cols)
    banner_h = 46
    canvas_w = cols * panel_w + (cols + 1) * pad
    canvas_h = banner_h + rows * (panel_h + header_h) + (rows + 1) * pad

    canvas = Image.new("RGB", (canvas_w, canvas_h), (18, 20, 24))
    draw = ImageDraw.Draw(canvas)
    font = ImageFont.load_default()

    title = (
        f"Zeta Planetary Biome Visual Harness  |  {len(BIOME_SCENES)} Biomes + river_scene + planet_coast  |  "
        f"Metrics: DR(p95-p05)  Shad(<0.18)  Lap(sharpness)  FFT_off(striping)  Sat"
    )
    draw.rectangle([0, 0, canvas_w, banner_h], fill=(28, 32, 38))
    draw.text((pad + 4, 15), title, fill=(242, 238, 226), font=font)

    for idx, (name, rgb, m, flags) in enumerate(items):
        col = idx % cols
        row = idx // cols
        x0 = pad + col * (panel_w + pad)
        y0 = banner_h + pad + row * (panel_h + header_h + pad)

        is_ref = name == "REFERENCE BATTLEMAP"
        hdr_bg = (34, 58, 42) if is_ref else ((32, 46, 38) if not flags else (58, 38, 28))
        border_col = (120, 195, 135) if is_ref else ((90, 145, 105) if not flags else (205, 130, 75))

        draw.rectangle([x0, y0, x0 + panel_w - 1, y0 + header_h + panel_h - 1], fill=hdr_bg, outline=border_col, width=2)
        stat_str = (
            f"{name}  |  DR:{m['lum_range']:.2f}  Shad:{m['shadow_frac'] * 100:.1f}%  "
            f"Lap:{m['laplacian_rms']:.3f}  FFT:{m['fft_offaxis']:.1f}  Sat:{m['sat_mean']:.2f}"
        )
        draw.text((x0 + 8, y0 + 8), stat_str, fill=(245, 242, 234), font=font)

        thumb = Image.fromarray(rgb, mode="RGB").resize((panel_w - 4, panel_h - 2), Image.Resampling.LANCZOS)
        canvas.paste(thumb, (x0 + 2, y0 + header_h))

    out_path.parent.mkdir(parents=True, exist_ok=True)
    canvas.save(out_path, format="PNG")


def format_table_row(
    name: str, m: dict[str, Any], status: str
) -> str:
    return (
        f"| `{name:<22}` | {m['lum_range']:5.3f} | {m['shadow_frac'] * 100:5.1f}% | "
        f"{m['highlight_frac'] * 100:5.1f}% | {m['sobel_rms']:6.4f} | {m['laplacian_rms']:6.4f} | "
        f"{m['fft_offaxis']:5.2f} | {m['fft_peak']:5.2f} | {m['sat_mean']:5.3f} | {status} |"
    )


def main() -> int:
    repo_root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("shots_dir", nargs="?", type=Path, help="directory containing rendered <scene>.png files")
    parser.add_argument("--shots-dir", dest="shots_dir_opt", type=Path, help="alias for shots_dir")
    parser.add_argument("--reference", type=Path, default=None, help="optional reference battlemap image path")
    parser.add_argument("--montage", type=Path, default=None, help="output path for labeled multi-biome montage PNG")
    parser.add_argument("--json", dest="json_out", type=Path, default=None, help="optional JSON report output path")
    parser.add_argument("--biomes-only", action="store_true", help="only require the 11 biome_* PNGs in shots_dir")
    parser.add_argument("--strict-quality", action="store_true", help="exit non-zero if any biome quality threshold warns")
    args = parser.parse_args()

    shots_dir = args.shots_dir_opt or args.shots_dir
    if shots_dir is None:
        parser.error("shots_dir is required")

    expected = BIOME_SCENES if args.biomes_only else ALL_SCENES

    # 1. Audit assets & attributions
    audit_summary, audit_errors = audit_terrain_assets(repo_root)

    # 2. Optional reference image stats
    ref_rgb: np.ndarray | None = None
    ref_metrics: dict[str, Any] | None = None
    if args.reference is not None:
        if not args.reference.is_file():
            print(f"ERROR: reference image not found: {args.reference}", file=sys.stderr)
            return 1
        with Image.open(args.reference) as rimg:
            ref_rgb = np.array(rimg.convert("RGB"), dtype=np.uint8)
        ref_metrics = compute_metrics(ref_rgb)

    # 3. Check scene PNGs & compute metrics
    integrity_errors: list[str] = []
    images: dict[str, np.ndarray] = {}
    metrics_by_scene: dict[str, dict[str, Any]] = {}
    flags_by_scene: dict[str, list[str]] = {}

    for scene in expected:
        png_path = shots_dir / f"{scene}.png"
        rgb, errs = check_image_integrity(png_path, scene)
        integrity_errors.extend(errs)
        if rgb is not None:
            images[scene] = rgb
            roi = terrain_roi(scene, rgb)
            m = compute_metrics(roi)
            metrics_by_scene[scene] = m
            if scene.startswith("biome_") or scene in ("river_scene", "planet_coast"):
                flags_by_scene[scene] = evaluate_quality_flags(scene, m)
            else:
                flags_by_scene[scene] = []

    # 4. Build montage if requested and images exist
    if args.montage is not None and images:
        build_montage(args.montage, images, metrics_by_scene, flags_by_scene, ref_rgb, ref_metrics)
        print(f"Saved montage: {args.montage}")

    # 5. Print concise Markdown summary table
    print("\n### Terrain Asset & Attribution Audit")
    print(
        f"- Palette entries: {audit_summary['palette_entries']} (uniform size: {audit_summary['palette_size']})\n"
        f"- Clutter rules: {audit_summary['clutter_rules']} | referenced sprites: {audit_summary['clutter_referenced_textures']} | "
        f"disk PNGs: {audit_summary['clutter_disk_pngs']}\n"
        f"- Audit errors: {len(audit_errors)}"
    )
    for err in audit_errors:
        print(f"  - ERROR: {err}")

    print("\n### Biome & Terrain Visual Quality Metrics")
    print("| Scene | DR (p95-p05) | Shadow <0.18 | High >0.72 | Sobel | Laplac | FFT_off | FFT_max | Sat | Status |")
    print("|---|---:|---:|---:|---:|---:|---:|---:|---:|---|")
    table_ref = ref_metrics if ref_metrics is not None else DEFAULT_REFERENCE_METRICS
    print(format_table_row("REFERENCE_BATTLEMAP", table_ref, "REF"))

    report_scenes = [s for s in MONTAGE_SCENES if s in metrics_by_scene]
    for scene in report_scenes:
        flags = flags_by_scene.get(scene, [])
        status = "PASS" if not flags else ("WARN: " + ", ".join(flags))
        print(format_table_row(scene, metrics_by_scene[scene], status))

    if integrity_errors:
        print(f"\n### Scene Integrity Errors ({len(integrity_errors)})")
        for err in integrity_errors:
            print(f"- ERROR: {err}")
    else:
        print(f"\nScene integrity: {len(images)}/{len(expected)} PNGs verified ({EXPECTED_WIDTH}x{EXPECTED_HEIGHT}, zero corrupt/blank regions).")

    if args.json_out is not None:
        args.json_out.parent.mkdir(parents=True, exist_ok=True)
        payload = {
            "audit": audit_summary,
            "audit_errors": audit_errors,
            "integrity_errors": integrity_errors,
            "reference": ref_metrics,
            "scenes": metrics_by_scene,
            "quality_flags": flags_by_scene,
        }
        args.json_out.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")

    if audit_errors or integrity_errors:
        return 1
    if args.strict_quality:
        biome_warns = sum(1 for s in BIOME_SCENES if flags_by_scene.get(s))
        if biome_warns > 0:
            print(f"Strict quality check failed: {biome_warns} biome scene(s) have quality warnings.", file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
