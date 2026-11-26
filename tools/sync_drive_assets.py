"""Resumable Google Drive asset indexer, local zip importer, and manifest builder for Zeta.

Organizes the shared MapTool visual asset library into `assets/library/` with clean
snake_case categories (`backgrounds`, `textures`, `objects`, `tiles`, `markers`, `maps`),
stripping MapTool macros (`5e_SyrikProps_12_13_20`), `_preview.png` thumbnails, and
non-visual files.

Usage:
    python tools/sync_drive_assets.py import-zips "maptools*.zip"
    python tools/sync_drive_assets.py import-zips "maptools*.zip" --dry-run
    python tools/sync_drive_assets.py plan
    python tools/sync_drive_assets.py manifest
"""

from __future__ import annotations

import argparse
from collections import Counter, defaultdict
import glob
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sys
import time
import zipfile

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_ROOT_ID = "1ZR49UXamOQmBIHEu8uh1PdgZWSImczpV"
DEFAULT_INDEX_PATH = REPO_ROOT / "target" / "drive_index.json"
DEFAULT_BATCH_PATH = REPO_ROOT / "target" / "drive_batch.jsonl"
DEFAULT_OUT_DIR = REPO_ROOT / "assets" / "library"

FOLDER_MIME = "application/vnd.google-apps.folder"
SKIP_TOP_FOLDERS = {"5e_SyrikProps_12_13_20"}

ROOT_SEED_FOLDERS = [
    {"id": "13S3REJiY5oc0i6gxsLugrf_iPv5fWq1a", "path": ["Maptool Textures"]},
    {"id": "1s1RF0DZZ3Ku4qZbLsDGdOHT_xzRT-tmp", "path": ["Maptool Objects"]},
    {"id": "1V7wlsnHRv26Pm-7XgnqwWpq6LQFwbZBw", "path": ["592260"]},
    {"id": "1JpheTs_AmUDuJyelDxXMKrluiTCycl6T", "path": ["Maptool Backgrounds"]},
    {"id": "1PB3aHC8KQ0XcOT830PB1YAj15mh_SmPN", "path": ["Maptool Markers"]},
    {"id": "1FzTmn7ig1THwXza9Murm_hzFWhny73v_", "path": ["maps"]},
]

IMAGE_EXTS = {".png", ".jpg", ".jpeg", ".webp", ".bmp", ".tga", ".gif"}
MAP_EXTRA_EXTS = {".dd2vtt", ".uvtt"}

TOP_CATEGORY_MAP = {
    "Maptool Textures": "textures",
    "Maptool Objects": "objects",
    "Maptool Backgrounds": "backgrounds",
    "Maptool Markers": "markers",
    "592260": "tiles",
    "maps": "maps",
}

FOLDER_TYPO_FIXES = {
    "cyrstals_and_ore": "crystals_and_ore",
    "enviroment": "environment",
    "enviroments": "environments",
}

MAX_STEM_LEN = 80


def slugify(segment: str, max_len: int = MAX_STEM_LEN) -> str:
    """Convert a folder or file stem to clean lowercase snake_case."""
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


def is_preview_or_junk(name: str) -> bool:
    low = name.strip().lower()
    if low in {"thumbs.db", ".ds_store", "desktop.ini"}:
        return True
    stem = Path(low).stem
    if stem == "_preview" or stem == "preview" or stem.endswith("_preview"):
        return True
    return False


def fmt_bytes(n: int) -> str:
    val = float(n)
    for unit in ("B", "KB", "MB", "GB"):
        if val < 1024.0 or unit == "GB":
            return f"{val:.1f} {unit}"
        val /= 1024.0
    return f"{n} B"


def load_index(path: Path) -> dict:
    if path.exists():
        return json.loads(path.read_text(encoding="utf-8"))
    return {
        "root_id": DEFAULT_ROOT_ID,
        "listed_folders": {
            DEFAULT_ROOT_ID: {"path": [], "child_count": len(ROOT_SEED_FOLDERS)}
        },
        "pending_folders": [dict(x) for x in ROOT_SEED_FOLDERS],
        "files": [],
        "complete": False,
    }


def save_index(path: Path, state: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(state, ensure_ascii=False, indent=2), encoding="utf-8")
    os.replace(tmp, path)


def write_batch(batch_path: Path, pending: list[dict], batch_size: int) -> int:
    batch_path.parent.mkdir(parents=True, exist_ok=True)
    chunk = pending[:batch_size]
    batch_path.write_text(
        "".join(json.dumps({"id": item["id"]}) + "\n" for item in chunk),
        encoding="utf-8",
    )
    return len(chunk)


