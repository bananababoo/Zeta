struct TerrainSplatUniform {
    // x = grid_width, y = grid_height, z = tiles_per_repeat, w = grid_alpha
    params: vec4<f32>,
    // Per-material UV scale multipliers for layer0..layer3.
    scales: vec4<f32>,
    // Per-map ambient lighting tint (rgb) to unify textures & entity sprites from mixed sources.
    ambient_tint: vec4<f32>,
}

@group(2) @binding(0) var<uniform> material: TerrainSplatUniform;
@group(2) @binding(1) var splat_texture: texture_2d<f32>;
@group(2) @binding(2) var splat_sampler: sampler;
@group(2) @binding(3) var layer0_texture: texture_2d<f32>;
@group(2) @binding(4) var layer0_sampler: sampler;
@group(2) @binding(5) var layer1_texture: texture_2d<f32>;
@group(2) @binding(6) var layer1_sampler: sampler;
@group(2) @binding(7) var layer2_texture: texture_2d<f32>;
@group(2) @binding(8) var layer2_sampler: sampler;
@group(2) @binding(9) var layer3_texture: texture_2d<f32>;
@group(2) @binding(10) var layer3_sampler: sampler;
@group(2) @binding(11) var region_mask_texture: texture_2d<f32>;
@group(2) @binding(12) var region_mask_sampler: sampler;

@fragment
fn fragment(@location(2) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let grid_size = material.params.xy;
    let repeat_div = max(material.params.z, 1.0);
    let grid_alpha = material.params.w;

    let tile_uv = uv * grid_size;
    let base_uv = tile_uv / repeat_div;

    let splat = textureSample(splat_texture, splat_sampler, uv);
    let c0 = textureSample(layer0_texture, layer0_sampler, base_uv * material.scales.x).rgb;
    let c1 = textureSample(layer1_texture, layer1_sampler, base_uv * material.scales.y).rgb;
    let c2 = textureSample(layer2_texture, layer2_sampler, base_uv * material.scales.z).rgb;
    let c3 = textureSample(layer3_texture, layer3_sampler, base_uv * material.scales.w).rgb;

    var color = c0;
    color = mix(color, c1, splat.r);
    color = mix(color, c2, splat.g);
    color = color * (1.0 - splat.a);
    color = mix(color, c3, splat.b);

    let cell = fract(tile_uv);
    let edge = min(min(cell.x, 1.0 - cell.x), min(cell.y, 1.0 - cell.y));
    let line = 1.0 - smoothstep(0.0, 0.025, edge);
    color = mix(color, vec3<f32>(1.0, 1.0, 1.0), line * grid_alpha);
    color = color * material.ambient_tint.rgb;

    let mask = textureSample(region_mask_texture, region_mask_sampler, uv).r;
    return vec4<f32>(color, mask);
}
