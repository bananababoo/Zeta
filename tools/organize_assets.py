"""Deduplicate, vision-rename, and reorganize Zeta's `assets/library/` into a cohesive taxonomy.

Subcommands:
    python tools/organize_assets.py audit
    python tools/organize_assets.py label-vision [--workers 4] [--model gemini-2.5-flash]
    python tools/organize_assets.py apply [--dry-run]
"""

from __future__ import annotations

import argparse
import base64
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor, as_completed
import hashlib
import io
import json
import os
from pathlib import Path
import re
import sys
import time
import urllib.error
import urllib.request

from PIL import Image, ImageDraw, ImageFont

REPO_ROOT = Path(__file__).resolve().parent.parent
LIB_DIR = REPO_ROOT / "assets" / "library"
MANIFEST_PATH = LIB_DIR / "manifest.json"
AUDIT_PATH = REPO_ROOT / "target" / "library_audit.json"
VISION_CACHE_PATH = REPO_ROOT / "target" / "vision_labels.json"
SHEETS_DIR = REPO_ROOT / "target" / "vision_sheets"

IMAGE_EXTS = {".png", ".jpg", ".jpeg", ".webp", ".bmp", ".tga", ".gif"}
MAX_STEM_LEN = 72

# --- Gibberish / non-descriptive filename detection ---
GIBBERISH_EXACT_RE = re.compile(
    r"^(?:"
    r"[0-9a-f]{10,}"
    r"|[0-9a-f]{8}_[0-9a-f]{4}_[0-9a-f]{4}_[0-9a-f]{4}_[0-9a-f]{12}"
    r"|\d+(?:_\d+)*"
    r"|(?:img|image|pic|photo|dsc|dscn|p|scan|file|asset|tile|tex|obj|unnamed|untitled|download|copy|new|temp|tmp)_?\d*"
    r"|[a-z]{1,2}\d{1,4}"
    r")$"
)
HEX_PREFIX_RE = re.compile(r"^[0-9a-f]{16,}_")
SHORT_HEX_PREFIX_RE = re.compile(r"^[0-9a-f]{3,4}_(?=[a-z])")

# Trailing artist/source/pack suffixes to strip from already-descriptive names.
NOISE_SUFFIX_RE = re.compile(
    r"(?:_(?:llk|sr|cis|me|ae|hi|bg|vtt|mt|dd|4e|pf|d20|seamless|texture))+$"
)
DEVIANTART_SUFFIX_RE = re.compile(r"_by_[a-z0-9]+_d[a-z0-9]{5,}$")
STOCK_PREFIX_RE = re.compile(r"^\d{4,6}_(?=[a-z]{3,})")

