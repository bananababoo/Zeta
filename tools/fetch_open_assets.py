"""Fetch, organize, and index open-licensed (CC0 / CC-BY / OFL) gap-filling assets for Zeta.

Fills the missing game-ready asset categories identified in our library audit:
  - `ui/`     : Fantasy UI borders, RPG panels/bars/buttons, cursors (Kenney CC0)
  - `icons/`  : 4,100+ RPG/5e/spell/item/condition icons (Game-Icons.net CC-BY 3.0 + Kenney CC0)
  - `audio/`  : RPG SFX, combat impacts, UI sounds (Kenney CC0, OGG)
  - `vfx/`    : Transparent particle & spell VFX sprites (Kenney CC0)
  - `fonts/`  : Fantasy display (Cinzel), narrative serif (EB Garamond), HUD sans (Inter) (SIL OFL 1.1)
  - `tokens/` : Top-down/RPG characters, monsters, and fantasy item sprites (Kenney CC0 + DCSS rltiles CC0)

Usage:
    python tools/fetch_open_assets.py
"""

from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import zipfile

REPO_ROOT = Path(__file__).resolve().parent.parent
LIB_DIR = REPO_ROOT / "assets" / "library"
MANIFEST_PATH = LIB_DIR / "manifest.json"
CACHE_DIR = REPO_ROOT / "target" / "open_assets_cache"

UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) ZetaAssetFetcher/1.0"

KENNEY_PACKS = [
    {
        "slug": "fantasy-ui-borders",
        "category": "ui",
        "subcategory": "fantasy_borders",
        "exts": {".png"},
        "prefer_path_contains": ["PNG/Double/Border", "PNG/Double/Panel", "PNG/Double/Divider", "PNG/Default"],
        "skip_path_contains": ["Sample", "Preview", "Tilemap"],
        "desc": "Ornate fantasy 9-slice UI borders, panels, and dividers",
    },
    {
        "slug": "ui-pack-rpg-expansion",
        "category": "ui",
        "subcategory": "rpg_pack",
        "exts": {".png"},
        "prefer_path_contains": ["PNG/"],
        "skip_path_contains": ["Sample", "Preview", "Spritesheet"],
        "desc": "RPG buttons, bars, panels, inventory frames, and cursors",
    },
    {
        "slug": "cursor-pack",
        "category": "ui",
        "subcategory": "cursors",
        "exts": {".png"},
        "prefer_path_contains": ["PNG/Outline/Default", "PNG/Basic/Default"],
        "skip_path_contains": ["Sample", "Preview", "Vector", "Spritesheet"],
        "desc": "Game cursors (gauntlets, swords, targets, pointers)",
    },
    {
        "slug": "game-icons",
        "category": "icons",
        "subcategory": "ui_hud",
        "exts": {".png"},
        "prefer_path_contains": ["PNG/White/2x/"],
        "skip_path_contains": ["Sample", "Preview", "Spritesheet"],
        "desc": "Clean white HUD, menu, audio, and control icons",
    },
    {
        "slug": "rpg-audio",
        "category": "audio",
        "subcategory": "rpg",
        "exts": {".ogg"},
        "prefer_path_contains": ["Audio/"],
        "skip_path_contains": [],
        "desc": "RPG foley SFX (swords, cloth, leather, footsteps, doors, coins, books)",
    },
    {
        "slug": "interface-sounds",
        "category": "audio",
        "subcategory": "ui",
        "exts": {".ogg"},
        "prefer_path_contains": ["Audio/"],
        "skip_path_contains": [],
        "desc": "UI clicks, hovers, confirms, opens, closes",
    },
    {
        "slug": "impact-sounds",
        "category": "audio",
        "subcategory": "impacts",
        "exts": {".ogg"},
        "prefer_path_contains": ["Audio/"],
        "skip_path_contains": [],
        "desc": "Wood, metal, stone, and soft combat impact SFX",
    },
    {
        "slug": "particle-pack",
        "category": "vfx",
        "subcategory": "particles",
        "exts": {".png"},
        "prefer_path_contains": ["PNG (Transparent)/"],
        "skip_path_contains": ["Sample", "Preview", "Black background"],
        "desc": "High-res transparent fire, smoke, magic, spark, and aura particle textures",
    },
    {
        "slug": "roguelike-characters",
        "category": "tokens",
        "subcategory": "kenney_characters",
        "exts": {".png"},
        "prefer_path_contains": ["Spritesheet/"],
        "skip_path_contains": ["Sample", "Preview"],
        "desc": "Modular fantasy RPG character spritesheet",
    },
    {
        "slug": "tiny-dungeon",
        "category": "tokens",
        "subcategory": "kenney_dungeon",
        "exts": {".png"},
        "prefer_path_contains": ["Tiles/", "Tilemap/"],
        "skip_path_contains": ["Sample", "Preview"],
        "desc": "Fantasy dungeon characters, monsters, and props",
    },
]

