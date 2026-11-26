"""Fetch warm/cozy OFL fonts, rasterize monochrome Game-Icons SVGs to PNGs, tag art_style in
manifest.json, and render a 4-option font pairing comparison sheet on the real UI panels.

Usage:
    python tools/build_ui_assets.py
"""

from __future__ import annotations

from collections import Counter
import json
import math
import os
from pathlib import Path
import re
import shutil
import sys
import urllib.request
import xml.etree.ElementTree as ET

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.patches import PathPatch
from matplotlib.path import Path as MplPath
import numpy as np
from PIL import Image, ImageDraw, ImageFont

REPO_ROOT = Path(__file__).resolve().parent.parent
LIB_DIR = REPO_ROOT / "assets" / "library"
MANIFEST_PATH = LIB_DIR / "manifest.json"
LIB_FONTS_DIR = LIB_DIR / "fonts"
TRACKED_FONTS_DIR = REPO_ROOT / "assets" / "fonts"
TRACKED_ICONS_DIR = REPO_ROOT / "assets" / "ui" / "icons"
SVG_DIR = LIB_DIR / "icons" / "rpg_svg"

UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) ZetaUiBuilder/1.0"

# Warm, cozy, slightly stylized SIL OFL 1.1 Google Fonts.
WARM_FONTS = [
    ("fraunces.ttf", "https://raw.githubusercontent.com/google/fonts/main/ofl/fraunces/Fraunces%5BSOFT%2CWONK%2Copsz%2Cwght%5D.ttf"),
    ("fredoka.ttf", "https://raw.githubusercontent.com/google/fonts/main/ofl/fredoka/Fredoka%5Bwdth%2Cwght%5D.ttf"),
    ("alegreya.ttf", "https://raw.githubusercontent.com/google/fonts/main/ofl/alegreya/Alegreya%5Bwght%5D.ttf"),
    ("alegreya_sans.ttf", "https://raw.githubusercontent.com/google/fonts/main/ofl/alegreyasans/AlegreyaSans-Regular.ttf"),
    ("bitter.ttf", "https://raw.githubusercontent.com/google/fonts/main/ofl/bitter/Bitter%5Bwght%5D.ttf"),
    ("patrick_hand.ttf", "https://raw.githubusercontent.com/google/fonts/main/ofl/patrickhand/PatrickHand-Regular.ttf"),
    ("nunito.ttf", "https://raw.githubusercontent.com/google/fonts/main/ofl/nunito/Nunito%5Bwght%5D.ttf"),
    ("lora.ttf", "https://raw.githubusercontent.com/google/fonts/main/ofl/lora/Lora%5Bwght%5D.ttf"),
    ("quicksand.ttf", "https://raw.githubusercontent.com/google/fonts/main/ofl/quicksand/Quicksand%5Bwght%5D.ttf"),
]

# Curated monochrome RPG icons to rasterize from Game-Icons.net SVGs into `assets/ui/icons/`.
# Each entry maps output PNG stem -> ordered candidate SVG stems in `assets/library/icons/rpg_svg/`.
CURATED_ICONS: dict[str, list[str]] = {
    "weapon": ["broadsword", "sword_brandish", "gladius"],
    "armor": ["bordered_shield", "round_shield", "shield"],
    "tool": ["wrench", "hammer_nails", "tools"],
    "gear": ["backpack", "knapsack", "satchel"],
    "d20": ["dice_twenty_faces_twenty", "dice_twenty_faces_one", "perspective_dice_six_faces_random"],
    "longsword": ["broadsword", "longsword"],
    "shield": ["bordered_shield", "round_shield"],
    "leather_armor": ["leather_armor", "leather_vest", "breastplate"],
    "torch": ["torch", "wall_sconce"],
    "rope": ["rope_coil", "lasso"],
    "rations": ["bread", "meat", "cheese_wedge"],
    "thieves_tools": ["lockpicks", "skeleton_key", "key"],
    "healers_kit": ["medical_pack", "bandages", "first_aid_kit"],
    "painters_supplies": ["paint_brush", "palette", "quill_ink"],
    "potion": ["potion_ball", "health_potion", "round_bottom_flask"],
    "scroll": ["scroll_unfurled", "tied_scroll"],
    "bow": ["bow_arrow", "pocket_bow"],
    "dagger": ["plain_dagger", "bowie_knife", "dagger_rose"],
    "axe": ["battle_axe", "wood_axe"],
    "helmet": ["visored_helm", "barbute"],
    "coin": ["coins", "two_coins"],
}


