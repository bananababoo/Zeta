// Terrain splat: blends palette texture-array layers through a per-chunk control map.
//
// Contract: `docs/architecture/world_geography.md` Section 4. Mesh UV (0, 0) is the top-left
// corner and V grows downward (bevy_mesh-0.20.1/src/primitives/dim2.rs:1068), matching tile
// `local_y`.
//
// Control map: `params.w` texels per tile plus a 1-texel halo on every side. Each texel holds
// R = palette index of layer A / 255, G = palette index of layer B / 255, B = weight of B,
// A = brightness (1 - edge darkening). Texels are read with `textureLoad` and blended
// bilinearly here, because filtering palette indices would mix unrelated layers.
//
// Shade map: `N` texels per tile (`2` on planet chunks, `1` on standalone maps) plus a 1-texel
// halo on every side; rgb holds composite 3D sphere-continuous shade (hillshade, canopy dapple,
// shoreline wetness, climate tint) / 2.

const TAU: f32 = 6.2831853;
const MACRO_INV_MULT: f32 = 0.03125;
const SECONDARY_UV_MULT: f32 = 4.0;
const LUMA_WEIGHTS: vec3<f32> = vec3<f32>(0.299, 0.587, 0.114);

struct TerrainSplatUniform {
    // x = width in tiles, y = height in tiles, z = grid line alpha, w = control texels per tile.
    params: vec4<f32>,
    // Per palette layer: xy = chunk origin tile modulo repeat, z = 1 / repeat,
    // w = packed secondary rotated origin (u * 4096 + v) modulo (32 * min(repeat, 127)).
    layer_params: array<vec4<f32>, 32>,
}

@group(2) @binding(0) var<uniform> material: TerrainSplatUniform;
@group(2) @binding(1) var control_texture: texture_2d<f32>;
@group(2) @binding(2) var shade_texture: texture_2d<f32>;
@group(2) @binding(3) var shade_sampler: sampler;
@group(2) @binding(4) var layer_textures: texture_2d_array<f32>;
@group(2) @binding(5) var layer_sampler: sampler;

fn layer_color(encoded: f32, tile: vec2<f32>, ddx: vec2<f32>, ddy: vec2<f32>) -> vec3<f32> {
    let index = min(u32(encoded * 255.0 + 0.5), 31u);
    let layer = material.layer_params[index];

    // Reconstruct the exact CPU-wrapped macro torus coordinate `base_uv` (period = 32 * repeat
    // rotated by atan2(2, 3) ~= 33.7 deg). Raw planetary coordinates never enter f32, and every
    // harmonic of `p = base_uv * TAU` is continuous across chunk boundaries.
    let packed = u32(layer.w + 0.5);
    let sec_origin = vec2<f32>(f32(packed >> 12u), f32(packed & 4095u));
    let rot_tile = vec2<f32>(3.0 * tile.x - 2.0 * tile.y, 2.0 * tile.x + 3.0 * tile.y);
    let base_scale = max(layer.z, 1.0 / 127.0) * MACRO_INV_MULT;
    let base_uv = (rot_tile + sec_origin) * base_scale;
    let phase = f32(index) * 1.73205;
    let p = base_uv * TAU;

    // Gentle periodic phase modulation (~0.26 UV swirl over 30-90 tiles) breaks 2D FFT lattice
    // spikes completely while keeping local grass blades, pebbles, and sand ripples undistorted.
    let swirl1 = vec2<f32>(
        sin(p.x + 0.70 * cos(p.y + phase)) + 0.45 * cos(2.0 * p.y + phase),
        cos(p.y - 0.70 * sin(p.x - phase)) + 0.45 * sin(2.0 * p.x - phase),
    ) * 0.26;
    let uv1 = (tile + layer.xy) * layer.z + swirl1;
    let c1 = textureSampleGrad(
        layer_textures,
        layer_sampler,
        uv1,
        index,
        ddx * layer.z,
        ddy * layer.z,
    ).rgb;

    // Secondary anti-tiling sample: rotated 33.7 deg at ~2.22x scale with independent swirl.
    let swirl2 = vec2<f32>(
        cos(p.y + 0.60 * sin(p.x - phase)),
        sin(p.x - 0.60 * cos(p.y + phase)),
    ) * 0.22;
    let uv2_scale = base_scale * SECONDARY_UV_MULT;
    let uv2 = base_uv * SECONDARY_UV_MULT + swirl2;
    let ddx2 = vec2<f32>(3.0 * ddx.x - 2.0 * ddx.y, 2.0 * ddx.x + 3.0 * ddx.y) * uv2_scale;
    let ddy2 = vec2<f32>(3.0 * ddy.x - 2.0 * ddy.y, 2.0 * ddy.x + 3.0 * ddy.y) * uv2_scale;
    let c2 = textureSampleGrad(layer_textures, layer_sampler, uv2, index, ddx2, ddy2).rgb;

    // Balanced energy-neutral cross-blend in [0.28, 0.72] so rotated cube-face seams never jump
    // in mean brightness, while all macro tonal wash comes from the 3D sphere-continuous `shade`.
    let n_blend = 0.50 * sin(p.x + 0.65 * cos(p.y + phase) + phase)
        + 0.32 * cos(2.0 * p.y - p.x + 1.15)
        + 0.18 * sin(3.0 * p.x + 2.0 * p.y - phase);
    let blend_w = smoothstep(-0.65, 0.65, n_blend) * 0.44 + 0.28;
    let blended = mix(c1, c2, blend_w);

    // Restore crisp micro-detail lost to two-sample averaging without shifting mean luminance.
    let lum1 = dot(c1, LUMA_WEIGHTS);
    let lum_mix = dot(blended, LUMA_WEIGHTS);
    let overlap = 4.0 * blend_w * (1.0 - blend_w);
    let rel_detail = (lum1 - lum_mix) / max(lum_mix, 0.04);
    let detail_gain = clamp(1.0 + rel_detail * (0.46 * overlap + 0.12), 0.78, 1.28);
    return blended * detail_gain;
}

