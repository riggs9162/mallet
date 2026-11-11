// Shaders for the lighting preview: every light of the map with Source's colours,
// falloff and cones, shadow maps, sky light, projected textures, texture lights,
// self-illumination, the skybox, fog and tone mapping. Also the depth-only shaders that
// draw shadow maps, the passes that bake light into faces' luxels, and the additive
// glows and spotlight beams.

struct View {
    view_proj: mat4x4<f32>,
};

// Per material: x alpha-test reference and y opacity, as `premultiplied` reads them,
// z flags (1 self-illum, 2 sky, 4 unlit, 8 texture light); then the self-illum tint and
// the texture light's colour.
struct Material {
    alpha: vec4<f32>,
    self_illum: vec4<f32>,
    emissive: vec4<f32>,
};

// What one frame of the lighting preview needs besides the lights.
struct Frame {
    // xyz: the camera; w: exposure multiplier.
    eye: vec4<f32>,
    // rgb: fill light every surface gets; w: share of each light's fake bounce.
    fill: vec4<f32>,
    // rgb: light_environment's ambient sky light.
    sky_light: vec4<f32>,
    // rgb: fog colour; w: most fog (0 for none).
    fog_color: vec4<f32>,
    // rgb: second fog colour; w: 1 when the fog blends two colours.
    fog_color2: vec4<f32>,
    // xyz: the way the first fog colour is seen.
    fog_dir: vec4<f32>,
    // x: fog start; y: fog end.
    fog_range: vec4<f32>,
    // rgb: flat sky colour; w: sky mode (0 black, 1 flat, 2 skybox, 3 painted).
    sky_color: vec4<f32>,
    // Painted sky: top colour and fade bias, bottom colour and dusk scale, dusk colour and
    // intensity.
    sky_top: vec4<f32>,
    sky_bottom: vec4<f32>,
    sky_dusk: vec4<f32>,
    // xyz: direction towards the sun glow; w: cosine of its radius (2 for none).
    sun_glow: vec4<f32>,
    sun_glow_color: vec4<f32>,
    // xyz: the light grid's lowest corner; w: its cell size.
    grid_origin: vec4<f32>,
    // xyz: cells along each axis; w: lights at the start of the list that light everywhere.
    grid_size: vec4<u32>,
    // x: flags (1 filmic, 2 self-illum, 4 texture light glow); y: sky samples; z: first
    // sun shadow matrix; w: first sky shadow matrix.
    options: vec4<u32>,
    // x: local shadow softness in texels; y: sun softness; z: 1 / local map size; w: 1 /
    // sun map size.
    shadow: vec4<f32>,
    // x: 1 / sky map size; y: sky sample normalisation; z: sun normal offset; w: sky
    // normal offset.
    sky_shadow: vec4<f32>,
    // Small shadow maps: x pixels across one, y along a side of an atlas layer, z in a
    // layer; w the first one's matrix.
    atlas: vec4<u32>,
    // The 3D skybox, which has its own sun shadow maps and no sky light shadows: its
    // lowest corner and 1 when the map has one, then its highest corner and its sun
    // normal offset.
    skybox_min: vec4<f32>,
    skybox_max: vec4<f32>,
    // Luxels: x the first light of the batch being baked, y how many there are, z 1 when
    // faces show their baked luxels.
    luxels: vec4<u32>,
};

struct Light {
    // xyz: position; w: kind (0 point, 1 spot, 2 directional, 3 projected, 4 surface).
    position: vec4<f32>,
    // xyz: the way it shines; w: cosine of the outer cone.
    direction: vec4<f32>,
    // rgb: colour and brightness; w: cosine of the inner cone.
    color: vec4<f32>,
    // Inverse: constant, linear, quadratic, cap. Projected: far. Radius: radius.
    falloff: vec4<f32>,
    // x: fade start; y: fade end; z: maximum distance; w: falloff kind (0 inverse, 1
    // projected, 2 radius, 3 none).
    fade: vec4<f32>,
    // x: first shadow layer (-1 for none); y: layers (1 or 6); z: cookie layer (-1 for
    // none); w: cone focus.
    shadow: vec4<f32>,
    // x: normal offset per unit of distance; y: bounce share; z: projection matrix.
    extra: vec4<f32>,
    // x: surfaces it lights (0 all, 1 brushes only, 2 models only); y: 1 when its shadow
    // maps are small ones in the atlas; z: 1 when it is baked into the luxels faces show.
    routing: vec4<f32>,
};