def download_file(url: str, dest: Path) -> bool:
    dest.parent.mkdir(parents=True, exist_ok=True)
    if dest.exists() and dest.stat().st_size > 0:
        return True
    part = dest.with_name(dest.name + ".part")
    try:
        req = urllib.request.Request(url, headers={"User-Agent": UA})
        with urllib.request.urlopen(req, timeout=60) as resp, part.open("wb") as out:
            shutil.copyfileobj(resp, out, length=1024 * 128)
        os.replace(part, dest)
        return True
    except Exception as exc:
        part.unlink(missing_ok=True)
        print(f"  WARN font download failed ({dest.name}): {exc}", file=sys.stderr)
        return False


def fetch_warm_fonts() -> None:
    print("[1/4] Downloading warm/cozy Google Fonts (SIL OFL 1.1)...", flush=True)
    for fname, url in WARM_FONTS:
        dest = LIB_FONTS_DIR / fname
        ok = download_file(url, dest)
        if ok:
            print(f"  -> {fname:20s} ({dest.stat().st_size // 1024} KB)", flush=True)

    # Promote the active Warm Wood & Linen default trio into tracked `assets/fonts/`.
    TRACKED_FONTS_DIR.mkdir(parents=True, exist_ok=True)
    for fname in ("fraunces.ttf", "lora.ttf", "nunito.ttf"):
        src = LIB_FONTS_DIR / fname
        if src.exists():
            shutil.copy2(src, TRACKED_FONTS_DIR / fname)


# --- Minimal, exact SVG path parser -> matplotlib.path.Path ---
TOKEN_RE = re.compile(r"[MmLlHhVvCcSsQqTtAaZz]|[-+]?(?:\d*\.\d+|\d+\.?)(?:[eE][-+]?\d+)?")


def arc_to_cubics(
    x1: float,
    y1: float,
    rx: float,
    ry: float,
    phi_deg: float,
    large_arc: int,
    sweep: int,
    x2: float,
    y2: float,
) -> list[tuple[tuple[float, float], tuple[float, float], tuple[float, float]]]:
    """Convert an SVG elliptical arc to cubic Bezier segments (W3C SVG 1.1 F.6.5)."""
    if (x1 == x2 and y1 == y2) or rx == 0.0 or ry == 0.0:
        return []
    rx, ry = abs(rx), abs(ry)
    phi = math.radians(phi_deg % 360.0)
    cos_p, sin_p = math.cos(phi), math.sin(phi)
    dx2 = (x1 - x2) * 0.5
    dy2 = (y1 - y2) * 0.5
    x1p = cos_p * dx2 + sin_p * dy2
    y1p = -sin_p * dx2 + cos_p * dy2

    lam = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry)
    if lam > 1.0:
        s = math.sqrt(lam)
        rx *= s
        ry *= s

    rx2, ry2 = rx * rx, ry * ry
    x1p2, y1p2 = x1p * x1p, y1p * y1p
    num = max(0.0, rx2 * ry2 - rx2 * y1p2 - ry2 * x1p2)
    den = rx2 * y1p2 + ry2 * x1p2
    coef = math.sqrt(num / den) if den > 1e-12 else 0.0
    if large_arc == sweep:
        coef = -coef
    cxp = coef * (rx * y1p / ry)
    cyp = coef * (-ry * x1p / rx)

    cx = cos_p * cxp - sin_p * cyp + (x1 + x2) * 0.5
    cy = sin_p * cxp + cos_p * cyp + (y1 + y2) * 0.5

    def angle(ux: float, uy: float, vx: float, vy: float) -> float:
        dot = ux * vx + uy * vy
        mag = math.hypot(ux, uy) * math.hypot(vx, vy)
        ang = math.acos(max(-1.0, min(1.0, dot / max(mag, 1e-12))))
        return -ang if (ux * vy - uy * vx) < 0.0 else ang

    vx1, vy1 = (x1p - cxp) / rx, (y1p - cyp) / ry
    vx2, vy2 = (-x1p - cxp) / rx, (-y1p - cyp) / ry
    theta1 = angle(1.0, 0.0, vx1, vy1)
    dtheta = angle(vx1, vy1, vx2, vy2)
    if sweep == 0 and dtheta > 0.0:
        dtheta -= 2.0 * math.pi
    elif sweep == 1 and dtheta < 0.0:
        dtheta += 2.0 * math.pi

    segs = max(1, int(math.ceil(abs(dtheta) / (math.pi * 0.5))))
    delta = dtheta / segs
    alpha = (4.0 / 3.0) * math.tan(delta * 0.25)

    out = []
    t = theta1
    for _ in range(segs):
        t2 = t + delta
        ct1, st1 = math.cos(t), math.sin(t)
        ct2, st2 = math.cos(t2), math.sin(t2)
        p1x, p1y = ct1 - alpha * st1, st1 + alpha * ct1
        p2x, p2y = ct2 + alpha * st2, st2 - alpha * ct2

        def to_world(ux: float, uy: float) -> tuple[float, float]:
            ex, ey = rx * ux, ry * uy
            return (cos_p * ex - sin_p * ey + cx, sin_p * ex + cos_p * ey + cy)

        out.append((to_world(p1x, p1y), to_world(p2x, p2y), to_world(ct2, st2)))
        t = t2
    return out