def cmd_init(args: argparse.Namespace) -> int:
    index_path = Path(args.index)
    state = {
        "root_id": DEFAULT_ROOT_ID,
        "listed_folders": {
            DEFAULT_ROOT_ID: {"path": [], "child_count": len(ROOT_SEED_FOLDERS)}
        },
        "pending_folders": [dict(x) for x in ROOT_SEED_FOLDERS],
        "files": [],
        "complete": False,
    }
    save_index(index_path, state)
    n = write_batch(Path(args.batch), state["pending_folders"], args.batch_size)
    print(f"Initialized index with {n} seed folders -> {args.batch}")
    return 0


def cmd_ingest(args: argparse.Namespace) -> int:
    index_path = Path(args.index)
    state = load_index(index_path)

    listed: dict[str, dict] = state["listed_folders"]
    pending_by_id: dict[str, dict] = {p["id"]: p for p in state["pending_folders"]}
    files: list[dict] = state["files"]
    seen_file_ids = {f["id"] for f in files}

    out_path = Path(args.jsonl)
    if not out_path.exists():
        print(f"JSONL output file not found: {out_path}", file=sys.stderr)
        return 1

    ok_count = 0
    err_count = 0
    added_dirs = 0
    added_files = 0
    capped_folders = []
    sample_errors = []

    for line in out_path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        fid = obj.get("id") or obj.get("target")
        if not fid or fid not in pending_by_id:
            continue
        if not obj.get("_ok"):
            err_count += 1
            if len(sample_errors) < 3:
                sample_errors.append(str(obj.get("error") or obj))
            continue

        entry = pending_by_id.pop(fid)
        data = obj.get("data") or []
        if len(data) >= 10000:
            capped_folders.append(entry["path"])

        ok_count += 1
        listed[fid] = {
            "path": entry["path"],
            "child_count": len(data),
            "has_preview": any(
                is_preview_or_junk(x.get("name", ""))
                and x.get("mimeType") != FOLDER_MIME
                for x in data
            ),
        }

        for child in data:
            cid = child["id"]
            cname = child.get("name", "")
            if child.get("mimeType") == FOLDER_MIME:
                if cid not in listed and cid not in pending_by_id:
                    pending_by_id[cid] = {"id": cid, "path": entry["path"] + [cname]}
                    added_dirs += 1
            else:
                if cid not in seen_file_ids:
                    seen_file_ids.add(cid)
                    files.append({
                        "id": cid,
                        "name": cname,
                        "mimeType": child.get("mimeType", ""),
                        "size": int(child.get("size") or 0),
                        "modifiedTime": child.get("modifiedTime", ""),
                        "folder_path": entry["path"],
                        "parent_id": fid,
                    })
                    added_files += 1

    remaining = list(pending_by_id.values())
    state["pending_folders"] = remaining
    state["complete"] = len(remaining) == 0
    save_index(index_path, state)
    next_n = write_batch(Path(args.batch), remaining, args.batch_size)

    print(
        f"Ingested: ok={ok_count} err={err_count} (+{added_files} files, +{added_dirs} dirs) | "
        f"Total listed={len(listed)} files={len(files)} remaining_folders={len(remaining)} next_batch={next_n}"
    )
    if sample_errors:
        print("Sample errors:")
        for e in sample_errors:
            print(" ", e[:240])
    if capped_folders:
        print("WARNING - folders hit 10000 cap:", capped_folders)
    if state["complete"]:
        print_plan_summary(state, Path(args.out))
    return 0