// Where a face's luxels are: its luxel grid directions, each with the texel position of
// its first luxel in w, and its normal, with w 1 when it has luxels.
struct LuxelFace {
    s: vec4<f32>,
    t: vec4<f32>,
    normal: vec4<f32>,
};

const NO_FACE: u32 = 0xffffffffu;

const FLAG_SELF_ILLUM: u32 = 1u;
const FLAG_SKY: u32 = 2u;
const FLAG_UNLIT: u32 = 4u;
const FLAG_EMISSIVE: u32 = 8u;

@group(0) @binding(0) var<uniform> view: View;
@group(1) @binding(0) var face_texture: texture_2d<f32>;
@group(1) @binding(1) var face_sampler: sampler;
@group(1) @binding(2) var<uniform> material: Material;
@group(2) @binding(0) var<uniform> frame: Frame;
@group(2) @binding(1) var<storage, read> lights: array<Light>;
@group(2) @binding(2) var<storage, read> cells: array<vec2<u32>>;
@group(2) @binding(3) var<storage, read> indices: array<u32>;
@group(2) @binding(4) var<storage, read> shadow_matrices: array<mat4x4<f32>>;
@group(2) @binding(5) var local_shadows: texture_depth_2d_array;
@group(2) @binding(6) var sun_shadows: texture_depth_2d_array;
@group(2) @binding(7) var sky_shadows: texture_depth_2d_array;
@group(2) @binding(8) var shadow_sampler: sampler_comparison;
@group(2) @binding(9) var cookies: texture_2d_array<f32>;
@group(2) @binding(10) var sky_faces: texture_2d_array<f32>;
@group(2) @binding(11) var clamp_sampler: sampler;
@group(2) @binding(12) var<storage, read> sky_samples: array<vec4<f32>>;
@group(2) @binding(13) var atlas_shadows: texture_depth_2d_array;
@group(2) @binding(14) var luxel_light: texture_2d<f32>;
@group(2) @binding(15) var<storage, read> luxel_faces: array<LuxelFace>;
@group(2) @binding(16) var luxel_points: texture_2d<u32>;

struct FaceIn {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) highlight: f32,
    @location(4) alpha: vec2<f32>,
    @location(5) normal: vec3<f32>,
    @location(6) face: u32,
};

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

struct LitOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec4<f32>,
    @location(2) highlight: f32,
    @location(3) alpha: vec2<f32>,
    @location(4) world: vec3<f32>,
    @location(5) normal: vec3<f32>,
    // 1 for models, 0 for brushes.
    @location(6) model: f32,
    @location(7) @interpolate(flat) face: u32,
};

@vertex
fn vs_lit_face(input: FaceIn) -> LitOut {
    var out: LitOut;
    out.clip = view.view_proj * vec4<f32>(input.position, 1.0);
    out.uv = input.uv;
    out.tint = input.color;
    out.highlight = input.highlight;
    out.alpha = input.alpha;
    out.world = input.position;
    out.normal = input.normal;
    out.model = 0.0;
    out.face = input.face;
    return out;
}

@vertex
fn vs_lit_model(input: ModelIn) -> LitOut {
    let model = mat4x4<f32>(input.column0, input.column1, input.column2, input.column3);
    let world = model * vec4<f32>(input.position, 1.0);
    var out: LitOut;
    out.clip = view.view_proj * world;
    out.uv = input.uv;
    out.tint = vec4<f32>(input.tint.rgb, 1.0);
    out.highlight = input.tint.a;
    out.alpha = material.alpha.xy;
    out.world = world.xyz;
    out.normal = (model * vec4<f32>(input.normal, 0.0)).xyz;
    out.model = 1.0;
    out.face = NO_FACE;
    return out;
}