def parse_svg_path(d: str) -> MplPath:
    tokens = TOKEN_RE.findall(d)
    verts: list[tuple[float, float]] = []
    codes: list[int] = []
    i = 0
    cx = cy = sx = sy = 0.0
    prev_cp: tuple[float, float] | None = None
    prev_cmd = ""
    cmd = ""

    def next_f() -> float:
        nonlocal i
        val = float(tokens[i])
        i += 1
        return val

    def is_num(idx: int) -> bool:
        return idx < len(tokens) and not tokens[idx][0].isalpha()

    while i < len(tokens):
        if tokens[i][0].isalpha():
            cmd = tokens[i]
            i += 1
        elif cmd == "M":
            cmd = "L"
        elif cmd == "m":
            cmd = "l"

        if cmd in ("M", "m"):
            x, y = next_f(), next_f()
            if cmd == "m":
                x += cx
                y += cy
            cx, cy = sx, sy = x, y
            verts.append((cx, cy))
            codes.append(MplPath.MOVETO)
            prev_cp = None
        elif cmd in ("L", "l"):
            x, y = next_f(), next_f()
            if cmd == "l":
                x += cx
                y += cy
            cx, cy = x, y
            verts.append((cx, cy))
            codes.append(MplPath.LINETO)
            prev_cp = None
        elif cmd in ("H", "h"):
            x = next_f() + (cx if cmd == "h" else 0.0)
            cx = x
            verts.append((cx, cy))
            codes.append(MplPath.LINETO)
            prev_cp = None
        elif cmd in ("V", "v"):
            y = next_f() + (cy if cmd == "v" else 0.0)
            cy = y
            verts.append((cx, cy))
            codes.append(MplPath.LINETO)
            prev_cp = None
        elif cmd in ("C", "c"):
            x1, y1, x2, y2, x, y = next_f(), next_f(), next_f(), next_f(), next_f(), next_f()
            if cmd == "c":
                x1 += cx; y1 += cy; x2 += cx; y2 += cy; x += cx; y += cy
            verts.extend([(x1, y1), (x2, y2), (x, y)])
            codes.extend([MplPath.CURVE4, MplPath.CURVE4, MplPath.CURVE4])
            prev_cp = (x2, y2)
            cx, cy = x, y
        elif cmd in ("S", "s"):
            x2, y2, x, y = next_f(), next_f(), next_f(), next_f()
            if cmd == "s":
                x2 += cx; y2 += cy; x += cx; y += cy
            if prev_cmd in ("C", "c", "S", "s") and prev_cp is not None:
                x1, y1 = 2.0 * cx - prev_cp[0], 2.0 * cy - prev_cp[1]
            else:
                x1, y1 = cx, cy
            verts.extend([(x1, y1), (x2, y2), (x, y)])
            codes.extend([MplPath.CURVE4, MplPath.CURVE4, MplPath.CURVE4])
            prev_cp = (x2, y2)
            cx, cy = x, y
        elif cmd in ("Q", "q"):
            x1, y1, x, y = next_f(), next_f(), next_f(), next_f()
            if cmd == "q":
                x1 += cx; y1 += cy; x += cx; y += cy
            verts.extend([(x1, y1), (x, y)])
            codes.extend([MplPath.CURVE3, MplPath.CURVE3])
            prev_cp = (x1, y1)
            cx, cy = x, y
        elif cmd in ("T", "t"):
            x, y = next_f(), next_f()
            if cmd == "t":
                x += cx; y += cy
            if prev_cmd in ("Q", "q", "T", "t") and prev_cp is not None:
                x1, y1 = 2.0 * cx - prev_cp[0], 2.0 * cy - prev_cp[1]
            else:
                x1, y1 = cx, cy
            verts.extend([(x1, y1), (x, y)])
            codes.extend([MplPath.CURVE3, MplPath.CURVE3])
            prev_cp = (x1, y1)
            cx, cy = x, y
        elif cmd in ("A", "a"):
            rx, ry, rot = next_f(), next_f(), next_f()
            large_arc, sweep = int(next_f()), int(next_f())
            x, y = next_f(), next_f()
            if cmd == "a":
                x += cx; y += cy
            for c1, c2, end in arc_to_cubics(cx, cy, rx, ry, rot, large_arc, sweep, x, y):
                verts.extend([c1, c2, end])
                codes.extend([MplPath.CURVE4, MplPath.CURVE4, MplPath.CURVE4])
            cx, cy = x, y
            prev_cp = None
        elif cmd in ("Z", "z"):
            verts.append((sx, sy))
            codes.append(MplPath.CLOSEPOLY)
            cx, cy = sx, sy
            prev_cp = None
        else:
            break
        prev_cmd = cmd
        if cmd in ("Z", "z") and not is_num(i):
            continue

    return MplPath(np.array(verts, dtype=np.float64), codes)