GOOGLE_FONTS = [
    (
        "cinzel.ttf",
        "https://raw.githubusercontent.com/google/fonts/main/ofl/cinzel/Cinzel%5Bwght%5D.ttf",
        "Cinzel (SIL OFL 1.1) — fantasy display/header serif",
    ),
    (
        "eb_garamond.ttf",
        "https://raw.githubusercontent.com/google/fonts/main/ofl/ebgaramond/EBGaramond%5Bwght%5D.ttf",
        "EB Garamond (SIL OFL 1.1) — narrative DM log & lore serif",
    ),
    (
        "eb_garamond_italic.ttf",
        "https://raw.githubusercontent.com/google/fonts/main/ofl/ebgaramond/EBGaramond-Italic%5Bwght%5D.ttf",
        "EB Garamond Italic (SIL OFL 1.1) — narrative emphasis",
    ),
    (
        "inter.ttf",
        "https://raw.githubusercontent.com/google/fonts/main/ofl/inter/Inter%5Bopsz%2Cwght%5D.ttf",
        "Inter (SIL OFL 1.1) — clean HUD & character sheet sans-serif",
    ),
]


def slugify(segment: str, max_len: int = 72) -> str:
    s = segment.strip().lower()
    s = s.replace("&", " and ")
    s = re.sub(r"[^a-z0-9]+", "_", s)
    s = re.sub(r"_+", "_", s).strip("_")
    if not s:
        return "unnamed"
    if len(s) > max_len:
        digest = hashlib.sha1(segment.encode("utf-8", errors="ignore")).hexdigest()[:6]
        s = f"{s[: max_len - 7].rstrip('_')}_{digest}"
    return s


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        while True:
            chunk = f.read(1024 * 256)
            if not chunk:
                break
            h.update(chunk)
    return h.hexdigest()


def download_file(url: str, dest: Path) -> Path:
    dest.parent.mkdir(parents=True, exist_ok=True)
    if dest.exists() and dest.stat().st_size > 0:
        return dest
    part = dest.with_name(dest.name + ".part")
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=120) as resp, part.open("wb") as out:
        shutil.copyfileobj(resp, out, length=1024 * 256)
    os.replace(part, dest)
    return dest


def resolve_kenney_zip(slug: str) -> str:
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    page_url = f"https://kenney.nl/assets/{slug}"
    req = urllib.request.Request(page_url, headers={"User-Agent": UA})
    html = opener.open(req, timeout=30).read().decode("utf-8", errors="ignore")

    m = re.search(r"data-download-id=['\"]([0-9a-f]+)['\"]", html)
    if m:
        dl_id = m.group(1)
        api_req = urllib.request.Request(
            f"https://kenney.nl/download-link/{dl_id}",
            headers={"User-Agent": UA, "Referer": page_url, "Accept": "application/json"},
        )
        data = json.loads(opener.open(api_req, timeout=30).read().decode("utf-8"))
        if data.get("url"):
            return data["url"]

    matches = re.findall(r'https://kenney\.nl/media/pages/assets/[^"]+?\.zip', html)
    if matches:
        return matches[0]
    raise RuntimeError(f"Could not resolve zip download link on {page_url}")