fn falloff(light: Light, distance: f32) -> f32 {
    let dist = max(distance, 1.0);
    let kind = u32(light.fade.w);
    if (kind == 0u) {
        if (light.fade.z > 0.0 && dist > light.fade.z) {
            return 0.0;
        }
        let capped = min(dist, light.falloff.w);
        let denominator = light.falloff.x + light.falloff.y * capped + light.falloff.z * capped * capped;
        var share = select(0.0, 1.0 / denominator, denominator > 0.0);
        if (light.fade.y > light.fade.x) {
            share = share * clamp((light.fade.y - dist) / (light.fade.y - light.fade.x), 0.0, 1.0);
        }
        return share;
    }
    if (kind == 1u) {
        let far = light.falloff.x;
        let fade = clamp((far - dist) / max(far * 0.4, 1.0), 0.0, 1.0);
        return min(100.0 / dist, 1.0) * fade;
    }
    if (kind == 2u) {
        let left = clamp(1.0 - dist / max(light.falloff.x, 1.0), 0.0, 1.0);
        return left * left;
    }
    return 1.0;
}

fn cone(light: Light, along: f32) -> f32 {
    let outer = light.direction.w;
    let inner = light.color.w;
    if (along <= outer) {
        return 0.0;
    }
    if (along >= inner || inner <= outer) {
        return 1.0;
    }
    let share = (along - outer) / (inner - outer);
    let focus = light.shadow.w;
    if (focus == 0.0 || focus == 1.0) {
        return share;
    }
    return pow(share, focus);
}

fn pcf(tex: texture_depth_2d_array, uv: vec2<f32>, layer: i32, depth: f32, step: f32) -> f32 {
    if (step <= 0.0) {
        return textureSampleCompareLevel(tex, shadow_sampler, uv, layer, depth);
    }
    var lit = 0.0;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let offset = vec2<f32>(f32(x), f32(y)) * step;
            lit = lit + textureSampleCompareLevel(tex, shadow_sampler, uv + offset, layer, depth);
        }
    }
    return lit / 9.0;
}

fn project(matrix_index: u32, world: vec3<f32>) -> vec4<f32> {
    let clip = shadow_matrices[matrix_index] * vec4<f32>(world, 1.0);
    if (clip.w <= 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, -1.0);
    }
    let ndc = clip.xyz / clip.w;
    if (abs(ndc.x) > 1.0 || abs(ndc.y) > 1.0 || ndc.z < 0.0 || ndc.z > 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, -1.0);
    }
    return vec4<f32>(ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5, 0.5), ndc.z, 1.0);
}

fn local_shadow(light: Light, world: vec3<f32>, normal: vec3<f32>, distance: f32) -> f32 {
    let first = i32(light.shadow.x);
    if (first < 0) {
        return 1.0;
    }
    let moved = world + normal * (light.extra.x * max(distance, 1.0));
    var face = 0;
    if (i32(light.shadow.y) == 6) {
        let away = moved - light.position.xyz;
        let size = abs(away);
        if (size.x >= size.y && size.x >= size.z) {
            face = select(1, 0, away.x > 0.0);
        } else if (size.y >= size.z) {
            face = select(3, 2, away.y > 0.0);
        } else {
            face = select(5, 4, away.z > 0.0);
        }
    }
    if (light.routing.y > 0.5) {
        return small_shadow(u32(first + face), moved);
    }
    let layer = first + face;
    let at = project(u32(layer), moved);
    if (at.w < 0.0) {
        return 1.0;
    }
    return pcf(local_shadows, at.xy, layer, at.z, frame.shadow.x * frame.shadow.z);
}

// One small shadow map of the atlas, read with the hardware's four-sample filter and kept
// half a pixel inside its tile.
fn small_shadow(tile: u32, world: vec3<f32>) -> f32 {
    let at = project(frame.atlas.w + tile, world);
    if (at.w < 0.0) {
        return 1.0;
    }
    let per_row = frame.atlas.y;
    let within = tile % frame.atlas.z;
    let corner = vec2<f32>(f32(within % per_row), f32(within / per_row));
    let edge = 0.5 / f32(frame.atlas.x);
    let inside = clamp(at.xy, vec2<f32>(edge), vec2<f32>(1.0 - edge));
    let uv = (corner + inside) / f32(per_row);
    return textureSampleCompareLevel(atlas_shadows, shadow_sampler, uv, i32(tile / frame.atlas.z), at.z);
}

