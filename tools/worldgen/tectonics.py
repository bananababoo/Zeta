"""True 3D spherical plate-tectonics terrain generator for Zeta's pre-humanoid world.

Evaluates all tectonic plates, crust geometry, boundary physics, volcanic arcs,
mantle-plume hotspots, and ridged multifractal terrain purely on the 3D unit
sphere S^2 (x = cos(lat)*cos(lon), y = cos(lat)*sin(lon), z = sin(lat)).
This eliminates equirectangular horizontal stretching, polar pinching, and
antimeridian seams.
"""

from __future__ import annotations

import math
import time
from dataclasses import dataclass
from pathlib import Path

import cv2
import numpy as np
from PIL import Image, ImageDraw, ImageFont

# ---------------------------------------------------------------------------
# Fast 3D Spherical Lattice Noise (C2 quintic smoothstep + per-octave SO(3))
# ---------------------------------------------------------------------------

_TABLE_BITS = 6
_TABLE_SIZE = 1 << _TABLE_BITS  # 64
_TABLE_MASK = _TABLE_SIZE - 1
_SHIFT_X = _TABLE_BITS * 2      # 12
_SHIFT_Y = _TABLE_BITS          # 6


class SphericalNoise3D:
    """Cache-friendly C2-smooth 3D value noise on S^2 with random SO(3) octaves."""

    def __init__(self, seed: int) -> None:
        rng = np.random.default_rng(seed)
        # 64^3 float32 = 1 MB -> stays hot in L2/L3 cache.
        self.table = rng.uniform(-1.0, 1.0, size=_TABLE_SIZE ** 3).astype(np.float32)
        self._rotations: list[np.ndarray] = []
        self._offsets: list[np.ndarray] = []
        for _ in range(48):
            mat = rng.standard_normal((3, 3))
            q, r = np.linalg.qr(mat)
            q *= np.sign(np.diag(r))
            if np.linalg.det(q) < 0:
                q[:, 0] = -q[:, 0]
            self._rotations.append(q.astype(np.float32))
            self._offsets.append(rng.uniform(0.0, float(_TABLE_SIZE), size=3).astype(np.float32))

    def octave(
        self,
        x: np.ndarray,
        y: np.ndarray,
        z: np.ndarray,
        freq: float,
        idx: int,
    ) -> np.ndarray:
        """Evaluate one rotated 3D quintic smoothstep noise octave in ~[-1, 1]."""
        r = self._rotations[idx % 48]
        off = self._offsets[idx % 48]
        f = np.float32(freq)

        rx = (r[0, 0] * x + r[0, 1] * y + r[0, 2] * z) * f + off[0]
        ry = (r[1, 0] * x + r[1, 1] * y + r[1, 2] * z) * f + off[1]
        rz = (r[2, 0] * x + r[2, 1] * y + r[2, 2] * z) * f + off[2]

        ix0 = np.floor(rx).astype(np.int32)
        iy0 = np.floor(ry).astype(np.int32)
        iz0 = np.floor(rz).astype(np.int32)

        fx = rx - ix0
        fy = ry - iy0
        fz = rz - iz0

        # Quintic smoothstep: 6t^5 - 15t^4 + 10t^3 (C2 continuous)
        ux = fx * fx * fx * (fx * (fx * np.float32(6.0) - np.float32(15.0)) + np.float32(10.0))
        uy = fy * fy * fy * (fy * (fy * np.float32(6.0) - np.float32(15.0)) + np.float32(10.0))
        uz = fz * fz * fz * (fz * (fz * np.float32(6.0) - np.float32(15.0)) + np.float32(10.0))

        ix1 = ((ix0 + 1) & _TABLE_MASK) << _SHIFT_X
        iy1 = ((iy0 + 1) & _TABLE_MASK) << _SHIFT_Y
        iz1 = (iz0 + 1) & _TABLE_MASK

        ix0 = (ix0 & _TABLE_MASK) << _SHIFT_X
        iy0 = (iy0 & _TABLE_MASK) << _SHIFT_Y
        iz0 = iz0 & _TABLE_MASK

        tbl = self.table
        one = np.float32(1.0)
        muz = one - uz
        muy = one - uy
        mux = one - ux

        c00 = tbl[ix0 | iy0 | iz0] * muz + tbl[ix0 | iy0 | iz1] * uz
        c01 = tbl[ix0 | iy1 | iz0] * muz + tbl[ix0 | iy1 | iz1] * uz
        c10 = tbl[ix1 | iy0 | iz0] * muz + tbl[ix1 | iy0 | iz1] * uz
        c11 = tbl[ix1 | iy1 | iz0] * muz + tbl[ix1 | iy1 | iz1] * uz

        c0 = c00 * muy + c01 * uy
        c1 = c10 * muy + c11 * uy
        return (c0 * mux + c1 * ux) * np.float32(1.72)

    def fbm(
        self,
        x: np.ndarray,
        y: np.ndarray,
        z: np.ndarray,
        base_freq: float,
        octaves: int,
        lacunarity: float = 2.05,
        gain: float = 0.5,
        idx_offset: int = 0,
    ) -> np.ndarray:
        """Standard 3D spherical FBM normalized roughly to [-1, 1]."""
        acc = np.zeros_like(x, dtype=np.float32)
        amp = np.float32(1.0)
        norm = np.float32(0.0)
        freq = float(base_freq)
        for k in range(octaves):
            acc += self.octave(x, y, z, freq, idx_offset + k) * amp
            norm += amp
            freq *= lacunarity
            amp *= np.float32(gain)
        return acc / norm

    def ridged_multifractal(
        self,
        x: np.ndarray,
        y: np.ndarray,
        z: np.ndarray,
        base_freq: float,
        octaves: int,
        lacunarity: float = 2.10,
        gain: float = 0.55,
        weight_gain: float = 2.0,
        idx_offset: int = 8,
    ) -> np.ndarray:
        """Musgrave heterogeneous ridged multifractal in [0, 1]."""
        acc = np.zeros_like(x, dtype=np.float32)
        weight = np.ones_like(x, dtype=np.float32)
        amp = np.float32(1.0)
        norm = np.float32(0.0)
        freq = float(base_freq)
        wg = np.float32(weight_gain)
        one = np.float32(1.0)

        for k in range(octaves):
            n = self.octave(x, y, z, freq, idx_offset + k)
            r = one - np.abs(n)
            np.clip(r, np.float32(0.0), one, out=r)
            r *= r
            r *= weight
            weight = np.clip(r * wg, np.float32(0.08), one)
            acc += r * amp
            norm += amp
            freq *= lacunarity
            amp *= np.float32(gain)

        return np.clip(acc / norm, np.float32(0.0), one)


# ---------------------------------------------------------------------------
# Spherical Geometry & Upsampling Helpers
# ---------------------------------------------------------------------------


def _sphere_grid_rows(
    width: int,
    height: int,
    row_start: int,
    row_end: int,
    lon_shift_deg: float = 0.0,
) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """Return unit sphere (x, y, z) float32 arrays for rows [row_start, row_end).

    `lon_shift_deg` rotates display longitude rigidly around the polar axis so
    a feature originally at `lon_orig` appears at `lon_orig + lon_shift_deg`.
    """
    rows = np.arange(row_start, row_end, dtype=np.float32)
    cols = np.arange(width, dtype=np.float32)
    lat = np.float32(math.pi * 0.5) - (rows + np.float32(0.5)) * np.float32(math.pi / height)
    lon = (
        np.float32(-math.pi - math.radians(lon_shift_deg))
        + (cols + np.float32(0.5)) * np.float32(2.0 * math.pi / width)
    )

    cos_lat = np.cos(lat)[:, None]
    sin_lat = np.sin(lat)[:, None]
    cos_lon = np.cos(lon)[None, :]
    sin_lon = np.sin(lon)[None, :]

    x = (cos_lat * cos_lon).astype(np.float32)
    y = (cos_lat * sin_lon).astype(np.float32)
    z = np.broadcast_to(sin_lat, (row_end - row_start, width)).astype(np.float32, copy=True)
    return x, y, z


def _upsample_equirect(
    arr: np.ndarray, target_w: int, target_h: int, cubic: bool = False
) -> np.ndarray:
    """Seam-free equirectangular upsampling with periodic horizontal wrap."""
    if arr.shape == (target_h, target_w):
        return arr.astype(np.float32, copy=False)
    pad = 2 if not cubic else 4
    padded = np.pad(arr, ((pad, pad), (pad, pad)), mode="edge")
    padded[pad:-pad, :pad] = arr[:, -pad:]
    padded[pad:-pad, -pad:] = arr[:, :pad]
    src_h, src_w = arr.shape
    scale_x = target_w / float(src_w)
    scale_y = target_h / float(src_h)
    up_w = int(round((src_w + 2 * pad) * scale_x))
    up_h = int(round((src_h + 2 * pad) * scale_y))
    interp = cv2.INTER_CUBIC if cubic else cv2.INTER_LINEAR
    resized = cv2.resize(padded, (up_w, up_h), interpolation=interp)
    px = int(round(pad * scale_x))
    py = int(round(pad * scale_y))
    out = resized[py : py + target_h, px : px + target_w]
    if out.shape != (target_h, target_w):
        out = cv2.resize(arr, (target_w, target_h), interpolation=interp)
    return out.astype(np.float32, copy=False)


def _smoothstep(edge0: float, edge1: float, x: np.ndarray) -> np.ndarray:
    t = np.clip((x - np.float32(edge0)) / np.float32(edge1 - edge0), np.float32(0.0), np.float32(1.0))
    return t * t * (np.float32(3.0) - np.float32(2.0) * t)


def _normalize3(v: np.ndarray) -> np.ndarray:
    n = np.linalg.norm(v, axis=-1, keepdims=True)
    return v / np.maximum(n, 1e-8)