def fetch_kenney_packs() -> list[dict]:
    added: list[dict] = []
    for pack in KENNEY_PACKS:
        slug = pack["slug"]
        print(f"[Kenney] {slug}...", flush=True)
        try:
            zip_url = resolve_kenney_zip(slug)
            zip_path = download_file(zip_url, CACHE_DIR / f"kenney_{slug}.zip")
        except Exception as exc:
            print(f"  WARN failed {slug}: {exc}", file=sys.stderr, flush=True)
            continue

        cat = pack["category"]
        sub = pack["subcategory"]
        exts = pack["exts"]
        prefers = pack["prefer_path_contains"]
        skips = [s.lower() for s in pack["skip_path_contains"]]

        out_base = LIB_DIR / cat / sub
        out_base.mkdir(parents=True, exist_ok=True)

        count = 0
        used_names: dict[str, int] = {}
        with zipfile.ZipFile(zip_path, "r") as zf:
            infos = [i for i in zf.infolist() if not i.is_dir()]
            # Filter by extension and skip patterns
            filtered = []
            for info in infos:
                norm = info.filename.replace("\\", "/")
                low = norm.lower()
                ext = Path(low).suffix
                if ext not in exts:
                    continue
                if any(sk in low for sk in skips):
                    continue
                if prefers and not any(p.lower() in low for p in prefers):
                    continue
                filtered.append(info)
            # Fallback if prefer filter was too strict
            if not filtered:
                for info in infos:
                    norm = info.filename.replace("\\", "/")
                    low = norm.lower()
                    if Path(low).suffix in exts and not any(sk in low for sk in skips):
                        filtered.append(info)

            for info in sorted(filtered, key=lambda x: x.filename.lower()):
                norm = info.filename.replace("\\", "/")
                ext = Path(norm).suffix.lower()
                stem = slugify(Path(norm).stem)
                fname = f"{stem}{ext}"
                if fname in used_names:
                    used_names[fname] += 1
                    fname = f"{stem}_{used_names[fname]}{ext}"
                else:
                    used_names[fname] = 1
                dest = out_base / fname
                if not dest.exists() or dest.stat().st_size != info.file_size:
                    with zf.open(info, "r") as src, dest.open("wb") as dst:
                        shutil.copyfileobj(src, dst)
                count += 1
                added.append({
                    "id": f"{cat}/{sub}/{dest.stem}",
                    "category": cat,
                    "subcategory": sub,
                    "path": f"library/{cat}/{sub}/{dest.name}",
                    "tags": [cat, sub, "kenney", "cc0", slug],
                    "source_path": f"kenney/{slug}/{norm}",
                    "size_bytes": dest.stat().st_size,
                    "ext": ext,
                })
        print(f"  -> {count} files in assets/library/{cat}/{sub}/", flush=True)
    return added


def fetch_game_icons() -> list[dict]:
    print("[Game-Icons.net] Downloading CC-BY 3.0 RPG icon library from GitHub...", flush=True)
    zip_url = "https://github.com/game-icons/icons/archive/refs/heads/master.zip"
    zip_path = download_file(zip_url, CACHE_DIR / "game-icons-master.zip")

    out_base = LIB_DIR / "icons" / "rpg_svg"
    out_base.mkdir(parents=True, exist_ok=True)

    added: list[dict] = []
    used_names: dict[str, int] = {}
    with zipfile.ZipFile(zip_path, "r") as zf:
        svg_infos = [
            i
            for i in zf.infolist()
            if not i.is_dir() and i.filename.lower().endswith(".svg")
        ]
        for info in sorted(svg_infos, key=lambda x: x.filename.lower()):
            parts = info.filename.replace("\\", "/").split("/")
            # Structure: icons-master/<author>/originals/svg/.../<name>.svg
            author = slugify(parts[1]) if len(parts) > 2 else "game_icons"
            stem = slugify(Path(parts[-1]).stem)
            fname = f"{stem}.svg"
            if fname in used_names:
                used_names[fname] += 1
                fname = f"{stem}_{author}.svg"
                if fname in used_names:
                    used_names[fname] += 1
                    fname = f"{stem}_{author}_{used_names[fname]}.svg"
                else:
                    used_names[fname] = 1
            else:
                used_names[fname] = 1

            dest = out_base / fname
            if not dest.exists() or dest.stat().st_size != info.file_size:
                with zf.open(info, "r") as src, dest.open("wb") as dst:
                    shutil.copyfileobj(src, dst)
            added.append({
                "id": f"icons/rpg_svg/{dest.stem}",
                "category": "icons",
                "subcategory": "rpg_svg",
                "path": f"library/icons/rpg_svg/{dest.name}",
                "tags": ["icons", "rpg", "game_icons", "cc_by_3", author],
                "source_path": f"game-icons/{author}/{parts[-1]}",
                "size_bytes": dest.stat().st_size,
                "ext": ".svg",
            })
    print(f"  -> {len(added)} RPG SVG icons in assets/library/icons/rpg_svg/", flush=True)
    return added