fn in_skybox(world: vec3<f32>) -> bool {
    return frame.skybox_min.w > 0.5
        && all(world >= frame.skybox_min.xyz)
        && all(world <= frame.skybox_max.xyz);
}

// The sun's shadow, from the 3D skybox's own map inside the skybox.
fn sun_shadow(sun: u32, world: vec3<f32>, normal: vec3<f32>) -> f32 {
    var layer = sun;
    var offset = frame.sky_shadow.z;
    if (frame.skybox_min.w > 0.5) {
        layer = sun * 2u;
        if (in_skybox(world)) {
            layer = layer + 1u;
            offset = frame.skybox_max.w;
        }
    }
    let at = project(frame.options.z + layer, world + normal * offset);
    if (at.w < 0.0) {
        return 1.0;
    }
    return pcf(sun_shadows, at.xy, i32(layer), at.z, frame.shadow.y * frame.shadow.w);
}

// How much of the sky a surface sees; the 3D skybox, which is open to its sky, only goes
// by which way it faces.
fn sky_visibility(world: vec3<f32>, normal: vec3<f32>) -> f32 {
    let count = frame.options.y;
    if (count == 0u || in_skybox(world)) {
        return 0.5 + 0.5 * normal.z;
    }
    var seen = 0.0;
    for (var index = 0u; index < count; index = index + 1u) {
        let toward = sky_samples[index].xyz;
        let weight = dot(normal, toward);
        if (weight <= 0.0) {
            continue;
        }
        let at = project(frame.options.w + index, world + normal * frame.sky_shadow.w);
        var open = 1.0;
        if (at.w >= 0.0) {
            open = textureSampleCompareLevel(sky_shadows, shadow_sampler, at.xy, i32(index), at.z);
        }
        seen = seen + weight * open;
    }
    return seen * frame.sky_shadow.y;
}

// A dynamic light lights brushes the way the engine's dlights do, as strongly as its
// distance gives on every surface facing it, and models with the usual angle.
fn shine(light: Light, world: vec3<f32>, normal: vec3<f32>, model: bool) -> vec3<f32> {
    let receivers = u32(light.routing.x);
    if ((receivers == 1u && model) || (receivers == 2u && !model)) {
        return vec3<f32>(0.0);
    }
    let kind = u32(light.position.w);
    if (kind == 2u) {
        let facing = dot(normal, -light.direction.xyz);
        if (facing <= 0.0) {
            return vec3<f32>(0.0);
        }
        var shadow = 1.0;
        if (light.shadow.x >= 0.0) {
            shadow = sun_shadow(u32(light.shadow.x), world, normal);
        }
        return light.color.rgb * facing * shadow;
    }
    let offset = light.position.xyz - world;
    let distance = length(offset);
    let toward = offset / max(distance, 0.0001);
    let along = dot(-toward, light.direction.xyz);
    var shape = vec3<f32>(1.0);
    if (kind == 1u) {
        shape = vec3<f32>(cone(light, along));
    } else if (kind == 4u) {
        shape = vec3<f32>(max(along, 0.0));
    } else if (kind == 3u) {
        let at = project(u32(light.extra.z), world);
        if (at.w < 0.0) {
            return vec3<f32>(0.0);
        }
        let cookie = i32(light.shadow.z);
        if (cookie >= 0) {
            shape = textureSampleLevel(cookies, clamp_sampler, at.xy, cookie, 0.0).rgb;
        }
    }
    let strength = shape * falloff(light, distance);
    if (all(strength <= vec3<f32>(0.0))) {
        return vec3<f32>(0.0);
    }
    var facing = max(dot(normal, toward), 0.0);
    if (u32(light.fade.w) == 2u && !model) {
        facing = select(0.0, 1.0, facing > 0.0);
    }
    let bounce = frame.fill.w * light.extra.y;
    if (facing <= 0.0 && bounce <= 0.0) {
        return vec3<f32>(0.0);
    }
    let shadow = local_shadow(light, world, normal, distance);
    return light.color.rgb * strength * shadow * (facing + bounce);
}

