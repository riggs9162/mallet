// Shaders for the textured 3D view.

struct Uniforms {
    view_proj: mat4x4<f32>,
    // xyz: the camera's position; w: 1 when there is a camera, 0 for the 2D views.
    eye: vec4<f32>,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(1) @binding(0) var face_texture: texture_2d<f32>;
@group(1) @binding(1) var face_sampler: sampler;
// x: alpha-test reference; y: opacity, as `premultiplied` reads them. Used by models,
// whose vertices are shared by many materials.
@group(1) @binding(2) var<uniform> material_alpha: vec4<f32>;

struct FaceIn {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) highlight: f32,
    @location(4) alpha: vec2<f32>,
    @location(5) normal: vec3<f32>,
    // rgb: the water's tint; w: its reflectivity, zero when the face is not water.
    @location(7) water: vec4<f32>,
};

struct FaceOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) highlight: f32,
    @location(3) alpha: vec2<f32>,
    @location(4) world: vec3<f32>,
    @location(5) normal: vec3<f32>,
    @location(6) water: vec4<f32>,
};

// Faces are shaded by direction like Hammer's textured mode: those facing up are
// brightest.
@vertex
fn vs_face(input: FaceIn) -> FaceOut {
    let shade = clamp(0.6 + 0.4 * dot(input.normal, normalize(vec3<f32>(0.3, 0.5, 1.0))), 0.35, 1.0);
    var out: FaceOut;
    out.clip = uniforms.view_proj * vec4<f32>(input.position, 1.0);
    out.uv = input.uv;
    out.color = vec4<f32>(input.color.rgb * shade, input.color.a);
    out.highlight = input.highlight;
    out.alpha = input.alpha;
    out.world = input.position;
    out.normal = input.normal;
    out.water = input.water;
    return out;
}

// The sky water reflects, from its horizon to straight up.
const WATER_HORIZON: vec3<f32> = vec3<f32>(0.78, 0.86, 0.92);
const WATER_ZENITH: vec3<f32> = vec3<f32>(0.32, 0.52, 0.82);

// Shades a water surface the way the game draws it, matching the water thumbnails: the
// water is its tint, not the texture (whose pixels are a normal map), the surface mirrors
// the sky by the Fresnel term, and ripples tilt it so the reflection shimmers.
fn water_shade(shade: vec3<f32>, world: vec3<f32>, normal: vec3<f32>, water: vec4<f32>) -> vec3<f32> {
    let n0 = normalize(normal);
    let ripple = vec3<f32>(
        sin(world.x * 0.11 + world.y * 0.05) + 0.5 * sin(world.y * 0.23 - world.x * 0.07),
        sin(world.y * 0.13 + world.x * 0.17) + 0.5 * sin(world.x * 0.31 + world.y * 0.09),
        0.0,
    ) * 0.07;
    // Only the part of the ripple lying in the face's plane tilts it.
    let n = normalize(n0 + ripple - n0 * dot(ripple, n0));
    // Without a camera, the surface is seen straight on.
    let view = normalize(select(uniforms.eye.xyz - world, vec3<f32>(0.0, 0.0, 1.0), uniforms.eye.w < 0.5));
    let facing = clamp(dot(n, view), 0.0, 1.0);
    let fresnel = 0.02 + 0.98 * pow(1.0 - facing, 5.0);
    let reflected = normalize(-view + 2.0 * facing * n);
    let sky = mix(WATER_HORIZON, WATER_ZENITH, clamp(reflected.z, 0.0, 1.0));
    // The sun glint comes from the same direction as the faces' shading.
    let sun = normalize(vec3<f32>(0.3, 0.5, 1.0));
    let glint = pow(max(dot(reflected, sun), 0.0), 64.0) * 0.3;
    let body = shade * water.rgb * 1.5;
    let amount = clamp(fresnel * (0.5 + water.w), 0.0, 1.0);
    return mix(body, sky, amount) + vec3<f32>(glint);
}

// Colour multiplied by opacity, with alpha the share of what is behind that is hidden:
// none for added materials. alpha.x is the alpha-test reference (0 for none, below 0 when
// the texture's alpha is ignored); alpha.y the opacity (0 for opaque, below 0 for added).
fn premultiplied(rgb: vec3<f32>, coverage: f32, alpha: vec2<f32>) -> vec4<f32> {
    let texture_alpha = select(coverage, 1.0, alpha.x < 0.0);
    let opacity = select(1.0, texture_alpha * abs(alpha.y), alpha.y != 0.0);
    return vec4<f32>(rgb * opacity, select(opacity, 0.0, alpha.y < 0.0));
}