def build_plan(state: dict, out_dir: Path) -> tuple[list[dict], list[dict]]:
    """Build organized destination paths for every visual asset in `state`."""
    listed = state.get("listed_folders", {})
    files = state.get("files", [])

    kept_raw: list[dict] = []
    skipped: list[dict] = []
    files_per_parent: dict[str, list[dict]] = defaultdict(list)

    for f in files:
        name = f["name"]
        folder_path = f.get("folder_path", [])
        top = folder_path[0] if folder_path else ""
        if top in SKIP_TOP_FOLDERS:
            skipped.append({**f, "reason": f"skipped_folder:{top}"})
            continue
        cat = TOP_CATEGORY_MAP.get(top)
        if not cat:
            skipped.append({**f, "reason": f"unmapped_top:{top}"})
            continue
        if is_preview_or_junk(name):
            skipped.append({**f, "reason": "preview_or_junk"})
            continue
        ext = Path(name).suffix.lower()
        allowed = IMAGE_EXTS | (MAP_EXTRA_EXTS if cat == "maps" else set())
        if ext not in allowed:
            skipped.append({**f, "reason": f"ext:{ext or '(none)'}"})
            continue

        entry = {**f, "category": cat, "ext": ext}
        kept_raw.append(entry)
        files_per_parent[f["parent_id"]].append(entry)

    folder_has_subdirs: set[tuple[str, ...]] = set()
    for meta in listed.values():
        p = tuple(meta.get("path", []))
        if len(p) >= 2:
            folder_has_subdirs.add(p[:-1])

    used_rel_paths: dict[str, int] = {}
    planned: list[dict] = []

    for f in sorted(
        kept_raw,
        key=lambda x: (
            x["category"],
            [s.lower() for s in x["folder_path"]],
            x["name"].lower(),
            x["id"],
        ),
    ):
        cat = f["category"]
        ext = f["ext"]
        sub_folders = list(f["folder_path"][1:])
        stem = Path(f["name"]).stem

        # Strip numeric wrapper directory in `592260/893071869/...`.
        if cat == "tiles" and sub_folders and sub_folders[0].isdigit():
            sub_folders = sub_folders[1:]

        cleaned_folders: list[str] = []
        for seg in sub_folders:
            s = slugify(seg, max_len=48)
            s = FOLDER_TYPO_FIXES.get(s, s)
            # Collapse consecutive duplicate segments (e.g. `floors/floors`).
            if cleaned_folders and cleaned_folders[-1] == s:
                continue
            cleaned_folders.append(s)

        parent_meta = listed.get(f["parent_id"], {})
        parent_path_tuple = tuple(f["folder_path"])
        siblings = files_per_parent[f["parent_id"]]
        is_leaf = parent_path_tuple not in folder_has_subdirs
        should_flatten = (
            is_leaf
            and len(siblings) == 1
            and len(cleaned_folders) >= 1
            and (
                parent_meta.get("has_preview", False)
                or (cat == "objects" and len(cleaned_folders) >= 2)
            )
        )

        if should_flatten:
            file_stem = cleaned_folders[-1]
            dir_parts = [cat] + cleaned_folders[:-1]
        else:
            file_stem = slugify(stem)
            dir_parts = [cat] + cleaned_folders

        # Guard Windows MAX_PATH (260 chars): keep full destination path <= 240 chars.
        rel_base = "/".join(dir_parts + [f"{file_stem}{ext}"])
        if len(str(out_dir / rel_base)) > 235:
            digest = hashlib.sha1(rel_base.encode("utf-8")).hexdigest()[:8]
            budget = max(16, 235 - len(str(out_dir)) - len("/".join(dir_parts)) - len(ext) - 12)
            file_stem = f"{file_stem[:budget].rstrip('_')}_{digest}"
            rel_base = "/".join(dir_parts + [f"{file_stem}{ext}"])

        if rel_base in used_rel_paths:
            used_rel_paths[rel_base] += 1
            n = used_rel_paths[rel_base]
            rel_path = "/".join(dir_parts + [f"{file_stem}_{n}{ext}"])
            while rel_path in used_rel_paths:
                n += 1
                rel_path = "/".join(dir_parts + [f"{file_stem}_{n}{ext}"])
            used_rel_paths[rel_path] = 1
        else:
            used_rel_paths[rel_base] = 1
            rel_path = rel_base

        subcategory = "/".join(dir_parts[1:]) if len(dir_parts) > 1 else ""
        item = {
            "id": rel_path.rsplit(".", 1)[0],
            "drive_id": f.get("drive_id", f["id"]),
            "category": cat,
            "subcategory": subcategory,
            "rel_path": rel_path,
            "dest_path": str(out_dir / rel_path),
            "source_path": "/".join(f["folder_path"] + [f["name"]]),
            "size": f["size"],
            "ext": ext,
        }
        if "zip_path" in f:
            item["zip_path"] = f["zip_path"]
            item["zip_member"] = f["zip_member"]
        planned.append(item)

    return planned, skipped