def rasterize_game_icon(svg_path: Path, out_png: Path, size: int = 128) -> bool:
    """Rasterize a monochrome Game-Icons.net SVG into a crisp white-on-transparent PNG."""
    tree = ET.parse(svg_path)
    root = tree.getroot()
    vb = root.attrib.get("viewBox", "0 0 512 512").replace(",", " ").split()
    vb_x, vb_y, vb_w, vb_h = [float(x) for x in vb[:4]]

    paths: list[str] = []
    for elem in root.iter():
        tag = elem.tag.rsplit("}", 1)[-1]
        if tag != "path":
            continue
        d = elem.attrib.get("d", "").strip()
        if not d:
            continue
        # Skip full-canvas dark background rect (`M0 0h512v512H0z`) present in Game-Icons SVGs.
        norm_d = re.sub(r"\s+", "", d).lower()
        if norm_d in ("m00h512v512h0z", "m0,0h512v512h-512z", "m00l5120l512512l0512z"):
            continue
        fill = elem.attrib.get("fill", "").strip().lower()
        if fill in ("#000", "#000000", "black"):
            continue
        paths.append(d)

    if not paths:
        return False

    dpi = 128
    fig = plt.figure(figsize=(size / dpi, size / dpi), dpi=dpi)
    fig.patch.set_alpha(0.0)
    ax = fig.add_axes([0.0, 0.0, 1.0, 1.0])
    ax.set_xlim(vb_x, vb_x + vb_w)
    ax.set_ylim(vb_y + vb_h, vb_y)  # Flip Y for SVG coords.
    ax.axis("off")
    ax.patch.set_alpha(0.0)

    for d in paths:
        mpl_path = parse_svg_path(d)
        patch = PathPatch(mpl_path, facecolor=(1.0, 1.0, 1.0, 1.0), edgecolor="none", antialiased=True)
        ax.add_patch(patch)

    out_png.parent.mkdir(parents=True, exist_ok=True)
    fig.savefig(out_png, dpi=dpi, transparent=True, format="png")
    plt.close(fig)
    return True