@fragment
fn fs_face(input: FaceOut) -> @location(0) vec4<f32> {
    let base = textureSample(face_texture, face_sampler, input.uv) * input.color;
    if (input.alpha.x > 0.0 && base.a < input.alpha.x) {
        discard;
    }
    var rgb = base.rgb;
    if (input.water.w > 0.0) {
        rgb = water_shade(input.color.rgb, input.world, input.normal, input.water);
    }
    rgb = mix(rgb, vec3<f32>(1.0, 0.0, 0.0), input.highlight);
    return premultiplied(rgb, base.a, input.alpha);
}

struct ModelIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) column0: vec4<f32>,
    @location(4) column1: vec4<f32>,
    @location(5) column2: vec4<f32>,
    @location(6) column3: vec4<f32>,
    @location(7) tint: vec4<f32>,
};

// Prop models: one shared mesh per model, placed by a per-instance matrix and shaded by
// direction like brushes.
@vertex
fn vs_model(input: ModelIn) -> FaceOut {
    let model = mat4x4<f32>(input.column0, input.column1, input.column2, input.column3);
    let world = model * vec4<f32>(input.position, 1.0);
    let normal = normalize((model * vec4<f32>(input.normal, 0.0)).xyz);
    let shade = clamp(0.6 + 0.4 * dot(normal, normalize(vec3<f32>(0.3, 0.5, 1.0))), 0.35, 1.0);
    var out: FaceOut;
    out.clip = uniforms.view_proj * world;
    out.uv = input.uv;
    out.color = vec4<f32>(input.tint.rgb * shade, 1.0);
    out.highlight = input.tint.a;
    out.alpha = material_alpha.xy;
    out.world = world.xyz;
    out.normal = normal;
    out.water = vec4<f32>(0.0);
    return out;
}

struct SpriteIn {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) fog: vec4<f32>,
    @location(4) mode: vec4<f32>,
};

struct SpriteOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) fog: vec4<f32>,
    @location(3) mode: vec4<f32>,
};

// The sprites of env_sprite and the like, as the game's Sprite shader draws them.
@vertex
fn vs_sprite(input: SpriteIn) -> SpriteOut {
    var out: SpriteOut;
    out.clip = uniforms.view_proj * vec4<f32>(input.position, 1.0);
    out.uv = input.uv;
    out.color = input.color;
    out.fog = input.fog;
    out.mode = input.mode;
    return out;
}

// The exact inverse of the sRGB curve the graphics card decodes textures with, so a
// sprite read in gamma space gets the bytes its texture stores.
fn srgb_encode(linear: vec3<f32>) -> vec3<f32> {
    let low = linear * 12.92;
    let high = 1.055 * pow(max(linear, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(high, low, linear <= vec3<f32>(0.0031308));
}

// mode.x is how the sprite blends: 0 opaque, 1 mixed by opacity, 2 added, 3 added over
// what is first dimmed by its opacity, 4 alpha-tested. mode.y is 1 when the texture is
// read in gamma space, mode.z the selection tint, and mode.w the alpha-test reference, or
// below 0 when the texture's alpha is ignored. The result is premultiplied, with alpha
// the share of what is behind that is hidden.
@fragment
fn fs_sprite(input: SpriteOut) -> @location(0) vec4<f32> {
    var texel = textureSample(face_texture, face_sampler, input.uv);
    if (input.mode.y > 0.5) {
        texel = vec4<f32>(srgb_encode(texel.rgb), texel.a);
    }
    if (input.mode.w < 0.0) {
        texel.a = 1.0;
    }
    let shaded = texel * input.color;
    if (input.mode.x > 3.5 && shaded.a < input.mode.w) {
        discard;
    }
    var rgb = mix(shaded.rgb, input.fog.rgb, input.fog.a);
    rgb = mix(rgb, vec3<f32>(1.0, 0.0, 0.0), input.mode.z);
    let opacity = clamp(shaded.a, 0.0, 1.0);
    if (input.mode.x < 0.5 || input.mode.x > 3.5) {
        return vec4<f32>(rgb, 1.0);
    }
    if (input.mode.x < 1.5) {
        return vec4<f32>(rgb * opacity, opacity);
    }
    if (input.mode.x < 2.5) {
        return vec4<f32>(rgb * opacity, 0.0);
    }
    return vec4<f32>(rgb, opacity);
}

struct LineIn {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct LineOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_line(input: LineIn) -> LineOut {
    var out: LineOut;
    out.clip = uniforms.view_proj * vec4<f32>(input.position, 1.0);
    out.color = input.color;
    return out;
}

@fragment
fn fs_line(input: LineOut) -> @location(0) vec4<f32> {
    return input.color;
}