# --- Unified 7-category taxonomy mapping ---
# Maps (old_category, old_top_subcat[, old_second_subcat]) -> (new_category, new_subcat_prefix)
EXACT_SUBCAT_MAP: dict[tuple[str, str], tuple[str, str]] = {
    # Textures
    ("textures", "natural/grass"): ("textures", "terrain/grass"),
    ("textures", "natural/dirt"): ("textures", "terrain/dirt"),
    ("textures", "natural/sand"): ("textures", "terrain/sand"),
    ("textures", "natural/rock"): ("textures", "terrain/rock"),
    ("textures", "natural/snow"): ("textures", "terrain/snow"),
    ("textures", "natural/ice"): ("textures", "terrain/ice"),
    ("textures", "natural/water"): ("textures", "terrain/water"),
    ("textures", "natural/ocean"): ("textures", "terrain/ocean"),
    ("textures", "natural/swamp"): ("textures", "terrain/swamp"),
    ("textures", "natural/lava"): ("textures", "terrain/lava"),
    ("textures", "natural/slime"): ("textures", "terrain/slime"),
    ("textures", "natural/bubbles"): ("textures", "terrain/bubbles"),
    ("textures", "natural/fire"): ("textures", "terrain/fire"),
    ("textures", "natural/undergrowth"): ("textures", "terrain/undergrowth"),
    ("textures", "natural/crystals_and_ore"): ("textures", "terrain/crystals_ore"),
    ("textures", "natural"): ("textures", "terrain/misc"),
    ("textures", "ocean"): ("textures", "terrain/ocean"),
    ("textures", "fabricated/tile"): ("textures", "floors/tile"),
    ("textures", "fabricated/marble"): ("textures", "floors/marble"),
    ("textures", "fabricated/stone"): ("textures", "floors/stone"),
    ("textures", "fabricated/wood"): ("textures", "floors/wood"),
    ("textures", "fabricated/metal"): ("textures", "floors/metal"),
    ("textures", "fabricated/sandstone"): ("textures", "floors/sandstone"),
    ("textures", "fabricated/carpet"): ("textures", "floors/carpet"),
    ("textures", "fabricated/mosaic"): ("textures", "floors/mosaic"),
    ("textures", "fabricated/concrete"): ("textures", "floors/concrete"),
    ("textures", "fabricated/ceramic"): ("textures", "floors/ceramic"),
    ("textures", "fabricated/linoleum"): ("textures", "floors/linoleum"),
    ("textures", "fabricated/road_path_trail"): ("textures", "floors/road"),
    ("textures", "fabricated/farmland"): ("textures", "floors/farmland"),
    ("textures", "fabricated/stucco"): ("textures", "materials/stucco"),
    ("textures", "fabricated/plaster"): ("textures", "materials/plaster"),
    ("textures", "fabricated/roof"): ("textures", "materials/roof"),
    ("textures", "fabricated/glass"): ("textures", "materials/glass"),
    ("textures", "fabricated/leather"): ("textures", "materials/leather"),
    ("textures", "fabricated/paper"): ("textures", "materials/paper"),
    ("textures", "fabricated/alien"): ("textures", "materials/alien"),
    ("textures", "fabricated"): ("textures", "floors/misc"),
    ("textures", "new/stone"): ("textures", "floors/stone"),
    ("textures", "new/rock"): ("textures", "terrain/rock"),
    ("textures", "new/tiles"): ("textures", "floors/tile"),
    ("textures", "new/wood"): ("textures", "floors/wood"),
    ("textures", "new"): ("textures", "floors/misc"),
    ("textures", "new_mak_2"): ("textures", "floors/misc"),
    ("textures", "creature/organic"): ("textures", "creature/organic"),
    ("textures", "creature/blood"): ("textures", "creature/blood"),
    ("textures", "creature/bone"): ("textures", "creature/bone"),
    ("textures", "creature/skin"): ("textures", "creature/skin"),
    ("textures", "creature"): ("textures", "creature/misc"),
    ("textures", "hex/singles"): ("textures", "hex"),
    ("textures", "hex"): ("textures", "hex"),
    ("textures", "overlays/shadows"): ("overlays", "shadows"),
    ("textures", "overlays/textures"): ("overlays", "ground"),
    ("textures", "overlays"): ("overlays", "ground"),
    # Tiles pack (592260)
    ("tiles", "floors/ground"): ("textures", "floors/ground"),
    ("tiles", "floors/marble_green"): ("textures", "floors/marble"),
    ("tiles", "floors/wood_basic_2"): ("textures", "floors/wood"),
    ("tiles", "floors/metal"): ("textures", "floors/metal"),
    ("tiles", "floors/stone_road"): ("textures", "floors/road"),
    ("tiles", "floors/stone1"): ("textures", "floors/stone"),
    ("tiles", "floors/overlay"): ("overlays", "ground"),
    ("tiles", "floors"): ("textures", "floors/misc"),
    ("tiles", "walls/main"): ("textures", "walls"),
    ("tiles", "walls"): ("textures", "walls"),
    # Backgrounds -> Nature / Structures / Overlays / Textures
    ("backgrounds", "environment/plants"): ("nature", "plants"),
    ("backgrounds", "environment/rocky"): ("nature", "rocks"),
    ("backgrounds", "environment/caves"): ("nature", "caves"),
    ("backgrounds", "environment/farming"): ("nature", "farming"),
    ("backgrounds", "environment/alien_plants"): ("nature", "alien_plants"),
    ("backgrounds", "environment/topiary"): ("nature", "topiary"),
    ("backgrounds", "environment/overlays"): ("overlays", "ground"),
    ("backgrounds", "environment/elavation_and_shadings"): ("overlays", "shadows"),
    ("backgrounds", "environment"): ("nature", "misc"),
    ("backgrounds", "structures/buildings"): ("structures", "buildings"),
    ("backgrounds", "structures/walls"): ("structures", "walls"),
    ("backgrounds", "structures/columns"): ("structures", "columns"),
    ("backgrounds", "structures/obstacles"): ("structures", "obstacles"),
    ("backgrounds", "structures/stairs"): ("structures", "stairs"),
    ("backgrounds", "structures/bridges"): ("structures", "bridges"),
    ("backgrounds", "structures/altar"): ("structures", "altars"),
    ("backgrounds", "structures/doors"): ("structures", "doors"),
    ("backgrounds", "structures/holes"): ("structures", "grates_holes"),
    ("backgrounds", "structures/grating"): ("structures", "grates_holes"),
    ("backgrounds", "structures/arches"): ("structures", "arches"),
    ("backgrounds", "structures/piers"): ("structures", "piers"),
    ("backgrounds", "structures/rubble"): ("structures", "rubble"),
    ("backgrounds", "structures/fountains"): ("structures", "fountains"),
    ("backgrounds", "structures/windows"): ("structures", "windows"),
    ("backgrounds", "structures/building_support"): ("structures", "columns"),
    ("backgrounds", "structures/awnings"): ("structures", "awnings"),
    ("backgrounds", "structures/privy"): ("structures", "privy"),
    ("backgrounds", "structures/monoliths"): ("structures", "monoliths"),
    ("backgrounds", "structures/pools"): ("structures", "pools"),
    ("backgrounds", "structures/pipes"): ("structures", "pipes"),
    ("backgrounds", "structures/gates"): ("structures", "gates"),
    ("backgrounds", "structures/dias"): ("structures", "altars"),
    ("backgrounds", "structures/wells"): ("structures", "wells"),
    ("backgrounds", "structures/portals"): ("structures", "portals"),
    ("backgrounds", "structures/forge"): ("structures", "forges"),
    ("backgrounds", "structures/poles"): ("structures", "poles"),
    ("backgrounds", "structures/aquaducts"): ("structures", "aqueducts"),
    ("backgrounds", "structures/farm"): ("structures", "farm"),
    ("backgrounds", "structures/gallows"): ("structures", "gallows"),
    ("backgrounds", "structures/defensive"): ("structures", "defensive"),
    ("backgrounds", "structures/pyramids"): ("structures", "buildings"),
    ("backgrounds", "structures/paddlewheels"): ("structures", "misc"),
    ("backgrounds", "structures/satellite_dishes"): ("structures", "misc"),
    ("backgrounds", "structures/statues"): ("structures", "statues"),
    ("backgrounds", "structures/floors"): ("textures", "floors/misc"),
    ("backgrounds", "structures/objects"): ("props", "misc"),
    ("backgrounds", "structures"): ("structures", "misc"),
    ("backgrounds", "overlays/railways"): ("overlays", "railways"),
    ("backgrounds", "overlays/roads"): ("overlays", "roads"),
    ("backgrounds", "overlays/foot_prints"): ("overlays", "footprints"),
    ("backgrounds", "overlays/bloody"): ("overlays", "blood"),
    ("backgrounds", "overlays/graveyard"): ("overlays", "graveyard"),
    ("backgrounds", "overlays/boneyard"): ("overlays", "graveyard"),
    ("backgrounds", "overlays/breakwater"): ("overlays", "ground"),
    ("backgrounds", "overlays"): ("overlays", "ground"),
    ("backgrounds", "templates/spells"): ("overlays", "templates"),
    ("backgrounds", "templates/fire"): ("overlays", "effects"),
    ("backgrounds", "templates/magicsphere01_sr"): ("overlays", "templates"),
    ("backgrounds", "templates"): ("overlays", "templates"),
    ("backgrounds", "planes"): ("overlays", "planes"),
    ("backgrounds", "floors/ground"): ("textures", "floors/ground"),
    ("backgrounds", "floors/marble_green"): ("textures", "floors/marble"),
    ("backgrounds", "floors/wood_basic_2"): ("textures", "floors/wood"),
    ("backgrounds", "floors/metal"): ("textures", "floors/metal"),
    ("backgrounds", "floors/stone_road"): ("textures", "floors/road"),
    ("backgrounds", "floors/stone1"): ("textures", "floors/stone"),
    ("backgrounds", "floors/overlay"): ("overlays", "ground"),
    ("backgrounds", "floors"): ("textures", "floors/misc"),
    # Maps & Markers
    ("maps", ""): ("maps", ""),
    ("markers", ""): ("markers", ""),
}

