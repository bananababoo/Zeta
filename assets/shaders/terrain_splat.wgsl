struct TerrainSplatUniform {
    // x = grid_width, y = grid_height, z = tiles_per_texture_repeat, w = grid_alpha
    params: vec4<f32>,
}

@group(2) @binding(0) var<uniform> material: TerrainSplatUniform;
@group(2) @binding(1) var splat_texture: texture_2d<f32>;
@group(2) @binding(2) var splat_sampler: sampler;
@group(2) @binding(3) var grass_texture: texture_2d<f32>;
@group(2) @binding(4) var grass_sampler: sampler;
@group(2) @binding(5) var dirt_texture: texture_2d<f32>;
@group(2) @binding(6) var dirt_sampler: sampler;
@group(2) @binding(7) var rock_texture: texture_2d<f32>;
@group(2) @binding(8) var rock_sampler: sampler;
@group(2) @binding(9) var water_texture: texture_2d<f32>;
@group(2) @binding(10) var water_sampler: sampler;

@fragment
fn fragment(@location(2) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let grid_size = material.params.xy;
    let repeat_div = max(material.params.z, 1.0);
    let grid_alpha = material.params.w;

    let tile_uv = uv * grid_size;
    let tex_uv = tile_uv / repeat_div;

    let splat = textureSample(splat_texture, splat_sampler, uv);
    let grass = textureSample(grass_texture, grass_sampler, tex_uv).rgb;
    let dirt = textureSample(dirt_texture, dirt_sampler, tex_uv * 1.15).rgb;
    let rock = textureSample(rock_texture, rock_sampler, tex_uv * 1.05).rgb;
    let water = textureSample(water_texture, water_sampler, tex_uv * 0.95).rgb;

    // Gentle low-frequency dappling so large grass areas look organic.
    let dapple = 0.90 + 0.14 * sin(tile_uv.x * 0.7 + sin(tile_uv.y * 0.6)) * cos(tile_uv.y * 0.5);
    var color = grass * dapple;

    // Blend dirt road/patches (R), rocky/muddy bank (G), wet rim darkening (A), and river water (B).
    color = mix(color, dirt, splat.r);
    color = mix(color, mix(dirt, rock, 0.45), splat.g);
    color = color * (1.0 - 0.32 * splat.a);
    let water_shaded = water * (0.76 + 0.24 * splat.b);
    color = mix(color, water_shaded, splat.b);

    // Subtle 5-foot VTT grid lines.
    let cell = fract(tile_uv);
    let edge = min(min(cell.x, 1.0 - cell.x), min(cell.y, 1.0 - cell.y));
    let line = 1.0 - smoothstep(0.0, 0.025, edge);
    color = mix(color, vec3<f32>(1.0, 1.0, 1.0), line * grid_alpha);

    return vec4<f32>(color, 1.0);
}