def fetch_google_fonts() -> list[dict]:
    print("[Google Fonts] Downloading SIL OFL 1.1 typography (Cinzel, EB Garamond, Inter)...", flush=True)
    out_base = LIB_DIR / "fonts"
    out_base.mkdir(parents=True, exist_ok=True)
    added: list[dict] = []
    for fname, url, desc in GOOGLE_FONTS:
        dest = out_base / fname
        try:
            download_file(url, dest)
            added.append({
                "id": f"fonts/{dest.stem}",
                "category": "fonts",
                "subcategory": "",
                "path": f"library/fonts/{dest.name}",
                "tags": ["fonts", "ofl", dest.stem],
                "source_path": f"google-fonts/{fname}",
                "size_bytes": dest.stat().st_size,
                "ext": ".ttf",
            })
            print(f"  -> {fname} ({dest.stat().st_size // 1024} KB)", flush=True)
        except Exception as exc:
            print(f"  WARN failed {fname}: {exc}", file=sys.stderr, flush=True)
    return added


def fetch_dcss_rltiles() -> list[dict]:
    """Sparse-checkout CC0 Dungeon Crawl Stone Soup rltiles (monsters, player sprites, items, spells, effects)."""
    print("[DCSS rltiles] Sparse-fetching CC0 monsters, characters, items, and spells...", flush=True)
    clone_dir = CACHE_DIR / "dcss_sparse"
    rltiles_root = clone_dir / "crawl-ref" / "source" / "rltiles"
    sparse_dirs = [
        "crawl-ref/source/rltiles/mon",
        "crawl-ref/source/rltiles/player",
        "crawl-ref/source/rltiles/item",
        "crawl-ref/source/rltiles/gui/spells",
        "crawl-ref/source/rltiles/effect",
    ]
    if not rltiles_root.exists():
        clone_dir.parent.mkdir(parents=True, exist_ok=True)
        if clone_dir.exists():
            shutil.rmtree(clone_dir, ignore_errors=True)
        cmds = [
            [
                "git",
                "clone",
                "--depth",
                "1",
                "--filter=blob:none",
                "--sparse",
                "https://github.com/crawl/crawl.git",
                str(clone_dir),
            ],
            ["git", "-C", str(clone_dir), "sparse-checkout", "set", *sparse_dirs],
        ]
        for cmd in cmds:
            proc = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace")
            if proc.returncode != 0:
                print(f"  WARN DCSS sparse clone failed: {proc.stderr[:300]}", file=sys.stderr, flush=True)
                return []

    mapping = [
        (rltiles_root / "mon", "tokens", "monsters"),
        (rltiles_root / "player", "tokens", "characters"),
        (rltiles_root / "item", "icons", "items_pixel"),
        (rltiles_root / "gui" / "spells", "icons", "spells_pixel"),
        (rltiles_root / "effect", "vfx", "effects_pixel"),
    ]
    added: list[dict] = []
    for src_root, cat, base_sub in mapping:
        if not src_root.exists():
            continue
        count = 0
        for p in sorted(src_root.rglob("*.png")):
            rel = p.relative_to(src_root)
            sub_parts = [base_sub] + [slugify(x, max_len=32) for x in rel.parent.parts]
            sub = "/".join(x for x in sub_parts if x)
            fname = f"{slugify(p.stem)}.png"
            dest = LIB_DIR / cat / sub / fname
            dest.parent.mkdir(parents=True, exist_ok=True)
            if not dest.exists() or dest.stat().st_size != p.stat().st_size:
                shutil.copy2(p, dest)
            count += 1
            added.append({
                "id": f"{cat}/{sub}/{dest.stem}",
                "category": cat,
                "subcategory": sub,
                "path": f"library/{cat}/{sub}/{dest.name}",
                "tags": [cat, *sub.split("/"), "dcss", "cc0"][:6],
                "source_path": f"dcss-rltiles/{p.relative_to(rltiles_root).as_posix()}",
                "size_bytes": dest.stat().st_size,
                "ext": ".png",
            })
        print(f"  -> {count:4d} PNGs in assets/library/{cat}/{base_sub}/", flush=True)
    return added