# Shared mapping for the 60 MapTool object buckets (which appear under `objects/`, `tiles/objects/`, and `backgrounds/floors/`).
OBJECT_BUCKET_MAP: dict[str, tuple[str, str]] = {
    "n_trees": ("nature", "trees"),
    "n_bushes_plants_stuff": ("nature", "plants"),
    "caves": ("nature", "caves"),
    "buildings": ("structures", "buildings"),
    "doors": ("structures", "doors"),
    "windows": ("structures", "windows"),
    "bridges": ("structures", "bridges"),
    "stairs_and_trapdoors": ("structures", "stairs"),
    "tents": ("structures", "tents"),
    "ruins": ("structures", "ruins"),
    "cemetery": ("structures", "cemetery"),
    "fireplace": ("structures", "fireplaces"),
    "grates_vents_holes": ("structures", "grates_holes"),
    "fountains_and_wells": ("structures", "fountains"),
    "altars": ("structures", "altars"),
    "pillars": ("structures", "pillars"),
    "railroad": ("structures", "railroad"),
    "sewers": ("structures", "sewers"),
    "boats_ships": ("structures", "ships"),
    "carts_wagons": ("structures", "vehicles"),
    "transportation": ("structures", "vehicles"),
    "siege_tools": ("structures", "siege"),
    "obj_statues_etc": ("structures", "statues"),
    "frn_beds": ("props", "furniture/beds"),
    "bedding": ("props", "furniture/beds"),
    "frn_tables_and_bartables": ("props", "furniture/tables"),
    "frn_chairs_thrones_etc": ("props", "furniture/chairs"),
    "frn_benches": ("props", "furniture/benches"),
    "frn_cabinets_desks_shelves": ("props", "furniture/cabinets"),
    "frn_chests": ("props", "furniture/chests"),
    "obj_carpets_and_pelts": ("props", "furniture/carpets"),
    "curtains": ("props", "furniture/curtains"),
    "obj_bath": ("props", "furniture/bath"),
    "crates_barrels_baskets": ("props", "containers"),
    "tabletop_food_drink": ("props", "tabletop/food_drink"),
    "tabletop_books_magic": ("props", "tabletop/books_magic"),
    "tables_craft_trade": ("props", "tabletop/craft_trade"),
    "tabletop_weapons_tools": ("props", "tabletop/weapons_tools"),
    "obj_kitchen": ("props", "tabletop/kitchen"),
    "obj_cauldrons": ("props", "tabletop/cauldrons"),
    "obj_weapons": ("props", "equipment/weapons"),
    "obj_armor": ("props", "equipment/armor"),
    "obj_clothing": ("props", "equipment/clothing"),
    "animals": ("props", "creatures/animals"),
    "obj_corpses": ("props", "creatures/corpses"),
    "obj_eggs_nests": ("props", "creatures/eggs_nests"),
    "lights": ("props", "dungeon/lights"),
    "campfires": ("props", "dungeon/campfires"),
    "traps": ("props", "dungeon/traps"),
    "obj_cages_chains": ("props", "dungeon/cages_chains"),
    "webs": ("props", "dungeon/webs"),
    "obj_wall_mounted": ("props", "dungeon/wall_mounted"),
    "barn_and_stables": ("props", "stables"),
    "items": ("props", "items"),
    "obj_objects": ("props", "misc"),
    "blood": ("overlays", "blood"),
    "overlays": ("overlays", "effects"),
    "overlays_ground": ("overlays", "ground"),
    "overlays_mosaic_circles": ("overlays", "mosaics"),
}