fn control_color(texel: vec2<i32>, tile: vec2<f32>, ddx: vec2<f32>, ddy: vec2<f32>) -> vec3<f32> {
    let dims = vec2<i32>(textureDimensions(control_texture));
    let control = textureLoad(control_texture, clamp(texel, vec2<i32>(0), dims - 1), 0);
    var color = layer_color(control.r, tile, ddx, ddy);
    if control.b > 0.002 && abs(control.g - control.r) > 0.001 {
        let color_b = layer_color(control.g, tile, ddx, ddy);
        let lum_a = dot(color, LUMA_WEIGHTS);
        let lum_b = dot(color_b, LUMA_WEIGHTS);
        // Gentle micro-luminance interlock at tactical zoom that stays soft like a watercolor
        // wash across broad variant cross-fades.
        let height_diff = (lum_b - lum_a) * 0.24 + (control.b * 2.0 - 1.0);
        let height_weight = smoothstep(-0.88, 0.88, height_diff);
        let weight = mix(control.b, height_weight, 0.25);
        color = mix(color, color_b, weight);
    }
    let rim_tint = pow(vec3<f32>(max(control.a, 0.001)), vec3<f32>(1.06, 1.00, 0.95));
    return color * rim_tint;
}

@fragment
fn fragment(@location(2) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let size = material.params.xy;
    let texels_per_tile = material.params.w;
    let tile = uv * size;
    let ddx = dpdx(tile);
    let ddy = dpdy(tile);

    let shade_dims = vec2<f32>(textureDimensions(shade_texture));
    let shade_per_tile = (shade_dims - vec2<f32>(2.0)) / size;
    let shade_uv = (tile * shade_per_tile + vec2<f32>(1.0)) / shade_dims;
    let shade = textureSample(shade_texture, shade_sampler, shade_uv).rgb * 2.0;

    // Texel centers sit at half-integers; the halo shifts every index by 1.
    let p = tile * texels_per_tile + vec2<f32>(0.5);
    let base = floor(p);
    let f = p - base;
    let t = vec2<i32>(base);
    let c00 = control_color(t, tile, ddx, ddy);
    let c10 = control_color(t + vec2<i32>(1, 0), tile, ddx, ddy);
    let c01 = control_color(t + vec2<i32>(0, 1), tile, ddx, ddy);
    let c11 = control_color(t + vec2<i32>(1, 1), tile, ddx, ddy);
    var color = mix(mix(c00, c10, f.x), mix(c01, c11, f.x), f.y) * shade;

    // Soft gouache-like highlight shoulder above 0.62 linear preserves crisp texture detail and
    // natural pigment saturation on bright sand beaches, desert dunes, and alpine snowfields.
    let shoulder = max(color - vec3<f32>(0.62), vec3<f32>(0.0));
    color = color / (vec3<f32>(1.0) + shoulder * 0.42);

    let grid_alpha = material.params.z;
    if grid_alpha > 0.0 {
        let cell = fract(tile);
        let edge = min(min(cell.x, 1.0 - cell.x), min(cell.y, 1.0 - cell.y));
        let line = 1.0 - smoothstep(0.0, 0.025, edge);
        color = mix(color, vec3<f32>(1.0), line * grid_alpha);
    }

    let final_luma = dot(color, LUMA_WEIGHTS);
    color = max(mix(vec3<f32>(final_luma), color, 1.10), vec3<f32>(0.0));
    return vec4<f32>(color, 1.0);
}