def _tangent_basis(p: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """Return local (east, north) unit tangent vectors on S^2 at unit vector p."""
    up = np.array([0.0, 0.0, 1.0], dtype=np.float64)
    east = np.cross(up, p)
    en = np.linalg.norm(east)
    if en < 1e-5:
        east = np.array([0.0, 1.0, 0.0], dtype=np.float64)
    else:
        east /= en
    north = np.cross(p, east)
    north /= np.linalg.norm(north)
    return east, north


def _move_geodesic(p: np.ndarray, tangent_dir: np.ndarray, angle_rad: float) -> np.ndarray:
    """Move unit vector p along tangent unit direction by angle_rad on S^2."""
    t = tangent_dir - np.dot(tangent_dir, p) * p
    tn = np.linalg.norm(t)
    if tn < 1e-8:
        return p.copy()
    t /= tn
    q = math.cos(angle_rad) * p + math.sin(angle_rad) * t
    return q / np.linalg.norm(q)


# ---------------------------------------------------------------------------
# Planetary Tectonic Model
# ---------------------------------------------------------------------------


@dataclass
class CratonLobe:
    """Spherical craton lobe anchoring continental crust (or irregular marine basin)."""
    plate_id: int
    center: np.ndarray        # (3,) unit vector
    axis_u: np.ndarray        # (3,) tangent major axis
    axis_v: np.ndarray        # (3,) tangent minor axis
    rad_u: float              # semi-major angular radius (rad)
    rad_v: float              # semi-minor angular radius (rad)
    strength: float
    is_basin: bool = False


@dataclass
class HotspotNode:
    """Single volcanic island or submerged seamount along a mantle-plume chain."""
    center: np.ndarray        # (3,) unit vector
    radius_rad: float
    height_m: float
    shelf_rad: float


@dataclass
class ArchipelagoZone:
    """Region where fractal volcanic/continental-shelf archipelagos emerge."""
    center: np.ndarray
    axis_u: np.ndarray
    axis_v: np.ndarray
    rad_u: float
    rad_v: float
    intensity: float
    is_subpolar: bool = False


@dataclass
class TectonicWorldSpec:
    seed: int
    n_plates: int
    centers: np.ndarray           # (N, 3) float32
    euler_poles: np.ndarray       # (N, 3) float32
    is_continental: np.ndarray    # (N,) bool
    plate_age: np.ndarray         # (N,) float32 in [0, 1]
    plate_base_bias: np.ndarray   # (N,) float32
    pair_diff_len: np.ndarray     # (N, N) float32
    craton_lobes: list[CratonLobe]
    hotspots: list[HotspotNode]
    archipelago_zones: list[ArchipelagoZone]
    warp_amp: float


def _fibonacci_sphere(n: int, rng: np.random.Generator) -> np.ndarray:
    """Generate well-spaced yet organic unit vectors on S^2."""
    idx = np.arange(n, dtype=np.float64) + 0.5
    z = 1.0 - 2.0 * idx / n
    r = np.sqrt(np.maximum(0.0, 1.0 - z * z))
    golden = math.pi * (3.0 - math.sqrt(5.0))
    theta = golden * idx
    pts = np.stack([r * np.cos(theta), r * np.sin(theta), z], axis=1)

    mat = rng.standard_normal((3, 3))
    q, rm = np.linalg.qr(mat)
    q *= np.sign(np.diag(rm))
    if np.linalg.det(q) < 0:
        q[:, 0] = -q[:, 0]
    pts = pts @ q.T

    pts += rng.normal(0.0, 0.21, size=pts.shape)
    pts = _normalize3(pts)
    for _ in range(4):
        for i in range(n):
            diff = pts[i] - pts
            dist = np.linalg.norm(diff, axis=1)
            dist[i] = 10.0
            close = dist < 0.62
            if np.any(close):
                push = np.sum(diff[close] / (dist[close, None] ** 2 + 0.05), axis=0)
                pts[i] = _normalize3(pts[i] + 0.045 * push)
    return pts


def _plate_adjacency(centers: np.ndarray) -> dict[int, set[int]]:
    """Compute exact spherical Voronoi adjacency graph on a coarse sphere."""
    n = len(centers)
    h, w = 96, 192
    x, y, z = _sphere_grid_rows(w, h, 0, h)
    p = np.stack([x, y, z], axis=-1).reshape(-1, 3)
    dots = p @ centers.astype(np.float32).T
    pid = np.argmax(dots, axis=1).reshape(h, w)

    adj: dict[int, set[int]] = {i: set() for i in range(n)}
    a = pid
    b = np.roll(pid, -1, axis=1)
    mask = a != b
    for u, v in zip(a[mask].tolist(), b[mask].tolist()):
        adj[u].add(v)
        adj[v].add(u)
    a2 = pid[:-1, :]
    b2 = pid[1:, :]
    mask2 = a2 != b2
    for u, v in zip(a2[mask2].tolist(), b2[mask2].tolist()):
        adj[u].add(v)
        adj[v].add(u)
    return adj


def _build_tectonic_spec(seed: int) -> TectonicWorldSpec:
    """Design a rich, non-Earth, non-horizontal planetary plate setup for `seed`."""
    rng = np.random.default_rng(seed * 7919 + 104729)
    n_plates = int(rng.integers(15, 18))  # 15..17 plates (within 14..18)
    centers = _fibonacci_sphere(n_plates, rng)
    adj = _plate_adjacency(centers)

    non_polar = [i for i in range(n_plates) if abs(centers[i, 2]) < 0.86]
    mid_plates = [i for i in range(n_plates) if abs(centers[i, 2]) < 0.76]
    if len(mid_plates) < 8:
        mid_plates = non_polar

    used: set[int] = set()
    forbidden_adj: set[int] = set()

    def _pair_score(u: int, v: int, prefer_ew_pair: bool = True) -> float:
        d = centers[v] - centers[u]
        east, north = _tangent_basis(_normalize3(centers[u] + centers[v]))
        de = abs(float(np.dot(d, east)))
        dn = abs(float(np.dot(d, north)))
        ang_score = (de / (de + dn + 1e-6)) if prefer_ew_pair else (dn / (de + dn + 1e-6))
        lat_pen = 0.45 * max(abs(centers[u, 2]), abs(centers[v, 2]))
        return ang_score * 1.4 - lat_pen + float(rng.uniform(0.0, 0.40))

    # Group 0: Colliding continent pair (2 adjacent plates -> interior alpine chain + high plateau)
    pairs = [
        (u, v)
        for u in mid_plates
        for v in adj[u]
        if v in mid_plates and u < v
    ]
    pairs.sort(key=lambda uv: _pair_score(uv[0], uv[1], True), reverse=True)
    g0 = pairs[int(rng.integers(0, min(3, len(pairs))))]
    collide_pair = (g0[0], g0[1])
    used.update(collide_pair)
    for p in collide_pair:
        forbidden_adj.update(adj[p])

    # Group 1: Rifting / Inland-Sea continent pair (strictly separated by ocean from Group 0)
    rift_candidates = [
        (u, v)
        for u in non_polar
        for v in adj[u]
        if v in non_polar
        and u < v
        and u not in used
        and v not in used
        and u not in forbidden_adj
        and v not in forbidden_adj
    ]
    if not rift_candidates:
        rift_candidates = [
            (u, v)
            for u in non_polar
            for v in adj[u]
            if v in non_polar and u < v and u not in used and v not in used
        ]
    rift_candidates.sort(
        key=lambda uv: (
            min(
                math.acos(np.clip(float(np.dot(centers[uv[0]], centers[p])), -1.0, 1.0))
                for p in collide_pair
            )
            + _pair_score(uv[0], uv[1], False) * 0.6
        ),
        reverse=True,
    )
    g1 = rift_candidates[int(rng.integers(0, min(3, len(rift_candidates))))]
    rift_pair = (g1[0], g1[1])
    used.update(rift_pair)
    for p in rift_pair:
        forbidden_adj.update(adj[p])

    # Group 2 & Group 3: 2 solitary continents (one temperate/tropical, one reaching subpolar
    # latitude as a rugged meridional landmass -> naturally balances projected pixel area!)
    solitary: list[int] = []
    for s_idx in range(2):
        cands = [i for i in non_polar if i not in used and i not in forbidden_adj]
        if not cands:
            cands = [i for i in range(n_plates) if i not in used and abs(centers[i, 2]) < 0.88]
        if not cands:
            break
        want_subpolar = (s_idx == 1)
        scored = sorted(
            cands,
            key=lambda i: (
                min(
                    math.acos(np.clip(float(np.dot(centers[i], centers[u])), -1.0, 1.0))
                    for u in used
                )
                + (0.55 * abs(centers[i, 2]) if want_subpolar else -0.35 * abs(centers[i, 2]))
                + float(rng.uniform(0.0, 0.16))
            ),
            reverse=True,
        )
        pick = scored[0]
        solitary.append(pick)
        used.add(pick)
        forbidden_adj.update(adj[pick])

    is_continental = np.zeros(n_plates, dtype=bool)
    for i in used:
        is_continental[i] = True

    # ------------------------------------------------------------------
    # Assign 3D Euler Rotation Poles (v_i(p) = omega_i x p)
    # ------------------------------------------------------------------
    euler_poles = np.zeros((n_plates, 3), dtype=np.float64)
    target_vels = np.zeros((n_plates, 3), dtype=np.float64)

    # 1) Colliding continent pair: strong convergence for dramatic alpine mountains!
    u0, v0 = collide_pair
    d0 = centers[v0] - centers[u0]
    d0 /= np.linalg.norm(d0)
    tang_u0 = _normalize3(d0 - np.dot(d0, centers[u0]) * centers[u0])
    tang_v0 = _normalize3(-d0 - np.dot(-d0, centers[v0]) * centers[v0])
    oblique = np.cross(centers[u0], tang_u0) * float(rng.uniform(-0.20, 0.20))
    target_vels[u0] = _normalize3(tang_u0 + oblique) * float(rng.uniform(0.85, 1.08))
    target_vels[v0] = _normalize3(tang_v0 - oblique) * float(rng.uniform(0.85, 1.08))

    # 2) Rifting continent pair: pulls apart into a rugged jigsaw sea / rift escarpments!
    u1, v1 = rift_pair
    d1 = centers[v1] - centers[u1]
    d1 /= np.linalg.norm(d1)
    tang_u1 = _normalize3(-d1 - np.dot(-d1, centers[u1]) * centers[u1])
    tang_v1 = _normalize3(d1 - np.dot(d1, centers[v1]) * centers[v1])
    shear1 = np.cross(centers[u1], tang_u1) * float(rng.uniform(-0.22, 0.22))
    target_vels[u1] = _normalize3(tang_u1 + shear1) * float(rng.uniform(0.65, 0.88))
    target_vels[v1] = _normalize3(tang_v1 - shear1) * float(rng.uniform(0.65, 0.88))

    # 3) Solitary continents
    for sc in solitary:
        east, north = _tangent_basis(centers[sc])
        ang = float(rng.uniform(0.0, 2.0 * math.pi))
        target_vels[sc] = (math.cos(ang) * east + math.sin(ang) * north) * float(rng.uniform(0.35, 0.65))

    # 4) Oceanic plates: 3 ocean-ocean volcanic island arcs + coastal cordilleras + MORs
    ocean_plates = [i for i in range(n_plates) if not is_continental[i]]
    oo_pairs = [
        (u, v)
        for u in ocean_plates
        for v in adj[u]
        if v in ocean_plates and u < v
    ]
    oo_pairs.sort(
        key=lambda uv: (
            -0.55 * (abs(centers[uv[0], 2]) + abs(centers[uv[1], 2]))
            + float(rng.uniform(0.0, 0.35))
        ),
        reverse=True,
    )

    assigned_ocean: set[int] = set()
    arc_pairs: list[tuple[int, int]] = []
    for u, v in oo_pairs:
        if len(arc_pairs) >= 3:
            break
        if u not in assigned_ocean and v not in assigned_ocean:
            arc_pairs.append((u, v))
            assigned_ocean.add(u)
            assigned_ocean.add(v)
            dv = centers[v] - centers[u]
            dv /= np.linalg.norm(dv)
            tu = _normalize3(dv - np.dot(dv, centers[u]) * centers[u])
            tv = _normalize3(-dv - np.dot(-dv, centers[v]) * centers[v])
            target_vels[u] = tu * float(rng.uniform(0.75, 1.02))
            target_vels[v] = tv * float(rng.uniform(0.68, 0.94))

    for o in ocean_plates:
        if o in assigned_ocean:
            continue
        cont_nbrs = [c for c in adj[o] if is_continental[c]]
        if cont_nbrs:
            tgt = cont_nbrs[int(rng.integers(0, len(cont_nbrs)))]
            dv = centers[tgt] - centers[o]
            t = _normalize3(dv - np.dot(dv, centers[o]) * centers[o])
            target_vels[o] = t * float(rng.uniform(0.78, 1.05))
        else:
            east, north = _tangent_basis(centers[o])
            ang = float(rng.uniform(0.0, 2.0 * math.pi))
            target_vels[o] = (math.cos(ang) * east + math.sin(ang) * north) * float(rng.uniform(0.55, 0.88))

    for i in range(n_plates):
        spin = float(rng.uniform(-0.18, 0.18)) * centers[i]
        euler_poles[i] = np.cross(centers[i], target_vels[i]) + spin

    plate_age = rng.uniform(0.10, 0.95, size=n_plates).astype(np.float32)
    for u, v in arc_pairs:
        if abs(plate_age[u] - plate_age[v]) < 0.30:
            plate_age[u] = 0.88
            plate_age[v] = 0.16

    plate_base_bias = rng.uniform(-140.0, 140.0, size=n_plates).astype(np.float32)

    # ------------------------------------------------------------------
    # Build Sculpted Non-Horizontal Continental Craton Assemblies
    # ------------------------------------------------------------------
    craton_lobes: list[CratonLobe] = []
    archipelago_zones: list[ArchipelagoZone] = []

    def _add_curved_continent(
        plate_id: int,
        anchor_dir: np.ndarray | None,
        scale: float,
        carve_inland_sea: bool = False,
        add_shelf_archipelago: bool = False,
        extend_high_lat: bool = False,
    ) -> None:
        c = centers[plate_id]
        east, north = _tangent_basis(c)

        # Strongly N-S / NW-SE / NE-SW spine orientation (NEVER horizontal bars!)
        sign_ns = 1.0 if rng.random() < 0.5 else -1.0
        sign_ew = 1.0 if rng.random() < 0.5 else -1.0
        tilt = float(rng.uniform(0.06, 0.70)) * sign_ew
        spine_dir = _normalize3(math.cos(tilt) * (sign_ns * north) + math.sin(tilt) * east)
        perp_dir = np.cross(c, spine_dir)
        perp_dir /= np.linalg.norm(perp_dir)

        base_c = c.copy()
        if anchor_dir is not None:
            ad = anchor_dir - np.dot(anchor_dir, c) * c
            adn = np.linalg.norm(ad)
            if adn > 1e-5:
                ad /= adn
                base_c = _move_geodesic(c, ad, float(rng.uniform(0.06, 0.13)))

        bend_sign = 1.0 if rng.random() < 0.5 else -1.0
        is_scurve = bool(rng.random() < 0.45)
        n_lobes = int(rng.integers(4, 6))
        s_span = 0.34 * scale
        s_positions = np.linspace(-s_span, s_span, n_lobes)
        max_z = 0.955 if extend_high_lat else 0.88

        for idx_l, s in enumerate(s_positions):
            t_norm = float(s) / max(0.05, s_span)
            if is_scurve:
                lateral = bend_sign * 0.11 * scale * math.sin(t_norm * math.pi * 0.75)
            else:
                lateral = bend_sign * 0.13 * scale * (t_norm * t_norm - 0.35)
            lateral += float(rng.uniform(-0.035, 0.035)) * scale

            p_lobe = _move_geodesic(base_c, spine_dir, float(s))
            p_lobe = _move_geodesic(p_lobe, perp_dir, float(lateral))
            if abs(p_lobe[2]) > max_z:
                p_lobe[2] = math.copysign(max_z, p_lobe[2])
                p_lobe = _normalize3(p_lobe)

            u_ax = _normalize3(spine_dir - np.dot(spine_dir, p_lobe) * p_lobe)
            v_ax = _normalize3(np.cross(p_lobe, u_ax))
            rot_ang = bend_sign * t_norm * 0.42 + float(rng.uniform(-0.28, 0.28))
            u_rot = _normalize3(math.cos(rot_ang) * u_ax + math.sin(rot_ang) * v_ax)
            v_rot = _normalize3(np.cross(p_lobe, u_rot))

            taper = 1.05 if (0 < idx_l < n_lobes - 1) else float(rng.uniform(0.68, 0.88))
            rad_u = float(rng.uniform(0.24, 0.33)) * scale * taper
            rad_v = float(rng.uniform(0.17, 0.25)) * scale * taper
            craton_lobes.append(
                CratonLobe(
                    plate_id=plate_id,
                    center=p_lobe.astype(np.float32),
                    axis_u=u_rot.astype(np.float32),
                    axis_v=v_rot.astype(np.float32),
                    rad_u=rad_u,
                    rad_v=rad_v,
                    strength=float(rng.uniform(0.88, 1.08)),
                    is_basin=False,
                )
            )

        arm_dir = _normalize3(0.35 * spine_dir + bend_sign * 0.93 * perp_dir)
        p_arm = _move_geodesic(base_c, arm_dir, float(rng.uniform(0.21, 0.29)) * scale)
        if abs(p_arm[2]) > max_z:
            p_arm[2] = math.copysign(max_z, p_arm[2])
            p_arm = _normalize3(p_arm)
        u_arm = _normalize3(arm_dir - np.dot(arm_dir, p_arm) * p_arm)
        v_arm = _normalize3(np.cross(p_arm, u_arm))
        craton_lobes.append(
            CratonLobe(
                plate_id=plate_id,
                center=p_arm.astype(np.float32),
                axis_u=u_arm.astype(np.float32),
                axis_v=v_arm.astype(np.float32),
                rad_u=float(rng.uniform(0.19, 0.25)) * scale,
                rad_v=float(rng.uniform(0.11, 0.17)) * scale,
                strength=float(rng.uniform(0.80, 0.96)),
                is_basin=False,
            )
        )

        # Carve an organic Mediterranean / Aegean / Hudson-style enclosed sea or deep gulf
        if carve_inland_sea:
            gulf_dir = _normalize3(bend_sign * perp_dir + float(rng.uniform(-0.25, 0.25)) * spine_dir)
            for k_b, (dist_b, ru_b, rv_b, str_b) in enumerate(
                [
                    (0.10 * scale, 0.145 * scale, 0.088 * scale, 1.18),
                    (0.22 * scale, 0.115 * scale, 0.075 * scale, 0.98),
                ]
            ):
                p_b = _move_geodesic(base_c, gulf_dir, dist_b)
                if k_b == 1:
                    p_b = _move_geodesic(p_b, spine_dir, float(rng.uniform(-0.08, 0.08)) * scale)
                ub = _normalize3(spine_dir - np.dot(spine_dir, p_b) * p_b)
                vb = _normalize3(np.cross(p_b, ub))
                ang_b = float(rng.uniform(-0.6, 0.6))
                ub_r = _normalize3(math.cos(ang_b) * ub + math.sin(ang_b) * vb)
                vb_r = _normalize3(np.cross(p_b, ub_r))
                craton_lobes.append(
                    CratonLobe(
                        plate_id=plate_id,
                        center=p_b.astype(np.float32),
                        axis_u=ub_r.astype(np.float32),
                        axis_v=vb_r.astype(np.float32),
                        rad_u=ru_b,
                        rad_v=rv_b,
                        strength=str_b,
                        is_basin=True,
                    )
                )

        if add_shelf_archipelago:
            tip = _move_geodesic(base_c, spine_dir, 0.42 * scale)
            tip = _move_geodesic(tip, perp_dir, bend_sign * 0.10 * scale)
            ua = _normalize3(spine_dir - np.dot(spine_dir, tip) * tip)
            va = _normalize3(np.cross(tip, ua))
            archipelago_zones.append(
                ArchipelagoZone(
                    center=tip.astype(np.float32),
                    axis_u=ua.astype(np.float32),
                    axis_v=va.astype(np.float32),
                    rad_u=0.28 * scale,
                    rad_v=0.18 * scale,
                    intensity=1.08,
                    is_subpolar=False,
                )
            )

    mid_col = _normalize3(centers[u0] + centers[v0])
    _add_curved_continent(u0, mid_col - centers[u0], scale=1.10, carve_inland_sea=True, add_shelf_archipelago=False)
    _add_curved_continent(v0, mid_col - centers[v0], scale=1.05, carve_inland_sea=False, add_shelf_archipelago=True)

    mid_rift = _normalize3(centers[u1] + centers[v1])
    _add_curved_continent(u1, mid_rift - centers[u1], scale=1.04, carve_inland_sea=False, add_shelf_archipelago=False)
    _add_curved_continent(v1, mid_rift - centers[v1], scale=1.00, carve_inland_sea=True, add_shelf_archipelago=False)

    for idx_s, sc in enumerate(solitary):
        oc_nbrs = [n for n in adj[sc] if not is_continental[n]]
        anchor = None
        if oc_nbrs:
            best_n = oc_nbrs[0]
            best_c = -999.0
            for n in oc_nbrs:
                mid_p = _normalize3(centers[sc] + centers[n])
                n_vec = _normalize3(centers[sc] - centers[n])
                v_sc = np.cross(euler_poles[sc], mid_p)
                v_n = np.cross(euler_poles[n], mid_p)
                conv = float(np.dot(v_n - v_sc, n_vec))
                if conv > best_c:
                    best_c = conv
                    best_n = n
            anchor = centers[best_n] - centers[sc]
        _add_curved_continent(
            sc,
            anchor,
            scale=1.14 if idx_s == 0 else 1.04,
            carve_inland_sea=(idx_s == 0),
            add_shelf_archipelago=(idx_s == len(solitary) - 1),
            extend_high_lat=(idx_s == 1),
        )

    # Rugged subpolar fjord-island archipelagos (like Greenland+Canadian Arctic / Svalbard)
    # Compact in longitude & oriented N-S -> ZERO polar horizontal bars, while bringing
    # equirectangular pixel land fraction naturally into 33-37% alongside 35.0% area land!
    for hemi_sign in (1.0, -1.0):
        lon_a = float(rng.uniform(-math.pi, math.pi))
        for k_sp, (lon_off, lat_lo, lat_hi, ru, rv, inten) in enumerate(
            [
                (0.0, 64.0, 74.0, 0.27, 0.20, 1.38),
                (math.pi * float(rng.uniform(0.62, 0.85)), 62.0, 73.0, 0.23, 0.18, 1.28),
                (math.pi * float(rng.uniform(1.25, 1.52)), 65.0, 75.0, 0.21, 0.16, 1.22),
            ]
        ):
            lon_p = lon_a + lon_off
            lat_p = hemi_sign * float(rng.uniform(math.radians(lat_lo), math.radians(lat_hi)))
            pa = np.array(
                [math.cos(lat_p) * math.cos(lon_p), math.cos(lat_p) * math.sin(lon_p), math.sin(lat_p)],
                dtype=np.float64,
            )
            ea, na = _tangent_basis(pa)
            ang_sp = float(rng.uniform(-0.48, 0.48))
            ua = _normalize3(math.cos(ang_sp) * na + math.sin(ang_sp) * ea)
            va = _normalize3(np.cross(pa, ua))
            archipelago_zones.append(
                ArchipelagoZone(
                    center=pa.astype(np.float32),
                    axis_u=ua.astype(np.float32),
                    axis_v=va.astype(np.float32),
                    rad_u=ru,
                    rad_v=rv,
                    intensity=inten,
                    is_subpolar=True,
                )
            )

    # ------------------------------------------------------------------
    # Build 3 Mantle-Plume Hotspot Chains in Wide Open Oceans
    # ------------------------------------------------------------------
    hotspots: list[HotspotNode] = []
    open_oceans = sorted(
        ocean_plates,
        key=lambda o: (
            min(
                math.acos(np.clip(float(np.dot(centers[o], centers[c])), -1.0, 1.0))
                for c in used
            )
            - 0.55 * abs(centers[o, 2])
            + float(rng.uniform(0.0, 0.14))
        ),
        reverse=True,
    )
    for pl_idx in range(min(3, len(open_oceans))):
        o_plate = open_oceans[pl_idx]
        east, north = _tangent_basis(centers[o_plate])
        ang = float(rng.uniform(0.0, 2.0 * math.pi))
        plume_origin = _move_geodesic(
            centers[o_plate],
            math.cos(ang) * east + math.sin(ang) * north,
            float(rng.uniform(0.02, 0.08)),
        )
        pos = plume_origin.copy()
        n_nodes = int(rng.integers(7, 10))
        for k in range(n_nodes):
            age_frac = k / float(n_nodes - 1)
            if k == 0:
                h_m = float(rng.uniform(5600.0, 6500.0))
                rad = float(rng.uniform(0.015, 0.021))
            elif k <= 2:
                h_m = float(rng.uniform(4600.0, 5350.0)) * (1.0 - 0.11 * k)
                rad = float(rng.uniform(0.011, 0.016))
            elif k == 3:
                h_m = float(rng.uniform(4100.0, 4550.0))
                rad = float(rng.uniform(0.009, 0.014))
            else:
                h_m = float(rng.uniform(2200.0, 3550.0)) * (1.0 - 0.35 * age_frac)
                rad = float(rng.uniform(0.010, 0.015))
            hotspots.append(
                HotspotNode(
                    center=pos.astype(np.float32),
                    radius_rad=rad,
                    height_m=h_m,
                    shelf_rad=rad * 2.2,
                )
            )
            vel = np.cross(euler_poles[o_plate], pos)
            vn = np.linalg.norm(vel)
            if vn < 1e-4:
                east_p, north_p = _tangent_basis(pos)
                step_dir = east_p * 0.6 + north_p * 0.8
            else:
                step_dir = vel / vn
            perp_p = np.cross(pos, step_dir)
            step_dir = _normalize3(step_dir + perp_p * float(rng.uniform(-0.18, 0.18)))
            pos = _move_geodesic(pos, step_dir, float(rng.uniform(0.028, 0.044)))

    diff = centers[:, None, :] - centers[None, :, :]
    pair_diff_len = np.linalg.norm(diff, axis=-1).astype(np.float32)
    np.fill_diagonal(pair_diff_len, 1.0)

    return TectonicWorldSpec(
        seed=seed,
        n_plates=n_plates,
        centers=centers.astype(np.float32),
        euler_poles=euler_poles.astype(np.float32),
        is_continental=is_continental,
        plate_age=plate_age,
        plate_base_bias=plate_base_bias,
        pair_diff_len=pair_diff_len,
        craton_lobes=craton_lobes,
        hotspots=hotspots,
        archipelago_zones=archipelago_zones,
        warp_amp=float(rng.uniform(0.165, 0.195)),
    )


# ---------------------------------------------------------------------------
# Multi-Scale Spherical Base Fields (fast + cache-friendly!)
# ---------------------------------------------------------------------------


def _compute_multiscale_base(
    spec: TectonicWorldSpec,
    noise_warp: SphericalNoise3D,
    noise_detail: SphericalNoise3D,
    width: int,
    height: int,
    lon_shift_deg: float = 0.0,
) -> dict[str, np.ndarray]:
    """Compute low-freq (512x256) and mid-freq (1024x512) spherical fields."""
    work_w = min(width, 512)
    work_h = min(height, 256)
    x, y, z = _sphere_grid_rows(work_w, work_h, 0, work_h, lon_shift_deg=lon_shift_deg)

    wx = (
        noise_warp.octave(x, y, z, 2.2, 0) * np.float32(0.62)
        + noise_warp.octave(x, y, z, 4.6, 1) * np.float32(0.26)
        + noise_warp.octave(x, y, z, 9.4, 2) * np.float32(0.12)
    )
    wy = (
        noise_warp.octave(x, y, z, 2.2, 3) * np.float32(0.62)
        + noise_warp.octave(x, y, z, 4.6, 4) * np.float32(0.26)
        + noise_warp.octave(x, y, z, 9.4, 5) * np.float32(0.12)
    )
    wz = (
        noise_warp.octave(x, y, z, 2.2, 6) * np.float32(0.62)
        + noise_warp.octave(x, y, z, 4.6, 7) * np.float32(0.26)
        + noise_warp.octave(x, y, z, 9.4, 8) * np.float32(0.12)
    )

    cw_x = noise_warp.fbm(x, y, z, 2.6, 3, 2.10, 0.52, idx_offset=10)
    cw_y = noise_warp.fbm(x, y, z, 2.6, 3, 2.10, 0.52, idx_offset=14)
    cw_z = noise_warp.fbm(x, y, z, 2.6, 3, 2.10, 0.52, idx_offset=18)

    fbm_low = noise_detail.fbm(x, y, z, 2.8, 3, 2.12, 0.52, idx_offset=0)
    strike_mod = noise_detail.fbm(x, y, z, 5.2, 2, 2.1, 0.50, idx_offset=12)

    amp_c = np.float32(0.125)
    cx = x + amp_c * cw_x
    cy = y + amp_c * cw_y
    cz = z + amp_c * cw_z
    inv_n = np.float32(1.0) / np.sqrt(cx * cx + cy * cy + cz * cz + np.float32(1e-8))
    cx *= inv_n
    cy *= inv_n
    cz *= inv_n

    amp_b = np.float32(0.145)
    bx = x + amp_b * wx
    by = y + amp_b * wy
    bz = z + amp_b * wz
    inv_b = np.float32(1.0) / np.sqrt(bx * bx + by * by + bz * bz + np.float32(1e-8))
    bx *= inv_b
    by *= inv_b
    bz *= inv_b

    craton_pos = np.zeros((work_h, work_w), dtype=np.float32)
    basin_neg = np.zeros((work_h, work_w), dtype=np.float32)

    for lobe in spec.craton_lobes:
        lc = lobe.center
        lu = lobe.axis_u
        lv = lobe.axis_v
        if lobe.is_basin:
            dot_c = bx * lc[0] + by * lc[1] + bz * lc[2]
            du = bx * lu[0] + by * lu[1] + bz * lu[2]
            dv = bx * lv[0] + by * lv[1] + bz * lv[2]
        else:
            dot_c = cx * lc[0] + cy * lc[1] + cz * lc[2]
            du = cx * lu[0] + cy * lu[1] + cz * lu[2]
            dv = cx * lv[0] + cy * lv[1] + cz * lv[2]

        q2 = (du / np.float32(lobe.rad_u)) ** 2 + (dv / np.float32(lobe.rad_v)) ** 2
        q2 = np.where(dot_c > np.float32(0.15), q2, np.float32(100.0))
        val = np.float32(lobe.strength) * np.exp(np.float32(-0.82) * q2)
        if lobe.is_basin:
            basin_neg = np.maximum(basin_neg, val)
        else:
            craton_pos = np.maximum(craton_pos, val) + np.float32(0.15) * np.minimum(craton_pos, val)

    # Keep only the extreme pole cap clear of solid smear (smooth broad transition!)
    polar_keep = np.float32(1.0) - np.float32(0.88) * _smoothstep(0.960, 0.996, np.abs(z))
    craton_pos *= polar_keep

    arch_shelf = np.zeros((work_h, work_w), dtype=np.float32)
    arch_subpolar = np.zeros((work_h, work_w), dtype=np.float32)
    for az in spec.archipelago_zones:
        ac, au, av = az.center, az.axis_u, az.axis_v
        dot_a = cx * ac[0] + cy * ac[1] + cz * ac[2]
        du = cx * au[0] + cy * au[1] + cz * au[2]
        dv = cx * av[0] + cy * av[1] + cz * av[2]
        q2 = (du / np.float32(az.rad_u)) ** 2 + (dv / np.float32(az.rad_v)) ** 2
        q2 = np.where(dot_a > np.float32(0.20), q2, np.float32(100.0))
        m = np.float32(az.intensity) * np.exp(np.float32(-0.85) * q2)
        if az.is_subpolar:
            arch_subpolar = np.maximum(arch_subpolar, m)
        else:
            arch_shelf = np.maximum(arch_shelf, m)
    arch_subpolar *= polar_keep

    # Mid-frequency fields at 1024x512 (still >8x Nyquist for freq<=27 -> super fast!)
    mid_w = min(width, 1024)
    mid_h = min(height, 512)
    mx, my, mz = _sphere_grid_rows(mid_w, mid_h, 0, mid_h, lon_shift_deg=lon_shift_deg)
    cwx_m = _upsample_equirect(cw_x, mid_w, mid_h, cubic=False)
    cwy_m = _upsample_equirect(cw_y, mid_w, mid_h, cubic=False)
    cwz_m = _upsample_equirect(cw_z, mid_w, mid_h, cubic=False)
    rw_a = np.float32(0.038)
    rx = mx + rw_a * cwx_m
    ry = my + rw_a * cwy_m
    rz = mz + rw_a * cwz_m
    inv_r = np.float32(1.0) / np.sqrt(rx * rx + ry * ry + rz * rz + np.float32(1e-8))
    rx *= inv_r
    ry *= inv_r
    rz *= inv_r
    ridge_mid = noise_detail.ridged_multifractal(rx, ry, rz, 6.2, 3, 2.12, 0.56, 2.0, idx_offset=6)
    p96 = float(np.percentile(ridge_mid, 96.0))
    if p96 > 1e-4:
        ridge_mid = np.clip(ridge_mid / np.float32(p96), np.float32(0.0), np.float32(1.15))

    oct16 = noise_detail.octave(mx, my, mz, 16.0, 18)

    # Keep fields at compact mid_w x mid_h (2 MB each instead of 33.5 MB each!)
    # and upsample ONLY each small chunk strip on the fly -> massive cache & speed win!
    return {
        "mid_w": mid_w,
        "mid_h": mid_h,
        "wx": _upsample_equirect(wx, mid_w, mid_h, cubic=True),
        "wy": _upsample_equirect(wy, mid_w, mid_h, cubic=True),
        "wz": _upsample_equirect(wz, mid_w, mid_h, cubic=True),
        "fbm_low": _upsample_equirect(fbm_low, mid_w, mid_h, cubic=False),
        "ridge_mid": ridge_mid,
        "oct16": oct16,
        "strike_mod": _upsample_equirect(strike_mod, mid_w, mid_h, cubic=False),
        "craton_pos": _upsample_equirect(craton_pos, mid_w, mid_h, cubic=False),
        "basin_neg": _upsample_equirect(basin_neg, mid_w, mid_h, cubic=False),
        "arch_shelf": _upsample_equirect(arch_shelf, mid_w, mid_h, cubic=False),
        "arch_subpolar": _upsample_equirect(arch_subpolar, mid_w, mid_h, cubic=False),
    }


def _sample_strip(
    arr: np.ndarray,
    full_w: int,
    full_h: int,
    r0: int,
    r1: int,
) -> np.ndarray:
    """Fast seam-free bilinear sampling of a mid-res equirectangular field for rows [r0, r1)."""
    src_h, src_w = arr.shape
    nh = r1 - r0
    if src_h == full_h and src_w == full_w:
        return arr[r0:r1].ravel()

    # Exact pixel-center coordinate mapping with periodic horizontal wrap
    fy = ((np.arange(r0, r1, dtype=np.float32) + np.float32(0.5)) * np.float32(src_h / full_h)) - np.float32(0.5)
    fx = ((np.arange(full_w, dtype=np.float32) + np.float32(0.5)) * np.float32(src_w / full_w)) - np.float32(0.5)

    y0 = np.floor(fy).astype(np.int32)
    wy = (fy - y0)[:, None]
    y1 = np.clip(y0 + 1, 0, src_h - 1)
    y0 = np.clip(y0, 0, src_h - 1)

    x0 = np.floor(fx).astype(np.int32)
    wx = (fx - x0)[None, :]
    x1 = (x0 + 1) % src_w
    x0 = x0 % src_w

    # Slice the tiny row band first for L1/L2 cache efficiency
    y_min = int(y0[0])
    y_max = int(y1[-1])
    band = arr[y_min : y_max + 1]
    ry0 = y0 - y_min
    ry1 = y1 - y_min

    # Separable bilinear interpolation (only 2 small 1D passes -> blazing fast!)
    # First interpolate horizontally on the tiny band [y_min..y_max]
    band_h = band[:, x0] * (np.float32(1.0) - wx) + band[:, x1] * wx
    # Then interpolate vertically into (nh, full_w)
    out = band_h[ry0] * (np.float32(1.0) - wy) + band_h[ry1] * wy
    return out.ravel()


# ---------------------------------------------------------------------------
# Boundary Physics Evaluation (Exact Symmetric Top-2-Neighbors, Zero Seams!)
# ---------------------------------------------------------------------------


def _eval_pair_envelopes(
    spec: TectonicWorldSpec,
    p_x: np.ndarray,
    p_y: np.ndarray,
    p_z: np.ndarray,
    pw_x: np.ndarray,
    pw_y: np.ndarray,
    pw_z: np.ndarray,
    u: np.ndarray,
    v: np.ndarray,
    s_uv: np.ndarray,
    gate: np.ndarray,
    strike: np.ndarray,
    fbm_lo_arr: np.ndarray,
    land_presence: np.ndarray,
) -> tuple[np.ndarray, np.ndarray, np.ndarray, np.ndarray, np.ndarray]:
    """Evaluate C1-smooth signed cross-boundary envelopes for canonical pair (u < v).

    Returns 5 smooth arrays (each >= 0 except depress <= 0), multiplied by `gate`:
      - pedestal: smooth uplift base (plateaus, foothill bases, MOR swell, submarine arc bank, outer rise)
      - ridge_amp: smooth alpine/cordillera/rift-shoulder amplitude to multiply full-res ridged multifractal
      - arc_amp: smooth volcanic island-arc peak amplitude to multiply full-res volcanic island beads
      - depress: smooth trenches, continental rifts, axial MOR rifts, transform troughs (<= 0)
      - crust_push: smooth continental crust welding factor
    """
    n_pts = u.shape[0]
    pedestal = np.zeros(n_pts, dtype=np.float32)
    ridge_amp = np.zeros(n_pts, dtype=np.float32)
    arc_amp = np.zeros(n_pts, dtype=np.float32)
    depress = np.zeros(n_pts, dtype=np.float32)
    crust_push = np.zeros(n_pts, dtype=np.float32)

    active = gate > np.float32(1e-5)
    if not np.any(active):
        return pedestal, ridge_amp, arc_amp, depress, crust_push

    idx = np.nonzero(active)[0]
    ua = u[idx]
    vb = v[idx]
    s = s_uv[idx]
    gt = gate[idx]
    px = p_x[idx]
    py = p_y[idx]
    pz = p_z[idx]
    pwx = pw_x[idx]
    pwy = pw_y[idx]
    pwz = pw_z[idx]
    st = strike[idx]
    fl = fbm_lo_arr[idx]
    lp = land_presence[idx]

    # Unit normal tangent to S^2 pointing from plate v into plate u
    cu = spec.centers[ua]
    cv = spec.centers[vb]
    nx = cu[:, 0] - cv[:, 0]
    ny = cu[:, 1] - cv[:, 1]
    nz = cu[:, 2] - cv[:, 2]
    ndot = nx * pwx + ny * pwy + nz * pwz
    nx -= ndot * pwx
    ny -= ndot * pwy
    nz -= ndot * pwz
    nlen = np.sqrt(nx * nx + ny * ny + nz * nz + np.float32(1e-8))
    nx /= nlen
    ny /= nlen
    nz /= nlen

    # Relative velocity dv = v_v(p) - v_u(p) = (omega_v - omega_u) x p
    dw = spec.euler_poles[vb] - spec.euler_poles[ua]
    dvx = dw[:, 1] * pz - dw[:, 2] * py
    dvy = dw[:, 2] * px - dw[:, 0] * pz
    dvz = dw[:, 0] * py - dw[:, 1] * px

    conv = dvx * nx + dvy * ny + dvz * nz
    shx = dvx - conv * nx
    shy = dvy - conv * ny
    shz = dvz - conv * nz
    shear = np.sqrt(shx * shx + shy * shy + shz * shz + np.float32(1e-8))

    cont_u = spec.is_continental[ua]
    cont_v = spec.is_continental[vb]
    age_u = spec.plate_age[ua]
    age_v = spec.plate_age[vb]

    s_abs = np.sqrt(s * s + np.float32(2e-5))  # C-infinity smoothed |s|

    w_mod = np.clip(
        np.float32(0.95) + np.float32(0.62) * fl + np.float32(0.45) * st,
        np.float32(0.42),
        np.float32(1.70),
    )
    depth_mod = np.clip(
        np.float32(0.62) + np.float32(0.68) * st + np.float32(0.38) * fl,
        np.float32(0.14),
        np.float32(1.30),
    )
    pass_mod = np.clip(
        np.float32(0.74) + np.float32(0.52) * st + np.float32(0.25) * fl,
        np.float32(0.18),
        np.float32(1.30),
    )
    fold_wave = np.float32(0.76) + np.float32(0.24) * np.cos(
        s_abs * np.float32(72.0) + st * np.float32(3.2) + fl * np.float32(2.2)
    )

    ped_a = np.zeros_like(s)
    rdg_a = np.zeros_like(s)
    arc_a = np.zeros_like(s)
    dep_a = np.zeros_like(s)
    cp_a = np.zeros_like(s)

    # Smooth regime ramps starting strictly at 0.0 (zero discontinuities!)
    conv_ramp = _smoothstep(0.05, 0.30, conv)
    div_ramp = _smoothstep(0.05, 0.30, -conv)
    trans_ramp = (np.float32(1.0) - _smoothstep(0.06, 0.24, np.abs(conv))) * _smoothstep(0.20, 0.45, shear)

    # 1) CONTINENT-CONTINENT CONVERGENCE: folded alpine chains + high interior plateau
    cc_conv = cont_u & cont_v & (conv > np.float32(0.05))
    if np.any(cc_conv):
        sa = s_abs[cc_conv]
        wm = w_mod[cc_conv]
        c_str = conv_ramp[cc_conv] * (np.float32(0.42) + np.float32(0.95) * np.clip(conv[cc_conv] / np.float32(0.90), np.float32(0.0), np.float32(1.05)))
        cm = _smoothstep(0.05, 0.38, lp[cc_conv])
        scale_c = c_str * (np.float32(0.35) + np.float32(0.65) * cm)
        dc_s = np.maximum(np.float32(0.0), sa + np.float32(0.014) * fl[cc_conv])
        alpine_w = np.float32(0.092) * wm
        plateau_w = np.float32(0.170) * (np.float32(0.55) + np.float32(0.45) * wm)
        alpine_env = np.exp(-((dc_s / alpine_w) ** 2))
        plateau_env = np.exp(-((sa / plateau_w) ** 2)) * _smoothstep(0.015, 0.065, sa)

        ped_a[cc_conv] += (
            np.float32(1650.0) * alpine_env * pass_mod[cc_conv]
            + np.float32(1350.0) * plateau_env
        ) * scale_c
        rdg_a[cc_conv] += (
            np.float32(5250.0) * alpine_env * fold_wave[cc_conv] * pass_mod[cc_conv]
            + np.float32(850.0) * plateau_env
        ) * scale_c
        cp_a[cc_conv] += np.float32(0.60) * np.exp(-((sa / (np.float32(0.125) * wm)) ** 2)) * c_str * cm

    # 2) OCEAN-CONTINENT SUBDUCTION: single C-infinity profile across s_over = 0!
    oc_conv = (cont_u != cont_v) & (conv > np.float32(0.05))
    if np.any(oc_conv):
        orient = np.where(cont_u[oc_conv], np.float32(1.0), np.float32(-1.0))
        so = orient * s[oc_conv]
        wm = w_mod[oc_conv]
        dm = depth_mod[oc_conv]
        c_str = conv_ramp[oc_conv] * (np.float32(0.36) + np.float32(0.95) * np.clip(conv[oc_conv] / np.float32(0.90), np.float32(0.0), np.float32(1.05)))
        cm = _smoothstep(0.12, 0.48, lp[oc_conv])

        cord_off = np.float32(0.054) + np.float32(0.016) * fl[oc_conv]
        cord_w = np.float32(0.044) * wm
        back_w = np.float32(0.068) * (np.float32(0.55) + np.float32(0.45) * wm)
        over_fade = _smoothstep(-0.028, 0.028, so)
        cord_env = np.exp(-(((so - cord_off) / cord_w) ** 2)) * over_fade
        back_env = np.exp(-(((so - (cord_off + np.float32(0.060))) / back_w) ** 2)) * _smoothstep(-0.01, 0.04, so)

        ped_a[oc_conv] += (
            np.float32(1350.0) * cord_env * pass_mod[oc_conv]
            + np.float32(700.0) * back_env
        ) * c_str * cm
        rdg_a[oc_conv] += (
            np.float32(4800.0) * cord_env * fold_wave[oc_conv] * pass_mod[oc_conv]
            + np.float32(750.0) * back_env
        ) * c_str * cm

        tr_off = np.float32(-0.034) - np.float32(0.010) * fl[oc_conv]
        tr_w = np.float32(0.030) * (np.float32(0.54) + np.float32(0.70) * wm)
        trench = np.float32(-3250.0) * np.exp(-(((so - tr_off) / tr_w) ** 2)) * _smoothstep(0.028, -0.018, so) * c_str * dm
        outer_swell = np.float32(340.0) * np.exp(-(((so - (tr_off - np.float32(0.056))) / np.float32(0.036)) ** 2)) * c_str

        ped_a[oc_conv] += outer_swell
        dep_a[oc_conv] += trench
        cp_a[oc_conv] += np.float32(0.34) * np.exp(-(((so - cord_off) / (np.float32(0.048) * wm)) ** 2)) * over_fade * c_str * cm

    # 3) OCEAN-OCEAN SUBDUCTION: smooth submarine arc bank + crisp volcanic island arc + trench!
    oo_conv = (~cont_u) & (~cont_v) & (conv > np.float32(0.08))
    if np.any(oo_conv):
        orient = np.where(age_u[oo_conv] < age_v[oo_conv], np.float32(1.0), np.float32(-1.0))
        so = orient * s[oo_conv]
        wm = w_mod[oo_conv]
        dm = depth_mod[oo_conv]
        c_str = _smoothstep(0.08, 0.34, conv[oo_conv]) * (np.float32(0.40) + np.float32(0.95) * np.clip(conv[oo_conv] / np.float32(0.85), np.float32(0.0), np.float32(1.05)))
        polar_arc = np.float32(1.0) - np.float32(0.70) * _smoothstep(0.92, 0.99, np.abs(pz[oo_conv]))

        arc_off = np.float32(0.048) + np.float32(0.022) * fl[oo_conv]
        over_fade = _smoothstep(-0.028, 0.028, so)
        # Keep the submarine volcanic bank submerged (-1800..-900 m) so the fault never emerges as a solid 5000 km needle wall.
        sub_ridge = np.float32(1550.0) * np.exp(-(((so - arc_off) / (np.float32(0.042) * wm)) ** 2)) * over_fade * c_str
        group_env = _smoothstep(-0.08, 0.55, st[oo_conv])
        volc_core = np.exp(-(((so - arc_off) / (np.float32(0.024) * (np.float32(0.68) + np.float32(0.45) * wm))) ** 2)) * over_fade

        ped_a[oo_conv] += sub_ridge
        # Discrete volcanic islands rise only where both group_env and bead_peak align.
        arc_a[oo_conv] += np.float32(4350.0) * volc_core * group_env * c_str * polar_arc

        tr_off = np.float32(-0.026) - np.float32(0.009) * fl[oo_conv]
        tr_w = np.float32(0.028) * (np.float32(0.54) + np.float32(0.70) * wm)
        trench = np.float32(-3350.0) * np.exp(-(((so - tr_off) / tr_w) ** 2)) * _smoothstep(0.028, -0.018, so) * c_str * dm
        dep_a[oo_conv] += trench

    # 4) CONTINENT-CONTINENT DIVERGENCE: symmetric ragged rift sea + flanking shoulders!
    cc_div = cont_u & cont_v & (conv < np.float32(-0.05))
    if np.any(cc_div):
        sv = s[cc_div]
        sa = s_abs[cc_div]
        wm = w_mod[cc_div]
        d_str = div_ramp[cc_div] * (np.float32(0.36) + np.float32(0.95) * np.clip((-conv[cc_div]) / np.float32(0.85), np.float32(0.0), np.float32(1.05)))
        cm = _smoothstep(0.10, 0.45, lp[cc_div])
        dc_j = np.maximum(np.float32(0.0), sa + np.float32(0.014) * fl[cc_div])
        rift_w = np.float32(0.048) * (np.float32(0.45) + np.float32(0.95) * wm)
        rift_depth_var = np.clip(np.float32(0.65) + np.float32(0.55) * st[cc_div] + np.float32(0.35) * fl[cc_div], np.float32(0.20), np.float32(1.35))
        trough = np.float32(-2250.0) * np.exp(-((dc_j / rift_w) ** 2)) * d_str * rift_depth_var

        sh_off = np.float32(0.072) * (np.float32(0.65) + np.float32(0.50) * wm)
        sh_w = np.float32(0.034) * wm
        sh_env = np.exp(-(((sv - sh_off) / sh_w) ** 2)) + np.exp(-(((sv + sh_off) / sh_w) ** 2))
        cp_env = np.exp(-(((sv - sh_off) / np.float32(0.058)) ** 2)) + np.exp(-(((sv + sh_off) / np.float32(0.058)) ** 2))

        dep_a[cc_div] += trough
        ped_a[cc_div] += np.float32(500.0) * sh_env * d_str * cm
        rdg_a[cc_div] += np.float32(1450.0) * sh_env * d_str * cm
        # Fractured horst islands in wider rift seas
        arc_a[cc_div] += np.float32(1150.0) * np.exp(-((sa / (rift_w * np.float32(0.75))) ** 2)) * d_str * _smoothstep(-0.1, 0.4, st[cc_div])
        cp_a[cc_div] += np.float32(0.45) * cp_env * d_str * cm

    # 5) OCEAN-OCEAN DIVERGENCE: strictly submerged mid-ocean ridges + axial rift + rare rift islands
    oo_div = (~cont_u) & (~cont_v) & (conv < np.float32(-0.05))
    if np.any(oo_div):
        sa = s_abs[oo_div]
        wm = w_mod[oo_div]
        d_str = div_ramp[oo_div] * (np.float32(0.32) + np.float32(0.95) * np.clip((-conv[oo_div]) / np.float32(0.85), np.float32(0.0), np.float32(1.05)))
        mor_w = np.float32(0.082) * (np.float32(0.65) + np.float32(0.45) * wm)
        mor_swell = np.float32(1650.0) * np.exp(-sa / mor_w) * d_str
        axial = np.float32(-420.0) * np.exp(-((sa / (np.float32(0.015) * wm)) ** 2)) * d_str * depth_mod[oo_div]
        polar_ok = np.float32(1.0) - _smoothstep(0.86, 0.96, np.abs(pz[oo_div]))
        # Rare rift volcanic nodes where strike_mod peaks strongly
        rift_surge = _smoothstep(0.45, 0.88, st[oo_div]) * np.exp(-((sa / np.float32(0.018)) ** 2)) * d_str * polar_ok
        ped_a[oo_div] += mor_swell
        arc_a[oo_div] += np.float32(2800.0) * rift_surge
        dep_a[oo_div] += axial

    # 6) TRANSFORM BOUNDARIES: subtle fault troughs & flanking pressure ridges
    trans = trans_ramp > np.float32(1e-4)
    if np.any(trans):
        sv = s[trans]
        sa = s_abs[trans]
        wm = w_mod[trans]
        s_str = trans_ramp[trans] * np.clip(shear[trans] / np.float32(0.95), np.float32(0.0), np.float32(1.15))
        valley_amp = np.where(cont_u[trans] & cont_v[trans], np.float32(-200.0), np.float32(-380.0))
        fault_valley = valley_amp * np.exp(-((sa / (np.float32(0.016) * wm)) ** 2)) * s_str * depth_mod[trans]
        pr_w = np.float32(0.022) * wm
        pr_env = np.exp(-(((sv - np.float32(0.026)) / pr_w) ** 2)) + np.exp(-(((sv + np.float32(0.026)) / pr_w) ** 2))
        dep_a[trans] += fault_valley
        rdg_a[trans] += (
            np.float32(780.0)
            * pr_env
            * s_str
            * np.clip(st[trans] + np.float32(0.25), np.float32(0.0), np.float32(1.0))
            * _smoothstep(0.18, 0.50, lp[trans])
        )

    pedestal[idx] = np.maximum(np.float32(0.0), ped_a) * gt
    ridge_amp[idx] = np.maximum(np.float32(0.0), rdg_a) * gt
    arc_amp[idx] = np.maximum(np.float32(0.0), arc_a) * gt
    depress[idx] = np.minimum(np.float32(0.0), dep_a) * gt
    crust_push[idx] = np.maximum(np.float32(0.0), cp_a) * gt
    return pedestal, ridge_amp, arc_amp, depress, crust_push


def _compute_smooth_tectonic_grid(
    spec: TectonicWorldSpec,
    low: dict[str, np.ndarray],
    crust_zero: np.float32,
    width: int,
    height: int,
    lon_shift_deg: float,
) -> dict[str, np.ndarray]:
    """Compute C-infinity softmax plate ownership + C1 6-pair signed boundary envelopes
    on a dedicated smooth working grid (up to 1536x768) for blazing 4K speed & zero cliffs.
    """
    tw = min(width, 1536)
    th = min(height, 768)
    m = tw * th

    x, y, z = _sphere_grid_rows(tw, th, 0, th, lon_shift_deg=lon_shift_deg)
    xf = x.ravel()
    yf = y.ravel()
    zf = z.ravel()

    wx = _upsample_equirect(low["wx"], tw, th, cubic=False).ravel()
    wy = _upsample_equirect(low["wy"], tw, th, cubic=False).ravel()
    wz = _upsample_equirect(low["wz"], tw, th, cubic=False).ravel()
    warp_amp = np.float32(spec.warp_amp)

    pwx = xf + warp_amp * wx
    pwy = yf + warp_amp * wy
    pwz = zf + warp_amp * wz
    inv_pw = np.float32(1.0) / np.sqrt(pwx * pwx + pwy * pwy + pwz * pwz + np.float32(1e-8))
    pwx *= inv_pw
    pwy *= inv_pw
    pwz *= inv_pw

    pw_mat = np.empty((m, 3), dtype=np.float32)
    pw_mat[:, 0] = pwx
    pw_mat[:, 1] = pwy
    pw_mat[:, 2] = pwz
    dots = pw_mat @ spec.centers.T  # (m, n_plates)

    # Single exp() call gives BOTH beta=19 (pair visibility) and beta=38 (exp^2, plate ownership)!
    dmax = np.max(dots, axis=1, keepdims=True)
    exp_vis = np.exp(np.float32(19.0) * (dots - dmax))
    w_vis = exp_vis / np.sum(exp_vis, axis=1, keepdims=True)
    exp_cont = exp_vis * exp_vis
    w_soft = exp_cont / np.sum(exp_cont, axis=1, keepdims=True)

    is_cont_f32 = spec.is_continental.astype(np.float32)
    w_cont_plate = w_soft @ is_cont_f32
    eff_bias = w_soft @ spec.plate_base_bias
    eff_age = w_soft @ spec.plate_age

    fbm_lo = _upsample_equirect(low["fbm_low"], tw, th, cubic=False).ravel()
    strike = _upsample_equirect(low["strike_mod"], tw, th, cubic=False).ravel()
    craton_pos = _upsample_equirect(low["craton_pos"], tw, th, cubic=False).ravel()
    basin_neg = _upsample_equirect(low["basin_neg"], tw, th, cubic=False).ravel()

    land_rel_pre = craton_pos * w_cont_plate - basin_neg * np.float32(0.85) + fbm_lo * np.float32(0.24) - crust_zero
    land_presence = _smoothstep(-0.20, 0.16, land_rel_pre)

    # Extract top-4 nearest plates
    idx_rows = np.arange(m)
    dots_tmp = dots.copy()
    top_k = np.empty((4, m), dtype=np.int16)
    top_d = np.empty((4, m), dtype=np.float32)
    for k_rank in range(4):
        ik = np.argmax(dots_tmp, axis=1).astype(np.int16)
        top_k[k_rank] = ik
        top_d[k_rank] = dots_tmp[idx_rows, ik]
        dots_tmp[idx_rows, ik] = np.float32(-2.0)

    ped_sq = np.zeros(m, dtype=np.float32)
    rdg_sq = np.zeros(m, dtype=np.float32)
    arc_sq = np.zeros(m, dtype=np.float32)
    dep_sq = np.zeros(m, dtype=np.float32)
    cp_sq = np.zeros(m, dtype=np.float32)

    for ra, rb in ((0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)):
        pa = top_k[ra]
        pb = top_k[rb]
        da = top_d[ra]
        db = top_d[rb]
        swap = pa > pb
        u_id = np.where(swap, pb, pa)
        v_id = np.where(swap, pa, pb)

        wu = w_vis[idx_rows, u_id]
        wv = w_vis[idx_rows, v_id]
        h_uv = (wu * wv) / (wu + wv + np.float32(1e-6))
        # Fast pre-filter: skip arcsin & boundary math where pair_vis is strictly 0!
        vis_mask = h_uv > np.float32(0.010)
        if not np.any(vis_mask):
            continue

        vidx = np.nonzero(vis_mask)[0]
        u_sub = u_id[vidx]
        v_sub = v_id[vidx]
        du_v = np.where(swap[vidx], db[vidx] - da[vidx], da[vidx] - db[vidx])
        pair_vis = _smoothstep(0.010, 0.060, h_uv[vidx])
        s_uv = np.arcsin(np.clip(du_v / spec.pair_diff_len[u_sub, v_sub], np.float32(-1.0), np.float32(1.0)))
        r_win = _smoothstep(0.24, 0.16, np.abs(s_uv))
        gate = pair_vis * r_win

        ped_p, rdg_p, arc_p, dep_p, cp_p = _eval_pair_envelopes(
            spec,
            xf[vidx], yf[vidx], zf[vidx],
            pwx[vidx], pwy[vidx], pwz[vidx],
            u_sub, v_sub, s_uv, gate,
            strike[vidx], fbm_lo[vidx], land_presence[vidx],
        )
        ped_sq[vidx] += ped_p * ped_p
        rdg_sq[vidx] += rdg_p * rdg_p
        arc_sq[vidx] += arc_p * arc_p
        dep_sq[vidx] += dep_p * dep_p
        cp_sq[vidx] += cp_p * cp_p

    # C-infinity permutation-invariant combination across all 6 pairs!
    bound_ped = (np.sqrt(ped_sq + np.float32(1.0)) - np.float32(1.0)).reshape(th, tw)
    bound_rdg = (np.sqrt(rdg_sq + np.float32(1.0)) - np.float32(1.0)).reshape(th, tw)
    bound_arc = (np.sqrt(arc_sq + np.float32(1.0)) - np.float32(1.0)).reshape(th, tw)
    bound_dep = (-(np.sqrt(dep_sq + np.float32(1.0)) - np.float32(1.0))).reshape(th, tw)
    bound_cp = (np.sqrt(cp_sq + np.float32(1e-6)) - np.float32(1e-3)).reshape(th, tw)
    base_ocean_smooth = (np.float32(-4250.0) + eff_bias - eff_age * np.float32(320.0)).reshape(th, tw)

    return {
        "w_cont_plate": w_cont_plate.reshape(th, tw),
        "base_ocean_smooth": base_ocean_smooth,
        "bound_ped": bound_ped,
        "bound_rdg": bound_rdg,
        "bound_arc": bound_arc,
        "bound_dep": bound_dep,
        "bound_cp": bound_cp,
    }


# ---------------------------------------------------------------------------
# Boundary Line Extraction (RFC 7946 lon/lat, O(K) fast, split at antimeridian)
# ---------------------------------------------------------------------------


def _extract_boundary_lines(
    spec: TectonicWorldSpec,
    plate_id_full: np.ndarray,
    lon_shift_deg: float = 0.0,
) -> list[dict]:
    """Extract clean, ordered tectonic fault LineStrings from plate_id."""
    h_full, w_full = plate_id_full.shape
    step_x = max(1, w_full // 1024)
    step_y = max(1, h_full // 512)
    pid = plate_id_full[::step_y, ::step_x]
    h, w = pid.shape

    pair_pixels: dict[tuple[int, int], set[tuple[int, int]]] = {}

    ha = pid[:, :-1]
    hb = pid[:, 1:]
    hmask = ha != hb
    hy, hx = np.nonzero(hmask)
    for y, x, a, b in zip(hy.tolist(), hx.tolist(), ha[hmask].tolist(), hb[hmask].tolist()):
        key = (a, b) if a < b else (b, a)
        pair_pixels.setdefault(key, set()).add((y, x))

    va = pid[:-1, :]
    vb = pid[1:, :]
    vmask = va != vb
    vy, vx = np.nonzero(vmask)
    for y, x, a, b in zip(vy.tolist(), vx.tolist(), va[vmask].tolist(), vb[vmask].tolist()):
        key = (a, b) if a < b else (b, a)
        pair_pixels.setdefault(key, set()).add((y, x))

    nbrs8 = [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (-1, 1), (1, -1), (1, 1)]
    lines: list[dict] = []
    seg_counter = 0

    for (a, b), pix_set in sorted(pair_pixels.items()):
        if len(pix_set) < 5:
            continue

        endpoints = [
            pt for pt in pix_set
            if sum(1 for dy, dx in nbrs8 if (pt[0] + dy, pt[1] + dx) in pix_set) <= 1
        ]
        ep_idx = 0
        unvisited = set(pix_set)

        while unvisited:
            start = None
            while ep_idx < len(endpoints):
                cand = endpoints[ep_idx]
                ep_idx += 1
                if cand in unvisited:
                    start = cand
                    break
            if start is None:
                start = next(iter(unvisited))

            chain = [start]
            unvisited.remove(start)
            cur = start
            while True:
                nxt = None
                for dy, dx in nbrs8:
                    cand = (cur[0] + dy, cur[1] + dx)
                    if cand in unvisited:
                        nxt = cand
                        break
                if nxt is None:
                    break
                chain.append(nxt)
                unvisited.remove(nxt)
                cur = nxt

            if len(chain) < 5:
                continue

            pts_arr = np.array([[c[1], c[0]] for c in chain], dtype=np.float32).reshape(-1, 1, 2)
            approx = cv2.approxPolyDP(pts_arr, epsilon=1.25, closed=False).reshape(-1, 2)
            if len(approx) < 2:
                continue

            coords: list[list[float]] = []
            for px_f, py_f in approx:
                lon = -180.0 + (float(px_f) + 0.5) * (360.0 / w)
                lat = 90.0 - (float(py_f) + 0.5) * (180.0 / h)
                lon = max(-180.0, min(180.0, lon))
                lat = max(-90.0, min(90.0, lat))
                coords.append([round(lon, 4), round(lat, 4)])

            sub_chains: list[list[list[float]]] = [[]]
            for pt in coords:
                if sub_chains[-1] and abs(pt[0] - sub_chains[-1][-1][0]) > 30.0:
                    sub_chains.append([pt])
                else:
                    sub_chains[-1].append(pt)

            for sc in sub_chains:
                if len(sc) < 2:
                    continue
                mid_lon, mid_lat = sc[len(sc) // 2]
                rlat = math.radians(mid_lat)
                rlon = math.radians(mid_lon - lon_shift_deg)
                mp = np.array(
                    [math.cos(rlat) * math.cos(rlon), math.cos(rlat) * math.sin(rlon), math.sin(rlat)],
                    dtype=np.float64,
                )
                n_vec = spec.centers[a].astype(np.float64) - spec.centers[b].astype(np.float64)
                n_vec -= np.dot(n_vec, mp) * mp
                n_norm = np.linalg.norm(n_vec)
                if n_norm > 1e-6:
                    n_vec /= n_norm
                dw = spec.euler_poles[b].astype(np.float64) - spec.euler_poles[a].astype(np.float64)
                dv = np.cross(dw, mp)
                conv = float(np.dot(dv, n_vec))
                both_cont = bool(spec.is_continental[a] and spec.is_continental[b])

                if conv > 0.12:
                    btype = "convergent" if both_cont else "subduction"
                elif conv < -0.10:
                    btype = "divergent"
                else:
                    btype = "transform"

                seg_counter += 1
                lines.append(
                    {
                        "id": f"fault_{seg_counter:03d}_p{a}_p{b}",
                        "boundary_type": btype,
                        "coords_lonlat": sc,
                    }
                )

    return lines


# ---------------------------------------------------------------------------
# Main Public API
# ---------------------------------------------------------------------------


def generate_tectonic_base(
    seed: int,
    width: int,
    height: int,
    target_land_fraction: float = 0.35,
    lon_shift_deg: float = 0.0,
) -> dict:
    """Generate a 3D spherical plate-tectonics heightfield and plate map.

    Args:
        seed: Deterministic integer world seed.
        width: Output equirectangular width in pixels (lon [-180, 180]).
        height: Output equirectangular height in pixels (lat [90, -90]).
        target_land_fraction: Target spherical surface-area land fraction (default 0.35).
        lon_shift_deg: Rigid longitude rotation on S^2 in degrees (positive shifts east).

    Returns:
        Dict with keys:
        - "elevation_m": float32 (height, width) in meters, 0.0 = sea level
        - "plate_id": int16 (height, width)
        - "boundary_lines": list[dict] GeoJSON-ready tectonic fault LineStrings
        - "land_fraction_area_weighted": float
        - "land_fraction_pixels": float
    """
    spec = _build_tectonic_spec(seed)
    noise_warp = SphericalNoise3D(seed * 3571 + 17)
    noise_detail = SphericalNoise3D(seed * 6133 + 911)

    low = _compute_multiscale_base(spec, noise_warp, noise_detail, width, height, lon_shift_deg=lon_shift_deg)

    elevation = np.empty((height, width), dtype=np.float32)
    plate_id = np.empty((height, width), dtype=np.int16)

    centers_T = spec.centers.T.copy()  # (3, N) float32
    warp_amp = np.float32(spec.warp_amp)

    # Coarse calibration pass using the exact same C-infinity N-plate softmax!
    is_cont_f32 = spec.is_continental.astype(np.float32)
    cal_w, cal_h = 320, 160
    cx0, cy0, cz0 = _sphere_grid_rows(cal_w, cal_h, 0, cal_h, lon_shift_deg=lon_shift_deg)
    wx_c = cv2.resize(low["wx"], (cal_w, cal_h), interpolation=cv2.INTER_AREA)
    wy_c = cv2.resize(low["wy"], (cal_w, cal_h), interpolation=cv2.INTER_AREA)
    wz_c = cv2.resize(low["wz"], (cal_w, cal_h), interpolation=cv2.INTER_AREA)
    pwx_c = cx0 + warp_amp * wx_c
    pwy_c = cy0 + warp_amp * wy_c
    pwz_c = cz0 + warp_amp * wz_c
    inv_c = np.float32(1.0) / np.sqrt(pwx_c * pwx_c + pwy_c * pwy_c + pwz_c * pwz_c + np.float32(1e-8))
    pw_c = np.stack([pwx_c * inv_c, pwy_c * inv_c, pwz_c * inv_c], axis=-1).reshape(-1, 3)
    dots_c = pw_c @ centers_T
    dmax_c = np.max(dots_c, axis=1, keepdims=True)
    exp_c = np.exp(np.float32(38.0) * (dots_c - dmax_c))
    w_soft_c = exp_c / np.sum(exp_c, axis=1, keepdims=True)
    cw_cont = (w_soft_c @ is_cont_f32).reshape(cal_h, cal_w)

    cal_rows = np.arange(cal_h, dtype=np.float64)
    cal_wgt = np.cos((math.pi * 0.5) - (cal_rows + 0.5) * (math.pi / cal_h))
    cal_wgt /= np.sum(cal_wgt)

    cal_craton = cv2.resize(low["craton_pos"], (cal_w, cal_h), interpolation=cv2.INTER_AREA) * cw_cont
    cal_basin = cv2.resize(low["basin_neg"], (cal_w, cal_h), interpolation=cv2.INTER_AREA)
    cal_fbm = cv2.resize(low["fbm_low"], (cal_w, cal_h), interpolation=cv2.INTER_AREA)
    cal_subp = cv2.resize(low["arch_subpolar"], (cal_w, cal_h), interpolation=cv2.INTER_AREA)
    cal_sub_fade = _smoothstep(0.02, 0.24, cal_subp)
    cal_sub_score = np.float32(-0.60) + cal_sub_fade * (cal_subp * (np.float32(0.46) + cal_fbm * np.float32(0.35)) + np.float32(0.60))
    cal_score = np.maximum(
        cal_craton - cal_basin * np.float32(0.85) + cal_fbm * np.float32(0.24),
        cal_sub_score,
    )
    cont_target = max(0.08, target_land_fraction - 0.025)
    lo_t, hi_t = -0.2, 1.4
    for _ in range(24):
        mt = 0.5 * (lo_t + hi_t)
        frac_c = float(np.dot(np.mean(cal_score > np.float32(mt), axis=1), cal_wgt))
        if frac_c > cont_target:
            lo_t = mt
        else:
            hi_t = mt
    crust_zero = np.float32(0.5 * (lo_t + hi_t))

    # Precompute smooth C-infinity multi-plate ownership + C1 6-pair signed boundary envelopes
    # at up to 1536x768 (7.1x fewer pixels than 4K -> blazing fast & guaranteed smooth!),
    # while applying full-4K ridged alpine spurs, crisp volcanic islands, hotspots, and plate_id!
    tect = _compute_smooth_tectonic_grid(spec, low, crust_zero, width, height, lon_shift_deg)

    chunk_rows = max(32, min(height, 524288 // max(1, width)))

    hs_centers = np.array([h.center for h in spec.hotspots], dtype=np.float32)
    hs_rad = np.maximum(np.array([h.radius_rad for h in spec.hotspots], dtype=np.float32), np.float32(0.013))
    hs_h = np.array([h.height_m for h in spec.hotspots], dtype=np.float32)
    hs_shelf = np.array([h.shelf_rad for h in spec.hotspots], dtype=np.float32)

    for r0 in range(0, height, chunk_rows):
        r1 = min(height, r0 + chunk_rows)
        nh = r1 - r0
        m = nh * width

        x, y, z = _sphere_grid_rows(width, height, r0, r1, lon_shift_deg=lon_shift_deg)
        xf = x.ravel()
        yf = y.ravel()
        zf = z.ravel()

        wx_s = _sample_strip(low["wx"], width, height, r0, r1)
        wy_s = _sample_strip(low["wy"], width, height, r0, r1)
        wz_s = _sample_strip(low["wz"], width, height, r0, r1)

        pwx = xf + warp_amp * wx_s
        pwy = yf + warp_amp * wy_s
        pwz = zf + warp_amp * wz_s
        inv_pw = np.float32(1.0) / np.sqrt(pwx * pwx + pwy * pwy + pwz * pwz + np.float32(1e-8))
        pwx *= inv_pw
        pwy *= inv_pw
        pwz *= inv_pw

        # Exact full-resolution plate_id via single fast BLAS SGEMM
        pw_mat = np.empty((m, 3), dtype=np.float32)
        pw_mat[:, 0] = pwx
        pw_mat[:, 1] = pwy
        pw_mat[:, 2] = pwz
        plate_id[r0:r1] = np.argmax(pw_mat @ centers_T, axis=1).astype(np.int16).reshape(nh, width)

        # 2 full-res 3D spherical octaves -> fast (~14s at 4K) & razor-sharp 4K ridges/fjords!
        oct16 = _sample_strip(low["oct16"], width, height, r0, r1)
        oct36 = noise_detail.octave(xf, yf, zf, 36.0, 19)
        oct78 = noise_detail.octave(xf, yf, zf, 78.0, 20)
        fbm_hi = oct16 * np.float32(0.55) + oct36 * np.float32(0.30) + oct78 * np.float32(0.15)

        r_hi1 = np.float32(1.0) - np.abs(oct36)
        r_hi1 *= r_hi1
        r_hi2 = np.float32(1.0) - np.abs(oct78)
        r_hi2 *= r_hi2

        ridge_mid = _sample_strip(low["ridge_mid"], width, height, r0, r1)
        ridge_full = np.clip(
            ridge_mid * np.float32(0.72)
            + (np.float32(0.20) + np.float32(0.80) * ridge_mid) * (r_hi1 * np.float32(0.24) + r_hi1 * r_hi2 * np.float32(0.11)),
            np.float32(0.0),
            np.float32(1.16),
        )

        # Smooth C-infinity volcanic island beads (discrete spaced cones, never a continuous thin wall)
        b1 = np.exp(np.float32(-3.2) * (oct16 * oct16))
        b2 = np.float32(0.5) + np.float32(0.5) * oct36
        arc_beads = np.clip(b1 * np.float32(0.74) + b2 * np.float32(0.40) - np.float32(0.14), np.float32(0.0), np.float32(1.18))
        bead_peak = _smoothstep(0.48, 0.92, arc_beads) * (np.float32(0.55) + np.float32(0.55) * ridge_full)

        fbm_lo = _sample_strip(low["fbm_low"], width, height, r0, r1)
        strike = _sample_strip(low["strike_mod"], width, height, r0, r1)
        craton_pos = _sample_strip(low["craton_pos"], width, height, r0, r1)
        basin_neg = _sample_strip(low["basin_neg"], width, height, r0, r1)
        arch_shelf = _sample_strip(low["arch_shelf"], width, height, r0, r1)
        arch_subpolar = _sample_strip(low["arch_subpolar"], width, height, r0, r1)

        # Sample smooth C1 tectonic envelopes cleanly at full resolution
        w_cont_plate = _sample_strip(tect["w_cont_plate"], width, height, r0, r1)
        base_ocean = _sample_strip(tect["base_ocean_smooth"], width, height, r0, r1)
        bound_ped = _sample_strip(tect["bound_ped"], width, height, r0, r1)
        bound_rdg = _sample_strip(tect["bound_rdg"], width, height, r0, r1)
        bound_arc = _sample_strip(tect["bound_arc"], width, height, r0, r1)
        bound_dep = _sample_strip(tect["bound_dep"], width, height, r0, r1)
        bound_cp = _sample_strip(tect["bound_cp"], width, height, r0, r1)

        # Combine smooth tectonic envelopes with full-4K jagged ridged mountains & volcanic beads!
        bound_up = bound_ped + bound_rdg * ridge_full + bound_arc * bead_peak

        # Smooth continental slope (no high-freq oct78 inside 4050m abyssal slope!)
        # + razor-sharp full-4K coastlines/fjords on the shelf & land transition!
        coast_lo = fbm_lo * np.float32(0.24) + oct16 * np.float32(0.065)
        coast_hi = oct36 * np.float32(0.036) + oct78 * np.float32(0.018)

        crust_base = (craton_pos + bound_cp * np.float32(0.45)) * w_cont_plate - basin_neg * np.float32(0.85) - crust_zero
        crust_slope = crust_base + coast_lo
        crust_rel = crust_slope + coast_hi

        # C1-smooth continental-shelf & subpolar archipelagos
        shelf_isl = arch_shelf * (arc_beads * np.float32(0.85) + fbm_lo * np.float32(0.22) - np.float32(0.32)) - np.float32(0.02)
        shelf_fade = _smoothstep(0.02, 0.24, arch_shelf)
        shelf_term = np.float32(-0.65) + shelf_fade * (shelf_isl + np.float32(0.65))
        crust_slope = np.maximum(crust_slope, shelf_term)
        crust_rel = np.maximum(crust_rel, shelf_term)

        sub_isl = arch_subpolar * (
            np.float32(0.50) + fbm_lo * np.float32(0.42) + fbm_hi * np.float32(0.24) + (arc_beads - np.float32(0.30)) * np.float32(0.34)
        ) - np.float32(0.27)
        sub_fade = _smoothstep(0.02, 0.24, arch_subpolar)
        sub_term = np.float32(-0.65) + sub_fade * (sub_isl + np.float32(0.65))
        crust_slope = np.maximum(crust_slope, sub_term)
        crust_rel = np.maximum(crust_rel, sub_term)

        base_ocean += fbm_lo * np.float32(250.0) + fbm_hi * np.float32(105.0)

        # Realistic smooth continental slope (-0.34..-0.04) + crisp 4K shelf/shoreline!
        slope_t = _smoothstep(-0.34, -0.04, crust_slope)
        shelf_t = _smoothstep(-0.07, 0.02, crust_rel)
        interior_t = _smoothstep(0.0, 0.42, crust_rel)

        elev = base_ocean + slope_t * np.float32(4050.0) + shelf_t * np.float32(230.0) + interior_t * np.float32(340.0)

        ancient_mask = _smoothstep(0.10, 0.42, crust_rel) * _smoothstep(-0.05, 0.55, strike)
        interior_relief = (
            shelf_t
            * (
                np.float32(230.0) * np.clip(fbm_lo + np.float32(0.35), np.float32(0.0), np.float32(1.3))
                + np.float32(1650.0) * (ridge_full * ridge_full) * ancient_mask
                + np.float32(130.0) * fbm_hi
            )
        )
        elev += interior_relief
        elev += bound_up + bound_dep

        basin_depth = _smoothstep(0.46, 0.92, basin_neg) * (np.float32(-1450.0) - np.float32(350.0) * np.abs(fbm_lo))
        elev += basin_depth

        # Mantle-plume hotspot chains in open oceans
        if len(spec.hotspots) > 0:
            p_mat = np.empty((m, 3), dtype=np.float32)
            p_mat[:, 0] = xf
            p_mat[:, 1] = yf
            p_mat[:, 2] = zf
            hs_dots = p_mat @ hs_centers.T
            max_dot = np.max(hs_dots, axis=1)
            near_hs = max_dot > np.float32(0.985)
            if np.any(near_hs):
                hidx = np.nonzero(near_hs)[0]
                hdots = np.clip(hs_dots[hidx], np.float32(-1.0), np.float32(1.0))
                hang = np.arccos(hdots)
                cone = hs_h[None, :] * np.exp(-((hang / hs_rad[None, :]) ** 2))
                apron = (hs_h[None, :] * np.float32(0.30)) * np.exp(-((hang / hs_shelf[None, :]) ** 2))
                hs_add = np.max(cone + apron, axis=1)
                hs_add *= np.float32(0.84) + np.float32(0.26) * r_hi1[hidx] + np.float32(0.10) * fbm_hi[hidx]
                elev[hidx] += hs_add

        elevation[r0:r1] = elev.reshape(nh, width)

    # ------------------------------------------------------------------
    # Exact Area-Weighted Sea-Level Calibration + Robust Mountain/Trench Scaling
    # ------------------------------------------------------------------
    rows = np.arange(height, dtype=np.float64)
    lat_rad = (math.pi * 0.5) - (rows + 0.5) * (math.pi / height)
    row_weights = np.cos(lat_rad)
    row_weights /= np.sum(row_weights)

    lo_s = float(np.percentile(elevation[::4, ::4], 5.0))
    hi_s = float(np.percentile(elevation[::4, ::4], 98.0))
    for _ in range(28):
        mid = 0.5 * (lo_s + hi_s)
        land_by_row = np.mean(elevation > np.float32(mid), axis=1, dtype=np.float64)
        frac = float(np.dot(land_by_row, row_weights))
        if frac > target_land_fraction:
            lo_s = mid
        else:
            hi_s = mid
    sea_shift = np.float32(0.5 * (lo_s + hi_s))
    elevation -= sea_shift

    pos_mask = elevation > np.float32(0.0)
    neg_mask = elevation < np.float32(0.0)
    if np.any(pos_mask):
        pos_sample = elevation[::2, ::2]
        pos_sample = pos_sample[pos_sample > np.float32(0.0)]
        if pos_sample.size > 100:
            p9985 = float(np.percentile(pos_sample, 99.85))
            target_high = 5350.0 + float((seed * 197) % 500)
            if p9985 > 200.0:
                scale_pos = np.float32(np.clip(target_high / p9985, 0.72, 1.55))
                elevation[pos_mask] *= scale_pos
        hi_m = elevation > np.float32(6200.0)
        if np.any(hi_m):
            excess = elevation[hi_m] - np.float32(6200.0)
            elevation[hi_m] = np.float32(6200.0) + np.float32(1250.0) * np.tanh(excess / np.float32(1500.0))

    if np.any(neg_mask):
        min_neg = float(np.min(elevation[neg_mask]))
        target_trench = -7850.0 - float((seed * 173) % 550)
        if min_neg < -500.0:
            scale_neg = np.float32(np.clip(target_trench / min_neg, 0.78, 1.32))
            elevation[neg_mask] *= scale_neg

    np.clip(elevation, np.float32(-8500.0), np.float32(7500.0), out=elevation)

    land_mask = elevation > np.float32(0.0)
    land_by_row = np.mean(land_mask, axis=1, dtype=np.float64)
    land_fraction_area = float(np.dot(land_by_row, row_weights))
    land_fraction_px = float(np.mean(land_mask, dtype=np.float64))

    boundary_lines = _extract_boundary_lines(spec, plate_id, lon_shift_deg=lon_shift_deg)

    return {
        "elevation_m": elevation,
        "plate_id": plate_id,
        "boundary_lines": boundary_lines,
        "land_fraction_area_weighted": land_fraction_area,
        "land_fraction_pixels": land_fraction_px,
    }


# ---------------------------------------------------------------------------
# Candidate Atlas Shaded-Relief Preview Renderer
# ---------------------------------------------------------------------------


def _colorize_relief(elevation_m: np.ndarray) -> np.ndarray:
    """Render a crisp shaded-relief atlas RGB uint8 image with ocean shelves & coastlines."""
    h, w = elevation_m.shape
    elev = elevation_m.astype(np.float32)

    ocean_stops = np.array([-8500.0, -6000.0, -4200.0, -2500.0, -700.0, -220.0, -30.0, 0.0], dtype=np.float32)
    ocean_rgb = np.array(
        [
            [11, 29, 64],    # hadal trench deep cobalt
            [16, 44, 88],    # deep abyssal
            [22, 62, 112],   # abyssal plain
            [33, 86, 138],   # mid-ocean ridge
            [45, 120, 162],  # continental slope
            [70, 166, 188],  # shallow turquoise shelf
            [112, 200, 204], # coastal shoal
            [132, 212, 210], # shoreline water
        ],
        dtype=np.float32,
    )

    land_stops = np.array([0.0, 180.0, 520.0, 1150.0, 2100.0, 3300.0, 4450.0, 7500.0], dtype=np.float32)
    land_rgb = np.array(
        [
            [74, 124, 72],   # coastal lowland green
            [102, 146, 84],  # green plains
            [154, 166, 100], # rolling uplands
            [184, 156, 102], # foothills / plateau
            [156, 118, 82],  # mountain flanks
            [124, 104, 92],  # alpine rock
            [222, 224, 232], # high snowline
            [250, 252, 255], # glacial peaks
        ],
        dtype=np.float32,
    )

    rgb = np.empty((h, w, 3), dtype=np.float32)
    is_land = elev > 0.0
    for ch in range(3):
        oc_ch = np.interp(elev, ocean_stops, ocean_rgb[:, ch])
        ln_ch = np.interp(elev, land_stops, land_rgb[:, ch])
        rgb[:, :, ch] = np.where(is_land, ln_ch, oc_ch)

    rows = np.arange(h, dtype=np.float32)
    lat = np.float32(math.pi * 0.5) - (rows + np.float32(0.5)) * np.float32(math.pi / h)
    cos_lat = np.maximum(np.cos(lat), np.float32(0.18))[:, None]

    dz_dx = (np.roll(elev, -1, axis=1) - np.roll(elev, 1, axis=1)) / (np.float32(2.0) * cos_lat)
    dz_dy = np.gradient(elev, axis=0)

    exag = np.float32(0.0016 * (w / 1280.0))
    gx = dz_dx * exag
    gy = dz_dy * exag
    inv_len = np.float32(1.0) / np.sqrt(gx * gx + gy * gy + np.float32(1.0))
    nx = -gx * inv_len
    ny = -gy * inv_len
    nz = inv_len

    l1 = np.array([-0.55, -0.55, 0.62], dtype=np.float32)
    l1 /= np.linalg.norm(l1)
    l2 = np.array([0.25, -0.50, 0.83], dtype=np.float32)
    l2 /= np.linalg.norm(l2)

    hs1 = np.clip(nx * l1[0] + ny * l1[1] + nz * l1[2], 0.0, 1.0)
    hs2 = np.clip(nx * l2[0] + ny * l2[1] + nz * l2[2], 0.0, 1.0)
    shade = np.float32(0.75) * hs1 + np.float32(0.25) * hs2

    land_mul = np.float32(0.48) + np.float32(0.92) * shade
    ocean_mul = np.float32(0.76) + np.float32(0.42) * shade
    mul = np.where(is_land, land_mul, ocean_mul)[:, :, None]
    rgb *= mul

    land_u8 = is_land.astype(np.uint8)
    eroded = cv2.erode(land_u8, np.ones((3, 3), np.uint8), iterations=1)
    coast = (land_u8 == 1) & (eroded == 0)
    rgb[coast] = rgb[coast] * np.float32(0.42) + np.array([20, 32, 40], dtype=np.float32) * np.float32(0.58)

    out = np.clip(rgb, 0, 255).astype(np.uint8)
    for deg in range(-60, 90, 30):
        ry = int(round((90.0 - deg) / 180.0 * h))
        if 0 <= ry < h:
            out[ry, :, :] = (out[ry, :, :].astype(np.float32) * 0.86 + 255.0 * 0.14).astype(np.uint8)
    for deg in range(-150, 180, 30):
        cx = int(round((deg + 180.0) / 360.0 * w))
        if 0 <= cx < w:
            out[:, cx, :] = (out[:, cx, :].astype(np.float32) * 0.86 + 255.0 * 0.14).astype(np.uint8)

    return out


def render_candidate_png(result: dict, seed: int, out_path: Path) -> None:
    """Save a shaded-relief candidate world preview PNG with stats badge."""
    out_path.parent.mkdir(parents=True, exist_ok=True)
    rgb = _colorize_relief(result["elevation_m"])
    img = Image.fromarray(rgb, mode="RGB")
    draw = ImageDraw.Draw(img, "RGBA")

    elev = result["elevation_m"]
    n_plates = int(np.max(result["plate_id"])) + 1
    label = (
        f"Seed {seed} | Area Land: {result['land_fraction_area_weighted']*100:.2f}% | "
        f"Pixel Land: {result['land_fraction_pixels']*100:.2f}% | "
        f"Elev: {float(elev.min()):.0f}m..+{float(elev.max()):.0f}m | "
        f"Plates: {n_plates} | Faults: {len(result['boundary_lines'])}"
    )
    font = ImageFont.load_default()
    bbox = draw.textbbox((0, 0), label, font=font)
    tw, th = bbox[2] - bbox[0], bbox[3] - bbox[1]
    draw.rectangle([8, 8, 22 + tw, 18 + th], fill=(10, 16, 26, 195))
    draw.text((15, 13), label, fill=(238, 244, 250, 255), font=font)

    img.save(out_path, optimize=True)


# ---------------------------------------------------------------------------
# Self-Test & Candidate Generation Entrypoint
# ---------------------------------------------------------------------------


if __name__ == "__main__":
    out_dir = Path(__file__).resolve().parent / "candidates"
    out_dir.mkdir(parents=True, exist_ok=True)

    print("=== Generating 4 candidate seeds at 1280x640 ===")
    for s in (1, 2, 3, 4):
        t0 = time.perf_counter()
        res = generate_tectonic_base(seed=s, width=1280, height=640, target_land_fraction=0.35)
        dt = time.perf_counter() - t0
        elev = res["elevation_m"]
        out_png = out_dir / f"seed_{s}.png"
        render_candidate_png(res, s, out_png)
        btypes: dict[str, int] = {}
        for bl in res["boundary_lines"]:
            bt = bl["boundary_type"]
            btypes[bt] = btypes.get(bt, 0) + 1
        land_elev = elev[elev > 0]
        ocean_elev = elev[elev <= 0]
        print(
            f"seed={s}: {dt:.2f}s | area_land={res['land_fraction_area_weighted']:.4f} "
            f"| px_land={res['land_fraction_pixels']:.4f} "
            f"| land_med={float(np.median(land_elev)):.0f}m p95={float(np.percentile(land_elev, 95)):.0f}m "
            f"ocean_med={float(np.median(ocean_elev)):.0f}m "
            f"| elev=[{elev.min():.0f}, {elev.max():.0f}]m "
            f"| plates={int(res['plate_id'].max())+1} | faults={btypes}"
        )

    print("\n=== Generating seed_3_polished.png (1280x640, lon_shift_deg=+16.0) ===")
    t0 = time.perf_counter()
    res_pol = generate_tectonic_base(seed=3, width=1280, height=640, target_land_fraction=0.35, lon_shift_deg=16.0)
    dt_pol = time.perf_counter() - t0
    pol_png = out_dir / "seed_3_polished.png"
    render_candidate_png(res_pol, 3, pol_png)
    print(
        f"seed=3 (shift=+16.0): {dt_pol:.2f}s | area_land={res_pol['land_fraction_area_weighted']:.4f} "
        f"| px_land={res_pol['land_fraction_pixels']:.4f} "
        f"| elev=[{res_pol['elevation_m'].min():.0f}, {res_pol['elevation_m'].max():.0f}]m "
        f"| faults={len(res_pol['boundary_lines'])} -> {pol_png}"
    )

    print("\n=== Benchmarking 4096x2048 (seed=3, lon_shift_deg=+16.0) ===")
    t0 = time.perf_counter()
    res4k = generate_tectonic_base(seed=3, width=4096, height=2048, target_land_fraction=0.35, lon_shift_deg=16.0)
    dt4k = time.perf_counter() - t0
    elev4k = res4k["elevation_m"]
    print(
        f"4096x2048: {dt4k:.2f}s | shape={elev4k.shape} dtype={elev4k.dtype} "
        f"| area_land={res4k['land_fraction_area_weighted']:.4f} "
        f"| px_land={res4k['land_fraction_pixels']:.4f} "
        f"| elev=[{elev4k.min():.0f}, {elev4k.max():.0f}]m "
        f"| plate_id={res4k['plate_id'].dtype} | faults={len(res4k['boundary_lines'])}"
    )
    assert dt4k < 30.0, f"4K runtime {dt4k:.2f}s exceeded 30s budget!"
    assert elev4k.shape == (2048, 4096) and elev4k.dtype == np.float32
    assert res4k["plate_id"].shape == (2048, 4096) and res4k["plate_id"].dtype == np.int16
    assert abs(res4k["land_fraction_area_weighted"] - 0.35) < 0.01

    # Automated max-gradient continuity check (ZERO artificial Voronoi step cliffs!)
    dx_4k = np.abs(np.diff(elev4k, axis=1))
    dx_wrap = np.abs(elev4k[:, 0] - elev4k[:, -1])
    dy_4k = np.abs(np.diff(elev4k, axis=0))
    max_dx = max(float(dx_4k.max()), float(dx_wrap.max()))
    max_dy = float(dy_4k.max())
    max_jump = max(max_dx, max_dy)
    p9999_dx = float(np.percentile(dx_4k, 99.99))
    p9999_dy = float(np.percentile(dy_4k, 99.99))
    over_1200 = int((dx_4k > 1200.0).sum() + (dx_wrap > 1200.0).sum() + (dy_4k > 1200.0).sum())
    print(
        f"4K continuity check: max_dx={max_dx:.1f} m/px, max_dy={max_dy:.1f} m/px, "
        f"p99.99_dx={p9999_dx:.1f} m/px, p99.99_dy={p9999_dy:.1f} m/px, >1200m_edges={over_1200}"
    )
    assert max_jump < 1200.0, f"Found artificial 1-px step cliff: {max_jump:.1f} m/px >= 1200 m/px!"

    # Verify boundary_lines schema & RFC 7946 bounds
    valid_types = {"convergent", "subduction", "divergent", "transform"}
    for bl in res4k["boundary_lines"]:
        assert set(bl.keys()) == {"id", "boundary_type", "coords_lonlat"}
        assert bl["boundary_type"] in valid_types
        assert len(bl["coords_lonlat"]) >= 2
        for lon, lat in bl["coords_lonlat"]:
            assert -180.0 <= lon <= 180.0 and -90.0 <= lat <= 90.0
    print("ALL SELF-CHECKS PASSED.")