# Keyword-based classifier for uncategorized root textures (`textures/(root)`, `textures/new`, `textures/new_mak_2`).
TEXTURE_KEYWORD_RULES: list[tuple[re.Pattern[str], str]] = [
    (re.compile(r"\b(?:grass|moss|meadow|lawn|turf|clover|foliage|leaf|leaves)\b"), "terrain/grass"),
    (re.compile(r"\b(?:dirt|mud|soil|earth|loam|clay)\b"), "terrain/dirt"),
    (re.compile(r"\b(?:sand|desert|dune|beach)\b"), "terrain/sand"),
    (re.compile(r"\b(?:water|river|lake|pond|stream|pool|ripple|wave)\b"), "terrain/water"),
    (re.compile(r"\b(?:ocean|sea|deep_water)\b"), "terrain/ocean"),
    (re.compile(r"\b(?:snow|frost|tundra|blizzard)\b"), "terrain/snow"),
    (re.compile(r"\b(?:ice|glacier|frozen)\b"), "terrain/ice"),
    (re.compile(r"\b(?:lava|magma|volcan|ember)\b"), "terrain/lava"),
    (re.compile(r"\b(?:swamp|marsh|bog|mire|slime|ooze)\b"), "terrain/swamp"),
    (re.compile(r"\b(?:crystal|ore|gem|quartz|obsidian)\b"), "terrain/crystals_ore"),
    (re.compile(r"\b(?:marble|granite|travertine|onyx|alabaster)\b"), "floors/marble"),
    (re.compile(r"\b(?:wood|plank|timber|oak|cherry|pine|bark|lumber|parquet|floorboard)\b"), "floors/wood"),
    (re.compile(r"\b(?:tile|paving|paver|cobble|flagstone|brick|masonry|terracotta|slate)\b"), "floors/tile"),
    (re.compile(r"\b(?:concrete|asphalt|cement|pavement|sidewalk|road|path)\b"), "floors/concrete"),
    (re.compile(r"\b(?:metal|steel|iron|copper|bronze|rust|plate|grate|chrome|gold|silver)\b"), "floors/metal"),
    (re.compile(r"\b(?:carpet|rug|fabric|cloth|wool|velvet|linen|woven)\b"), "floors/carpet"),
    (re.compile(r"\b(?:roof|shingle|thatch)\b"), "materials/roof"),
    (re.compile(r"\b(?:plaster|stucco|wall|wallpaper|drywall)\b"), "materials/plaster"),
    (re.compile(r"\b(?:leather|hide|suede)\b"), "materials/leather"),
    (re.compile(r"\b(?:rock|stone|cliff|boulder|gravel|pebble|crag|basalt|limestone|sandstone)\b"), "terrain/rock"),
]

VALID_VISION_CATEGORIES = {"textures", "nature", "structures", "props", "overlays", "markers", "maps"}


def slugify(segment: str, max_len: int = MAX_STEM_LEN) -> str:
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


def clean_descriptive_stem(stem: str) -> str:
    """Strip stock IDs, deviantArt suffixes, and MapTool author initials without losing meaning."""
    s = stem
    s = DEVIANTART_SUFFIX_RE.sub("", s)
    s = STOCK_PREFIX_RE.sub("", s)
    s = SHORT_HEX_PREFIX_RE.sub("", s)
    s = re.sub(r"_google_search_fine_art$", "", s)
    cleaned = NOISE_SUFFIX_RE.sub("", s)
    if len(cleaned) >= 3:
        s = cleaned
    return slugify(s)


def is_gibberish_stem(stem: str) -> bool:
    if len(stem) <= 2:
        return True
    if GIBBERISH_EXACT_RE.match(stem):
        return True
    if HEX_PREFIX_RE.match(stem):
        return True
    return False


def map_category(cat: str, subcat: str, stem: str) -> tuple[str, str]:
    """Map an existing (category, subcategory) into the unified 7-category taxonomy."""
    parts = [p for p in subcat.split("/") if p]

    # Direct exact match on (cat, top/second) or (cat, top)
    if len(parts) >= 2:
        key2 = (cat, f"{parts[0]}/{parts[1]}")
        if key2 in EXACT_SUBCAT_MAP:
            new_cat, new_base = EXACT_SUBCAT_MAP[key2]
            rest = parts[2:]
            new_sub = "/".join([p for p in [new_base, *rest] if p])
            return new_cat, new_sub

    if len(parts) >= 1:
        # Check OBJECT_BUCKET_MAP for `objects/<bucket>`, `tiles/objects/<bucket>`, `backgrounds/floors/<bucket>`, `backgrounds/structures/objects/<bucket>`
        if cat == "objects" and parts[0] in OBJECT_BUCKET_MAP:
            new_cat, new_base = OBJECT_BUCKET_MAP[parts[0]]
            rest = parts[1:]
            return new_cat, "/".join([p for p in [new_base, *rest] if p])
        if cat == "tiles" and parts[0] == "objects" and len(parts) >= 2 and parts[1] in OBJECT_BUCKET_MAP:
            new_cat, new_base = OBJECT_BUCKET_MAP[parts[1]]
            rest = parts[2:]
            return new_cat, "/".join([p for p in [new_base, *rest] if p])
        if cat == "backgrounds" and parts[0] == "floors" and len(parts) >= 2 and parts[1] in OBJECT_BUCKET_MAP:
            new_cat, new_base = OBJECT_BUCKET_MAP[parts[1]]
            rest = parts[2:]
            return new_cat, "/".join([p for p in [new_base, *rest] if p])
        if cat == "backgrounds" and len(parts) >= 3 and parts[0] == "structures" and parts[1] == "objects" and parts[2] in OBJECT_BUCKET_MAP:
            new_cat, new_base = OBJECT_BUCKET_MAP[parts[2]]
            rest = parts[3:]
            return new_cat, "/".join([p for p in [new_base, *rest] if p])

        key1 = (cat, parts[0])
        if key1 in EXACT_SUBCAT_MAP:
            new_cat, new_base = EXACT_SUBCAT_MAP[key1]
            rest = parts[1:]
            # Keyword-reclassify `textures/new` and `textures/new_mak_2` if possible.
            if cat == "textures" and parts[0] in ("new", "new_mak_2") and not rest:
                kw_sub = classify_texture_by_name(stem)
                if kw_sub:
                    return "textures", kw_sub
            return new_cat, "/".join([p for p in [new_base, *rest] if p])

    # Root-level uncategorized files
    if cat == "textures":
        kw_sub = classify_texture_by_name(stem)
        return "textures", kw_sub or "floors/misc"
    if cat == "maps":
        return "maps", ""
    if cat == "markers":
        return "markers", ""
    if cat == "objects":
        return "props", "misc"
    if cat == "backgrounds":
        return "structures", "misc"
    return "props", "misc"


