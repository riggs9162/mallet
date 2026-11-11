// Shaders for the textured 3D view.

struct Uniforms {
    view_proj: mat4x4<f32>,
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
};

struct FaceOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) highlight: f32,
    @location(3) alpha: vec2<f32>,
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
    return out;
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
    let rgb = mix(base.rgb, vec3<f32>(1.0, 0.0, 0.0), input.highlight);
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
    return out;
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