fn cell_index(world: vec3<f32>) -> u32 {
    let size = vec3<i32>(frame.grid_size.xyz);
    let at = vec3<i32>(floor((world - frame.grid_origin.xyz) / frame.grid_origin.w));
    let cell = clamp(at, vec3<i32>(0), size - vec3<i32>(1));
    return u32(cell.x + size.x * (cell.y + size.y * cell.z));
}

// Adds up the light reaching a point. A face that takes its light from its luxels only
// adds the fill light and the lights that are not baked into them.
fn gather(world: vec3<f32>, normal: vec3<f32>, model: bool, lightmapped: bool) -> vec3<f32> {
    var total = frame.fill.rgb;
    if (!lightmapped && any(frame.sky_light.rgb > vec3<f32>(0.0))) {
        total = total + frame.sky_light.rgb * sky_visibility(world, normal);
    }
    let everywhere = frame.grid_size.w;
    for (var index = 0u; index < everywhere; index = index + 1u) {
        let light = lights[index];
        if (lightmapped && light.routing.z > 0.5) {
            continue;
        }
        total = total + shine(light, world, normal, model);
    }
    let range = cells[cell_index(world)];
    for (var index = 0u; index < range.y; index = index + 1u) {
        let light = lights[indices[range.x + index]];
        if (lightmapped && light.routing.z > 0.5) {
            continue;
        }
        total = total + shine(light, world, normal, model);
    }
    return total;
}

// The light baked into a face's luxels at a point on it, filtered between the four
// nearest luxels as the engine filters lightmaps; w is 0 when the face shows none.
fn baked(face: u32, world: vec3<f32>) -> vec4<f32> {
    if (frame.luxels.z == 0u || face >= arrayLength(&luxel_faces)) {
        return vec4<f32>(0.0);
    }
    let entry = luxel_faces[face];
    if (entry.normal.w < 0.5) {
        return vec4<f32>(0.0);
    }
    let texel = vec2<f32>(dot(world, entry.s.xyz) + entry.s.w, dot(world, entry.t.xyz) + entry.t.w);
    let uv = texel / vec2<f32>(textureDimensions(luxel_light));
    return vec4<f32>(textureSampleLevel(luxel_light, clamp_sampler, uv, 0.0).rgb, 1.0);
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
    return textureSampleLevel(sky_faces, clamp_sampler, uv, layer, 0.0).rgb;
}

fn painted(direction: vec3<f32>) -> vec3<f32> {
    let height = clamp(direction.z, 0.0, 1.0);
    let fade = pow(height, max(frame.sky_top.w, 0.01));
    var color = mix(frame.sky_bottom.rgb, frame.sky_top.rgb, fade);
    let horizon = pow(1.0 - abs(direction.z), max(4.0 / max(frame.sky_bottom.w, 0.05), 0.5));
    color = color + frame.sky_dusk.rgb * frame.sky_dusk.w * horizon * 0.5;
    return color;
}

fn sky(direction: vec3<f32>) -> vec3<f32> {
    let mode = u32(frame.sky_color.w);
    var color = frame.sky_color.rgb;
    if (mode == 0u) {
        color = vec3<f32>(0.0);
    } else if (mode == 2u) {
        color = skybox(direction);
    } else if (mode == 3u) {
        color = painted(direction);
    }
    if (frame.sun_glow.w < 1.5) {
        let along = dot(direction, frame.sun_glow.xyz);
        let core = smoothstep(frame.sun_glow.w, mix(frame.sun_glow.w, 1.0, 0.3), along);
        let halo = pow(max(along, 0.0), 256.0) * 0.6;
        color = color + frame.sun_glow_color.rgb * (core + halo);
    }
    return color;
}

