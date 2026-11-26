"""Realistic dendritic mountain erosion, watershed drainage, and wind-advected
Whittaker climate engine for Zeta's pre-humanoid world map.

Exports:
    erode_and_compute_climate(elevation_m: np.ndarray, seed: int) -> dict
"""

from __future__ import annotations

import math
import time
from typing import Any

import cv2
import numpy as np
from scipy.ndimage import distance_transform_edt
from scipy.sparse import csr_matrix
from scipy.sparse.csgraph import (
    breadth_first_order,
    dijkstra,
    minimum_spanning_tree,
)


# ---------------------------------------------------------------------------
# Coordinate helpers (pixel-center equirectangular convention)
# ---------------------------------------------------------------------------

def _px_to_lonlat(col: float, row: float, width: int, height: int) -> tuple[float, float]:
    """Convert pixel center (col + 0.5, row + 0.5) to RFC 7946 (lon, lat)."""
    lon = -180.0 + ((col + 0.5) / float(width)) * 360.0
    lat = 90.0 - ((row + 0.5) / float(height)) * 180.0
    return (
        float(max(-180.0, min(180.0, lon))),
        float(max(-90.0, min(90.0, lat))),
    )


def _xy_to_lonlat(x: float, y: float, width: int, height: int) -> tuple[float, float]:
    """Convert continuous pixel-edge coordinate (x in [0, W], y in [0, H]) to (lon, lat)."""
    lon = -180.0 + (x / float(width)) * 360.0
    lat = 90.0 - (y / float(height)) * 180.0
    return (
        float(max(-180.0, min(180.0, lon))),
        float(max(-90.0, min(90.0, lat))),
    )


def _lonlat_to_rowcol(lon: float, lat: float, width: int, height: int) -> tuple[int, int]:
    """Inverse of pixel-center mapping: (lon, lat) -> (row, col)."""
    col = int(np.clip(math.floor((lon + 180.0) / 360.0 * width), 0, width - 1))
    row = int(np.clip(math.floor((90.0 - lat) / 180.0 * height), 0, height - 1))
    return row, col


def _smoothstep(edge0: float, edge1: float, x: np.ndarray) -> np.ndarray:
    t = np.clip((x - edge0) / (edge1 - edge0), 0.0, 1.0)
    return t * t * (3.0 - 2.0 * t)


# ---------------------------------------------------------------------------
# Fast 3D spherical Perlin gradient noise & heterogeneous ridged multifractal
# ---------------------------------------------------------------------------

_GRAD3 = np.array(
    [
        [1, 1, 0], [-1, 1, 0], [1, -1, 0], [-1, -1, 0],
        [1, 0, 1], [-1, 0, 1], [1, 0, -1], [-1, 0, -1],
        [0, 1, 1], [0, -1, 1], [0, 1, -1], [0, -1, -1],
        [1, 1, 0], [-1, 1, 0], [0, -1, 1], [0, -1, -1],
    ],
    dtype=np.float32,
)


def _make_perm(rng: np.random.Generator) -> np.ndarray:
    p = rng.permutation(256).astype(np.int32)
    return np.concatenate([p, p])


def _random_rot3(rng: np.random.Generator) -> np.ndarray:
    """Return a seeded 3x3 orthogonal rotation matrix to avoid lattice alignment."""
    m = rng.standard_normal((3, 3))
    q, r = np.linalg.qr(m)
    d = np.sign(np.diag(r))
    d[d == 0] = 1.0
    q = q * d
    if np.linalg.det(q) < 0:
        q[:, 0] = -q[:, 0]
    return q.astype(np.float32)


def _grad_dot3(h: np.ndarray, dx: np.ndarray, dy: np.ndarray, dz: np.ndarray) -> np.ndarray:
    g = _GRAD3[h & 15]
    return g[:, 0] * dx + g[:, 1] * dy + g[:, 2] * dz


def _perlin3d_vec(
    x: np.ndarray,
    y: np.ndarray,
    z: np.ndarray,
    perm: np.ndarray,
) -> np.ndarray:
    """Vectorized 3D Perlin gradient noise for 1D float32 coordinate arrays."""
    xi = np.floor(x).astype(np.int32)
    yi = np.floor(y).astype(np.int32)
    zi = np.floor(z).astype(np.int32)

    xf = x - xi
    yf = y - yi
    zf = z - zi

    u = xf * xf * xf * (xf * (xf * 6.0 - 15.0) + 10.0)
    v = yf * yf * yf * (yf * (yf * 6.0 - 15.0) + 10.0)
    w = zf * zf * zf * (zf * (zf * 6.0 - 15.0) + 10.0)

    X = xi & 255
    Y = yi & 255
    Z = zi & 255

    A = perm[X] + Y
    AA = perm[A] + Z
    AB = perm[A + 1] + Z
    B = perm[X + 1] + Y
    BA = perm[B] + Z
    BB = perm[B + 1] + Z

    x00 = _grad_dot3(perm[AA], xf, yf, zf)
    x10 = _grad_dot3(perm[BA], xf - 1.0, yf, zf)
    x01 = _grad_dot3(perm[AB], xf, yf - 1.0, zf)
    x11 = _grad_dot3(perm[BB], xf - 1.0, yf - 1.0, zf)
    y0 = x00 + u * (x10 - x00)
    y1 = x01 + u * (x11 - x01)
    z0 = y0 + v * (y1 - y0)

    x00 = _grad_dot3(perm[AA + 1], xf, yf, zf - 1.0)
    x10 = _grad_dot3(perm[BA + 1], xf - 1.0, yf, zf - 1.0)
    x01 = _grad_dot3(perm[AB + 1], xf, yf - 1.0, zf - 1.0)
    x11 = _grad_dot3(perm[BB + 1], xf - 1.0, yf - 1.0, zf - 1.0)
    y0 = x00 + u * (x10 - x00)
    y1 = x01 + u * (x11 - x01)
    z1 = y0 + v * (y1 - y0)

    return (z0 + w * (z1 - z0)).astype(np.float32)


def _sphere_coords_for_indices(
    rows: np.ndarray,
    cols: np.ndarray,
    height: int,
    width: int,
) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """Compute unit-sphere (x, y, z) coordinates for selected pixel indices."""
    lon = (-np.pi + (cols.astype(np.float32) + 0.5) * (2.0 * np.pi / width)).astype(np.float32)
    lat = (0.5 * np.pi - (rows.astype(np.float32) + 0.5) * (np.pi / height)).astype(np.float32)
    clat = np.cos(lat)
    x = clat * np.cos(lon)
    y = clat * np.sin(lon)
    z = np.sin(lat)
    return x, y, z


def _ridged_multifractal_sphere(
    rows: np.ndarray,
    cols: np.ndarray,
    height: int,
    width: int,
    freqs: list[float],
    amps: list[float],
    rng: np.random.Generator,
    warp_freq: float = 0.0,
    warp_amp: float = 0.0,
    gain: float = 1.9,
    sharpness: float = 2.1,
) -> np.ndarray:
    """Evaluate domain-warped heterogeneous ridged multifractal noise on the 3D sphere."""
    n_pts = rows.size
    if n_pts == 0:
        return np.empty(0, dtype=np.float32)

    x, y, z = _sphere_coords_for_indices(rows, cols, height, width)

    if warp_freq > 0.0 and warp_amp > 0.0:
        perm_w = _make_perm(rng)
        wx = _perlin3d_vec(x * warp_freq + 3.1, y * warp_freq + 7.4, z * warp_freq + 1.9, perm_w)
        wy = _perlin3d_vec(x * warp_freq + 19.3, y * warp_freq + 2.7, z * warp_freq + 11.5, perm_w)
        wz = _perlin3d_vec(x * warp_freq + 5.7, y * warp_freq + 23.1, z * warp_freq + 17.3, perm_w)
        x = x + warp_amp * wx
        y = y + warp_amp * wy
        z = z + warp_amp * wz

    out = np.zeros(n_pts, dtype=np.float32)
    weight = np.ones(n_pts, dtype=np.float32)
    amp_sum = 0.0

    for freq, amp in zip(freqs, amps):
        perm = _make_perm(rng)
        rot = _random_rot3(rng)
        off = rng.uniform(-50.0, 50.0, size=3).astype(np.float32)
        rx = (rot[0, 0] * x + rot[0, 1] * y + rot[0, 2] * z) * freq + off[0]
        ry = (rot[1, 0] * x + rot[1, 1] * y + rot[1, 2] * z) * freq + off[1]
        rz = (rot[2, 0] * x + rot[2, 1] * y + rot[2, 2] * z) * freq + off[2]

        n = _perlin3d_vec(rx, ry, rz, perm)
        # Ridged signal: crisp alpine creases along zero crossings
        sig = np.clip(1.0 - np.abs(n) * 1.45, 0.0, 1.0)
        sig = np.power(sig, sharpness, dtype=np.float32)
        sig *= weight
        weight = np.clip(sig * gain, 0.18, 1.0)
        out += sig * amp
        amp_sum += amp

    if amp_sum > 0.0:
        out /= float(amp_sum)
    return out


def _fbm_sphere(
    rows: np.ndarray,
    cols: np.ndarray,
    height: int,
    width: int,
    freqs: list[float],
    amps: list[float],
    rng: np.random.Generator,
) -> np.ndarray:
    """Evaluate smooth 3D spherical FBM in approx [-1, 1]."""
    n_pts = rows.size
    if n_pts == 0:
        return np.empty(0, dtype=np.float32)
    x, y, z = _sphere_coords_for_indices(rows, cols, height, width)
    out = np.zeros(n_pts, dtype=np.float32)
    amp_sum = 0.0
    for freq, amp in zip(freqs, amps):
        perm = _make_perm(rng)
        rot = _random_rot3(rng)
        off = rng.uniform(-50.0, 50.0, size=3).astype(np.float32)
        rx = (rot[0, 0] * x + rot[0, 1] * y + rot[0, 2] * z) * freq + off[0]
        ry = (rot[1, 0] * x + rot[1, 1] * y + rot[1, 2] * z) * freq + off[1]
        rz = (rot[2, 0] * x + rot[2, 1] * y + rot[2, 2] * z) * freq + off[2]
        out += _perlin3d_vec(rx, ry, rz, perm) * amp
        amp_sum += amp
    if amp_sum > 0.0:
        out /= float(amp_sum)
    return out


# ---------------------------------------------------------------------------
# Cylindrical-wrap filtering & spherical gradient utilities
# ---------------------------------------------------------------------------

