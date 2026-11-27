// The 2D skybox behind everything in the 3D camera views: a cube around the camera,
// drawn first and at the far plane, showing worldspawn's skybox images or Garry's Mod's
// painted sky in every direction no face covers.

struct Uniforms {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
};

// x: what the sky shows (0 nothing, 2 skybox images, 3 painted sky).
// Painted sky: top colour and fade bias, bottom colour and dusk scale, dusk colour and
// intensity. Sun glow: direction and the cosine of its size (above 1.5 for none), colour.
struct Sky {
    mode: vec4<f32>,
    top: vec4<f32>,
    bottom: vec4<f32>,
    dusk: vec4<f32>,
    sun_glow: vec4<f32>,
    sun_glow_color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(1) @binding(0) var<uniform> sky: Sky;
@group(1) @binding(1) var faces: texture_2d_array<f32>;
@group(1) @binding(2) var face_sampler: sampler;

struct SkyOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) direction: vec3<f32>,
};

// The 36 corners of a cube's 12 triangles, two per side.
fn corner(index: u32) -> vec3<f32> {
    var sides = array<vec3<u32>, 12>(
        vec3<u32>(0u, 1u, 3u), vec3<u32>(0u, 3u, 2u),
        vec3<u32>(4u, 6u, 7u), vec3<u32>(4u, 7u, 5u),
        vec3<u32>(0u, 4u, 5u), vec3<u32>(0u, 5u, 1u),
        vec3<u32>(2u, 3u, 7u), vec3<u32>(2u, 7u, 6u),
        vec3<u32>(0u, 2u, 6u), vec3<u32>(0u, 6u, 4u),
        vec3<u32>(1u, 5u, 7u), vec3<u32>(1u, 7u, 3u),
    );
    let corner = sides[index / 3u][index % 3u];
    return vec3<f32>(
        f32(corner & 1u) * 2.0 - 1.0,
        f32((corner >> 1u) & 1u) * 2.0 - 1.0,
        f32((corner >> 2u) & 1u) * 2.0 - 1.0,
    );
}

@vertex
fn vs_sky(@builtin(vertex_index) index: u32) -> SkyOut {
    let direction = corner(index);
    var clip = uniforms.view_proj * vec4<f32>(uniforms.eye.xyz + direction * 8.0, 1.0);
    clip.z = clip.w;
    var out: SkyOut;
    out.clip = clip;
    out.direction = direction;
    return out;
}

fn skybox(direction: vec3<f32>) -> vec3<f32> {
    let size = abs(direction);
    var layer = 0;
    var s = 0.0;
    var t = 0.0;
    if (size.x >= size.y && size.x >= size.z) {
        if (direction.x > 0.0) {
            layer = 0;
            s = -direction.y / size.x;
        } else {
            layer = 1;
            s = direction.y / size.x;
        }
        t = direction.z / size.x;
    } else if (size.y >= size.z) {
        if (direction.y > 0.0) {
            layer = 2;
            s = direction.x / size.y;
        } else {
            layer = 3;
            s = -direction.x / size.y;
        }
        t = direction.z / size.y;
    } else {
        s = -direction.y / size.z;
        if (direction.z > 0.0) {
            layer = 4;
            t = -direction.x / size.z;
        } else {
            layer = 5;
            t = direction.x / size.z;
        }
    }
    let uv = vec2<f32>((s + 1.0) * 0.5, 1.0 - (t + 1.0) * 0.5);
    return textureSampleLevel(faces, face_sampler, uv, layer, 0.0).rgb;
}

fn painted(direction: vec3<f32>) -> vec3<f32> {
    let height = clamp(direction.z, 0.0, 1.0);
    let fade = pow(height, max(sky.top.w, 0.01));
    var color = mix(sky.bottom.rgb, sky.top.rgb, fade);
    let horizon = pow(1.0 - abs(direction.z), max(4.0 / max(sky.bottom.w, 0.05), 0.5));
    color = color + sky.dusk.rgb * sky.dusk.w * horizon * 0.5;
    return color;
}

@fragment
fn fs_sky(input: SkyOut) -> @location(0) vec4<f32> {
    let direction = normalize(input.direction);
    var color = painted(direction);
    if (u32(sky.mode.x) == 2u) {
        color = skybox(direction);
    }
    if (sky.sun_glow.w < 1.5) {
        let along = dot(direction, sky.sun_glow.xyz);
        let core = smoothstep(sky.sun_glow.w, mix(sky.sun_glow.w, 1.0, 0.3), along);
        let halo = pow(max(along, 0.0), 256.0) * 0.6;
        color = color + sky.sun_glow_color.rgb * (core + halo);
    }
    return vec4<f32>(color, 1.0);
}
