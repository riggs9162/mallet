// The selection glow drawn around selected props in the 3D view, as Hammer++ draws it.

@group(0) @binding(0) var glow_mask: texture_2d<f32>;
// rgb: the glow's colour; w: how many pixels it reaches beyond the prop's silhouette.
@group(0) @binding(1) var<uniform> glow: vec4<f32>;

// One triangle that covers the whole viewport.
@vertex
fn vs_glow(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

// Hammer++'s selection glow: a band in the glow colour just outside where selected props
// cover the image, fading towards its outer edge.
@fragment
fn fs_glow(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let size = vec2<i32>(textureDimensions(glow_mask));
    let pixel = vec2<i32>(position.xy);
    if (textureLoad(glow_mask, pixel, 0).r > 0.5) {
        discard;
    }
    let reach = i32(glow.w);
    var nearest = 1000.0;
    for (var y = -reach; y <= reach; y++) {
        for (var x = -reach; x <= reach; x++) {
            let at = clamp(pixel + vec2<i32>(x, y), vec2<i32>(0), size - 1);
            if (textureLoad(glow_mask, at, 0).r > 0.5) {
                nearest = min(nearest, length(vec2<f32>(f32(x), f32(y))));
            }
        }
    }
    if (nearest > glow.w) {
        discard;
    }
    let strength = 1.0 - smoothstep(glow.w * 0.5, glow.w, nearest);
    return vec4<f32>(glow.rgb * strength, strength);
}