fn fogged(color: vec3<f32>, world: vec3<f32>) -> vec3<f32> {
    let most = frame.fog_color.w;
    if (most <= 0.0) {
        return color;
    }
    let offset = world - frame.eye.xyz;
    let distance = length(offset);
    let amount = clamp((distance - frame.fog_range.x) / max(frame.fog_range.y - frame.fog_range.x, 1.0), 0.0, 1.0) * most;
    var tint = frame.fog_color.rgb;
    if (frame.fog_color2.w > 0.5) {
        let blend = dot(offset / max(distance, 0.0001), frame.fog_dir.xyz) * 0.5 + 0.5;
        tint = mix(frame.fog_color2.rgb, frame.fog_color.rgb, blend);
    }
    return mix(color, tint, amount);
}

fn tonemap(color: vec3<f32>) -> vec3<f32> {
    let exposed = max(color * frame.eye.w, vec3<f32>(0.0));
    if ((frame.options.x & 1u) != 0u) {
        let curve = (exposed * (2.51 * exposed + 0.03)) / (exposed * (2.43 * exposed + 0.59) + 0.14);
        return clamp(curve, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    return clamp(exposed, vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_lit(input: LitOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let texel = textureSample(face_texture, face_sampler, input.uv);
    if (input.alpha.x > 0.0 && texel.a < input.alpha.x) {
        discard;
    }
    let base = texel.rgb * input.tint.rgb;
    let flags = u32(material.alpha.z);
    var color = base;
    if ((flags & FLAG_SKY) != 0u) {
        color = sky(normalize(input.world - frame.eye.xyz));
    } else {
        if ((flags & FLAG_UNLIT) == 0u) {
            var normal = normalize(input.normal);
            if (!front) {
                normal = -normal;
            }
            let lightmap = baked(input.face, input.world);
            color = base * (gather(input.world, normal, input.model > 0.5, lightmap.w > 0.5) + lightmap.rgb);
            if ((flags & FLAG_SELF_ILLUM) != 0u && (frame.options.x & 2u) != 0u) {
                color = mix(color, base * material.self_illum.rgb, texel.a);
            }
            if ((flags & FLAG_EMISSIVE) != 0u && (frame.options.x & 4u) != 0u) {
                color = max(color, base * material.emissive.rgb);
            }
            color = tonemap(color);
        }
        if (input.alpha.y < 0.0) {
            color = fogged(color, input.world) - fogged(vec3<f32>(0.0), input.world);
        } else {
            color = fogged(color, input.world);
        }
    }
    color = mix(color, vec3<f32>(1.0, 0.0, 0.0), input.highlight);
    return premultiplied(color, texel.a, input.alpha);
}

// Colour multiplied by opacity, with alpha the share of what is behind that is hidden:
// none for added materials. alpha.x is the alpha-test reference (0 for none, below 0 when
// the texture's alpha is ignored); alpha.y the opacity (0 for opaque, below 0 for added).
fn premultiplied(rgb: vec3<f32>, coverage: f32, alpha: vec2<f32>) -> vec4<f32> {
    let texture_alpha = select(coverage, 1.0, alpha.x < 0.0);
    let opacity = select(1.0, texture_alpha * abs(alpha.y), alpha.y != 0.0);
    return vec4<f32>(rgb * opacity, select(opacity, 0.0, alpha.y < 0.0));
}

@vertex
fn vs_shadow_solid(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return view.view_proj * vec4<f32>(position, 1.0);
}

// A triangle covering a whole small shadow map at the far plane, to clear it.
@vertex
fn vs_shadow_clear(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 1.0, 1.0);
}

// A triangle covering the whole luxel atlas.
@vertex
fn vs_bake_all(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

// One face's luxels, a corner of the strip at a time; `rect` holds the left, top, right
// and bottom of them in clip space.
@vertex
fn vs_bake_rect(@builtin(vertex_index) index: u32, @location(0) rect: vec4<f32>) -> @builtin(position) vec4<f32> {
    let x = select(rect.x, rect.z, (index & 1u) != 0u);
    let y = select(rect.y, rect.w, (index & 2u) != 0u);
    return vec4<f32>(x, y, 0.0, 1.0);
}

// The point a luxel of the atlas lights, and its face's normal in w of the second; the
// normal is zero for texels no face uses.
fn luxel_point(position: vec4<f32>) -> array<vec3<f32>, 2> {
    let data = textureLoad(luxel_points, vec2<i32>(position.xy), 0);
    if (data.w == 0u) {
        return array<vec3<f32>, 2>(vec3<f32>(0.0), vec3<f32>(0.0));
    }
    return array<vec3<f32>, 2>(bitcast<vec3<f32>>(data.xyz), luxel_faces[data.w - 1u].normal.xyz);
}

// Bakes sky light and sunlight into every luxel, starting a bake afresh.
@fragment
fn fs_bake_sky(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let point = luxel_point(position);
    let world = point[0];
    let normal = point[1];
    if (all(normal == vec3<f32>(0.0))) {
        return vec4<f32>(0.0);
    }
    var total = vec3<f32>(0.0);
    if (any(frame.sky_light.rgb > vec3<f32>(0.0))) {
        total = total + frame.sky_light.rgb * sky_visibility(world, normal);
    }
    let everywhere = frame.grid_size.w;
    for (var index = 0u; index < everywhere; index = index + 1u) {
        let light = lights[index];
        if (u32(light.position.w) == 2u && light.routing.z > 0.5) {
            total = total + shine(light, world, normal, false);
        }
    }
    return vec4<f32>(total, 1.0);
}

// Adds this frame's batch of lights to the luxels of the faces they reach.
@fragment
fn fs_bake_lights(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let point = luxel_point(position);
    let world = point[0];
    let normal = point[1];
    if (all(normal == vec3<f32>(0.0))) {
        return vec4<f32>(0.0);
    }
    var total = vec3<f32>(0.0);
    for (var index = 0u; index < frame.luxels.y; index = index + 1u) {
        total = total + shine(lights[frame.luxels.x + index], world, normal, false);
    }
    return vec4<f32>(total, 0.0);
}

struct ShadowOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
};

@vertex
fn vs_shadow_face(input: FaceIn) -> ShadowOut {
    var out: ShadowOut;
    out.clip = view.view_proj * vec4<f32>(input.position, 1.0);
    out.uv = input.uv;
    out.alpha = input.alpha.x;
    return out;
}

@vertex
fn vs_shadow_model(input: ModelIn) -> ShadowOut {
    let model = mat4x4<f32>(input.column0, input.column1, input.column2, input.column3);
    var out: ShadowOut;
    out.clip = view.view_proj * (model * vec4<f32>(input.position, 1.0));
    out.uv = input.uv;
    out.alpha = material.alpha.x;
    return out;
}

@fragment
fn fs_shadow(input: ShadowOut) {
    let coverage = textureSample(face_texture, face_sampler, input.uv).a;
    if (input.alpha > 0.0 && coverage < input.alpha) {
        discard;
    }
}

struct GlowIn {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) shape: vec3<f32>,
};

struct GlowOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) shape: vec3<f32>,
};

@vertex
fn vs_glow(input: GlowIn) -> GlowOut {
    var out: GlowOut;
    out.clip = view.view_proj * vec4<f32>(input.position, 1.0);
    out.color = input.color;
    out.shape = input.shape;
    return out;
}

// shape.z is 0 for a round glow, where xy runs from -1 to 1 across it, and 1 for a beam,
// where x runs across it from -1 to 1 and y along it from 0 to 1.
@fragment
fn fs_glow(input: GlowOut) -> @location(0) vec4<f32> {
    var strength = 0.0;
    if (input.shape.z < 0.5) {
        let left = max(1.0 - dot(input.shape.xy, input.shape.xy), 0.0);
        strength = left * left;
    } else {
        let across = max(1.0 - abs(input.shape.x), 0.0);
        strength = across * across * (1.0 - clamp(input.shape.y, 0.0, 1.0));
    }
    return vec4<f32>(input.color.rgb * input.color.a * strength, 0.0);
}