def rasterize_curated_icons() -> None:
    print("[2/4] Rasterizing curated monochrome RPG icons to assets/ui/icons/...", flush=True)
    TRACKED_ICONS_DIR.mkdir(parents=True, exist_ok=True)
    ok_count = 0
    for out_name, candidates in CURATED_ICONS.items():
        svg_file = None
        for cand in candidates:
            p = SVG_DIR / f"{cand}.svg"
            if p.exists():
                svg_file = p
                break
        if svg_file is None:
            print(f"  WARN no SVG found for {out_name} (tried {candidates})", file=sys.stderr)
            continue
        out_png = TRACKED_ICONS_DIR / f"{out_name}.png"
        if rasterize_game_icon(svg_file, out_png, size=128):
            ok_count += 1
            print(f"  -> {out_name}.png  (from {svg_file.name})", flush=True)
    print(f"  Rasterized {ok_count}/{len(CURATED_ICONS)} monochrome icons.", flush=True)


def render_font_comparison_sheet(out_path: Path) -> None:
    """Render 4 Warm Wood & Linen font pairings onto the actual wood/parchment panels."""
    print(f"[3/4] Rendering 4-option font pairing sheet -> {out_path}...", flush=True)
    pairings = [
        (
            "Option A (Active Default): Fraunces + Lora + Nunito",
            "Warm woodblock storybook — chunky soft serif + full serif + rounded HUD",
            LIB_FONTS_DIR / "fraunces.ttf",
            LIB_FONTS_DIR / "lora.ttf",
            LIB_FONTS_DIR / "nunito.ttf",
        ),
        (
            "Option B: Fredoka + Nunito",
            "Friendly cozy cartoony — soft rounded display + matching rounded HUD",
            LIB_FONTS_DIR / "fredoka.ttf",
            LIB_FONTS_DIR / "nunito.ttf",
            LIB_FONTS_DIR / "nunito.ttf",
        ),
        (
            "Option C: Alegreya + Lora + Alegreya Sans",
            "Warm fantasy RPG — calligraphic humanist display + matching warm sans",
            LIB_FONTS_DIR / "alegreya.ttf",
            LIB_FONTS_DIR / "lora.ttf",
            LIB_FONTS_DIR / "alegreya_sans.ttf",
        ),
        (
            "Option D: Bitter + Patrick Hand + Quicksand",
            "Carved woodblock + hand-inked tabletop journal + soft rounded HUD",
            LIB_FONTS_DIR / "bitter.ttf",
            LIB_FONTS_DIR / "patrick_hand.ttf",
            LIB_FONTS_DIR / "quicksand.ttf",
        ),
    ]

    w, h = 1440, 960
    canvas = Image.new("RGB", (w, h), (38, 39, 43))
    wood_tile = Image.open(REPO_ROOT / "assets" / "ui" / "panel_wood.png").convert("RGBA")
    parch_tile = Image.open(REPO_ROOT / "assets" / "ui" / "panel_parchment.png").convert("RGBA")
    btn_tile = Image.open(REPO_ROOT / "assets" / "ui" / "button_parchment.png").convert("RGBA")

    draw = ImageDraw.Draw(canvas)
    try:
        meta_font = ImageFont.truetype(str(LIB_FONTS_DIR / "nunito.ttf"), 17)
        meta_sub = ImageFont.truetype(str(LIB_FONTS_DIR / "nunito.ttf"), 13)
    except Exception:
        meta_font = ImageFont.load_default()
        meta_sub = meta_font

    card_w, card_h = 684, 446
    positions = [(24, 24), (732, 24), (24, 490), (732, 490)]

    for (title, desc, f_head, f_narr, f_ui), (ox, oy) in zip(pairings, positions):
        draw.text((ox + 6, oy), title, fill=(251, 245, 234), font=meta_font)
        draw.text((ox + 6, oy + 22), desc, fill=(196, 188, 174), font=meta_sub)

        try:
            head_font = ImageFont.truetype(str(f_head), 23)
            dice_font = ImageFont.truetype(str(f_head), 46)
            narr_font = ImageFont.truetype(str(f_narr), 17)
            ui_font = ImageFont.truetype(str(f_ui), 16)
        except Exception:
            head_font = dice_font = narr_font = ui_font = meta_font

        # Wood banner card (left half)
        wood_panel = wood_tile.resize((326, 210), Image.Resampling.LANCZOS)
        canvas.paste(wood_panel, (ox, oy + 46), wood_panel)
        draw.text((ox + 22, oy + 64), "YOUR TURN - ROUND 1", fill=(251, 245, 234), font=head_font)
        draw.text((ox + 22, oy + 98), "Movement: available   Action: available", fill=(221, 208, 190), font=ui_font)
        btn = btn_tile.resize((175, 40), Image.Resampling.LANCZOS)
        canvas.paste(btn, (ox + 22, oy + 132), btn)
        draw.text((ox + 42, oy + 141), "End turn (Space)", fill=(47, 34, 25), font=ui_font)
        draw.text((ox + 226, oy + 124), "18", fill=(124, 207, 124), font=dice_font)
        draw.text((ox + 22, oy + 186), "INVENTORY: 53 LB CARRIED", fill=(251, 245, 234), font=ui_font)

        # Parchment journal card (right half)
        parch_panel = parch_tile.resize((340, 290), Image.Resampling.LANCZOS)
        canvas.paste(parch_panel, (ox + 340, oy + 46), parch_panel)
        lines = [
            "Cold river mist drifts over the",
            "timber bridge as the goblin eyes",
            "the oil barrel.",
            "",
            'Bram says: "Hold the bridge —',
            'I will flank left!"',
            "",
            "Aria rolls 16 + 3 = 19 vs DC 14",
            "(Athletics: shove the barrel):",
            "Success!",
        ]
        ly = oy + 64
        for line in lines:
            draw.text((ox + 362, ly), line, fill=(47, 34, 25), font=narr_font)
            ly += 23

        # Bottom prompt plank
        prompt_plank = wood_tile.resize((680, 68), Image.Resampling.LANCZOS)
        canvas.paste(prompt_plank, (ox, oy + 348), prompt_plank)
        draw.text((ox + 22, oy + 360), "Your action was resolved.", fill=(245, 213, 168), font=meta_sub)
        draw.text((ox + 22, oy + 382), "> I kick the oil barrel across the wooden bridge_", fill=(251, 245, 234), font=ui_font)

    out_path.parent.mkdir(parents=True, exist_ok=True)
    canvas.save(out_path, format="PNG")