def classify_texture_by_name(stem: str) -> str | None:
    text = stem.replace("_", " ")
    for pat, sub in TEXTURE_KEYWORD_RULES:
        if pat.search(text):
            return sub
    return None


def dedup_preference_score(asset: dict) -> tuple[int, int, int]:
    """Higher score wins when deduplicating identical SHA-256 files."""
    cat_priority = {
        "textures": 6,
        "objects": 5,
        "markers": 5,
        "maps": 5,
        "backgrounds": 3,
        "tiles": 2,
    }.get(asset["category"], 1)
    stem = asset["id"].rsplit("/", 1)[-1]
    not_gib = 0 if is_gibberish_stem(stem) else 1
    has_sub = 1 if asset.get("subcategory") else 0
    return (not_gib, cat_priority, has_sub)


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        while True:
            chunk = f.read(1024 * 256)
            if not chunk:
                break
            h.update(chunk)
    return h.hexdigest()


def cmd_audit(args: argparse.Namespace) -> int:
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    assets = manifest["assets"]
    print(f"Hashing {len(assets)} files for exact deduplication...", flush=True)
    t0 = time.monotonic()

    groups: dict[str, list[dict]] = defaultdict(list)
    missing = 0
    for idx, a in enumerate(assets, 1):
        p = REPO_ROOT / "assets" / a["path"]
        if not p.exists():
            missing += 1
            continue
        digest = sha256_file(p)
        a_copy = {**a, "sha256": digest}
        groups[digest].append(a_copy)
        if idx % 5000 == 0 or idx == len(assets):
            print(f"  hashed {idx}/{len(assets)} ({time.monotonic() - t0:.1f}s)", flush=True)

    unique_assets: list[dict] = []
    dup_files = 0
    dup_bytes = 0
    for digest, members in groups.items():
        members_sorted = sorted(members, key=dedup_preference_score, reverse=True)
        winner = members_sorted[0]
        winner["duplicate_paths"] = [m["path"] for m in members_sorted[1:]]
        unique_assets.append(winner)
        for m in members_sorted[1:]:
            dup_files += 1
            dup_bytes += m["size_bytes"]

    vision_targets: list[dict] = []
    for a in unique_assets:
        if a["ext"] not in IMAGE_EXTS:
            continue
        stem = a["id"].rsplit("/", 1)[-1]
        is_gib = is_gibberish_stem(stem)
        is_uncat_root = (
            a["category"] in ("backgrounds", "objects")
            and not a.get("subcategory")
        )
        if is_gib or is_uncat_root:
            vision_targets.append({
                "sha256": a["sha256"],
                "path": a["path"],
                "old_category": a["category"],
                "old_subcategory": a["subcategory"],
                "old_stem": stem,
                "reason": "gibberish" if is_gib else "uncategorized_root",
            })

    audit = {
        "total_input": len(assets),
        "missing": missing,
        "unique_count": len(unique_assets),
        "duplicate_count": dup_files,
        "duplicate_bytes": dup_bytes,
        "unique_assets": unique_assets,
        "vision_targets": vision_targets,
    }
    AUDIT_PATH.parent.mkdir(parents=True, exist_ok=True)
    AUDIT_PATH.write_text(json.dumps(audit, ensure_ascii=False, indent=2), encoding="utf-8")

    sheets_needed = (len(vision_targets) + 24) // 25
    print(f"\n=== Audit Summary ===")
    print(f"  Input files       : {len(assets)}")
    print(f"  Exact duplicates  : {dup_files} ({dup_bytes / (1024**3):.2f} GB reclaimable)")
    print(f"  Unique assets     : {len(unique_assets)}")
    print(f"  Vision targets    : {len(vision_targets)} (~{sheets_needed} contact sheets of 25)")
    print(f"  By old category   : {dict(Counter(v['old_category'] for v in vision_targets))}")
    print(f"  By reason         : {dict(Counter(v['reason'] for v in vision_targets))}")
    print(f"Saved audit to {AUDIT_PATH}")
    return 0


def load_dotenv_key(key_name: str = "GEMINI_API_KEY") -> str:
    val = os.environ.get(key_name, "").strip()
    if val:
        return val
    env_path = REPO_ROOT / ".env"
    if env_path.exists():
        for line in env_path.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            if k.strip() == key_name:
                return v.strip().strip('"').strip("'")
    raise RuntimeError(f"{key_name} not found in environment or .env")


