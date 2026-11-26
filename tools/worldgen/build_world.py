"""CLI pipeline runner for Zeta's pre-humanoid 4K world map.

Chains:
  1. tectonics.generate_tectonic_base(seed, width, height, target_land_fraction, lon_shift_deg)
  2. climate_erosion.erode_and_compute_climate(elevation_m, seed)
  3. export_render.export_world(...) -> world_map.geojson + world_elevation.png (16-bit)
  4. export_render.render_from_files(...) -> world_map_clean.png, world_map_pre_humanoid.png,
                                             and crops/crop_{1..4}.png
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path
from typing import Any

import numpy as np

# Ensure sibling modules in target/worldgen/ are importable from any working directory
_WORLDGEN_DIR = Path(__file__).resolve().parent
if str(_WORLDGEN_DIR) not in sys.path:
    sys.path.insert(0, str(_WORLDGEN_DIR))

import climate_erosion  # noqa: E402
import export_render  # noqa: E402
import tectonics  # noqa: E402


def build_world(
    seed: int = 3,
    width: int = 4096,
    height: int = 2048,
    target_land_fraction: float = 0.35,
    lon_shift_deg: float = 16.0,
    out_dir: Path | str = _WORLDGEN_DIR / "output",
) -> dict[str, Any]:
    """Run the full 4-stage worldgen pipeline and return complete measured stats."""
    out_path = Path(out_dir).resolve()
    crops_dir = out_path / "crops"
    out_path.mkdir(parents=True, exist_ok=True)
    crops_dir.mkdir(parents=True, exist_ok=True)

    t_total_0 = time.perf_counter()

    # Stage 1: Spherical plate tectonics
    print(
        f"[1/4] Generating tectonic base (seed={seed}, {width}x{height}, "
        f"target_land={target_land_fraction:.2f}, lon_shift_deg={lon_shift_deg:+.2f})..."
    )
    t0 = time.perf_counter()
    tect = tectonics.generate_tectonic_base(
        seed=seed,
        width=width,
        height=height,
        target_land_fraction=target_land_fraction,
        lon_shift_deg=lon_shift_deg,
    )
    t_tect = time.perf_counter() - t0
    print(
        f"      done in {t_tect:.2f}s | area_land={tect['land_fraction_area_weighted']:.4f} "
        f"pixel_land={tect['land_fraction_pixels']:.4f} "
        f"elev=[{float(np.min(tect['elevation_m'])):.1f}, {float(np.max(tect['elevation_m'])):.1f}]m "
        f"faults={len(tect['boundary_lines'])}"
    )

    # Stage 2: Erosion, branching river network & wind-advected Whittaker climate
    print("[2/4] Running alpine erosion, priority-flood drainage, and climate engine...")
    t1 = time.perf_counter()
    clim = climate_erosion.erode_and_compute_climate(tect["elevation_m"], seed=seed)
    t_clim = time.perf_counter() - t1
    elev_final = clim["elevation_m"]
    elev_min_actual = float(np.min(elev_final))
    elev_max_actual = float(np.max(elev_final))
    print(
        f"      done in {t_clim:.2f}s | elev=[{elev_min_actual:.1f}, {elev_max_actual:.1f}]m "
        f"rivers={len(clim['rivers'])} biomes={len(clim['biome_names'])}"
    )

    # Stage 3: High-vertex-density GeoJSON + 16-bit lossless heightmap export
    print(f"[3/4] Exporting GeoJSON + 16-bit elevation heightmap to {out_path}...")
    t2 = time.perf_counter()
    exp_stats = export_render.export_world(
        elevation_m=elev_final,
        rivers=clim["rivers"],
        boundary_lines=tect["boundary_lines"],
        biome_id=clim["biome_id"],
        biome_names=clim["biome_names"],
        seed=seed,
        out_dir=out_path,
    )
    t_exp = time.perf_counter() - t2
    print(
        f"      done in {t_exp:.2f}s | land_polys={exp_stats['land_polygons']} "
        f"holes={exp_stats['holes']} coastline_verts={exp_stats['coastline_vertices']} "
        f"total_verts={exp_stats['total_vertices']} "
        f"geojson={exp_stats['geojson_bytes'] / (1024 * 1024):.2f} MB "
        f"elev_png={exp_stats['elevation_png_bytes'] / (1024 * 1024):.2f} MB"
    )

    # Stage 4: Strict single-source-of-truth 4K atlas + 4 zoomed 1:1 crops
    out_png = out_path / "world_map_pre_humanoid.png"
    print(f"[4/4] Rendering 4K atlases and 4 zoomed 1:1 crops from disk files...")
    t3 = time.perf_counter()
    ren_stats = export_render.render_from_files(
        geojson_path=Path(exp_stats["geojson_path"]),
        elevation_png_path=Path(exp_stats["elevation_png_path"]),
        biome_rgb=clim["biome_rgb"],
        out_png=out_png,
        crops_dir=crops_dir,
    )
    t_ren = time.perf_counter() - t3
    t_total = time.perf_counter() - t_total_0

    val = ren_stats["validation"]
    print(
        f"      done in {t_ren:.2f}s | validation_passed={val['passed']} "
        f"unclosed_rings={val['unclosed_rings']} "
        f"river_inland_checked={val['river_inland_vertices_checked']} "
        f"river_ocean_violations={val['river_ocean_violations']}"
    )

    # Collect exact file sizes for every output artifact
    file_paths = {
        "world_map_geojson": Path(exp_stats["geojson_path"]),
        "world_elevation_png": Path(exp_stats["elevation_png_path"]),
        "world_map_clean_png": Path(ren_stats["clean_png"]),
        "world_map_pre_humanoid_png": Path(ren_stats["labelled_png"]),
    }
    for idx, cm in enumerate(ren_stats["crops"], start=1):
        file_paths[f"crop_{idx}_{cm['name']}"] = Path(cm["path"])

    artifacts = {
        k: {
            "path": str(p),
            "bytes": int(p.stat().st_size),
            "mb": round(p.stat().st_size / (1024 * 1024), 3),
        }
        for k, p in file_paths.items()
    }

    summary = {
        "params": {
            "seed": int(seed),
            "width": int(width),
            "height": int(height),
            "target_land_fraction": float(target_land_fraction),
            "lon_shift_deg": float(lon_shift_deg),
        },
        "land_fraction": {
            "area_weighted": round(float(tect["land_fraction_area_weighted"]), 5),
            "pixels_tectonic": round(float(tect["land_fraction_pixels"]), 5),
            "pixels_geojson": float(ren_stats["land_fraction_geojson"]),
        },
        "elevation_m": {
            "min": round(elev_min_actual, 2),
            "max": round(elev_max_actual, 2),
        },
        "export": exp_stats,
        "render": ren_stats,
        "artifacts": artifacts,
        "timings_s": {
            "tectonics": round(t_tect, 3),
            "climate_erosion": round(t_clim, 3),
            "export_world": round(t_exp, 3),
            "render_from_files": round(t_ren, 3),
            "total": round(t_total, 3),
        },
    }

    stats_path = out_path / "build_stats.json"
    stats_path.write_text(json.dumps(summary, indent=2), encoding="utf-8", newline="\n")
    summary["stats_json"] = str(stats_path)

    print("\n=== BUILD COMPLETE ===")
    print(f"Total runtime: {t_total:.2f}s  |  Stats saved: {stats_path}")
    for k, info in artifacts.items():
        print(f"  - {k}: {info['path']} ({info['bytes']:,} bytes / {info['mb']:.2f} MB)")
    for cm in ren_stats["crops"]:
        print(f"    * {cm['name']}: center={cm['center_lonlat']} bbox={cm['bbox_px']} ({cm['metric']})")

    if not val["passed"]:
        raise RuntimeError(f"GeoJSON / river validation failed: {val}")

    return summary


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Build Zeta's 4K pre-humanoid world map.")
    parser.add_argument("--seed", type=int, default=3, help="World generation seed (default: 3)")
    parser.add_argument("--width", type=int, default=4096, help="Raster width (default: 4096)")
    parser.add_argument("--height", type=int, default=2048, help="Raster height (default: 2048)")
    parser.add_argument(
        "--land-fraction",
        type=float,
        default=0.35,
        help="Target sphere area-weighted land fraction (default: 0.35)",
    )
    parser.add_argument(
        "--lon-shift-deg",
        type=float,
        default=16.0,
        help="Longitude rotation in degrees passed to tectonics.generate_tectonic_base (default: 16.0)",
    )
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=_WORLDGEN_DIR / "output",
        help="Output directory (default: target/worldgen/output)",
    )
    args = parser.parse_args(argv)
    build_world(
        seed=args.seed,
        width=args.width,
        height=args.height,
        target_land_fraction=args.land_fraction,
        lon_shift_deg=args.lon_shift_deg,
        out_dir=args.out_dir,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