def print_plan_summary(state: dict, out_dir: Path) -> None:
    planned, skipped = build_plan(state, out_dir)
    by_cat_count: Counter[str] = Counter()
    by_cat_bytes: Counter[str] = Counter()
    for item in planned:
        by_cat_count[item["category"]] += 1
        by_cat_bytes[item["category"]] += item["size"]

    total_files = len(planned)
    total_bytes = sum(by_cat_bytes.values())
    print("\n=== Organized Asset Plan ===")
    for cat in sorted(by_cat_count):
        print(
            f"  {cat:14s}: {by_cat_count[cat]:5d} files  ({fmt_bytes(by_cat_bytes[cat])})"
        )
    print(f"  {'TOTAL KEPT':14s}: {total_files:5d} files  ({fmt_bytes(total_bytes)})")

    skip_reasons = Counter(s["reason"] for s in skipped)
    if skip_reasons:
        print("\n=== Skipped Files ===")
        for reason, cnt in skip_reasons.most_common():
            print(f"  {reason:28s}: {cnt:5d}")

    max_path_len = max((len(p["dest_path"]) for p in planned), default=0)
    print(f"\nMax destination path length: {max_path_len} chars")

    print("\n=== Sample Mapped Paths ===")
    seen_cats: Counter[str] = Counter()
    for item in planned:
        if seen_cats[item["category"]] < 3:
            seen_cats[item["category"]] += 1
            print(f"  {item['source_path']}  -->  assets/library/{item['rel_path']}")


def state_from_zips(zip_paths: list[Path]) -> dict:
    """Scan local Google Drive zip archives and construct a `build_plan`-compatible state."""
    listed: dict[str, dict] = {}
    files: list[dict] = []
    seen_members: set[tuple[str, str]] = set()

    for zp in zip_paths:
        with zipfile.ZipFile(zp, "r") as zf:
            for info in zf.infolist():
                if info.is_dir():
                    continue
                raw = info.filename.replace("\\", "/")
                parts = [p for p in raw.split("/") if p]
                if not parts:
                    continue
                # Strip Drive's top-level `maptools ` wrapper folder if present.
                if parts[0].strip().lower() == "maptools":
                    parts = parts[1:]
                if len(parts) < 2:
                    continue

                folder_path = parts[:-1]
                name = parts[-1]
                parent_key = "/".join(folder_path)

                # Register all ancestor folders so `folder_has_subdirs` is exact.
                for depth in range(1, len(folder_path) + 1):
                    sub = folder_path[:depth]
                    key = "/".join(sub)
                    if key not in listed:
                        listed[key] = {"path": sub, "child_count": 0, "has_preview": False}

                listed[parent_key]["child_count"] += 1
                if is_preview_or_junk(name):
                    listed[parent_key]["has_preview"] = True

                member_sig = (parent_key, name)
                if member_sig in seen_members:
                    # Same relative path appeared in multiple split zips; keep first.
                    continue
                seen_members.add(member_sig)

                files.append({
                    "id": f"{zp.name}::{info.filename}",
                    "drive_id": "",
                    "name": name,
                    "size": info.file_size,
                    "folder_path": folder_path,
                    "parent_id": parent_key,
                    "zip_path": str(zp),
                    "zip_member": info.filename,
                })

    return {
        "root_id": DEFAULT_ROOT_ID,
        "listed_folders": listed,
        "pending_folders": [],
        "files": files,
        "complete": True,
    }


def write_manifest_from_planned(planned: list[dict], out_dir: Path) -> Path:
    entries = []
    for item in planned:
        dest = Path(item["dest_path"])
        if not dest.exists() or dest.stat().st_size == 0:
            continue
        entries.append({
            "id": item["id"],
            "category": item["category"],
            "subcategory": item["subcategory"],
            "path": f"library/{item['rel_path']}",
            "source_path": item["source_path"],
            "size_bytes": dest.stat().st_size,
            "ext": item["ext"],
        })
    by_category = dict(sorted(Counter(e["category"] for e in entries).items()))
    manifest = {
        "source_folder_id": DEFAULT_ROOT_ID,
        "count": len(entries),
        "total_bytes": sum(e["size_bytes"] for e in entries),
        "categories": by_category,
        "assets": entries,
    }
    out_dir.mkdir(parents=True, exist_ok=True)
    manifest_path = out_dir / "manifest.json"
    tmp = manifest_path.with_suffix(".tmp")
    tmp.write_text(json.dumps(manifest, ensure_ascii=False, indent=2), encoding="utf-8")
    os.replace(tmp, manifest_path)
    return manifest_path