def tag_manifest_art_styles() -> None:
    print("[4/4] Tagging `art_style` quarantine metadata in assets/library/manifest.json...", flush=True)
    if not MANIFEST_PATH.exists():
        return
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    assets = manifest.get("assets", [])

    style_counts: Counter[str] = Counter()
    for a in assets:
        cat = a["category"]
        sub = a.get("subcategory", "")
        if cat in ("textures", "structures", "props", "nature", "overlays", "maps", "markers"):
            style = "painterly"
        elif cat == "ui":
            style = "wood_ui" if sub == "rpg_pack" else "monochrome_ui"
        elif cat == "icons":
            style = "monochrome_ui" if sub in ("rpg_svg", "ui_hud") else "pixel_placeholder"
        elif cat == "tokens":
            style = "pixel_placeholder"
        elif cat == "vfx":
            style = "vfx" if sub == "particles" else "pixel_placeholder"
        elif cat == "audio":
            style = "audio"
        elif cat == "fonts":
            style = "font"
        else:
            style = "other"
        a["art_style"] = style
        style_counts[style] += 1

    manifest["art_styles"] = dict(sorted(style_counts.items()))
    tmp = MANIFEST_PATH.with_suffix(".tmp")
    tmp.write_text(json.dumps(manifest, ensure_ascii=False, indent=2), encoding="utf-8")
    os.replace(tmp, MANIFEST_PATH)
    print("  art_style breakdown:", dict(style_counts), flush=True)


def main() -> int:
    fetch_warm_fonts()
    rasterize_curated_icons()
    render_font_comparison_sheet(REPO_ROOT / "target" / "ui_shots" / "font_pairings_preview.png")
    tag_manifest_art_styles()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