def _gauss_wrap(arr: np.ndarray, sigma: float | tuple[float, float]) -> np.ndarray:
    """Fast SIMD Gaussian filter (OpenCV) with exact longitudinal wrap and polar clamp."""
    if isinstance(sigma, tuple):
        sy, sx = float(sigma[0]), float(sigma[1])
    else:
        sy = sx = float(sigma)
    if sx <= 0.0 and sy <= 0.0:
        return arr.astype(np.float32, copy=True)
    h, w = arr.shape
    pad = min(max(4, int(math.ceil(sx * 3.5)) + 1), w // 2)
    padded = np.concatenate([arr[:, -pad:], arr, arr[:, :pad]], axis=1).astype(np.float32, copy=False)
    blurred = cv2.GaussianBlur(
        padded,
        ksize=(0, 0),
        sigmaX=max(sx, 1e-4),
        sigmaY=max(sy, 1e-4),
        borderType=cv2.BORDER_REPLICATE,
    )
    return blurred[:, pad: pad + w].astype(np.float32, copy=False)


def _spherical_slope(elev: np.ndarray) -> np.ndarray:
    """Compute physical slope in meters per kilometer on equirectangular grid."""
    h, w = elev.shape
    earth_r_km = 6371.0
    dy_km = (math.pi * earth_r_km) / float(h)
    lat = 0.5 * math.pi - (np.arange(h, dtype=np.float32) + 0.5) * (math.pi / float(h))
    dx_km = np.maximum(np.cos(lat), 0.12) * (2.0 * math.pi * earth_r_km / float(w))

    dz_dy = np.empty_like(elev, dtype=np.float32)
    dz_dy[1:-1, :] = (elev[2:, :] - elev[:-2, :]) * (0.5 / dy_km)
    dz_dy[0, :] = (elev[1, :] - elev[0, :]) / dy_km
    dz_dy[-1, :] = (elev[-1, :] - elev[-2, :]) / dy_km

    dz_dx = (np.roll(elev, -1, axis=1) - np.roll(elev, 1, axis=1)) * (0.5 / dx_km[:, None])
    return np.hypot(dz_dx, dz_dy).astype(np.float32)


def _resize_wrap_cubic(arr: np.ndarray, target_w: int, target_h: int) -> np.ndarray:
    """Upsample/downsample with longitudinal wrap padding so +/-180 has zero seam."""
    h, w = arr.shape
    pad = min(16, w // 4)
    padded = np.concatenate([arr[:, -pad:], arr, arr[:, :pad]], axis=1)
    scale_x = float(target_w) / float(w)
    pad_out = int(round(pad * scale_x))
    # Resize padded array so the interior maps cleanly to target_w
    resized = cv2.resize(
        padded.astype(np.float32),
        (target_w + 2 * pad_out, target_h),
        interpolation=cv2.INTER_CUBIC,
    )
    return resized[:, pad_out: pad_out + target_w].astype(np.float32)


# ---------------------------------------------------------------------------
# Priority-Flood depression filling (via MST), exact flat resolution, D8 & DAG
# ---------------------------------------------------------------------------

_D8_OFFSETS = (
    (-1, 0),  # 0: N
    (-1, 1),  # 1: NE
    (0, 1),   # 2: E
    (1, 1),   # 3: SE
    (1, 0),   # 4: S
    (1, -1),  # 5: SW
    (0, -1),  # 6: W
    (-1, -1), # 7: NW
)


def _priority_flood_fill(elev: np.ndarray) -> np.ndarray:
    """Exact Priority-Flood depression filling with 360-degree longitudinal wrap.

    Uses the Kruskal Minimum Bottleneck Spanning Tree equivalence via
    scipy.sparse.csgraph.minimum_spanning_tree + breadth_first_order.
    Guarantees every land cell has a monotonically non-increasing path to ocean
    (elev <= 0.0).
    """
    h, w = elev.shape
    elev64 = elev.astype(np.float64)
    land = elev64 > 0.0
    n_land = int(land.sum())
    if n_land == 0:
        return elev64.copy()

    # Map land pixels to 1..n_land, all ocean/sea (elev <= 0) to super-node 0
    idx = np.zeros((h, w), dtype=np.int32)
    idx[land] = np.arange(1, n_land + 1, dtype=np.int32)
    z_land = np.empty(n_land + 1, dtype=np.float64)
    z_land[0] = 0.0
    z_land[1:] = elev64[land]

    # Identify coastal land cells (any 8-neighbor has idx == 0)
    ocean = ~land
    is_coastal = np.zeros((h, w), dtype=bool)
    for dr, dc in _D8_OFFSETS:
        if dr == 0:
            nbr_ocean = np.roll(ocean, -dc, axis=1)
            is_coastal |= nbr_ocean
        elif dr == 1:
            is_coastal[:-1, :] |= np.roll(ocean[1:, :], -dc, axis=1)
        else:
            is_coastal[1:, :] |= np.roll(ocean[:-1, :], -dc, axis=1)
    is_coastal &= land

    us: list[np.ndarray] = []
    vs: list[np.ndarray] = []
    ws: list[np.ndarray] = []

    # Unique edge (0, u) per coastal land pixel (no duplicate edges to node 0)
    coastal_u = idx[is_coastal]
    if coastal_u.size == 0:
        # Edge case: all-land planet; treat bottom row as sink
        coastal_u = idx[-1, :]
    us.append(np.zeros(coastal_u.size, dtype=np.int32))
    vs.append(coastal_u)
    ws.append(z_land[coastal_u] + 1.0)

    # 4 undirected 8-neighbor half-directions between land pixels (wrapped in lon)
    for dr, dc in ((0, 1), (1, 0), (1, 1), (1, -1)):
        if dr == 0:
            u = idx
            v = np.roll(idx, -dc, axis=1)
        else:
            u = idx[:-1, :]
            v = np.roll(idx[1:, :], -dc, axis=1)
        mask = (u > 0) & (v > 0) & (u != v)
        uu = u[mask]
        vv = v[mask]
        swap = uu > vv
        u_tri = np.where(swap, vv, uu)
        v_tri = np.where(swap, uu, vv)
        # Positive strict weight = max(Z(u), Z(v)) + 1.0
        w_edge = np.maximum(z_land[u_tri], z_land[v_tri]) + 1.0
        us.append(u_tri)
        vs.append(v_tri)
        ws.append(w_edge)

    u_all = np.concatenate(us)
    v_all = np.concatenate(vs)
    w_all = np.concatenate(ws)

    # Note: on width==2 wrap could create duplicate (u, v); normal grids W>=8 have none
    graph = csr_matrix((w_all, (u_all, v_all)), shape=(n_land + 1, n_land + 1))
    mst = minimum_spanning_tree(graph)
    order, pred = breadth_first_order(mst, i_start=0, directed=False, return_predecessors=True)

    # Propagate exact bottleneck spillway elevation along BFS order
    filled_land = z_land.copy()
    order_list = order.tolist()
    pred_list = pred.tolist()
    fl = filled_land.tolist()
    zl = z_land.tolist()
    for node in order_list[1:]:
        p = pred_list[node]
        fp = fl[p]
        zn = zl[node]
        fl[node] = fp if fp > zn else zn

    filled = elev64.copy()
    filled[land] = np.asarray(fl[1:], dtype=np.float64)
    return filled


def _compute_d8_and_flow(
    elev: np.ndarray,
    runoff: np.ndarray,
) -> dict[str, Any]:
    """Compute exact Priority-Flood filled surface, acyclic D8 receivers,
    runoff-weighted flow accumulation, and local slope.
    """
    h, w = elev.shape
    n_cells = h * w
    land = elev > 0.0
    filled = _priority_flood_fill(elev)

    lat = 0.5 * math.pi - (np.arange(h, dtype=np.float64) + 0.5) * (math.pi / float(h))
    cos_lat = np.maximum(np.cos(lat), 0.15)  # (h,)

    # Evaluate D8 steepest descent on `filled` using true spherical angular step lengths
    best_slope = np.zeros((h, w), dtype=np.float64)
    best_dir = np.full((h, w), -1, dtype=np.int8)

    for k, (dr, dc) in enumerate(_D8_OFFSETS):
        dist = np.hypot(float(dr), float(dc) * cos_lat)[:, None]  # (h, 1)
        if dr == 0:
            nbr_f = np.roll(filled, -dc, axis=1)
            valid = np.ones((h, w), dtype=bool)
        elif dr == 1:
            nbr_f = np.empty_like(filled)
            nbr_f[:-1, :] = np.roll(filled[1:, :], -dc, axis=1)
            nbr_f[-1, :] = np.inf
            valid = np.ones((h, w), dtype=bool)
            valid[-1, :] = False
        else:  # dr == -1
            nbr_f = np.empty_like(filled)
            nbr_f[1:, :] = np.roll(filled[:-1, :], -dc, axis=1)
            nbr_f[0, :] = np.inf
            valid = np.ones((h, w), dtype=bool)
            valid[0, :] = False

        slope = np.where(valid, (filled - nbr_f) / dist, -1.0)
        better = land & (slope > best_slope)
        best_slope = np.where(better, slope, best_slope)
        best_dir = np.where(better, k, best_dir)

    # Resolve any flat land cells (best_dir == -1) via multi-source Dijkstra on
    # equal-elevation flat components toward already-resolved flat-boundary outlets.
    is_flat = land & (best_dir < 0)
    if np.any(is_flat):
        # Identify boundary outlets: already-resolved land cells adjacent to a flat
        # cell with the exact same `filled` elevation.
        is_outlet = np.zeros((h, w), dtype=bool)
        for dr, dc in _D8_OFFSETS:
            if dr == 0:
                nbr_flat = np.roll(is_flat, -dc, axis=1)
                nbr_f = np.roll(filled, -dc, axis=1)
                is_outlet |= (best_dir >= 0) & land & nbr_flat & (nbr_f == filled)
            elif dr == 1:
                nbr_flat = np.roll(is_flat[1:, :], -dc, axis=1)
                nbr_f = np.roll(filled[1:, :], -dc, axis=1)
                is_outlet[:-1, :] |= (
                    (best_dir[:-1, :] >= 0) & land[:-1, :] & nbr_flat & (nbr_f == filled[:-1, :])
                )
            else:
                nbr_flat = np.roll(is_flat[:-1, :], -dc, axis=1)
                nbr_f = np.roll(filled[:-1, :], -dc, axis=1)
                is_outlet[1:, :] |= (
                    (best_dir[1:, :] >= 0) & land[1:, :] & nbr_flat & (nbr_f == filled[1:, :])
                )

        active = is_flat | is_outlet
        n_active = int(active.sum())
        act_idx = np.full((h, w), -1, dtype=np.int32)
        act_idx[active] = np.arange(n_active, dtype=np.int32)
        outlet_nodes = act_idx[is_outlet]

        # Normalize original elevation softly for natural thalweg routing across flats
        elev_norm = np.clip(elev.astype(np.float64) / 8000.0, 0.0, 1.0)

        fus: list[np.ndarray] = []
        fvs: list[np.ndarray] = []
        fws: list[np.ndarray] = []
        for dr, dc in ((0, 1), (1, 0), (1, 1), (1, -1)):
            step_len = np.hypot(float(dr), float(dc) * cos_lat)[:, None]
            if dr == 0:
                u = act_idx
                v = np.roll(act_idx, -dc, axis=1)
                fu = filled
                fv = np.roll(filled, -dc, axis=1)
                eu = elev_norm
                ev = np.roll(elev_norm, -dc, axis=1)
                d_row = step_len
            else:
                u = act_idx[:-1, :]
                v = np.roll(act_idx[1:, :], -dc, axis=1)
                fu = filled[:-1, :]
                fv = np.roll(filled[1:, :], -dc, axis=1)
                eu = elev_norm[:-1, :]
                ev = np.roll(elev_norm[1:, :], -dc, axis=1)
                d_row = step_len[:-1, :]
            mask = (u >= 0) & (v >= 0) & (fu == fv) & (u != v)
            if np.any(mask):
                uu = u[mask]
                vv = v[mask]
                # Edge length biased slightly toward lower original terrain (natural basin thalweg)
                ww = (d_row * (1.0 + 0.02 * (eu + ev)))[mask]
                fus.append(uu)
                fvs.append(vv)
                fws.append(ww)

        if fus and outlet_nodes.size > 0:
            fgraph = csr_matrix(
                (np.concatenate(fws), (np.concatenate(fus), np.concatenate(fvs))),
                shape=(n_active, n_active),
            )
            d_act = dijkstra(fgraph, directed=False, indices=outlet_nodes, min_only=True)
            d_out = np.full((h, w), np.inf, dtype=np.float64)
            d_out[active] = d_act

            best_flat_grad = np.zeros((h, w), dtype=np.float64)
            for k, (dr, dc) in enumerate(_D8_OFFSETS):
                dist = np.hypot(float(dr), float(dc) * cos_lat)[:, None]
                if dr == 0:
                    nbr_f = np.roll(filled, -dc, axis=1)
                    nbr_d = np.roll(d_out, -dc, axis=1)
                    valid = np.ones((h, w), dtype=bool)
                elif dr == 1:
                    nbr_f = np.empty_like(filled)
                    nbr_d = np.full_like(d_out, np.inf)
                    nbr_f[:-1, :] = np.roll(filled[1:, :], -dc, axis=1)
                    nbr_d[:-1, :] = np.roll(d_out[1:, :], -dc, axis=1)
                    valid = np.ones((h, w), dtype=bool)
                    valid[-1, :] = False
                else:
                    nbr_f = np.empty_like(filled)
                    nbr_d = np.full_like(d_out, np.inf)
                    nbr_f[1:, :] = np.roll(filled[:-1, :], -dc, axis=1)
                    nbr_d[1:, :] = np.roll(d_out[:-1, :], -dc, axis=1)
                    valid = np.ones((h, w), dtype=bool)
                    valid[0, :] = False

                finite_mask = valid & is_flat & (nbr_f == filled) & np.isfinite(d_out) & np.isfinite(nbr_d)
                diff = np.where(finite_mask, d_out, 0.0) - np.where(finite_mask, nbr_d, 0.0)
                grad = np.where(finite_mask, diff / dist, -1.0)
                better = is_flat & (grad > best_flat_grad)
                best_flat_grad = np.where(better, grad, best_flat_grad)
                best_dir = np.where(better, k, best_dir)

    # Convert best_dir to flat receiver indices (-1 for ocean or unresolved)
    rr, cc = np.indices((h, w), dtype=np.int32)
    dr_lut = np.array([o[0] for o in _D8_OFFSETS], dtype=np.int32)
    dc_lut = np.array([o[1] for o in _D8_OFFSETS], dtype=np.int32)
    has_dir = land & (best_dir >= 0)
    receiver = np.full(n_cells, -1, dtype=np.int32)
    if np.any(has_dir):
        d_sel = best_dir[has_dir]
        nr = rr[has_dir] + dr_lut[d_sel]
        nc = (cc[has_dir] + dc_lut[d_sel]) % w
        receiver[has_dir.ravel()] = nr * w + nc

    # Vectorized Kahn's wave peeling for exact flow accumulation on the DAG
    cell_area = cos_lat[:, None].astype(np.float32)
    acc = (runoff.astype(np.float32) * cell_area).ravel()
    acc[~land.ravel()] = 0.0

    land_flat = land.ravel()
    valid_rec = land_flat & (receiver >= 0)
    in_deg = np.bincount(receiver[valid_rec], minlength=n_cells).astype(np.int32)

    frontier = np.flatnonzero(land_flat & (in_deg == 0))
    max_iters = n_cells + 1
    iters = 0
    while frontier.size > 0 and iters < max_iters:
        iters += 1
        rec = receiver[frontier]
        valid = rec >= 0
        if not np.any(valid):
            break
        rec_v = rec[valid]
        f_acc = acc[frontier[valid]]
        np.add.at(acc, rec_v, f_acc)
        np.add.at(in_deg, rec_v, -1)
        u = np.unique(rec_v)
        # Only propagate forward through land cells (ocean cells are terminal sinks)
        frontier = u[land_flat[u] & (in_deg[u] == 0)]

    return {
        "filled": filled.astype(np.float32),
        "receiver": receiver,
        "flow_acc": acc.reshape(h, w),
        "best_dir": best_dir,
    }


# ---------------------------------------------------------------------------
# True Global Wind-Belt Climate Engine & Smooth Whittaker Palette
# ---------------------------------------------------------------------------

def _compute_wind_field(h: int, w: int, rng: np.random.Generator) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """Compute global 3-cell atmospheric winds (Hadley, Ferrel, Polar) with organic
    3D spherical meandering and ITCZ/polar-front convergence factor.

    Returns:
        u_wind: zonal wind (+east, -west), shape (h, w)
        v_wind: meridional wind (+north, -south), shape (h, w)
        conv: atmospheric convergence boost (>0 in ITCZ & polar front, <0 in subtropics)
    """
    lat_deg = 90.0 - (np.arange(h, dtype=np.float32) + 0.5) * (180.0 / float(h))
    abs_lat = np.abs(lat_deg)
    sgn_lat = np.where(lat_deg >= 0.0, 1.0, -1.0).astype(np.float32)

    # Zonal wind profile:
    # 0..30 deg: Trade winds blowing WEST (u < 0)
    # 30..60 deg: Mid-latitude Westerlies blowing EAST (u > 0)
    # 60..90 deg: Polar Easterlies blowing WEST (u < 0)
    u_lat = np.empty(h, dtype=np.float32)
    m_trade = abs_lat < 30.0
    m_west = (abs_lat >= 30.0) & (abs_lat < 60.0)
    m_polar = abs_lat >= 60.0

    # Strong smooth trade winds peaking around 10-20 deg, tapering cleanly to 0 at 30 deg
    u_lat[m_trade] = -0.95 * np.cos(abs_lat[m_trade] * (math.pi / 60.0))
    # Strong mid-latitude westerlies peaking around 45 deg
    u_lat[m_west] = 1.05 * np.sin((abs_lat[m_west] - 30.0) * (math.pi / 30.0))
    # Polar easterlies peaking around 75 deg
    u_lat[m_polar] = -0.70 * np.sin((abs_lat[m_polar] - 60.0) * (math.pi / 30.0))

    # Meridional wind profile (+north, -south):
    # Hadley (0..30): blows toward equator (NE trades in NH, SE trades in SH)
    # Ferrel (30..60): blows poleward
    # Polar (60..90): blows equatorward toward 60 deg
    v_lat = np.empty(h, dtype=np.float32)
    v_lat[m_trade] = -sgn_lat[m_trade] * 0.34 * np.sin(abs_lat[m_trade] * (math.pi / 30.0))
    v_lat[m_west] = sgn_lat[m_west] * 0.28 * np.sin((abs_lat[m_west] - 30.0) * (math.pi / 30.0))
    v_lat[m_polar] = -sgn_lat[m_polar] * 0.22 * np.sin((abs_lat[m_polar] - 60.0) * (math.pi / 30.0))

    # Large-scale atmospheric convergence/subsidence factor (organic ITCZ / subtropical high / polar front)
    itcz = np.exp(-0.5 * (abs_lat / 10.5) ** 2)
    subtrop_high = np.exp(-0.5 * ((abs_lat - 29.0) / 8.5) ** 2)
    polar_front = np.exp(-0.5 * ((abs_lat - 58.0) / 9.5) ** 2)
    conv_lat = (0.75 * itcz + 0.45 * polar_front - 0.55 * subtrop_high).astype(np.float32)

    # Add gentle seeded Rossby-wave meandering on the 3D sphere
    rr, cc = np.indices((h, w), dtype=np.int32)
    wave_u = _fbm_sphere(rr.ravel(), cc.ravel(), h, w, [2.5, 5.0], [0.7, 0.3], rng).reshape(h, w)
    wave_v = _fbm_sphere(rr.ravel(), cc.ravel(), h, w, [2.5, 5.0], [0.7, 0.3], rng).reshape(h, w)

    u_wind = u_lat[:, None] + 0.14 * wave_u
    v_wind = v_lat[:, None] + 0.16 * wave_v
    conv = np.broadcast_to(conv_lat[:, None], (h, w)).copy()
    return u_wind.astype(np.float32), v_wind.astype(np.float32), conv


def _bilinear_sample_wrap(field: np.ndarray, src_r: np.ndarray, src_c: np.ndarray) -> np.ndarray:
    """Fast vectorized bilinear interpolation with longitudinal wrap and polar clamp."""
    h, w = field.shape
    r0 = np.floor(src_r).astype(np.int32)
    c0 = np.floor(src_c).astype(np.int32)
    fr = (src_r - r0).astype(np.float32)
    fc = (src_c - c0).astype(np.float32)

    r0 = np.clip(r0, 0, h - 1)
    r1 = np.clip(r0 + 1, 0, h - 1)
    c0 = c0 % w
    c1 = (c0 + 1) % w

    v00 = field[r0, c0]
    v01 = field[r0, c1]
    v10 = field[r1, c0]
    v11 = field[r1, c1]
    top = v00 + fc * (v01 - v00)
    bot = v10 + fc * (v11 - v10)
    return top + fr * (bot - top)


def _compute_temperature_coarse(
    elev_c: np.ndarray,
    u_wind: np.ndarray,
    rng: np.random.Generator,
) -> tuple[np.ndarray, np.ndarray]:
    """Compute coarse sea-level temperature (with coastal moderation and ocean currents)
    and actual surface temperature (with 6.5 C/km lapse rate).
    """
    hc, wc = elev_c.shape
    lat = 0.5 * math.pi - (np.arange(hc, dtype=np.float32) + 0.5) * (math.pi / float(hc))
    cos_lat = np.cos(lat)[:, None]
    abs_lat_deg = np.abs(np.degrees(lat))[:, None]

    # Realistic planetary sea-level insolation curve (+29.5C equator -> -17C poles)
    t_sea_base = (-17.0 + 46.5 * np.power(np.maximum(cos_lat, 0.0), 1.15)).astype(np.float32)

    # Continentality: distance inland from ocean (wrapped horizontally)
    land_c = elev_c > 0.0
    pad = wc // 4
    land_pad = np.concatenate([land_c[:, -pad:], land_c, land_c[:, :pad]], axis=1)
    dist_px = distance_transform_edt(land_pad)[:, pad: pad + wc].astype(np.float32)
    # Normalize to approximate thousands of km (1 px at hc=256 is ~78 km)
    dist_km = dist_px * (math.pi * 6371.0 / float(hc))
    continentality = np.clip(1.0 - np.exp(-dist_km / 950.0), 0.0, 1.0)

    # Deep interiors: slightly warmer in low-lat deserts (+2.2C), colder in high-lat interiors (-4.2C)
    cont_shift = np.where(
        abs_lat_deg < 34.0,
        2.2 * continentality * (1.0 - abs_lat_deg / 34.0),
        -4.2 * continentality * np.clip((abs_lat_deg - 34.0) / 32.0, 0.0, 1.0),
    ).astype(np.float32)

    # Maritime windward moderation: onshore winds from ocean moderate coastal temperatures
    ocean_f = (~land_c).astype(np.float32)
    # Check ocean presence upwind (4 pixels upwind)
    rr, cc = np.indices((hc, wc), dtype=np.float32)
    cos_safe = np.maximum(np.cos(lat), 0.25)[:, None]
    upwind_ocean = _bilinear_sample_wrap(ocean_f, rr, cc - u_wind * (4.5 / cos_safe))
    coastal_mod = np.where(
        land_c,
        upwind_ocean * np.where(abs_lat_deg > 38.0, 3.2, 0.8) * (1.0 - continentality),
        0.0,
    ).astype(np.float32)

    # Subtle 3D spherical planetary climate undulation (warm/cool ocean gyre & air-mass variation)
    rr_i, cc_i = np.indices((hc, wc), dtype=np.int32)
    clim_var = _fbm_sphere(rr_i.ravel(), cc_i.ravel(), hc, wc, [2.0, 4.0], [0.7, 0.3], rng).reshape(hc, wc)
    t_sea = t_sea_base + cont_shift + coastal_mod + 1.8 * clim_var

    # Elevation lapse rate: -6.5 C per km above sea level
    t_surf = t_sea - 0.0065 * np.maximum(elev_c, 0.0)
    return t_sea.astype(np.float32), t_surf.astype(np.float32)


def _advect_wind_moisture(
    elev_c: np.ndarray,
    t_sea: np.ndarray,
    t_surf: np.ndarray,
    u_wind: np.ndarray,
    v_wind: np.ndarray,
    conv: np.ndarray,
) -> tuple[np.ndarray, np.ndarray]:
    """True global wind-belt semi-Lagrangian moisture advection + orographic rain shadow.

    Returns:
        moisture_c: effective Whittaker moisture in [0, 1], shape (hc, wc)
        precip_c: raw relative precipitation rate (for hydrological runoff), shape (hc, wc)
    """
    hc, wc = elev_c.shape
    lat = 0.5 * math.pi - (np.arange(hc, dtype=np.float32) + 0.5) * (math.pi / float(hc))
    cos_safe = np.maximum(np.cos(lat), 0.28)[:, None].astype(np.float32)

    land = elev_c > 0.0
    ocean = ~land
    elev_pos = np.maximum(elev_c, 0.0).astype(np.float32)
    # Slightly smoothed orographic barrier elevation so air feels coherent ranges
    elev_oro = _gauss_wrap(elev_pos, sigma=1.1)

    # Clausius-Clapeyron saturation vapor capacity vs temperature (normalized ~0.08..2.6)
    q_sat = (0.42 * np.exp(0.056 * np.clip(t_surf, -30.0, 35.0))).astype(np.float32)
    # Ocean evaporation capacity (driven by sea surface temperature)
    q_ocean_eq = (0.42 * np.exp(0.056 * np.clip(t_sea, -2.0, 32.0))).astype(np.float32)

    rr, cc = np.indices((hc, wc), dtype=np.float32)

    # Run two slight seasonal ITCZ shifts (+6 deg NH summer, -6 deg SH summer) so
    # monsoon / tropical savanna belts emerge organically without sharp latitude lines
    total_precip = np.zeros((hc, wc), dtype=np.float32)
    shift_rows = [hc * (-5.5 / 180.0), hc * (5.5 / 180.0)]

    dt = 2.0  # ~2.0 grid cells per step (~155 km at 512x256) for realistic inland penetration
    n_spinup = 45
    n_accum = 85

    for row_shift in shift_rows:
        # Shifted convergence belt for this season
        conv_s = _bilinear_sample_wrap(conv, np.clip(rr + row_shift, 0.0, hc - 1.0), cc)
        # Mild seasonal deflection of meridional wind
        v_s = _bilinear_sample_wrap(v_wind, np.clip(rr + row_shift, 0.0, hc - 1.0), cc)
        u_s = _bilinear_sample_wrap(u_wind, np.clip(rr + row_shift, 0.0, hc - 1.0), cc)

        # Backtrace coordinates (exact upwind position along wind vector)
        # u > 0 blows EAST (larger col), so upwind is smaller col (cc - u*dt/cos)
        # v > 0 blows NORTH (smaller row), so upwind is larger row (rr + v*dt)
        src_c = cc - u_s * (dt / cos_safe)
        src_r = np.clip(rr + v_s * dt, 0.0, float(hc - 1))

        # Upwind elevation difference (>0 = windward ascent, <0 = leeward descent)
        elev_upwind = _bilinear_sample_wrap(elev_oro, src_r, src_c)
        dh_up = np.maximum(elev_oro - elev_upwind, 0.0)  # meters climbed in 1 step
        # Leeward Foehn descent suppresses background rainfall cleanly
        dh_down = np.maximum(elev_upwind - elev_oro, 0.0)

        # Initialize atmospheric vapor near ocean equilibrium everywhere
        q = np.where(ocean, q_ocean_eq * 0.94, q_sat * 0.40).astype(np.float32)
        precip_s = np.zeros((hc, wc), dtype=np.float32)

        # Precompute spatially-varying convective rain efficiency
        # Higher in ITCZ/polar-front convergence, lower in subtropical subsidence & leeward descent
        rain_eff = np.clip(
            0.024 * (1.0 + 1.15 * conv_s) * np.exp(-dh_down / 560.0),
            0.006,
            0.095,
        ).astype(np.float32)

        for step in range(n_spinup + n_accum):
            # 1. Semi-Lagrangian advection of atmospheric water vapor
            q = _bilinear_sample_wrap(q, src_r, src_c)

            # Lateral storm-track turbulent eddy diffusion every 4 steps
            if (step & 3) == 3:
                q = 0.86 * q + 0.14 * _gauss_wrap(q, sigma=0.85)

            # 2. Ocean evaporation recharge (strong over oceans & inland seas)
            q = np.where(ocean, q + 0.46 * (q_ocean_eq - q), q)

            # 3. Orographic forced ascent condensation on windward slopes!
            #    Air forced up mountains condenses heavily, depleting `q` before the leeward side.
            oro_frac = 1.0 - np.exp(-dh_up / 640.0)
            cond_oro = q * (0.50 * oro_frac)
            q = q - cond_oro

            # 4. Saturation condensation (cooling with altitude/latitude)
            excess = np.maximum(q - q_sat, 0.0)
            cond_sat = 0.62 * excess
            q = q - cond_sat

            # 5. Background convective / frontal rainfall over land
            cond_conv = np.where(land, q * rain_eff, 0.0)
            q = q - cond_conv

            # 6. Land evapotranspiration recycling (forests/soil return modest moisture)
            evap_land = np.where(land, 0.28 * (cond_conv + 0.26 * cond_oro), 0.0)
            q = np.minimum(q + evap_land, q_sat * 1.15)

            if step >= n_spinup:
                precip_s += cond_oro + cond_sat + cond_conv

        total_precip += precip_s / float(n_accum)

    precip_c = (total_precip * 0.5).astype(np.float32)
    # Gentle smoothing across 0.75 px to remove grid aliasing while preserving crisp rain shadows
    precip_c = _gauss_wrap(precip_c, sigma=0.75)

    # Convert physical precipitation to effective Whittaker moisture [0, 1] using
    # Temperature-dependent Potential Evapotranspiration (PET) normalized to ~1.0 at 15 C:
    # Hot tropics need much higher rainfall for rainforest than cool boreal taiga!
    pet = (0.22 + 0.78 * np.power(np.clip((t_surf + 15.0) / 30.0, 0.04, 1.85), 1.45)).astype(np.float32)
    aridity = precip_c / np.maximum(pet, 0.16)

    # Calibrate Michaelis-Menten half-saturation against a physical maritime 15C reference
    # softly anchored to the world's land median so any continental layout gets rich biome range.
    p_phys = 0.022
    if np.any(land):
        land_ar = aridity[land]
        p52 = float(np.percentile(land_ar, 52))
        p_half = float(np.clip(0.55 * p_phys + 0.45 * max(p52, 0.004), 0.007, 0.040))
    else:
        p_half = p_phys

    moisture_c = aridity / (aridity + p_half)

    # Subtle coastal marine humidity right along shorelines
    ocean_blur = _gauss_wrap(ocean.astype(np.float32), sigma=1.4)
    moisture_c = np.where(
        land,
        np.clip(moisture_c + 0.09 * ocean_blur * (1.0 - moisture_c), 0.0, 1.0),
        1.0,
    ).astype(np.float32)

    return moisture_c, precip_c


# ---------------------------------------------------------------------------
# Smooth 2D Whittaker Satellite-Atlas Biome LUT & Classification
# ---------------------------------------------------------------------------

BIOME_NAMES: dict[int, str] = {
    0: "Ocean",
    1: "Polar Ice & Glacier",
    2: "Tundra",
    3: "Alpine Rock & Peak",
    4: "Boreal Taiga",
    5: "Cold Steppe & Desert",
    6: "Temperate Grassland",
    7: "Temperate Forest",
    8: "Temperate Rainforest",
    9: "Hot Desert",
    10: "Arid Shrubland",
    11: "Tropical Savanna",
    12: "Tropical Seasonal Forest",
    13: "Tropical Rainforest",
}


def _build_whittaker_lut(size: int = 256) -> np.ndarray:
    """Build a silky-smooth 2D (temperature_u, moisture_u) -> RGB satellite-atlas LUT.

    Axis 0: t_u in [0, 1] mapping temperature_c from -20 C to +32 C.
    Axis 1: m_u in [0, 1] mapping moisture from 0.0 to 1.0.
    Uses natural growing-season satellite-atlas colors (NASA Blue Marble / Natural Earth):
    boreal taiga remains rich dark conifer down to ~-5 C, arctic tundra olive-brown
    from -12 C to -5 C, and permanent glacier ice below ~-12 C. ZERO cartoon stripes!
    """
    # Anchor points: (temp_c, moisture, R, G, B)
    anchors = np.array(
        [
            # Permanent Polar Ice Sheet (-20 .. -13 C): crisp glacial snow & ice
            [-20.0, 0.00, 224, 230, 236],
            [-20.0, 0.50, 234, 239, 244],
            [-20.0, 1.00, 240, 244, 248],
            [-13.5, 0.00, 206, 210, 212],
            [-13.5, 0.50, 218, 224, 228],
            [-13.5, 1.00, 228, 234, 238],

            # Arctic & Alpine Tundra (-10 .. -5 C): natural lichen moss, peat & rocky heather
            [-8.5,  0.00, 166, 160, 142],
            [-8.5,  0.35, 146, 146, 124],
            [-8.5,  0.70, 122, 132, 110],
            [-8.5,  1.00, 112, 126, 108],

            # Boreal Taiga / Subarctic (-4 .. +3 C): deep spruce forest vs cold dry steppe
            [-1.5,  0.00, 178, 168, 138],
            [-1.5,  0.22, 142, 144, 110],
            [-1.5,  0.48, 76,  102, 74],
            [-1.5,  0.75, 52,  82,  60],
            [-1.5,  1.00, 42,  74,  54],

            # Cool Temperate (+5 .. +12 C): cold desert -> steppe/prairie -> deciduous forest -> mossy rainforest
            [8.0,   0.00, 196, 180, 140],
            [8.0,   0.18, 170, 162, 114],
            [8.0,   0.36, 132, 144, 88],
            [8.0,   0.58, 72,  108, 62],
            [8.0,   0.82, 46,  84,  52],
            [8.0,   1.00, 34,  72,  44],

            # Warm Temperate / Mediterranean (+15 .. +20 C)
            [17.0,  0.00, 212, 188, 142],
            [17.0,  0.16, 186, 168, 116],
            [17.0,  0.34, 144, 150, 88],
            [17.0,  0.56, 78,  114, 60],
            [17.0,  0.78, 48,  88,  48],
            [17.0,  1.00, 34,  74,  42],

            # Tropical (+24 .. +32 C): warm ochre desert -> shrubland -> golden savanna -> monsoon -> emerald rainforest
            [26.0,  0.00, 218, 190, 140],
            [26.0,  0.14, 196, 170, 118],
            [26.0,  0.30, 152, 152, 86],
            [26.0,  0.50, 88,  120, 60],
            [26.0,  0.72, 48,  90,  44],
            [26.0,  1.00, 28,  68,  34],

            [32.0,  0.00, 222, 192, 138],
            [32.0,  0.30, 156, 152, 84],
            [32.0,  0.70, 46,  86,  42],
            [32.0,  1.00, 26,  64,  32],
        ],
        dtype=np.float32,
    )

    t_norm = (anchors[:, 0] - (-20.0)) / (32.0 - (-20.0))
    m_norm = anchors[:, 1]
    rgb_a = anchors[:, 2:5]  # (K, 3)

    tg = np.linspace(0.0, 1.0, size, dtype=np.float32)
    mg = np.linspace(0.0, 1.0, size, dtype=np.float32)
    T, M = np.meshgrid(tg, mg, indexing="ij")

    dt = (T[:, :, None] - t_norm[None, None, :]) / 0.11
    dm = (M[:, :, None] - m_norm[None, None, :]) / 0.14
    d2 = dt * dt + dm * dm
    w = np.exp(-0.5 * d2)
    w_sum = np.sum(w, axis=2, keepdims=True) + 1e-8
    lut = np.einsum("ijk,kl->ijl", w, rgb_a) / w_sum
    return np.clip(lut, 0.0, 255.0).astype(np.float32)


def _classify_and_color_biomes(
    elevation_m: np.ndarray,
    temperature_c: np.ndarray,
    moisture: np.ndarray,
    slope_mpk: np.ndarray,
    rng: np.random.Generator,
) -> tuple[np.ndarray, np.ndarray]:
    """Compute discrete biome_id (uint8) and smoothly interpolated satellite-atlas biome_rgb (uint8).

    Evaluates LUT interpolation and alpine rock/snow blending ONLY on land pixels
    for 3x faster performance at 4096x2048.
    """
    h, w = elevation_m.shape
    land = elevation_m > 0.0
    biome_id = np.zeros((h, w), dtype=np.uint8)
    biome_rgb = np.empty((h, w, 3), dtype=np.uint8)
    biome_rgb[:, :, 0] = 26
    biome_rgb[:, :, 1] = 58
    biome_rgb[:, :, 2] = 92

    if not np.any(land):
        return biome_id, biome_rgb

    # Extract 1D land vectors (~35% of pixels -> 3x faster & 3x lower RAM!)
    e_l = elevation_m[land]
    t_l = temperature_c[land]
    m_l = moisture[land]
    s_l = slope_mpk[land]

    # 1. Smoothly interpolated RGB via 256x256 Whittaker LUT (bilinear lookup)
    lut = _build_whittaker_lut(256)
    t_u = np.clip((t_l - (-20.0)) / (32.0 - (-20.0)), 0.0, 1.0) * 255.0
    m_u = np.clip(m_l, 0.0, 1.0) * 255.0

    ti0 = np.floor(t_u).astype(np.int32)
    mi0 = np.floor(m_u).astype(np.int32)
    ti1 = np.minimum(ti0 + 1, 255)
    mi1 = np.minimum(mi0 + 1, 255)
    ft = (t_u - ti0)[:, None]
    fm = (m_u - mi0)[:, None]

    c00 = lut[ti0, mi0]
    c01 = lut[ti0, mi1]
    c10 = lut[ti1, mi0]
    c11 = lut[ti1, mi1]
    rgb_l = (c00 + fm * (c01 - c00)) * (1.0 - ft) + (c10 + fm * (c11 - c10)) * ft

    # 2. Continuous alpine rock scree & snow blending on high mountains
    # Steep high alpine slopes expose granite/slate bedrock; peaks hold permanent glaciers
    high_alt = _smoothstep(2100.0, 3800.0, e_l)
    cold_rock = _smoothstep(1.0, -7.5, t_l)
    steepness = _smoothstep(42.0, 165.0, s_l)

    rock_blend = np.clip(high_alt * (0.28 + 0.62 * steepness) + 0.30 * cold_rock * steepness, 0.0, 0.86)
    rock_col = np.array([124.0, 118.0, 110.0], dtype=np.float32)
    rgb_l = rgb_l * (1.0 - rock_blend[:, None]) + rock_col[None, :] * rock_blend[:, None]

    # Permanent polar ice & high-altitude alpine snowcaps (sheer cliffs shed snow cleanly)
    snow_temp = _smoothstep(-7.5, -13.5, t_l)
    snow_alt = _smoothstep(2950.0, 4400.0, e_l) * _smoothstep(1.5, -5.0, t_l)
    cliff_shed = 1.0 - 0.58 * _smoothstep(95.0, 225.0, s_l)
    snow_blend = np.clip(np.maximum(snow_temp, snow_alt) * cliff_shed, 0.0, 0.96)
    snow_col = np.array([238.0, 242.0, 246.0], dtype=np.float32)
    rgb_l = rgb_l * (1.0 - snow_blend[:, None]) + snow_col[None, :] * snow_blend[:, None]

    # Subtle natural geological tonal micro-variation on land
    hc, wc = min(h, 256), min(w, 512)
    rrc, ccc = np.indices((hc, wc), dtype=np.int32)
    tex_c = _fbm_sphere(rrc.ravel(), ccc.ravel(), hc, wc, [6.0, 14.0, 30.0], [0.5, 0.3, 0.2], rng).reshape(hc, wc)
    tex_l = _resize_wrap_cubic(tex_c, w, h)[land]
    tone_amp = (1.0 - snow_blend) * 7.5
    rgb_l = rgb_l + (tex_l * tone_amp)[:, None]

    biome_rgb[land] = np.clip(np.rint(rgb_l), 0, 255).astype(np.uint8)

    # 3. Clean discrete biome_id matching BIOME_NAMES
    b_l = np.full(e_l.size, 6, dtype=np.uint8)

    # Tropical (T >= 19 C)
    trop = t_l >= 19.0
    b_l[trop & (m_l < 0.16)] = 9   # Hot Desert
    b_l[trop & (m_l >= 0.16) & (m_l < 0.30)] = 10  # Arid Shrubland
    b_l[trop & (m_l >= 0.30) & (m_l < 0.50)] = 11  # Tropical Savanna
    b_l[trop & (m_l >= 0.50) & (m_l < 0.72)] = 12  # Tropical Seasonal Forest
    b_l[trop & (m_l >= 0.72)] = 13  # Tropical Rainforest

    # Temperate (4.5 <= T < 19 C)
    temp = (t_l >= 4.5) & (t_l < 19.0)
    b_l[temp & (m_l < 0.16)] = 9   # Hot/Warm Desert
    b_l[temp & (m_l >= 0.16) & (m_l < 0.28)] = 10  # Arid Shrubland
    b_l[temp & (m_l >= 0.28) & (m_l < 0.48)] = 6   # Temperate Grassland
    b_l[temp & (m_l >= 0.48) & (m_l < 0.74)] = 7   # Temperate Forest
    b_l[temp & (m_l >= 0.74)] = 8   # Temperate Rainforest

    # Boreal / Subarctic (-4.5 <= T < 4.5 C)
    bor = (t_l >= -4.5) & (t_l < 4.5)
    b_l[bor & (m_l < 0.28)] = 5    # Cold Steppe & Desert
    b_l[bor & (m_l >= 0.28)] = 4   # Boreal Taiga

    # Tundra (-12.0 <= T < -4.5 C)
    tun = (t_l >= -12.0) & (t_l < -4.5)
    b_l[tun] = 2

    # Alpine Rock & Peak (high rugged alpine rock below permanent ice)
    alp_rock = (e_l >= 3100.0) & (t_l < 0.0) & (snow_blend < 0.55)
    b_l[alp_rock] = 3

    # Polar Ice & Glacier (T < -10 C or permanent alpine snowpack)
    ice = (t_l < -10.0) | (snow_blend >= 0.62)
    b_l[ice] = 1

    biome_id[land] = b_l
    return biome_id, biome_rgb


# ---------------------------------------------------------------------------
# Hydrological Dendritic Valley Carving & Alluvial Plain Smoothing
# ---------------------------------------------------------------------------

def _carve_valleys_and_alluvial(
    elev_d: np.ndarray,
    runoff_d: np.ndarray,
    ridge_crest_d: np.ndarray,
    n_passes: int = 2,
) -> tuple[np.ndarray, np.ndarray]:
    """Carve realistic branching dendritic mountain valleys and smooth lowland alluvial plains."""
    hd, wd = elev_d.shape
    cur = elev_d.astype(np.float32).copy()
    land = cur > 0.0
    if not np.any(land):
        return cur, np.zeros_like(cur)

    alluvial_accum = np.zeros((hd, wd), dtype=np.float32)

    for p_idx in range(n_passes):
        drain = _compute_d8_and_flow(cur, runoff_d)
        flow = drain["flow_acc"]
        slope = _spherical_slope(cur)

        # Normalize flow robustly using positive land quantiles
        land_flow = flow[land]
        f_scale = float(np.percentile(land_flow, 92)) if land_flow.size > 0 else 10.0
        f_norm = np.clip(flow / max(f_scale, 1.0), 0.0, 200.0)

        # Stream power index: concentrates carving in dendritic mountain channels
        spi = np.power(f_norm, 0.42, dtype=np.float32) * np.power(np.clip(slope / 40.0, 0.0, 4.0), 0.65, dtype=np.float32)
        alt_weight = _smoothstep(80.0, 1100.0, cur)

        # Raw channel incision depth (meters) for this pass
        pass_scale = 0.62 if p_idx == 0 else 0.45
        incision_raw = np.clip(spi * 210.0 * alt_weight * pass_scale, 0.0, 460.0 * pass_scale)

        # Multi-scale V-shaped valley profile:
        #   - Narrow sharp thalweg gorge (sigma=0.75 px)
        #   - Sloped V-shaped canyon walls (sigma=2.1 px)
        #   - Broad glacial/fluvial trunk valleys for major rivers (sigma=5.0 px)
        major_incision = np.where(f_norm > 0.75, incision_raw, 0.0)
        gorge = _gauss_wrap(incision_raw, sigma=0.75)
        walls = _gauss_wrap(incision_raw, sigma=2.1)
        trunk = _gauss_wrap(major_incision, sigma=5.0)

        # Protect sharp primary ridge spines from broad smoothing so peaks stay crisp!
        crest_protect = 1.0 - 0.52 * ridge_crest_d
        valley_carve = (0.55 * gorge + 0.45 * walls * crest_protect + 0.35 * trunk * crest_protect)

        # Never carve deeper than 42% of current altitude above sea level
        max_safe_carve = np.maximum(cur - 12.0, 0.0) * 0.42
        valley_carve = np.minimum(valley_carve, max_safe_carve)
        cur = np.where(land, cur - valley_carve, cur)

        # Lowland alluvial floodplain smoothing & gentle river bed seating
        lowland = _smoothstep(520.0, 60.0, cur)
        flatness = _smoothstep(45.0, 6.0, slope)
        river_prox = np.clip(np.log1p(f_norm * 4.0) / 3.0, 0.0, 1.0)
        river_prox_wide = _gauss_wrap(river_prox, sigma=2.2)
        alluvial_w = np.clip(lowland * (0.35 * flatness + 0.65 * river_prox_wide), 0.0, 0.90)
        alluvial_accum = np.maximum(alluvial_accum, alluvial_w)

        # Smooth only land elevation for alluvial blending (preserve coast elevation smoothly)
        cur_land_pos = np.maximum(cur, 0.0)
        smoothed = _gauss_wrap(cur_land_pos, sigma=2.4)
        cur = np.where(land, cur * (1.0 - 0.55 * alluvial_w) + smoothed * (0.55 * alluvial_w), cur)

        # Gentle lowland trunk channel seating so lowland rivers follow clean valleys
        low_channel = _gauss_wrap(
            np.where(land & (f_norm > 1.2), np.minimum(np.log1p(f_norm) * 8.0, 32.0), 0.0).astype(np.float32),
            sigma=1.1,
        )
        coast_fade = _smoothstep(0.0, 45.0, elev_d)
        cur = np.where(land, np.maximum(cur - low_channel * coast_fade * 0.45, 0.6), cur)

    return cur.astype(np.float32), alluvial_accum.astype(np.float32)


# ---------------------------------------------------------------------------
# Branching River Network Extraction & Strict Full-Res Coastal Snapping
# ---------------------------------------------------------------------------

def _rdp_simplify_indices(
    pts: list[tuple[float, float]],
    eps: float,
    pinned: set[int] | None = None,
    max_seg_px: float = 24.0,
) -> list[int]:
    """Ramer-Douglas-Peucker simplification returning kept vertex indices.
    Always preserves endpoints, any pinned confluence indices, and caps max segment
    length to `max_seg_px` so polar/plain rivers never span huge longitude jumps.
    """
    n = len(pts)
    if n <= 2 or eps <= 0.0:
        return list(range(n))

    keep = [False] * n
    keep[0] = True
    keep[-1] = True
    if pinned:
        for idx in pinned:
            if 0 <= idx < n:
                keep[idx] = True

    # Build initial spans split at pinned vertices
    kept_sorted = [i for i in range(n) if keep[i]]
    stack: list[tuple[int, int]] = []
    for k in range(len(kept_sorted) - 1):
        stack.append((kept_sorted[k], kept_sorted[k + 1]))

    eps2 = eps * eps
    max_seg2 = max_seg_px * max_seg_px

    while stack:
        i0, i1 = stack.pop()
        if i1 <= i0 + 1:
            continue
        x0, y0 = pts[i0]
        x1, y1 = pts[i1]
        dx = x1 - x0
        dy = y1 - y0
        len2 = dx * dx + dy * dy
        max_d2 = -1.0
        max_idx = -1
        for i in range(i0 + 1, i1):
            px, py = pts[i]
            if len2 <= 1e-12:
                d2 = (px - x0) ** 2 + (py - y0) ** 2
            else:
                t = ((px - x0) * dx + (py - y0) * dy) / len2
                t = 0.0 if t < 0.0 else (1.0 if t > 1.0 else t)
                qx = x0 + t * dx
                qy = y0 + t * dy
                d2 = (px - qx) ** 2 + (py - qy) ** 2
            if d2 > max_d2:
                max_d2 = d2
                max_idx = i
        if max_d2 > eps2 and max_idx > i0:
            keep[max_idx] = True
            stack.append((i0, max_idx))
            stack.append((max_idx, i1))
        elif len2 > max_seg2:
            mid = (i0 + i1) // 2
            if i0 < mid < i1:
                keep[mid] = True
                stack.append((i0, mid))
                stack.append((mid, i1))

    return [i for i in range(n) if keep[i]]


def _local_land_step_path(
    r0: int,
    c0: int,
    r1: int,
    c1: int,
    elev_full: np.ndarray,
    allow_end_ocean: bool,
) -> list[tuple[int, int]] | None:
    """Connect (r0, c0) to (r1, c1) on full-res grid stepping strictly through land
    pixels (and optionally allowing only the final point to be ocean).
    Uses fast 8-connected greedy+BFS inside a small local corridor box.
    """
    h, w = elev_full.shape
    # Compute shortest signed longitudinal delta (handling antimeridian)
    dc_total = c1 - c0
    if dc_total > w // 2:
        dc_total -= w
    elif dc_total < -(w // 2):
        dc_total += w
    dr_total = r1 - r0

    if dr_total == 0 and dc_total == 0:
        return [(r0, c0)]

    # Fast greedy walk first (handles 98%+ of interior segments in microseconds)
    cur_r, cur_c = r0, c0
    path = [(cur_r, cur_c)]
    max_steps = abs(dr_total) + abs(dc_total) + 4
    greedy_ok = True
    for _ in range(max_steps):
        if cur_r == r1 and cur_c == c1:
            break
        dc_rem = c1 - cur_c
        if dc_rem > w // 2:
            dc_rem -= w
        elif dc_rem < -(w // 2):
            dc_rem += w
        dr_rem = r1 - cur_r
        sr = 0 if dr_rem == 0 else (1 if dr_rem > 0 else -1)
        sc = 0 if dc_rem == 0 else (1 if dc_rem > 0 else -1)

        # Prefer lowest-elevation valid land step advancing toward target
        cands = []
        if sr != 0 and sc != 0:
            cands.append((cur_r + sr, (cur_c + sc) % w, True))
        if sr != 0:
            cands.append((cur_r + sr, cur_c, False))
        if sc != 0:
            cands.append((cur_r, (cur_c + sc) % w, False))

        chosen = None
        best_e = float("inf")
        for nr, nc, is_diag in cands:
            if not (0 <= nr < h):
                continue
            if nr == r1 and nc == c1:
                if allow_end_ocean or elev_full[nr, nc] > 0.0:
                    # For diagonal final step into ocean, avoid cutting across corner if cardinal land exists
                    chosen = (nr, nc)
                    break
            elif elev_full[nr, nc] > 0.0:
                if is_diag:
                    # Ensure diagonal step doesn't bridge across open water between two ocean corners
                    if elev_full[cur_r, nc] <= 0.0 and elev_full[nr, cur_c] <= 0.0:
                        continue
                e = float(elev_full[nr, nc])
                if e < best_e:
                    best_e = e
                    chosen = (nr, nc)

        if chosen is None:
            greedy_ok = False
            break
        cur_r, cur_c = chosen
        path.append((cur_r, cur_c))
    else:
        greedy_ok = (cur_r == r1 and cur_c == c1)

    if greedy_ok and cur_r == r1 and cur_c == c1:
        return path

    # Fallback: local bounded BFS strictly on land pixels (radius margin = 8 px)
    margin = 10
    r_min = max(0, min(r0, r1) - margin)
    r_max = min(h - 1, max(r0, r1) + margin)
    c_span = abs(dc_total) + 2 * margin
    c_start_rel = -margin if dc_total >= 0 else dc_total - margin

    from collections import deque
    q: deque[tuple[int, int]] = deque([(r0, 0)])
    parent: dict[tuple[int, int], tuple[int, int] | None] = {(r0, 0): None}
    target_key = (r1, dc_total)

    while q:
        rr, dcr = q.popleft()
        if (rr, dcr) == target_key:
            out: list[tuple[int, int]] = []
            cur: tuple[int, int] | None = target_key
            while cur is not None:
                out.append((cur[0], (c0 + cur[1]) % w))
                cur = parent[cur]
            out.reverse()
            return out

        for dr, dc in _D8_OFFSETS:
            nr = rr + dr
            ndcr = dcr + dc
            if nr < r_min or nr > r_max:
                continue
            if ndcr < c_start_rel or ndcr > c_start_rel + c_span:
                continue
            key = (nr, ndcr)
            if key in parent:
                continue
            nc = (c0 + ndcr) % w
            is_target = key == target_key
            if not is_target and elev_full[nr, nc] <= 0.0:
                continue
            if is_target and (not allow_end_ocean) and elev_full[nr, nc] <= 0.0:
                continue
            if dr != 0 and dc != 0:
                # Don't squeeze diagonally across a water gap
                c_prev = (c0 + dcr) % w
                if elev_full[rr, nc] <= 0.0 and elev_full[nr, c_prev] <= 0.0:
                    continue
            parent[key] = (rr, dcr)
            q.append(key)

    return None


def _walk_downhill_to_exact_mouth(
    start_r: int,
    start_c: int,
    hint_r: int,
    hint_c: int,
    elev_full: np.ndarray,
    max_steps: int = 80,
) -> list[tuple[int, int]] | None:
    """From a land pixel near the coast, step downhill strictly on land pixels until
    hitting the EXACT first pixel with elevation_m <= 0.0 (the mouth).

    First runs a tiny BFS from (start_r, start_c) biased downhill toward the nearest
    elevation_m <= 0.0 pixel so the river NEVER loops or gets trapped in a 4K coastal lip.
    """
    h, w = elev_full.shape
    if elev_full[start_r, start_c] <= 0.0:
        return None

    import heapq
    # Dijkstra over local coastal zone preferring downhill steps and proximity to coarse outlet
    pq: list[tuple[float, int, int]] = [(0.0, start_r, start_c)]
    parent: dict[tuple[int, int], tuple[int, int] | None] = {(start_r, start_c): None}
    best_cost: dict[tuple[int, int], float] = {(start_r, start_c): 0.0}
    steps = 0

    while pq and steps < max_steps * 24:
        cost, r, c = heapq.heappop(pq)
        if cost > best_cost.get((r, c), float("inf")) + 1e-9:
            continue
        steps += 1
        cur_e = float(elev_full[r, c])

        # Check immediate cardinal neighbors first, then diagonal, for an exact ocean/lake mouth
        best_mouth = None
        best_mouth_d = float("inf")
        for dr, dc in _D8_OFFSETS:
            nr = r + dr
            if not (0 <= nr < h):
                continue
            nc = (c + dc) % w
            if elev_full[nr, nc] <= 0.0:
                # Prefer cardinal mouth over diagonal, and closer to coarse hint
                dh_c = nc - hint_c
                if dh_c > w // 2:
                    dh_c -= w
                elif dh_c < -(w // 2):
                    dh_c += w
                d = math.hypot(nr - hint_r, dh_c) + (0.4 if (dr != 0 and dc != 0) else 0.0)
                if d < best_mouth_d:
                    best_mouth_d = d
                    best_mouth = (nr, nc)

        if best_mouth is not None:
            path = [best_mouth, (r, c)]
            cur = parent[(r, c)]
            while cur is not None:
                path.append(cur)
                cur = parent[cur]
            path.reverse()
            return path

        # Expand strictly to land neighbors
        for dr, dc in _D8_OFFSETS:
            nr = r + dr
            if not (0 <= nr < h):
                continue
            nc = (c + dc) % w
            if elev_full[nr, nc] <= 0.0:
                continue
            if dr != 0 and dc != 0:
                if elev_full[r, nc] <= 0.0 and elev_full[nr, c] <= 0.0:
                    continue
            ne = float(elev_full[nr, nc])
            step_len = 1.4142 if (dr != 0 and dc != 0) else 1.0
            # Penalize uphill moves strongly, reward downhill moves
            uphill_pen = max(0.0, ne - cur_e) * 0.65
            dh_c = nc - hint_c
            if dh_c > w // 2:
                dh_c -= w
            elif dh_c < -(w // 2):
                dh_c += w
            hint_dist = math.hypot(nr - hint_r, dh_c)
            if hint_dist > max_steps:
                continue
            ncost = cost + step_len + uphill_pen + 0.04 * hint_dist
            if ncost < best_cost.get((nr, nc), float("inf")):
                best_cost[(nr, nc)] = ncost
                parent[(nr, nc)] = (r, c)
                heapq.heappush(pq, (ncost, nr, nc))

    return None


def _extract_rivers_fullres(
    elev_full: np.ndarray,
    elev_d: np.ndarray,
    runoff_d: np.ndarray,
) -> tuple[list[dict[str, Any]], np.ndarray]:
    """Extract realistic branching river networks on the drainage grid, trace and snap
    every path cleanly onto full-resolution (H, W) valleys, enforce exact downhill &
    strict ocean-free inland invariants, and split any antimeridian crossings.

    Returns:
        rivers: list of dicts with exact schema {id, stream_order, max_flow, coords_lonlat}
        elev_out: full-res elevation_m with gentle monotonic centerline seating along rivers
    """
    h_full, w_full = elev_full.shape
    hd, wd = elev_d.shape
    elev_out = elev_full.copy()

    drain = _compute_d8_and_flow(elev_d, runoff_d)
    receiver = drain["receiver"]
    flow_acc = drain["flow_acc"]
    land_d = elev_d > 0.0
    if not np.any(land_d):
        return [], elev_out

    land_flow = flow_acc[land_d]
    # Adaptive discharge threshold targeting a rich branching Strahler 1..4/5 river network
    n_land_d = int(land_d.sum())
    target_river_cells = max(1800, min(14500, int(n_land_d * 0.032)))
    if land_flow.size <= target_river_cells:
        flow_thresh = float(np.percentile(land_flow, 94))
    else:
        flow_thresh = float(np.partition(land_flow, land_flow.size - target_river_cells)[-target_river_cells])
    flow_thresh = max(flow_thresh, 1.8)

    is_river = (land_d & (flow_acc >= flow_thresh)).ravel()
    river_nodes = np.flatnonzero(is_river)
    if river_nodes.size == 0:
        return [], elev_out

    flow_flat = flow_acc.ravel()
    land_d_flat = land_d.ravel()

    # Build upstream adjacency strictly within river nodes
    upstream: dict[int, list[int]] = {}
    for u in river_nodes.tolist():
        r = int(receiver[u])
        if r >= 0 and is_river[r]:
            upstream.setdefault(r, []).append(u)

    # Compute exact Strahler stream order on the river tree via Kahn's topological sweep
    in_deg_r = {u: len(upstream.get(u, ())) for u in river_nodes.tolist()}
    strahler: dict[int, int] = {}
    queue = [u for u, d in in_deg_r.items() if d == 0]
    q_idx = 0
    while q_idx < len(queue):
        u = queue[q_idx]
        q_idx += 1
        ups = upstream.get(u)
        if not ups:
            strahler[u] = 1
        else:
            orders = [strahler[p] for p in ups]
            mx = max(orders)
            strahler[u] = mx + 1 if orders.count(mx) >= 2 else mx
        r = int(receiver[u])
        if r >= 0 and is_river[r]:
            in_deg_r[r] -= 1
            if in_deg_r[r] == 0:
                queue.append(r)

    # Decompose the river tree cleanly into Horton/Hack continuous branches:
    # At every confluence, the highest-flow parent continues uninterrupted as the trunk;
    # every secondary parent ends at the confluence as a tributary!
    dominant_parent: dict[int, int] = {}
    for v, ups in upstream.items():
        # Pick upstream branch with highest flow (tie-break: highest Strahler order)
        best_p = max(ups, key=lambda p: (float(flow_flat[p]), strahler.get(p, 1)))
        dominant_parent[v] = best_p

    # Identify every river outlet cell (flows into ocean/lake) and every tributary termination cell
    outlets: list[int] = []
    tributary_ends: list[tuple[int, int]] = []  # (trib_last_cell, confluence_cell)
    for u in river_nodes.tolist():
        r = int(receiver[u])
        if r < 0 or not land_d_flat[r]:
            outlets.append(u)
        elif is_river[r] and dominant_parent.get(r) != u:
            tributary_ends.append((u, r))

    def trace_upstream_branch(end_cell: int) -> list[int]:
        chain = [end_cell]
        cur = end_cell
        seen = {cur}
        while cur in dominant_parent:
            p = dominant_parent[cur]
            if p in seen:
                break
            seen.add(p)
            chain.append(p)
            cur = p
        chain.reverse()
        return chain

    # Build hierarchical list of coarse branches: main stems first, then tributaries sorted
    # from high flow to low flow so parent confluence vertices exist before tributaries snap to them!
    coarse_branches: list[dict[str, Any]] = []
    for out_u in outlets:
        chain = trace_upstream_branch(out_u)
        if len(chain) < 6:
            continue
        mouth_d = int(receiver[out_u])
        coarse_branches.append(
            {
                "cells": chain,
                "is_main": True,
                "mouth_d": mouth_d,
                "conf_cell_d": -1,
                "order": int(strahler.get(out_u, 1)),
                "max_flow": float(flow_flat[out_u]),
            }
        )

    for trib_u, conf_v in tributary_ends:
        chain = trace_upstream_branch(trib_u)
        if len(chain) < 4:
            continue
        coarse_branches.append(
            {
                "cells": chain,
                "is_main": False,
                "mouth_d": -1,
                "conf_cell_d": conf_v,
                "order": int(strahler.get(trib_u, 1)),
                "max_flow": float(flow_flat[trib_u]),
            }
        )

    # Sort descending by (is_main, order, max_flow) so parent stems are mapped to 4K before their tributaries
    coarse_branches.sort(key=lambda b: (b["is_main"], b["order"], b["max_flow"]), reverse=True)

    scale_r = float(h_full) / float(hd)
    scale_c = float(w_full) / float(wd)
    snap_rad = max(1, int(math.ceil(max(scale_r, scale_c) * 0.75)))

    # Map coarse drainage cell -> exact snapped full-res land pixel (r, c) for exact confluence snapping
    coarse_to_full_anchor: dict[int, tuple[int, int]] = {}
    # Map exact full-res confluence pixel -> exact final (lon, lat) vertex on parent river
    full_confluence_lonlat: dict[tuple[int, int], tuple[float, float]] = {}

    def snap_coarse_cell_to_full_land(cell_d: int) -> tuple[int, int] | None:
        if cell_d in coarse_to_full_anchor:
            return coarse_to_full_anchor[cell_d]
        rd = cell_d // wd
        cd = cell_d % wd
        rc = int(np.clip(round((rd + 0.5) * scale_r - 0.5), 0, h_full - 1))
        cc = int(round((cd + 0.5) * scale_c - 0.5)) % w_full

        # Search small valley window for the lowest strictly-land pixel
        best_pt = None
        best_val = float("inf")
        for rad in (snap_rad, snap_rad + 2):
            r0 = max(0, rc - rad)
            r1 = min(h_full - 1, rc + rad)
            for rr in range(r0, r1 + 1):
                for d_col in range(-rad, rad + 1):
                    cc_t = (cc + d_col) % w_full
                    e = float(elev_out[rr, cc_t])
                    if e > 0.0:
                        # Prefer lowest valley bottom with slight center proximity bias
                        score = e + 1.5 * math.hypot(rr - rc, d_col)
                        if score < best_val:
                            best_val = score
                            best_pt = (rr, cc_t)
            if best_pt is not None:
                break
        if best_pt is not None:
            coarse_to_full_anchor[cell_d] = best_pt
        return best_pt

    final_rivers: list[dict[str, Any]] = []
    # Track exact coarse cells belonging to accepted parent rivers
    accepted_coarse_cells: set[int] = set()
    # Map coarse cell -> its exact full-res pixel on the accepted parent river
    parent_coarse_pixel: dict[int, tuple[int, int]] = {}
    # Precompute all coarse confluence cells that have child tributaries so we PIN them!
    confluence_coarse_set: set[int] = {conf_v for _, conf_v in tributary_ends}

    for br in coarse_branches:
        cells = br["cells"]
        is_main = br["is_main"]
        conf_cell_d = br["conf_cell_d"]

        # Tributaries only attach if their parent river was actually kept
        if (not is_main) and (conf_cell_d not in accepted_coarse_cells):
            continue

        # Snap coarse nodes to full-res land anchors
        anchors: list[tuple[int, int, int]] = []  # (cell_d, r_f, c_f)
        for u in cells:
            pt = snap_coarse_cell_to_full_land(u)
            if pt is not None:
                if not anchors or (anchors[-1][1], anchors[-1][2]) != pt:
                    anchors.append((u, pt[0], pt[1]))

        if len(anchors) < 4:
            continue

        # Append confluence target anchor for tributaries
        if not is_main:
            conf_pt = parent_coarse_pixel[conf_cell_d]
            if (anchors[-1][1], anchors[-1][2]) != conf_pt:
                anchors.append((conf_cell_d, conf_pt[0], conf_pt[1]))

        # Connect anchors strictly across full-res land pixels
        full_pixels: list[tuple[int, int]] = [(anchors[0][1], anchors[0][2])]
        anchor_coarse_pos: dict[int, tuple[int, int]] = {anchors[0][0]: (anchors[0][1], anchors[0][2])}
        connected = True
        for i in range(1, len(anchors)):
            u_next, r_next, c_next = anchors[i]
            r_prev, c_prev = full_pixels[-1]
            seg = _local_land_step_path(r_prev, c_prev, r_next, c_next, elev_out, allow_end_ocean=False)
            if seg is None:
                if is_main and i >= len(anchors) - 3:
                    # Very close to coast — let exact downhill mouth walker finish from here
                    break
                connected = False
                break
            full_pixels.extend(seg[1:])
            anchor_coarse_pos[u_next] = (r_next, c_next)

        if not connected or len(full_pixels) < 8:
            continue

        # Deduplicate any immediate loops so river path is simple
        seen_idx: dict[tuple[int, int], int] = {}
        simple_px: list[tuple[int, int]] = []
        for pt in full_pixels:
            if pt in seen_idx:
                # Trim loop cleanly
                cut = seen_idx[pt]
                for rem in simple_px[cut + 1:]:
                    seen_idx.pop(rem, None)
                simple_px = simple_px[: cut + 1]
            else:
                seen_idx[pt] = len(simple_px)
                simple_px.append(pt)
        full_pixels = simple_px

        if len(full_pixels) < 8:
            continue

        # For main-stem rivers, walk downhill strictly on land to the EXACT first elev <= 0.0 mouth pixel
        if is_main:
            mouth_d = br["mouth_d"]
            if mouth_d >= 0:
                hint_r = int(np.clip(round((mouth_d // wd + 0.5) * scale_r - 0.5), 0, h_full - 1))
                hint_c = int(round((mouth_d % wd + 0.5) * scale_c - 0.5)) % w_full
            else:
                hint_r, hint_c = full_pixels[-1]

            tail = _walk_downhill_to_exact_mouth(
                full_pixels[-1][0], full_pixels[-1][1], hint_r, hint_c, elev_out
            )
            if tail is None or len(tail) < 2:
                continue
            # Avoid duplicating the join pixel; verify no pixel before final is ocean
            for pt in tail[1:-1]:
                if pt in seen_idx:
                    cut = seen_idx[pt]
                    for rem in full_pixels[cut + 1:]:
                        seen_idx.pop(rem, None)
                    full_pixels = full_pixels[: cut + 1]
                else:
                    seen_idx[pt] = len(full_pixels)
                    full_pixels.append(pt)
            mouth_pt = tail[-1]
            if elev_out[mouth_pt[0], mouth_pt[1]] > 0.0:
                continue
            full_pixels.append(mouth_pt)
        else:
            # Verify tributary ends cleanly on its parent confluence pixel on land
            conf_pt = parent_coarse_pixel[conf_cell_d]
            if full_pixels[-1] != conf_pt:
                continue

        # Final strict check: every inland vertex before mouth must have elev_out > 0.0
        inland_end = len(full_pixels) - 1 if is_main else len(full_pixels)
        if any(elev_out[r, c] <= 0.0 for (r, c) in full_pixels[:inland_end]):
            continue

        # Ensure river actually flows downhill (headwater higher than end)
        e_start = float(elev_out[full_pixels[0][0], full_pixels[0][1]])
        e_end = float(elev_out[full_pixels[-1][0], full_pixels[-1][1]])
        if e_start <= max(e_end, 1.0):
            continue

        # Enforce smooth monotonically non-increasing elevation directly along the 4K
        # river centerline (gently seats out any tiny 1-3m alpine micro-bumps)
        running_e = e_start
        for k in range(1, inland_end):
            r, c = full_pixels[k]
            cur_e = float(elev_out[r, c])
            if cur_e > running_e:
                # Gently carve tiny bump down to running_e (never below 0.5 m on land)
                new_e = max(0.5, running_e)
                elev_out[r, c] = new_e
                # Softly ease immediate land neighbors so centerline sits smoothly in banks
                for dr, dc in ((-1, 0), (1, 0), (0, -1), (0, 1)):
                    nr = r + dr
                    if 0 <= nr < h_full:
                        nc = (c + dc) % w_full
                        if elev_out[nr, nc] > new_e + 12.0:
                            elev_out[nr, nc] = min(float(elev_out[nr, nc]), 0.5 * (float(elev_out[nr, nc]) + new_e + 8.0))
            else:
                running_e = max(0.5, cur_e)

        # Record accepted coarse cells for child tributaries
        for u_c, pt_c in anchor_coarse_pos.items():
            if pt_c in seen_idx:
                accepted_coarse_cells.add(u_c)
                parent_coarse_pixel[u_c] = pt_c

        # Split polyline if it crosses the antimeridian (lon = +/-180) so no map-spanning lines exist
        segments: list[list[tuple[int, int]]] = []
        cur_seg = [full_pixels[0]]
        for k in range(1, len(full_pixels)):
            prev_c = full_pixels[k - 1][1]
            curr_c = full_pixels[k][1]
            if abs(curr_c - prev_c) > w_full // 2:
                if len(cur_seg) >= 4:
                    segments.append(cur_seg)
                cur_seg = [full_pixels[k]]
            else:
                cur_seg.append(full_pixels[k])
        if len(cur_seg) >= 3:
            segments.append(cur_seg)

        for seg_idx, seg_px in enumerate(segments):
            seg_is_mouth = is_main and (seg_idx == len(segments) - 1)
            seg_is_trib_end = (not is_main) and (seg_idx == len(segments) - 1)

            # Identify exact indices on this segment that are parent confluences (PIN them!)
            seg_px_index = {pt: idx_k for idx_k, pt in enumerate(seg_px)}
            pinned_indices: set[int] = set()
            pinned_coarse_at_idx: dict[int, tuple[int, int]] = {}
            for u_c, pt_c in anchor_coarse_pos.items():
                if u_c in confluence_coarse_set and pt_c in seg_px_index:
                    k_pin = seg_px_index[pt_c]
                    if not (seg_is_mouth and k_pin == len(seg_px) - 1):
                        pinned_indices.add(k_pin)
                        pinned_coarse_at_idx[k_pin] = pt_c

            # Smooth interior river coordinates in continuous pixel space (col+0.5, row+0.5),
            # keeping endpoints & pinned confluence vertices exact, and checking rounded lon/lat
            # so NO smoothed vertex ever crosses a pixel edge into ocean!
            pts_xy = [(float(c) + 0.5, float(r) + 0.5) for (r, c) in seg_px]
            if len(pts_xy) >= 4:
                smoothed_xy = [pts_xy[0]]
                for k in range(1, len(pts_xy) - 1):
                    if k in pinned_indices or seg_px[k] in full_confluence_lonlat:
                        smoothed_xy.append(pts_xy[k])
                        continue
                    sx = 0.25 * pts_xy[k - 1][0] + 0.5 * pts_xy[k][0] + 0.25 * pts_xy[k + 1][0]
                    sy = 0.25 * pts_xy[k - 1][1] + 0.5 * pts_xy[k][1] + 0.25 * pts_xy[k + 1][1]
                    lon_s, lat_s = _xy_to_lonlat(sx, sy, w_full, h_full)
                    sr, sc = _lonlat_to_rowcol(round(lon_s, 5), round(lat_s, 5), w_full, h_full)
                    if elev_out[sr, sc] > 0.0:
                        smoothed_xy.append((sx, sy))
                    else:
                        smoothed_xy.append(pts_xy[k])
                smoothed_xy.append(pts_xy[-1])
            else:
                smoothed_xy = list(pts_xy)

            # Lock exact parent confluence coordinate for tributaries (0-gap junction!)
            locked_end_lonlat: tuple[float, float] | None = None
            if seg_is_trib_end:
                conf_pt = seg_px[-1]
                if conf_pt in full_confluence_lonlat:
                    locked_end_lonlat = full_confluence_lonlat[conf_pt]
                    clon, clat = locked_end_lonlat
                    smoothed_xy[-1] = (
                        (clon + 180.0) / 360.0 * w_full,
                        (90.0 - clat) / 180.0 * h_full,
                    )
                else:
                    # Parent was split/dropped before recording this confluence — skip dangling stub
                    continue

            # Tight RDP simplification (0.45 px) keeping pinned confluence vertices & <=24px spacing
            kept_idx = _rdp_simplify_indices(smoothed_xy, eps=0.45, pinned=pinned_indices, max_seg_px=24.0)

            # Convert to rounded (lon, lat) and STRICTLY verify every rounded vertex on elev_out!
            coords_lonlat: list[list[float]] = []
            valid_seg = True
            kept_pos_map: dict[int, tuple[float, float]] = {}

            for out_k, orig_k in enumerate(kept_idx):
                is_last = out_k == len(kept_idx) - 1
                if seg_is_trib_end and is_last and locked_end_lonlat is not None:
                    lon_r, lat_r = locked_end_lonlat
                elif seg_is_mouth and is_last:
                    mr, mc = seg_px[-1]
                    lon_m, lat_m = _px_to_lonlat(mc, mr, w_full, h_full)
                    lon_r, lat_r = round(lon_m, 5), round(lat_m, 5)
                else:
                    x, y = smoothed_xy[orig_k]
                    lon_v, lat_v = _xy_to_lonlat(x, y, w_full, h_full)
                    lon_r, lat_r = round(lon_v, 5), round(lat_v, 5)
                    rr_chk, cc_chk = _lonlat_to_rowcol(lon_r, lat_r, w_full, h_full)
                    if elev_out[rr_chk, cc_chk] <= 0.0:
                        # Revert to exact unsmoothed land pixel center (0.5 px from every boundary!)
                        pr_o, pc_o = seg_px[orig_k]
                        lon_v, lat_v = _px_to_lonlat(pc_o, pr_o, w_full, h_full)
                        lon_r, lat_r = round(lon_v, 5), round(lat_v, 5)
                        rr_chk, cc_chk = _lonlat_to_rowcol(lon_r, lat_r, w_full, h_full)
                        if elev_out[rr_chk, cc_chk] <= 0.0:
                            valid_seg = False
                            break

                # Final guard on mouth vertex
                if seg_is_mouth and is_last:
                    rr_m, cc_m = _lonlat_to_rowcol(lon_r, lat_r, w_full, h_full)
                    if elev_out[rr_m, cc_m] > 0.0:
                        valid_seg = False
                        break

                coords_lonlat.append([lon_r, lat_r])
                kept_pos_map[orig_k] = (lon_r, lat_r)

            if not valid_seg or len(coords_lonlat) < 2:
                continue

            # Guard against any large longitude jump inside a segment
            if any(abs(coords_lonlat[k][0] - coords_lonlat[k - 1][0]) > 10.0 for k in range(1, len(coords_lonlat))):
                continue

            # Verify strict overall downhill elevation on the final rounded endpoints
            r_s, c_s = _lonlat_to_rowcol(coords_lonlat[0][0], coords_lonlat[0][1], w_full, h_full)
            r_e, c_e = _lonlat_to_rowcol(coords_lonlat[-1][0], coords_lonlat[-1][1], w_full, h_full)
            if elev_out[r_s, c_s] <= elev_out[r_e, c_e]:
                continue

            # Record exact pinned rounded (lon, lat) for child tributaries (guaranteed 0-gap match!)
            for k_pin, pt_c in pinned_coarse_at_idx.items():
                if k_pin in kept_pos_map:
                    full_confluence_lonlat[pt_c] = kept_pos_map[k_pin]

            final_rivers.append(
                {
                    "id": "",  # assigned after sorting
                    "stream_order": int(br["order"]),
                    "max_flow": round(float(br["max_flow"]), 3),
                    "coords_lonlat": coords_lonlat,
                }
            )

    # Sort ascending by (stream_order, max_flow) so higher-order trunks render cleanly on top
    final_rivers.sort(key=lambda r: (r["stream_order"], r["max_flow"]))
    for idx_r, r in enumerate(final_rivers, start=1):
        r["id"] = f"river_{idx_r:04d}"

    return final_rivers, elev_out


# ---------------------------------------------------------------------------
# Public API
# ---------------------------------------------------------------------------

def erode_and_compute_climate(elevation_m: np.ndarray, seed: int) -> dict[str, Any]:
    """Refine tectonic elevation with heterogeneous ridged multifractal detail,
    dendritic watershed valley carving, smooth alluvial plains, a strict branching
    river network, and global wind-belt Whittaker climate & smooth biome RGB.

    Args:
        elevation_m: float32 array of shape (H, W) in meters, 0.0 = sea level,
            equirectangular lon [-180, 180], lat [90, -90].
        seed: deterministic RNG seed.

    Returns:
        dict with keys:
            - "elevation_m": np.ndarray float32 (H, W)
            - "temperature_c": np.ndarray float32 (H, W)
            - "moisture": np.ndarray float32 (H, W) in [0.0, 1.0]
            - "biome_id": np.ndarray uint8 (H, W)
            - "biome_names": dict[int, str]
            - "biome_rgb": np.ndarray uint8 (H, W, 3)
            - "rivers": list[dict]
    """
    orig_elev = np.asarray(elevation_m, dtype=np.float32)
    if orig_elev.ndim != 2:
        raise ValueError(f"elevation_m must be 2D (H, W), got shape {orig_elev.shape}")

    h, w = orig_elev.shape
    rng = np.random.default_rng(int(seed))
    orig_land = orig_elev > 0.0

    # Handle degenerate all-ocean input cleanly
    if not np.any(orig_land):
        hc, wc = min(h, 256), min(w, 512)
        u_w, _, _ = _compute_wind_field(hc, wc, rng)
        elev_c = cv2.resize(orig_elev, (wc, hc), interpolation=cv2.INTER_AREA)
        t_sea_c, _ = _compute_temperature_coarse(elev_c, u_w, rng)
        temp_full = _resize_wrap_cubic(t_sea_c, w, h)
        moist_full = np.ones((h, w), dtype=np.float32)
        biome_id = np.zeros((h, w), dtype=np.uint8)
        biome_rgb = np.broadcast_to(np.array([26, 58, 92], dtype=np.uint8), (h, w, 3)).copy()
        return {
            "elevation_m": orig_elev.copy(),
            "temperature_c": temp_full,
            "moisture": moist_full,
            "biome_id": biome_id,
            "biome_names": dict(BIOME_NAMES),
            "biome_rgb": biome_rgb,
            "rivers": [],
        }

    # Working resolutions:
    # - Drainage & macro-mid alpine erosion at up to 1536x768 (fast & crisp!)
    # - Wind-belt climate simulation at up to 512x256
    wd = min(w, 1536)
    hd = max(64, int(round(wd * (float(h) / float(w)))))
    hd = min(h, hd)

    wc = min(w, 512)
    hc = max(32, int(round(wc * (float(h) / float(w)))))
    hc = min(h, hc)

    # Downsample base elevation for drainage grid
    if (hd, wd) == (h, w):
        elev_d_base = orig_elev.copy()
    else:
        elev_d_base = cv2.resize(orig_elev, (wd, hd), interpolation=cv2.INTER_AREA).astype(np.float32)

    land_d = elev_d_base > 0.0

    # ------------------------------------------------------------------
    # Stage 1: Heterogeneous Ridged Multifractal Mountain Detail (on 3D sphere)
    # Breaks smooth/puffy tectonic tubes into crisp peaks, passes, and spurs!
    # ------------------------------------------------------------------
    elev_d_ridged = elev_d_base.copy()
    ridge_crest_d = np.zeros((hd, wd), dtype=np.float32)

    if np.any(land_d):
        lr_d, lc_d = np.nonzero(land_d)
        e_l = elev_d_base[lr_d, lc_d]

        # Domain-warped heterogeneous ridged multifractal (5 octaves: range -> massif -> spurs)
        rmf_macro = _ridged_multifractal_sphere(
            lr_d, lc_d, hd, wd,
            freqs=[10.0, 21.0, 44.0, 92.0, 185.0],
            amps=[0.40, 0.28, 0.18, 0.09, 0.05],
            rng=rng,
            warp_freq=7.5,
            warp_amp=0.055,
            gain=1.95,
            sharpness=2.2,
        )
        # Secondary cross-ridge spur field for branching mountain ridges & cols/passes
        rmf_spur = _ridged_multifractal_sphere(
            lr_d, lc_d, hd, wd,
            freqs=[16.0, 36.0, 78.0, 160.0],
            amps=[0.42, 0.30, 0.18, 0.10],
            rng=rng,
            warp_freq=11.0,
            warp_amp=0.04,
            gain=1.85,
            sharpness=2.0,
        )
        # Gentle rolling hill variation for lowlands & foothills
        hills = _fbm_sphere(lr_d, lc_d, hd, wd, [14.0, 30.0, 62.0], [0.55, 0.30, 0.15], rng)

        combined_ridge = 0.65 * rmf_macro + 0.35 * rmf_spur
        ridge_crest_d[lr_d, lc_d] = np.clip(combined_ridge * 1.35, 0.0, 1.0)

        # Smooth fade near sea level so coastlines remain sharp and land never punches underwater
        coast_fade = _smoothstep(0.0, 95.0, e_l)
        alpine_fade = _smoothstep(280.0, 1800.0, e_l)

        # Multiplicative modulation breaks uniform tectonic tubes into peaks & passes!
        tube_breaker = 1.0 + alpine_fade * ((combined_ridge - 0.38) * 0.72)
        # Additive alpine spurs & rolling foothills
        alpine_add = (
            alpine_fade * (combined_ridge - 0.24) * 1050.0
            + coast_fade * (1.0 - 0.6 * alpine_fade) * (rmf_spur - 0.30) * 210.0
            + coast_fade * hills * 75.0
        )
        e_new = np.maximum(e_l * tube_breaker + alpine_add, np.minimum(e_l, 0.5))
        elev_d_ridged[lr_d, lc_d] = np.clip(e_new, 0.5, 8300.0)

    # ------------------------------------------------------------------
    # Stage 2: Initial Wind-Belt Moisture & Runoff Preview (drives fluvial erosion!)
    # ------------------------------------------------------------------
    u_wind_c, v_wind_c, conv_c = _compute_wind_field(hc, wc, rng)
    elev_c_init = cv2.resize(elev_d_ridged, (wc, hc), interpolation=cv2.INTER_AREA).astype(np.float32)
    t_sea_init, t_surf_init = _compute_temperature_coarse(elev_c_init, u_wind_c, rng)
    _, precip_init_c = _advect_wind_moisture(elev_c_init, t_sea_init, t_surf_init, u_wind_c, v_wind_c, conv_c)
    runoff_d = np.maximum(_resize_wrap_cubic(precip_init_c, wd, hd), 0.02)

    # ------------------------------------------------------------------
    # Stage 3: Multi-Pass Dendritic Valley Carving & Alluvial Plain Smoothing
    # ------------------------------------------------------------------
    elev_d_eroded, alluvial_d = _carve_valleys_and_alluvial(
        elev_d_ridged, runoff_d, ridge_crest_d, n_passes=2
    )

    # ------------------------------------------------------------------
    # Stage 4: Upsample Erosion Delta to Full Resolution + 4K Fine Alpine Crags
    # ------------------------------------------------------------------
    delta_d = np.where(land_d, elev_d_eroded - elev_d_base, 0.0).astype(np.float32)
    if (hd, wd) == (h, w):
        delta_full = delta_d
        alluvial_full = alluvial_d
    else:
        delta_full = _resize_wrap_cubic(delta_d, w, h)
        alluvial_full = np.clip(_resize_wrap_cubic(alluvial_d, w, h), 0.0, 1.0)

    coast_fade_full = _smoothstep(0.0, 55.0, orig_elev)
    elev_full = np.where(
        orig_land,
        np.maximum(orig_elev + delta_full * coast_fade_full, np.minimum(orig_elev, 0.5)),
        orig_elev,
    ).astype(np.float32)

    # Add crisp high-frequency 4K alpine crags & ridge arêtes ONLY on hilly/mountainous land
    # (suppressed inside flat alluvial plains so floodplains stay silky smooth!)
    slope_pre = _spherical_slope(elev_full)
    crag_mask = orig_land & (elev_full > 160.0)
    if np.any(crag_mask):
        lr_f, lc_f = np.nonzero(crag_mask)
        e_f = elev_full[lr_f, lc_f]
        s_f = slope_pre[lr_f, lc_f]
        al_f = alluvial_full[lr_f, lc_f]

        fine_ridge = _ridged_multifractal_sphere(
            lr_f, lc_f, h, w,
            freqs=[340.0, 680.0],
            amps=[0.65, 0.35],
            rng=rng,
            gain=1.8,
            sharpness=2.1,
        )
        alt_w = _smoothstep(160.0, 1600.0, e_f)
        slope_w = 0.35 + 0.65 * _smoothstep(18.0, 110.0, s_f)
        crag_amp = alt_w * slope_w * (1.0 - 0.85 * al_f) * 240.0
        elev_full[lr_f, lc_f] = np.clip(e_f + (fine_ridge - 0.35) * crag_amp, 0.5, 8450.0)

    # Guarantee exact sea-level preservation: ocean untouched, land strictly > 0.0
    elev_full = np.where(orig_land, np.maximum(elev_full, np.minimum(orig_elev, 0.5)), orig_elev).astype(np.float32)

    # ------------------------------------------------------------------
    # Stage 5: Final Wind-Belt Climate Simulation on Final Eroded Elevation
    # ------------------------------------------------------------------
    elev_c_final = cv2.resize(elev_full, (wc, hc), interpolation=cv2.INTER_AREA).astype(np.float32)
    t_sea_final, t_surf_final = _compute_temperature_coarse(elev_c_final, u_wind_c, rng)
    moisture_c, precip_final_c = _advect_wind_moisture(
        elev_c_final, t_sea_final, t_surf_final, u_wind_c, v_wind_c, conv_c
    )

    # High-resolution temperature = smooth sea-level field - exact 6.5 C/km lapse on 4K DEM
    t_sea_full = _resize_wrap_cubic(t_sea_final, w, h)
    temperature_c = (t_sea_full - 0.0065 * np.maximum(elev_full, 0.0)).astype(np.float32)

    # High-resolution moisture = smooth wind-advected field + subtle local valley-vs-ridge wetness
    moisture_base = np.clip(_resize_wrap_cubic(moisture_c, w, h), 0.0, 1.0)
    # Local topographic curvature: carved valleys retain slightly more moisture than exposed ridges
    elev_local_mean = _gauss_wrap(np.maximum(elev_full, 0.0), sigma=4.0)
    valley_wet = np.clip((elev_local_mean - np.maximum(elev_full, 0.0)) / 320.0, -0.05, 0.08)
    moisture = np.where(
        orig_land,
        np.clip(moisture_base + valley_wet * (0.4 + 0.6 * moisture_base), 0.0, 1.0),
        1.0,
    ).astype(np.float32)

    # ------------------------------------------------------------------
    # Stage 6: Final Branching River Network Extraction & Strict 4K Snapping
    # ------------------------------------------------------------------
    if (hd, wd) == (h, w):
        elev_d_final = elev_full.copy()
    else:
        elev_d_final = cv2.resize(elev_full, (wd, hd), interpolation=cv2.INTER_AREA).astype(np.float32)
    runoff_final_d = np.maximum(_resize_wrap_cubic(precip_final_c, wd, hd), 0.02)

    rivers, elev_full = _extract_rivers_fullres(elev_full, elev_d_final, runoff_final_d)

    # Recompute exact high-res slope after final river centerline seating
    slope_final = _spherical_slope(elev_full)

    # ------------------------------------------------------------------
    # Stage 7: Smooth Whittaker Satellite-Atlas Biome Palette & Biome IDs
    # ------------------------------------------------------------------
    biome_id, biome_rgb = _classify_and_color_biomes(
        elev_full, temperature_c, moisture, slope_final, rng
    )

    return {
        "elevation_m": elev_full.astype(np.float32),
        "temperature_c": temperature_c.astype(np.float32),
        "moisture": moisture.astype(np.float32),
        "biome_id": biome_id.astype(np.uint8),
        "biome_names": dict(BIOME_NAMES),
        "biome_rgb": biome_rgb.astype(np.uint8),
        "rivers": rivers,
    }


# ---------------------------------------------------------------------------
# Self-Test Suite (4096x2048 runtime, 0 river ocean violations, rain shadow)
# ---------------------------------------------------------------------------

def _make_synthetic_test_world(width: int = 4096, height: int = 2048) -> np.ndarray:
    """Create a synthetic equirectangular world with:
    1. A mid-latitude continent (25N..65N, 120W..30W) with a crisp N-S coastal cordillera
       along its west edge (testing Westerlies windward-west vs leeward-east rain shadow).
    2. A tropical continent (22S..22N, 10E..95E) with a high eastern mountain chain
       (testing Trade Winds windward-east vs leeward-west rain shadow) + an enclosed inland sea.
    3. A southern continent crossing the antimeridian (lon = +/-180) to verify wrap safety.
    """
    lon = -180.0 + (np.arange(width, dtype=np.float32) + 0.5) * (360.0 / width)
    lat = 90.0 - (np.arange(height, dtype=np.float32) + 0.5) * (180.0 / height)
    LON, LAT = np.meshgrid(lon, lat)

    elev = np.full((height, width), -3500.0, dtype=np.float32)

    # 1. Mid-latitude continent (Westerlies belt: 24N..66N, -125..-35)
    d1 = np.hypot((LON + 80.0) / 46.0, (LAT - 45.0) / 22.0)
    c1 = 1100.0 * np.clip(1.0 - d1 * d1, 0.0, 1.0)
    # N-S western coastal cordillera at lon ~ -102 (windward west, leeward east rain shadow!)
    ridge1 = 3600.0 * np.exp(-0.5 * ((LON + 102.0) / 4.2) ** 2) * np.exp(-0.5 * ((LAT - 46.0) / 14.0) ** 2)
    mask1 = d1 < 1.0
    elev = np.where(mask1, np.maximum(elev, 120.0 + c1 + ridge1), elev)

    # 2. Tropical continent (Trade Winds belt: -24..+24, +12..+98)
    d2 = np.hypot((LON - 55.0) / 44.0, (LAT - 0.0) / 24.0)
    c2 = 950.0 * np.clip(1.0 - d2 * d2, 0.0, 1.0)
    # High eastern N-S cordillera at lon ~ +78 (Trade Winds blow West -> east is windward, west is leeward!)
    ridge2 = 3800.0 * np.exp(-0.5 * ((LON - 78.0) / 4.5) ** 2) * np.exp(-0.5 * ((LAT - 2.0) / 16.0) ** 2)
    # Enclosed inland sea inside western basin of tropical continent
    d_sea = np.hypot((LON - 36.0) / 6.5, (LAT - 4.0) / 4.5)
    mask2 = d2 < 1.0
    elev2 = 100.0 + c2 + ridge2
    elev2 = np.where(d_sea < 1.0, -450.0 * (1.0 - d_sea * d_sea) - 20.0, elev2)
    elev = np.where(mask2, np.where(d_sea < 1.0, elev2, np.maximum(elev, elev2)), elev)

    # 3. Antimeridian-crossing southern island continent (lat -52..-22, lon near +/-180)
    dlon_wrap = np.minimum(np.abs(LON - 180.0), np.abs(LON + 180.0))
    d3 = np.hypot(dlon_wrap / 22.0, (LAT + 38.0) / 14.0)
    c3 = 850.0 * np.clip(1.0 - d3 * d3, 0.0, 1.0)
    ridge3 = 2600.0 * np.exp(-0.5 * (dlon_wrap / 3.8) ** 2) * np.exp(-0.5 * ((LAT + 38.0) / 9.0) ** 2)
    elev = np.where(d3 < 1.0, np.maximum(elev, 90.0 + c3 + ridge3), elev)

    # Continental shelves
    land = elev > 0.0
    dist_sea = distance_transform_edt(~land).astype(np.float32)
    elev = np.where(~land, np.maximum(-3800.0, -40.0 - 28.0 * dist_sea), elev)
    return elev.astype(np.float32)


def _run_self_test() -> None:
    print("=== climate_erosion.py Self-Test (4096x2048) ===")
    W, H = 4096, 2048
    t_gen0 = time.perf_counter()
    base_elev = _make_synthetic_test_world(W, H)
    t_gen1 = time.perf_counter()
    print(f"Synthetic 4096x2048 base terrain generated in {t_gen1 - t_gen0:.2f}s (land={(base_elev > 0).mean()*100:.1f}%)")

    t0 = time.perf_counter()
    res = erode_and_compute_climate(base_elev, seed=42)
    elapsed = time.perf_counter() - t0
    print(f"erode_and_compute_climate(4096x2048) runtime: {elapsed:.2f}s (budget < 45.0s)")

    # 1. Schema, dtypes, shapes, finite checks
    expected_keys = {
        "elevation_m", "temperature_c", "moisture",
        "biome_id", "biome_names", "biome_rgb", "rivers",
    }
    assert set(res.keys()) == expected_keys, f"Unexpected keys: {set(res.keys())}"
    elev = res["elevation_m"]
    temp = res["temperature_c"]
    moist = res["moisture"]
    bid = res["biome_id"]
    bnames = res["biome_names"]
    brgb = res["biome_rgb"]
    rivers = res["rivers"]

    assert elev.shape == (H, W) and elev.dtype == np.float32
    assert temp.shape == (H, W) and temp.dtype == np.float32
    assert moist.shape == (H, W) and moist.dtype == np.float32
    assert bid.shape == (H, W) and bid.dtype == np.uint8
    assert brgb.shape == (H, W, 3) and brgb.dtype == np.uint8
    assert np.all(np.isfinite(elev)) and np.all(np.isfinite(temp)) and np.all(np.isfinite(moist))
    assert float(moist.min()) >= 0.0 and float(moist.max()) <= 1.0
    assert set(np.unique(bid).tolist()).issubset(set(bnames.keys()))

    # 2. Sea-level preservation check
    orig_land = base_elev > 0.0
    new_land = elev > 0.0
    mask_diff = int(np.sum(orig_land != new_land))
    assert mask_diff == 0, f"Sea-level mask altered at {mask_diff} pixels!"
    ocean_diff = float(np.max(np.abs(elev[~orig_land] - base_elev[~orig_land])))
    assert ocean_diff == 0.0, f"Ocean floor modified by {ocean_diff}m!"
    print(f"Elevation range: [{elev.min():.1f}m, {elev.max():.1f}m] | sea-level mask diff: {mask_diff} px")

    # 3. STRICT RIVER INVARIANT verification
    inland_ocean_violations = 0
    wrap_violations = 0
    downhill_violations = 0
    coastal_mouths = 0
    confluence_terminations = 0
    dangling_ends = 0
    max_order = 0

    # Collect all river interior vertices for confluence verification
    all_vertices_by_river: list[set[tuple[float, float]]] = []
    for r in rivers:
        all_vertices_by_river.append({(p[0], p[1]) for p in r["coords_lonlat"]})

    for idx_r, r in enumerate(rivers):
        coords = r["coords_lonlat"]
        assert len(coords) >= 2
        assert isinstance(r["id"], str) and r["id"]
        assert isinstance(r["stream_order"], int) and r["stream_order"] >= 1
        assert isinstance(r["max_flow"], float) and r["max_flow"] > 0.0
        max_order = max(max_order, r["stream_order"])

        # Check every vertex BEFORE final point is strictly on land (elev > 0.0)
        for lon, lat in coords[:-1]:
            assert -180.0 <= lon <= 180.0 and -90.0 <= lat <= 90.0
            rr, cc = _lonlat_to_rowcol(lon, lat, W, H)
            if elev[rr, cc] <= 0.0:
                inland_ocean_violations += 1

        # Check consecutive antimeridian jumps
        for k in range(1, len(coords)):
            if abs(coords[k][0] - coords[k - 1][0]) > 10.0:
                wrap_violations += 1

        # Check overall downhill flow (headwater > end)
        r0, c0 = _lonlat_to_rowcol(coords[0][0], coords[0][1], W, H)
        r_end, c_end = _lonlat_to_rowcol(coords[-1][0], coords[-1][1], W, H)
        if elev[r0, c0] <= elev[r_end, c_end]:
            downhill_violations += 1

        # Check final vertex: either exact mouth (elev <= 0.0) or exact confluence on another river
        if elev[r_end, c_end] <= 0.0:
            coastal_mouths += 1
        else:
            end_pt = (coords[-1][0], coords[-1][1])
            joined = any(
                (j != idx_r and end_pt in all_vertices_by_river[j])
                for j in range(len(rivers))
            )
            if joined:
                confluence_terminations += 1
            else:
                dangling_ends += 1

    print(
        f"Rivers: {len(rivers)} total (coastal mouths={coastal_mouths}, "
        f"confluences={confluence_terminations}, dangling={dangling_ends}, max_order={max_order})"
    )
    print(
        f"River invariant checks: inland_ocean_violations={inland_ocean_violations}, "
        f"wrap_violations={wrap_violations}, downhill_violations={downhill_violations}"
    )
    assert len(rivers) >= 30, f"Too few rivers extracted: {len(rivers)}"
    assert max_order >= 2, f"Expected branching rivers with stream_order >= 2, got {max_order}"
    assert inland_ocean_violations == 0, f"FAILED: {inland_ocean_violations} inland river vertices in ocean!"
    assert wrap_violations == 0, f"FAILED: {wrap_violations} antimeridian wrap jumps!"
    assert downhill_violations == 0, f"FAILED: {downhill_violations} rivers not flowing downhill!"

    # 4. Windward vs Leeward Rain Shadow checks (both Westerlies & Trade Winds!)
    lon = -180.0 + (np.arange(W, dtype=np.float32) + 0.5) * (360.0 / W)
    lat = 90.0 - (np.arange(H, dtype=np.float32) + 0.5) * (180.0 / H)
    LON, LAT = np.meshgrid(lon, lat)

    # Westerlies (36N..56N): west flank (-116..-103) is windward, east basin (-96..-70) is leeward rain shadow
    w_windward = orig_land & (LAT >= 36.0) & (LAT <= 56.0) & (LON >= -116.0) & (LON <= -103.0)
    w_leeward = orig_land & (LAT >= 36.0) & (LAT <= 56.0) & (LON >= -95.0) & (LON <= -70.0)
    m_west_ww = float(moist[w_windward].mean())
    m_west_lw = float(moist[w_leeward].mean())
    west_delta = m_west_ww - m_west_lw
    print(f"Westerlies rain shadow (36N-56N): windward={m_west_ww:.3f}, leeward={m_west_lw:.3f}, delta=+{west_delta:.3f}")
    assert west_delta > 0.12, f"Westerlies rain shadow delta too small: {west_delta:.3f}"

    # Trade Winds (5N..20N): east flank (+80..+92) is windward, west basin (+52..+72) is leeward rain shadow
    t_windward = orig_land & (LAT >= 5.0) & (LAT <= 20.0) & (LON >= 80.0) & (LON <= 92.0)
    t_leeward = orig_land & (LAT >= 5.0) & (LAT <= 20.0) & (LON >= 52.0) & (LON <= 72.0)
    m_trade_ww = float(moist[t_windward].mean())
    m_trade_lw = float(moist[t_leeward].mean())
    trade_delta = m_trade_ww - m_trade_lw
    print(f"Trade Winds rain shadow (5N-20N): windward={m_trade_ww:.3f}, leeward={m_trade_lw:.3f}, delta=+{trade_delta:.3f}")
    assert trade_delta > 0.10, f"Trade Winds rain shadow delta too small: {trade_delta:.3f}"

    # 5. Runtime budget check
    assert elapsed < 45.0, f"Runtime {elapsed:.2f}s exceeded 45s budget!"
    print("ALL SELF-TEST ASSERTIONS PASSED!")


if __name__ == "__main__":
    _run_self_test()