def cmd_import_zips(args: argparse.Namespace) -> int:
    zip_paths: list[Path] = []
    for pattern in args.zips:
        matches = sorted(Path(p) for p in glob.glob(pattern))
        zip_paths.extend(matches)
    zip_paths = sorted(set(zip_paths))
    if not zip_paths:
        print(f"No zip files matched: {args.zips}", file=sys.stderr)
        return 1

    print(f"Scanning {len(zip_paths)} zip archive(s)...", flush=True)
    t0 = time.monotonic()
    state = state_from_zips(zip_paths)
    out_dir = Path(args.out)
    print_plan_summary(state, out_dir)
    print(f"Scanned in {time.monotonic() - t0:.1f}s.", flush=True)

    if args.dry_run:
        return 0

    planned, _ = build_plan(state, out_dir)
    # Group extractions by zip file so each archive is opened once.
    by_zip: dict[str, list[dict]] = defaultdict(list)
    for item in planned:
        by_zip[item["zip_path"]].append(item)

    total = len(planned)
    extracted = 0
    skipped_existing = 0
    errors = 0
    t_ext = time.monotonic()

    for zp_str in sorted(by_zip):
        items = by_zip[zp_str]
        print(f"\nExtracting from {Path(zp_str).name} ({len(items)} files)...", flush=True)
        with zipfile.ZipFile(zp_str, "r") as zf:
            for item in items:
                dest = Path(item["dest_path"])
                if dest.exists() and dest.stat().st_size == item["size"] and item["size"] > 0:
                    skipped_existing += 1
                    continue
                try:
                    dest.parent.mkdir(parents=True, exist_ok=True)
                    with zf.open(item["zip_member"], "r") as src, dest.open("wb") as dst:
                        shutil.copyfileobj(src, dst, length=1024 * 256)
                    extracted += 1
                except Exception as exc:
                    errors += 1
                    print(f"  ERROR {item['rel_path']}: {exc}", file=sys.stderr)

                done = extracted + skipped_existing + errors
                if done % 2500 == 0 or done == total:
                    elapsed = time.monotonic() - t_ext
                    print(
                        f"  [{done}/{total}] extracted={extracted} skipped={skipped_existing} "
                        f"errors={errors} ({elapsed:.1f}s)",
                        flush=True,
                    )

    manifest_path = write_manifest_from_planned(planned, out_dir)
    elapsed = time.monotonic() - t_ext
    print(
        f"\nImport complete in {elapsed:.1f}s: extracted={extracted} "
        f"skipped={skipped_existing} errors={errors}",
        flush=True,
    )
    print(f"Manifest written: {manifest_path}", flush=True)
    return 1 if errors else 0


def cmd_plan(args: argparse.Namespace) -> int:
    state = load_index(Path(args.index))
    print(
        f"Index complete={state.get('complete')} folders={len(state.get('listed_folders', {}))} "
        f"pending={len(state.get('pending_folders', []))} raw_files={len(state.get('files', []))}"
    )
    print_plan_summary(state, Path(args.out))
    return 0


def cmd_manifest(args: argparse.Namespace) -> int:
    state = load_index(Path(args.index))
    out_dir = Path(args.out)
    planned, _ = build_plan(state, out_dir)
    manifest_path = write_manifest_from_planned(planned, out_dir)
    print(f"Wrote {manifest_path}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Drive asset indexer, zip importer, and manifest tool.")
    parser.add_argument("--index", default=str(DEFAULT_INDEX_PATH))
    parser.add_argument("--batch", default=str(DEFAULT_BATCH_PATH))
    parser.add_argument("--out", default=str(DEFAULT_OUT_DIR))

    sub = parser.add_subparsers(dest="cmd", required=True)

    p_init = sub.add_parser("init", help="Reset Drive index and emit level-1 batch JSONL.")
    p_init.add_argument("--batch-size", type=int, default=120)

    p_ing = sub.add_parser("ingest", help="Ingest a `gdrive readonly stream ls` output JSONL and emit next batch.")
    p_ing.add_argument("jsonl")
    p_ing.add_argument("--batch-size", type=int, default=120)

    sub.add_parser("plan", help="Print organized plan summary from the Drive index.")

    p_imp = sub.add_parser("import-zips", help="Extract and organize local Google Drive zip archives into assets/library/.")
    p_imp.add_argument("zips", nargs="+", help="Zip file paths or glob patterns (e.g. 'maptools*.zip').")
    p_imp.add_argument("--dry-run", action="store_true", help="Print plan without extracting files.")

    sub.add_parser("manifest", help="Build assets/library/manifest.json from downloaded files.")

    args = parser.parse_args()
    if args.cmd == "init":
        return cmd_init(args)
    if args.cmd == "ingest":
        return cmd_ingest(args)
    if args.cmd == "plan":
        return cmd_plan(args)
    if args.cmd == "import-zips":
        return cmd_import_zips(args)
    if args.cmd == "manifest":
        return cmd_manifest(args)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