def build_contact_sheet(batch: list[dict], cell_size: int = 224, cols: int = 5) -> bytes:
    """Render up to 25 labeled thumbnails onto a single JPEG contact sheet."""
    rows = (len(batch) + cols - 1) // cols
    header_h = 26
    pad = 6
    tile_w = cell_size + pad * 2
    tile_h = cell_size + header_h + pad * 2
    sheet = Image.new("RGB", (cols * tile_w, rows * tile_h), (28, 30, 34))
    draw = ImageDraw.Draw(sheet)

    try:
        font = ImageFont.truetype("arial.ttf", 18)
    except Exception:
        font = ImageFont.load_default()

    # Subtle checkerboard tile so transparent sprites stand out cleanly.
    checker = Image.new("RGB", (cell_size, cell_size), (195, 198, 204))
    cdraw = ImageDraw.Draw(checker)
    sq = 16
    for y in range(0, cell_size, sq):
        for x in range(0, cell_size, sq):
            if ((x // sq) + (y // sq)) % 2 == 1:
                cdraw.rectangle([x, y, x + sq - 1, y + sq - 1], fill=(162, 166, 174))

    for i, item in enumerate(batch):
        r, c = divmod(i, cols)
        ox = c * tile_w + pad
        oy = r * tile_h + pad

        # Label bar
        draw.rectangle([ox, oy, ox + cell_size, oy + header_h], fill=(18, 20, 24))
        hint = item["old_category"][:4]
        draw.text((ox + 6, oy + 4), f"{i + 1:02d} [{hint}]", fill=(255, 224, 102), font=font)

        # Paste thumbnail onto checkerboard
        cell_bg = checker.copy()
        img_path = REPO_ROOT / "assets" / item["path"]
        try:
            with Image.open(img_path) as im:
                im = im.convert("RGBA")
                im.thumbnail((cell_size, cell_size), Image.Resampling.LANCZOS)
                px = (cell_size - im.width) // 2
                py = (cell_size - im.height) // 2
                cell_bg.paste(im, (px, py), im)
        except Exception:
            edraw = ImageDraw.Draw(cell_bg)
            edraw.text((10, cell_size // 2), "UNREADABLE", fill=(200, 30, 30), font=font)

        sheet.paste(cell_bg, (ox, oy + header_h))

    buf = io.BytesIO()
    sheet.save(buf, format="JPEG", quality=86)
    return buf.getvalue()


VISION_PROMPT = """You are organizing a 2D top-down fantasy RPG tabletop game asset library.
This contact sheet contains {count} numbered thumbnails (01 to {count:02d}).
For EACH numbered tile from 1 to {count}, identify what the image depicts and return a JSON array of exactly {count} objects with:
- "idx": integer 1..{count}
- "name": concise, descriptive lowercase snake_case filename stem (2 to 5 words, NO file extension, NO numbers like 01 unless intrinsic; e.g. "mossy_cobblestone_floor", "wooden_tavern_table", "stone_arch_bridge", "forest_clearing_battlemap", "cracked_basalt_texture")
- "category": one of ["textures", "nature", "structures", "props", "overlays", "markers", "maps"]
  * "textures": seamless/tileable ground, floor, wall, or material surfaces
  * "nature": trees, bushes, rocks, plants, caves, water features
  * "structures": buildings, roofs, walls, doors, bridges, stairs, tents, ruins, ships, wagons, altars, fountains, statues, forges
  * "props": furniture, crates/barrels, food, books, weapons, armor, lights, traps, corpses, animals, small items
  * "overlays": transparent decals, shadows, blood, fire/magic/lighting effects, roads, spell templates
  * "markers": world/regional map POI icons
  * "maps": full playable top-down battlemaps / rooms / dungeons
- "subcategory": short clean lowercase path inside the category (e.g. "terrain/grass", "terrain/rock", "floors/stone", "floors/wood", "floors/marble", "floors/tile", "walls", "plants", "trees", "rocks", "buildings", "bridges", "doors", "ruins", "furniture/tables", "furniture/beds", "furniture/cabinets", "containers", "dungeon/lights", "ground", "effects", or "" for maps/markers)
- "tags": array of 2 to 4 short lowercase keywords

Return ONLY valid JSON: an array of {count} objects."""


def call_gemini_sheet(
    jpeg_bytes: bytes,
    count: int,
    api_key: str,
    model: str,
    max_retries: int = 5,
) -> list[dict]:
    url = f"https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}"
    payload = {
        "contents": [
            {
                "parts": [
                    {"text": VISION_PROMPT.format(count=count)},
                    {
                        "inline_data": {
                            "mime_type": "image/jpeg",
                            "data": base64.b64encode(jpeg_bytes).decode("ascii"),
                        }
                    },
                ]
            }
        ],
        "generationConfig": {
            "temperature": 0.2,
            "responseMimeType": "application/json",
        },
    }
    body = json.dumps(payload).encode("utf-8")

    for attempt in range(1, max_retries + 1):
        req = urllib.request.Request(
            url,
            data=body,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=120) as resp:
                data = json.loads(resp.read().decode("utf-8"))
            text = data["candidates"][0]["content"]["parts"][0]["text"]
            parsed = json.loads(text)
            if isinstance(parsed, dict) and "items" in parsed:
                parsed = parsed["items"]
            if isinstance(parsed, list):
                return parsed
            raise ValueError(f"Unexpected JSON type: {type(parsed)}")
        except urllib.error.HTTPError as exc:
            err_body = exc.read().decode("utf-8", errors="replace")[:300]
            if exc.code in (400, 401, 403, 404) or attempt == max_retries:
                raise RuntimeError(f"HTTP {exc.code}: {err_body}") from exc
            wait = min(4.0 * (2 ** (attempt - 1)), 45.0)
            time.sleep(wait)
        except Exception as exc:
            wait = min(3.0 * (2 ** (attempt - 1)), 30.0)
            if attempt == max_retries:
                raise RuntimeError(f"Gemini call failed: {exc}") from exc
            time.sleep(wait)
    return []


def cmd_label_vision(args: argparse.Namespace) -> int:
    if not AUDIT_PATH.exists():
        print("Audit file missing; run `audit` first.", file=sys.stderr)
        return 1

    audit = json.loads(AUDIT_PATH.read_text(encoding="utf-8"))
    targets: list[dict] = audit["vision_targets"]
    api_key = load_dotenv_key("GEMINI_API_KEY")

    cache: dict[str, dict] = {}
    if VISION_CACHE_PATH.exists():
        cache = json.loads(VISION_CACHE_PATH.read_text(encoding="utf-8"))

    pending = [t for t in targets if t["sha256"] not in cache]
    batch_size = args.batch_size
    batches = [pending[i : i + batch_size] for i in range(0, len(pending), batch_size)]

    print(
        f"Vision labeling: total={len(targets)} cached={len(cache)} pending={len(pending)} "
        f"({len(batches)} contact sheets of {batch_size}, model={args.model}, workers={args.workers})",
        flush=True,
    )
    if not batches:
        print("All vision targets already labeled!")
        return 0

    t0 = time.monotonic()
    failed_sheets = 0

    def process_batch(sheet_idx: int, batch: list[dict]) -> tuple[int, list[dict], list[dict], str]:
        try:
            jpeg = build_contact_sheet(batch)
            results = call_gemini_sheet(jpeg, len(batch), api_key, args.model)
            return sheet_idx, batch, results, ""
        except Exception as exc:
            return sheet_idx, batch, [], str(exc)

    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        futs = [pool.submit(process_batch, idx, b) for idx, b in enumerate(batches, 1)]
        done_sheets = 0
        for fut in as_completed(futs):
            sheet_idx, batch, results, err = fut.result()
            done_sheets += 1
            if err:
                failed_sheets += 1
                print(f"  [sheet {sheet_idx}/{len(batches)}] ERROR: {err}", file=sys.stderr, flush=True)
                continue

            by_idx = {}
            for r in results:
                if isinstance(r, dict) and "idx" in r:
                    try:
                        by_idx[int(r["idx"])] = r
                    except (TypeError, ValueError):
                        pass

            matched = 0
            for i, item in enumerate(batch, 1):
                res = by_idx.get(i)
                if not res:
                    continue
                name = slugify(str(res.get("name") or ""))
                cat = str(res.get("category") or "").strip().lower()
                if cat not in VALID_VISION_CATEGORIES:
                    cat = item["old_category"] if item["old_category"] in VALID_VISION_CATEGORIES else "props"
                raw_sub = str(res.get("subcategory") or "").strip().strip("/")
                sub = "/".join(slugify(p, max_len=32) for p in raw_sub.split("/") if p)
                raw_tags = res.get("tags") or []
                tags = [slugify(str(t), max_len=24) for t in raw_tags if str(t).strip()][:5]
                cache[item["sha256"]] = {
                    "name": name,
                    "category": cat,
                    "subcategory": sub,
                    "tags": tags,
                }
                matched += 1

            # Checkpoint after every completed sheet.
            tmp = VISION_CACHE_PATH.with_suffix(".tmp")
            tmp.write_text(json.dumps(cache, ensure_ascii=False, indent=2), encoding="utf-8")
            os.replace(tmp, VISION_CACHE_PATH)

            elapsed = time.monotonic() - t0
            print(
                f"  [sheet {done_sheets}/{len(batches)}] matched={matched}/{len(batch)} "
                f"total_labeled={len(cache)}/{len(targets)} ({elapsed:.1f}s)",
                flush=True,
            )

    print(
        f"\nVision labeling finished: {len(cache)}/{len(targets)} labeled, "
        f"failed_sheets={failed_sheets} -> {VISION_CACHE_PATH}",
        flush=True,
    )
    return 1 if failed_sheets else 0


def cmd_apply(args: argparse.Namespace) -> int:
    if not AUDIT_PATH.exists():
        print("Audit file missing; run `audit` first.", file=sys.stderr)
        return 1

    audit = json.loads(AUDIT_PATH.read_text(encoding="utf-8"))
    unique_assets: list[dict] = audit["unique_assets"]
    vision_cache: dict[str, dict] = (
        json.loads(VISION_CACHE_PATH.read_text(encoding="utf-8"))
        if VISION_CACHE_PATH.exists()
        else {}
    )

    used_rel_paths: dict[str, int] = {}
    moves: list[dict] = []
    deletes: list[Path] = []
    vision_renamed = 0

    for a in sorted(unique_assets, key=lambda x: (x["category"], x["subcategory"], x["id"])):
        src_path = REPO_ROOT / "assets" / a["path"]
        if not src_path.exists():
            continue

        ext = a["ext"]
        old_stem = a["id"].rsplit("/", 1)[-1]
        v = vision_cache.get(a["sha256"])

        if v and v.get("name") and v["name"] != "unnamed":
            new_cat = v["category"]
            new_sub = v.get("subcategory", "")
            stem = slugify(v["name"])
            tags = v.get("tags", [])
            vision_renamed += 1
        else:
            new_cat, new_sub = map_category(a["category"], a.get("subcategory", ""), old_stem)
            stem = clean_descriptive_stem(old_stem)
            # Derive lightweight tags from category + subcategory path segments.
            tags = [p for p in [new_cat, *new_sub.split("/")] if p and p != "misc"][:5]

        # Collapse consecutive duplicate segments in subcategory.
        sub_parts: list[str] = []
        for p in [x for x in new_sub.split("/") if x]:
            s = slugify(p, max_len=40)
            if sub_parts and sub_parts[-1] == s:
                continue
            sub_parts.append(s)
        dir_parts = [new_cat] + sub_parts

        rel_base = "/".join(dir_parts + [f"{stem}{ext}"])
        if len(str(LIB_DIR / rel_base)) > 235:
            digest = hashlib.sha1(rel_base.encode("utf-8")).hexdigest()[:8]
            budget = max(16, 235 - len(str(LIB_DIR)) - len("/".join(dir_parts)) - len(ext) - 12)
            stem = f"{stem[:budget].rstrip('_')}_{digest}"
            rel_base = "/".join(dir_parts + [f"{stem}{ext}"])

        if rel_base in used_rel_paths:
            used_rel_paths[rel_base] += 1
            n = used_rel_paths[rel_base]
            rel_path = "/".join(dir_parts + [f"{stem}_{n}{ext}"])
            while rel_path in used_rel_paths:
                n += 1
                rel_path = "/".join(dir_parts + [f"{stem}_{n}{ext}"])
            used_rel_paths[rel_path] = 1
        else:
            used_rel_paths[rel_base] = 1
            rel_path = rel_base

        dest_path = LIB_DIR / rel_path
        moves.append({
            "src": src_path,
            "dest": dest_path,
            "id": rel_path.rsplit(".", 1)[0],
            "category": new_cat,
            "subcategory": "/".join(sub_parts),
            "path": f"library/{rel_path}",
            "source_path": a["source_path"],
            "sha256": a["sha256"],
            "size_bytes": a["size_bytes"],
            "ext": ext,
            "tags": tags,
        })

        for dup_rel in a.get("duplicate_paths", []):
            dp = REPO_ROOT / "assets" / dup_rel
            if dp.exists():
                deletes.append(dp)

    by_cat = Counter(m["category"] for m in moves)
    by_cat_bytes = Counter()
    for m in moves:
        by_cat_bytes[m["category"]] += m["size_bytes"]

    print("\n=== Unified Reorganization Plan ===")
    for c in sorted(by_cat):
        print(f"  {c:14s}: {by_cat[c]:5d} files  ({by_cat_bytes[c] / (1024**2):7.1f} MB)")
    print(f"  {'TOTAL UNIQUE':14s}: {len(moves):5d} files  ({sum(by_cat_bytes.values()) / (1024**3):.2f} GB)")
    print(f"  Vision renamed: {vision_renamed}")
    print(f"  Dups removed  : {len(deletes)}")

    if args.dry_run:
        print("\nSample vision renames:")
        shown = 0
        for m in moves:
            if m["sha256"] in vision_cache and shown < 15:
                shown += 1
                print(f"  {m['source_path']}  -->  assets/{m['path']}")
        return 0

    t0 = time.monotonic()
    # 1. Remove exact duplicate copies first (frees paths & disk space).
    removed = 0
    for dp in deletes:
        if dp.exists():
            dp.unlink(missing_ok=True)
            removed += 1
    print(f"Removed {removed} exact duplicate files.", flush=True)

    # 2. Move winners into a staging sibling folder then swap, OR two-phase safe rename
    #    (two-phase avoids case-insensitive Windows collisions when src == dest or src overlaps another dest).
    staging_dir = REPO_ROOT / "assets" / "_library_staging"
    if staging_dir.exists():
        print(f"Staging dir already exists: {staging_dir}", file=sys.stderr)
        return 1
    staging_dir.mkdir(parents=True, exist_ok=True)

    moved = 0
    for m in moves:
        src: Path = m["src"]
        rel_inside = m["path"].removeprefix("library/")
        stage_dest = staging_dir / rel_inside
        stage_dest.parent.mkdir(parents=True, exist_ok=True)
        os.replace(src, stage_dest)
        moved += 1
        if moved % 5000 == 0 or moved == len(moves):
            print(f"  staged {moved}/{len(moves)} ({time.monotonic() - t0:.1f}s)", flush=True)

    # 3. Clean up empty old `assets/library/` tree and swap staging into `assets/library/`.
    #    Remove old manifest before rmtree/rmdir.
    if MANIFEST_PATH.exists():
        MANIFEST_PATH.unlink()
    leftover_files = [p for p in LIB_DIR.rglob("*") if p.is_file()]
    if leftover_files:
        print(f"WARNING: {len(leftover_files)} unexpected files left in {LIB_DIR}; keeping in _untracked_leftovers/")
        leftover_dir = staging_dir / "_leftovers"
        leftover_dir.mkdir(parents=True, exist_ok=True)
        for lf in leftover_files:
            os.replace(lf, leftover_dir / lf.name)

    # Remove now-empty directory tree under LIB_DIR
    for root, dirs, _ in os.walk(LIB_DIR, topdown=False):
        for d in dirs:
            try:
                Path(root, d).rmdir()
            except OSError:
                pass
    try:
        LIB_DIR.rmdir()
    except OSError:
        pass
    os.replace(staging_dir, LIB_DIR)

    # 4. Write upgraded manifest.json
    manifest = {
        "version": 2,
        "count": len(moves),
        "total_bytes": sum(m["size_bytes"] for m in moves),
        "categories": dict(sorted(by_cat.items())),
        "assets": [
            {
                "id": m["id"],
                "category": m["category"],
                "subcategory": m["subcategory"],
                "path": m["path"],
                "tags": m["tags"],
                "source_path": m["source_path"],
                "sha256": m["sha256"],
                "size_bytes": m["size_bytes"],
                "ext": m["ext"],
            }
            for m in moves
        ],
    }
    tmp = MANIFEST_PATH.with_suffix(".tmp")
    tmp.write_text(json.dumps(manifest, ensure_ascii=False, indent=2), encoding="utf-8")
    os.replace(tmp, MANIFEST_PATH)

    print(
        f"\nReorganization complete in {time.monotonic() - t0:.1f}s! "
        f"{len(moves)} unique assets organized under {LIB_DIR}",
        flush=True,
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Audit, vision-rename, and reorganize assets/library/.")
    sub = parser.add_subparsers(dest="cmd", required=True)

    sub.add_parser("audit", help="Hash all files, find duplicates, and collect vision targets.")

    p_vis = sub.add_parser("label-vision", help="Batch-label gibberish/uncategorized images with Gemini Vision.")
    p_vis.add_argument("--batch-size", type=int, default=25)
    p_vis.add_argument("--workers", type=int, default=2)
    p_vis.add_argument("--model", default="gemini-3.5-flash-lite")

    p_app = sub.add_parser("apply", help="Deduplicate, rename, and reorganize assets/library/.")
    p_app.add_argument("--dry-run", action="store_true")

    args = parser.parse_args()
    if args.cmd == "audit":
        return cmd_audit(args)
    if args.cmd == "label-vision":
        return cmd_label_vision(args)
    if args.cmd == "apply":
        return cmd_apply(args)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