def write_attributions() -> None:
    content = """# Asset Library Attributions & Licenses

Every pack downloaded by `tools/fetch_open_assets.py` is permissively licensed for commercial and non-commercial use.

| Pack | Category | License | Attribution / Source |
|---|---|---|---|
| Kenney Fantasy UI Borders, UI Pack RPG Expansion, Cursor Pack, Game Icons, RPG Audio, Interface Sounds, Impact Sounds, Particle Pack, Roguelike Characters, Tiny Dungeon | `ui/`, `icons/`, `audio/`, `vfx/`, `tokens/` | **CC0 1.0 (Public Domain)** | Kenney — https://kenney.nl/assets |
| Game-Icons.net RPG Icon Library | `icons/rpg_svg/` | **CC-BY 3.0** | Lorc, Delapouite, Carl Olsen, Skoll, sbed, and contributors via https://game-icons.net / https://github.com/game-icons/icons (individual author recorded in `manifest.json` `source_path`) |
| Dungeon Crawl Stone Soup (`rltiles`) | `tokens/monsters/`, `tokens/characters/`, `icons/items_pixel/`, `icons/spells_pixel/`, `vfx/effects_pixel/` | **CC0 1.0 (Public Domain)** | Dungeon Crawl Stone Soup tile artists — https://github.com/crawl/crawl/tree/master/crawl-ref/source/rltiles |
| Cinzel, EB Garamond, Inter | `fonts/` | **SIL OFL 1.1** | Google Fonts — https://github.com/google/fonts |
"""
    (LIB_DIR / "ATTRIBUTIONS.md").write_text(content, encoding="utf-8")


def merge_into_manifest(new_entries: list[dict]) -> None:
    if MANIFEST_PATH.exists():
        manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
        existing = manifest.get("assets", [])
    else:
        existing = []

    by_path: dict[str, dict] = {a["path"]: a for a in existing}
    for entry in new_entries:
        p = REPO_ROOT / "assets" / entry["path"]
        if not p.exists():
            continue
        entry["sha256"] = sha256_file(p)
        by_path[entry["path"]] = entry

    all_assets = sorted(by_path.values(), key=lambda x: (x["category"], x["subcategory"], x["id"]))
    by_cat = dict(sorted(Counter(a["category"] for a in all_assets).items()))
    manifest = {
        "version": 2,
        "count": len(all_assets),
        "total_bytes": sum(a["size_bytes"] for a in all_assets),
        "categories": by_cat,
        "assets": all_assets,
    }
    tmp = MANIFEST_PATH.with_suffix(".tmp")
    tmp.write_text(json.dumps(manifest, ensure_ascii=False, indent=2), encoding="utf-8")
    os.replace(tmp, MANIFEST_PATH)
    print(f"\nUpdated {MANIFEST_PATH}: {len(all_assets)} total assets across {len(by_cat)} categories.")
    for c, n in by_cat.items():
        print(f"  {c:14s}: {n:5d}")


def main() -> int:
    parser = argparse.ArgumentParser(description="Fetch open-licensed gap-filling assets into assets/library/.")
    parser.add_argument("--skip-dcss", action="store_true")
    parser.add_argument("--skip-game-icons", action="store_true")
    args = parser.parse_args()

    t0 = time.monotonic()
    CACHE_DIR.mkdir(parents=True, exist_ok=True)

    new_entries: list[dict] = []
    new_entries.extend(fetch_kenney_packs())
    new_entries.extend(fetch_google_fonts())
    if not args.skip_game_icons:
        new_entries.extend(fetch_game_icons())
    if not args.skip_dcss:
        new_entries.extend(fetch_dcss_rltiles())

    write_attributions()
    merge_into_manifest(new_entries)
    print(f"\nAll open-licensed gap packs fetched & indexed in {time.monotonic() - t0:.1f}s.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
