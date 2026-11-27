// Terrain splat: blends palette texture-array layers through a per-chunk control map.
//
// Contract: `terrain_contract.md` Section 4. Mesh UV (0, 0) is the top-left corner and V grows
// downward (bevy_mesh-0.20.1/src/primitives/dim2.rs:1068), matching tile `local_y`.
//
// Control map: `params.w` texels per tile plus a 1-texel halo on every side. Each texel holds
// R = palette index of layer A / 255, G = palette index of layer B / 255, B = weight of B,
// A = brightness (1 - edge darkening). Texels are read with `textureLoad` and blended
// bilinearly here, because filtering palette indices would mix unrelated layers.
//
// Shade map: 1 texel per tile plus a 1-texel halo; rgb holds hillshade x climate tint / 2.

struct TerrainSplatUniform {
    // x = width in tiles, y = height in tiles, z = grid line alpha, w = control texels per tile.
    params: vec4<f32>,
    // Per palette layer: xy = chunk origin tile modulo the repeat (integers), z = 1 / repeat.
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
    let uv = (tile + layer.xy) * layer.z;
    return textureSampleGrad(layer_textures, layer_sampler, uv, index, ddx * layer.z, ddy * layer.z).rgb;
}

fn control_color(texel: vec2<i32>, tile: vec2<f32>, ddx: vec2<f32>, ddy: vec2<f32>) -> vec3<f32> {
    let dims = vec2<i32>(textureDimensions(control_texture));
    let control = textureLoad(control_texture, clamp(texel, vec2<i32>(0), dims - 1), 0);
    var color = layer_color(control.r, tile, ddx, ddy);
    if control.b > 0.002 && abs(control.g - control.r) > 0.001 {
        color = mix(color, layer_color(control.g, tile, ddx, ddy), control.b);
    }
    return color * control.a;
}

@fragment
fn fragment(@location(2) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let size = material.params.xy;
    let texels_per_tile = material.params.w;
    let tile = uv * size;
    let ddx = dpdx(tile);
    let ddy = dpdy(tile);

    let shade_uv = (tile + vec2<f32>(1.0)) / (size + vec2<f32>(2.0));
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

    let grid_alpha = material.params.z;
    if grid_alpha > 0.0 {
        let cell = fract(tile);
        let edge = min(min(cell.x, 1.0 - cell.x), min(cell.y, 1.0 - cell.y));
        let line = 1.0 - smoothstep(0.0, 0.025, edge);
        color = mix(color, vec3<f32>(1.0), line * grid_alpha);
    }
    return vec4<f32>(color, 1.0);
}
