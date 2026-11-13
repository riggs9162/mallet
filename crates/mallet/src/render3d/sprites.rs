//! The sprites of env_sprite and the like, drawn the way the game draws them: the
//! `Sprite` shader's render modes and orientations, the client's glow fading and screen
//! size, animation frames, and blending in gamma space, all following the Source SDK's
//! `C_SpriteRenderer` and `Sprite_DX9`.

use std::ops::Range;

use eframe::wgpu::{self, util::DeviceExt};
use glam::{DVec3, Vec3, Vec4};
use mallet_content::{AlphaMode, MaterialLoader, SpriteLook, SpriteOrientation, Surface};
use mallet_vmf::light::Fog;

use super::{box_visible, material_key, GpuTexture, Renderer3D, COLOR_FORMAT, DEPTH_FORMAT, SELECTION_TINT};
use crate::scene::{GameSprite, ModelCache, Overlay, Scene, SceneFace, WireMesh};
use crate::view3d::Camera3D;

/// The format the camera image is viewed in for sprites that blend in gamma space: its
/// bytes as they are, without the sRGB curve.
pub(super) const GAMMA_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The most animation frames of one sprite loaded.
const MAX_FRAMES: u32 = 64;
/// How far away a glow sprite is at full brightness; it fades with the square of the
/// distance past it.
const GLOW_FULL_DISTANCE: f64 = 1200.0;
/// The distance at which a Glow sprite is drawn at its own size; it grows with distance so
/// it keeps its size on screen.
const GLOW_SIZE_DISTANCE: f64 = 200.0;
/// The cosine of one degree. Upright sprites are not drawn when the view direction is
/// closer than this to straight up or down.
const UPRIGHT_LIMIT: f64 = 0.999848;
/// The exponent of the game's conversion between gamma and linear colour.
const GAMMA: f32 = 2.2;
/// How often, in frames a second, a view with moving sprites is redrawn.
pub(super) const ANIMATION_RATE: f64 = 30.0;

/// The render modes, numbered as the game numbers them.
const NORMAL: u8 = 0;
const TRANS_COLOR: u8 = 1;
const TRANS_TEXTURE: u8 = 2;
const GLOW: u8 = 3;
const TRANS_ALPHA: u8 = 4;
const TRANS_ADD: u8 = 5;
const TRANS_ADD_FRAME_BLEND: u8 = 7;
const TRANS_ALPHA_ADD: u8 = 8;
const WORLD_GLOW: u8 = 9;

/// The render effects that change a sprite over time, and Constant Glow.
const FX_PULSE_SLOW: u8 = 1;
const FX_PULSE_FAST: u8 = 2;
const FX_PULSE_SLOW_WIDE: u8 = 3;
const FX_PULSE_FAST_WIDE: u8 = 4;
const FX_FADE_SLOW: u8 = 5;
const FX_FADE_FAST: u8 = 6;
const FX_STROBE_SLOW: u8 = 9;
const FX_STROBE_FAST: u8 = 10;
const FX_STROBE_FASTER: u8 = 11;
const FX_FLICKER_SLOW: u8 = 12;
const FX_FLICKER_FAST: u8 = 13;
const FX_NO_DISSIPATION: u8 = 14;
const FX_DISTORT: u8 = 15;
const FX_HOLOGRAM: u8 = 16;
const FX_PULSE_FAST_WIDER: u8 = 24;

/// One corner of a sprite on the GPU.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct SpriteVertex {
    position: [f32; 3],
    uv: [f32; 2],
    /// What the texture is multiplied by, colour and opacity.
    color: [f32; 4],
    /// rgb: the colour fog turns the sprite; a: how far it has turned.
    fog: [f32; 4],
    /// x: how it blends, as [`Blend`] numbers it; y: 1 when the texture is read in gamma
    /// space; z: the selection tint; w: the alpha-test reference, or below 0 when the
    /// texture's alpha is ignored.
    mode: [f32; 4],
}

const SPRITE_ATTRIBUTES: [wgpu::VertexAttribute; 5] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4];

/// How a sprite's colour meets what is behind it.
#[derive(Clone, Copy, PartialEq)]
enum Blend {
    /// Covers what is behind.
    Opaque,
    /// Mixed with what is behind by its opacity.
    Mix,
    /// Added to what is behind, scaled by its opacity.
    Add,
    /// Added to what is behind, which is first dimmed by its opacity.
    AlphaAdd,
    /// Covers what is behind where its opacity reaches the reference, and is cut out
    /// elsewhere.
    Test(f32),
}

impl Blend {
    fn number(self) -> f32 {
        match self {
            Self::Opaque => 0.0,
            Self::Mix => 1.0,
            Self::Add => 2.0,
            Self::AlphaAdd => 3.0,
            Self::Test(_) => 4.0,
        }
    }

    /// Whether fog fades the sprite to black rather than to the fog's colour, as it does
    /// for sprites that add light.
    fn fogs_to_black(self) -> bool {
        matches!(self, Self::Add | Self::AlphaAdd)
    }
}

/// The four pipelines sprites are drawn with, by whether they blend in gamma space and
/// whether they are hidden behind what is in front of them.
pub(super) struct SpritePipelines {
    linear: [wgpu::RenderPipeline; 2],
    /// `None` when the graphics card cannot view the camera image without the sRGB
    /// curve. Gamma sprites then blend in linear light.
    gamma: Option<[wgpu::RenderPipeline; 2]>,
}

impl SpritePipelines {
    pub(super) fn new(
        device: &wgpu::Device,
        layout: &wgpu::PipelineLayout,
        shader: &wgpu::ShaderModule,
        gamma_view: bool,
    ) -> Self {
        let make = |format, depth_test| sprite_pipeline(device, layout, shader, format, depth_test);
        Self {
            linear: [make(COLOR_FORMAT, true), make(COLOR_FORMAT, false)],
            gamma: gamma_view.then(|| [make(GAMMA_FORMAT, true), make(GAMMA_FORMAT, false)]),
        }
    }

    /// Whether the camera image has a view for sprites that blend in gamma space.
    pub(super) fn gamma_view(&self) -> bool {
        self.gamma.is_some()
    }
}

fn sprite_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    depth_test: bool,
) -> wgpu::RenderPipeline {
    let buffer = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<SpriteVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &SPRITE_ATTRIBUTES,
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("mallet sprites"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_sprite"),
            buffers: &[buffer],
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: false,
            depth_compare: if depth_test {
                wgpu::CompareFunction::Less
            } else {
                wgpu::CompareFunction::Always
            },
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_sprite"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        multiview: None,
        cache: None,
    })
}

/// What the lighting preview changes about sprites: the fog they fade into, the exposure,
/// and whether they are seen as in an HDR game.
#[derive(Clone, Copy)]
pub(super) struct SpriteLighting {
    pub fog: Option<Fog>,
    pub exposure: f64,
    pub hdr: bool,
    /// Show sprites that start hidden.
    pub hidden_too: bool,
}

/// One sprite ready to sort and draw.
struct SpriteQuad {
    /// For a glow, the point the camera must see for the sprite to show at all.
    sight: Option<DVec3>,
    center: Vec3,
    min: Vec3,
    max: Vec3,
    key: String,
    vertices: Vec<SpriteVertex>,
    depth_test: bool,
    gamma: bool,
}

/// Sprites in one vertex buffer, back to front, in runs that share a texture and a
/// pipeline: the index into [`SpritePipelines`] is whether they are depth tested.
pub(super) struct SpriteDraws {
    buffer: wgpu::Buffer,
    batches: Vec<(String, bool, Range<u32>)>,
}

/// The sprites of one camera frame: those that blend in gamma space and those that blend
/// in linear light, each drawn in its own pass.
#[derive(Default)]
pub(super) struct FrameSprites {
    pub gamma: Option<SpriteDraws>,
    pub linear: Option<SpriteDraws>,
    /// Whether any of them changes over time, so the view is redrawn while it shows them.
    pub animated: bool,
}

impl FrameSprites {
    pub(super) fn is_empty(&self) -> bool {
        self.gamma.is_none() && self.linear.is_none()
    }
}

/// The key of animation frame `frame` of a material's texture among the uploaded
/// textures; the first frame is the material's own key.
fn frame_key(material: &str, frame: u32) -> String {
    if frame == 0 {
        material.to_owned()
    } else {
        format!("{material}#{frame}")
    }
}

/// The material and frame a texture key names, for frames past the first.
pub(super) fn split_frame_key(key: &str) -> Option<(&str, u32)> {
    let (material, frame) = key.rsplit_once('#')?;
    Some((material, frame.parse().ok()?))
}

/// Loads one animation frame of a sprite on a loading thread.
pub(super) fn load_frame(
    loader: &MaterialLoader,
    material: &str,
    frame: u32,
    blocks: bool,
) -> Option<mallet_content::vtf::Mips> {
    loader.load_frame_mips(material, frame, blocks)
}

/// The game's conversion of a gamma colour to linear light.
fn to_linear(value: f32) -> f32 {
    value.max(0.0).powf(GAMMA)
}

/// The game's conversion of linear light to a gamma colour.
fn to_gamma(value: f32) -> f32 {
    value.max(0.0).powf(1.0 / GAMMA)
}

/// The sprite's opacity from its render effect, from 0 to 255, as the client works it out
/// each frame: pulses swing it, strobes and flickers switch it off and on, and fades
/// have long since finished.
fn fx_blend(sprite: &GameSprite, camera: &Camera3D, origin: DVec3, now: f64) -> i32 {
    let amount = i32::from(sprite.amount);
    let swing = |size: f64, wave: f64| (f64::from(amount) + size * wave) as i32;
    let offset = f64::from(sprite.seed) * 363.0;
    let switch = |wave: f64| if ((20.0 * wave) as i32) < 0 { 0 } else { amount };
    let blend = match sprite.fx {
        FX_PULSE_SLOW_WIDE => swing(64.0, (now * 2.0 + offset).sin()),
        FX_PULSE_FAST_WIDE => swing(64.0, (now * 8.0 + offset).sin()),
        FX_PULSE_FAST_WIDER => (255.0 * (now * 12.0 + offset).sin().abs()) as i32,
        FX_PULSE_SLOW => swing(16.0, (now * 2.0 + offset).sin()),
        FX_PULSE_FAST => swing(16.0, (now * 8.0 + offset).sin()),
        FX_FADE_SLOW | FX_FADE_FAST => 0,
        7 | 8 => 255,
        FX_STROBE_SLOW => switch((now * 4.0 + offset).sin()),
        FX_STROBE_FAST => switch((now * 16.0 + offset).sin()),
        FX_STROBE_FASTER => switch((now * 36.0 + offset).sin()),
        FX_FLICKER_SLOW => switch((now * 2.0).sin() + (now * 17.0 + offset).sin()),
        FX_FLICKER_FAST => switch((now * 16.0).sin() + (now * 23.0 + offset).sin()),
        FX_HOLOGRAM | FX_DISTORT => {
            let ahead = if sprite.fx == FX_DISTORT {
                1.0
            } else {
                (origin - camera.position).dot(camera.forward())
            };
            if ahead <= 0.0 {
                0
            } else if ahead <= 100.0 {
                180
            } else {
                ((1.0 - (ahead - 100.0) / 400.0) * 180.0) as i32
            }
        }
        _ if sprite.mode == NORMAL => 255,
        _ => amount,
    };
    blend.clamp(0, 255)
}

/// Whether a render effect changes the sprite over time.
fn fx_animates(fx: u8) -> bool {
    matches!(
        fx,
        FX_PULSE_SLOW
            | FX_PULSE_FAST
            | FX_PULSE_SLOW_WIDE
            | FX_PULSE_FAST_WIDE
            | FX_STROBE_SLOW
            | FX_STROBE_FAST
            | FX_STROBE_FASTER
            | FX_FLICKER_SLOW
            | FX_FLICKER_FAST
            | FX_PULSE_FAST_WIDER
    )
}

/// The sprite's right and up directions, as the game turns each orientation, or `None`
/// where the game draws nothing: an upright sprite seen from straight above or below.
///
/// An env_sprite parallel to the screen with a roll turns on the screen by it. A
/// `facing_upright` sprite faces the world's origin rather than the camera, as the game's
/// code has it.
fn sprite_axes(
    orientation: SpriteOrientation,
    origin: DVec3,
    angles: DVec3,
    camera: &Camera3D,
) -> Option<(DVec3, DVec3)> {
    let forward = camera.forward();
    let view_right = camera.right();
    let view_up = view_right.cross(forward).normalize_or(DVec3::Z);
    let orientation = match orientation {
        SpriteOrientation::Parallel if angles.z != 0.0 => SpriteOrientation::ParallelOriented,
        other => other,
    };
    match orientation {
        SpriteOrientation::FacingUpright => {
            let toward = (-origin).normalize_or_zero();
            if toward.z.abs() > UPRIGHT_LIMIT {
                return None;
            }
            let right = DVec3::new(toward.y, -toward.x, 0.0).normalize_or_zero();
            Some((right, DVec3::Z))
        }
        SpriteOrientation::Parallel => Some((view_right, view_up)),
        SpriteOrientation::ParallelUpright => {
            if forward.z.abs() > UPRIGHT_LIMIT {
                return None;
            }
            let right = DVec3::new(forward.y, -forward.x, 0.0).normalize_or_zero();
            Some((right, DVec3::Z))
        }
        SpriteOrientation::Oriented => {
            let (_, right, up) = angle_vectors(angles);
            Some((right, up))
        }
        SpriteOrientation::ParallelOriented => {
            let (sin, cos) = angles.z.to_radians().sin_cos();
            Some((view_right * cos + view_up * sin, view_right * -sin + view_up * cos))
        }
    }
}

/// The game's forward, right and up directions for pitch, yaw and roll in degrees.
fn angle_vectors(angles: DVec3) -> (DVec3, DVec3, DVec3) {
    let (sp, cp) = angles.x.to_radians().sin_cos();
    let (sy, cy) = angles.y.to_radians().sin_cos();
    let (sr, cr) = angles.z.to_radians().sin_cos();
    let forward = DVec3::new(cp * cy, cp * sy, -sp);
    let right = DVec3::new(-sr * sp * cy + cr * sy, -sr * sp * sy - cr * cy, -sr * cp);
    let up = DVec3::new(cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp);
    (forward, right, up)
}

/// How far the fog has turned something at `point` seen from `eye`, and the fog's colour
/// there in linear light, as the lighting preview fogs surfaces.
fn fog_at(fog: &Fog, eye: DVec3, point: DVec3) -> (f32, DVec3) {
    let offset = point - eye;
    let distance = offset.length();
    let amount = ((distance - fog.start) / (fog.end - fog.start).max(1.0)).clamp(0.0, 1.0) * fog.max_density;
    let color = match fog.blend {
        Some((second, direction)) => {
            let facing = (offset / distance.max(1e-4)).dot(direction) * 0.5 + 0.5;
            second.lerp(fog.color, facing)
        }
        None => fog.color,
    };
    (amount as f32, color)
}

/// How much of a Glow or World Space Glow sprite shows while its proxy is in sight, from
/// 0 to 1, as `StandardGlowBlend` works it out, and the proxy point the camera must see:
/// less with the square of the distance past 1200 units, unless it has Constant Glow. A
/// Glow sprite also grows with distance, so it keeps its size on screen. `None` for a
/// sprite the camera stands in.
fn glow_blend(camera: &Camera3D, game: &GameSprite, origin: DVec3, scale: &mut f64) -> Option<(f64, DVec3)> {
    let offset = origin - camera.position;
    let distance = offset.length();
    if distance <= 0.0 {
        return None;
    }
    let proxy = origin - offset / distance * game.proxy.min(distance);
    if game.fx == FX_NO_DISSIPATION {
        return Some((f64::from(game.amount) / 255.0, proxy));
    }
    let fade = (GLOW_FULL_DISTANCE * GLOW_FULL_DISTANCE / (distance * distance)).clamp(0.0, 1.0);
    if game.mode != WORLD_GLOW {
        if *scale == 0.0 {
            *scale = 1.0;
        }
        *scale *= distance / GLOW_SIZE_DISTANCE;
    }
    Some((fade, proxy))
}

impl Renderer3D {
    /// The game sprite of a mesh, with its material's first frame once it has loaded.
    fn game_sprite<'a>(&'a self, mesh: &'a WireMesh) -> Option<(&'a GameSprite, String, &'a GpuTexture)> {
        let sprite = mesh.sprite.as_ref()?;
        let game = sprite.game.as_ref()?;
        let texture = self.sprite_texture(mesh)?;
        Some((game, material_key(&sprite.material), texture))
    }

    /// Asks for the animation frames of the sprites that animate, once their first frame
    /// has shown how many there are.
    pub(super) fn load_sprite_frames(&mut self, scene: &Scene, loader: Option<&MaterialLoader>) {
        let Some(loader) = loader else {
            return;
        };
        let mut wanted: Vec<String> = scene
            .meshes
            .iter()
            .filter_map(|mesh| self.game_sprite(mesh))
            .filter(|(game, _, texture)| game.framerate > 0.0 && texture.facts.frames > 2)
            .flat_map(|(_, key, texture)| {
                (1..texture.facts.frames.min(MAX_FRAMES)).map(move |frame| frame_key(&key, frame))
            })
            .filter(|key| !self.textures.contains_key(key) && !self.loading.contains(key))
            .collect();
        if wanted.is_empty() {
            return;
        }
        wanted.sort();
        wanted.dedup();
        self.spawn_loads(wanted, loader.clone());
    }

    /// Builds this frame's game sprites as the game would draw them from this camera,
    /// sorted back to front and split by how they blend. `lighting` is set for the
    /// lighting preview.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn frame_sprites(
        &self,
        scene: &Scene,
        overlay: &Overlay,
        camera: &Camera3D,
        models: &ModelCache,
        planes: &[Vec4; 6],
        lighting: Option<SpriteLighting>,
        now: f64,
    ) -> FrameSprites {
        let mut animated = false;
        let mut quads: Vec<SpriteQuad> = Vec::new();
        for mesh in &scene.meshes {
            let Some((game, key, texture)) = self.game_sprite(mesh) else {
                continue;
            };
            if game.starts_off && lighting.is_some_and(|lighting| !lighting.hidden_too) {
                continue;
            }
            let (_, remap) = mesh.appearance(overlay);
            let origin = mesh.origin.unwrap_or_else(|| mesh.bounds.center());
            let origin = remap.map_or(origin, |remap| remap.apply(origin));
            let changes =
                (game.framerate > 0.0 && texture.facts.frames > 2) || (game.mode != NORMAL && fx_animates(game.fx));
            let point = origin.as_vec3();
            animated |= changes && box_visible(planes, point, point);
            if let Some(quad) = self.sprite_quad(camera, mesh, game, &key, texture, origin, lighting, now) {
                quads.push(quad);
            }
        }
        let eye = camera.position.as_vec3();
        quads.retain(|quad| {
            box_visible(planes, quad.min, quad.max)
                && quad
                    .sight
                    .is_none_or(|proxy| !self.glow_hidden(scene, models, camera, proxy))
        });
        quads.sort_by(|a, b| {
            b.center
                .distance_squared(eye)
                .total_cmp(&a.center.distance_squared(eye))
        });
        let (gamma, linear): (Vec<SpriteQuad>, Vec<SpriteQuad>) = quads.into_iter().partition(|quad| quad.gamma);
        FrameSprites {
            gamma: self.sprite_draws(&gamma),
            linear: self.sprite_draws(&linear),
            animated,
        }
    }

    /// One sprite as `C_SpriteRenderer::DrawSprite` draws it, or `None` when the game would
    /// draw nothing. A glow is returned with the point the camera must see for it to
    /// show, in [`SpriteQuad::sight`], which is left for the caller to test.
    #[allow(clippy::too_many_arguments)]
    fn sprite_quad(
        &self,
        camera: &Camera3D,
        mesh: &WireMesh,
        game: &GameSprite,
        key: &str,
        texture: &GpuTexture,
        origin: DVec3,
        lighting: Option<SpriteLighting>,
        now: f64,
    ) -> Option<SpriteQuad> {
        let look = texture.facts.sprite;
        let mode = game.mode;
        if !matches!(mode, 0..=5 | 7..=9) {
            return None;
        }
        let mut color = game.color.map(f64::from);
        let mut scale = game.scale;
        let mut sight = None;
        if mode != NORMAL {
            let mut blend = f64::from(fx_blend(game, camera, origin, now)) / 255.0;
            if matches!(mode, GLOW | WORLD_GLOW) {
                let (shown, proxy) = glow_blend(camera, game, origin, &mut scale)?;
                blend *= shown;
                color = color.map(|channel| channel * blend);
                sight = Some(proxy);
            }
            if blend <= 0.0 {
                return None;
            }
        }
        let (right, up) = sprite_axes(look.orientation, origin, game.angles, camera)?;
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let vertex = [
            color[0] as f32 / 255.0,
            color[1] as f32 / 255.0,
            color[2] as f32 / 255.0,
            f32::from(game.amount) / 255.0,
        ];
        let (mut tint, blend, mut depth_test) = sprite_shading(look, texture.facts.alpha, mode, vertex);
        depth_test &= !look.ignore_z;
        let gamma = look.gamma && self.sprite_pipelines.gamma_view();
        if look.gamma && !gamma {
            for channel in &mut tint[..3] {
                *channel = to_linear(*channel);
            }
        }
        let mut fog = [0.0; 4];
        if let Some(lighting) = lighting {
            let brightness = if gamma {
                to_gamma(lighting.exposure as f32)
            } else {
                lighting.exposure as f32
            };
            let hdr = if lighting.hdr && look.sprite_shader {
                game.hdr_scale.max(0.0) as f32
            } else {
                1.0
            };
            for channel in &mut tint[..3] {
                *channel *= brightness * hdr;
            }
            if let Some(settings) = &lighting.fog {
                let (amount, fog_color) = fog_at(settings, camera.position, origin);
                let fog_color = if blend.fogs_to_black() {
                    [0.0; 3]
                } else if gamma {
                    fog_color.as_vec3().to_array().map(to_gamma)
                } else {
                    fog_color.as_vec3().to_array()
                };
                fog = [fog_color[0], fog_color[1], fog_color[2], amount];
            }
        }
        let frame = if game.framerate > 0.0 && texture.facts.frames > 2 {
            let last = f64::from(texture.facts.frames.min(MAX_FRAMES) - 1);
            ((game.framerate * now) % last) as u32
        } else {
            0
        };
        let frame_texture = frame_key(key, frame);
        let key = match self.textures.get(&frame_texture) {
            Some(Some(_)) if !self.loading.contains(&frame_texture) => frame_texture,
            _ => key.to_owned(),
        };
        let (width, height) = (f64::from(texture.width), f64::from(texture.height));
        let left = -width * f64::from(look.origin[0]);
        let right_edge = width + left;
        let top = height * f64::from(look.origin[1]);
        let bottom = top - height;
        let (min_u, min_v) = (0.5 / width.max(1.0), 0.5 / height.max(1.0));
        let (max_u, max_v) = (1.0 - min_u, 1.0 - min_v);
        let corner = |across: f64, along: f64, u: f64, v: f64| SpriteVertex {
            position: (origin + up * (along * scale) + right * (across * scale))
                .as_vec3()
                .to_array(),
            uv: [u as f32, v as f32],
            color: tint,
            fog,
            mode: [
                blend.number(),
                if gamma { 1.0 } else { 0.0 },
                if mesh.selected { SELECTION_TINT } else { 0.0 },
                match blend {
                    Blend::Test(reference) => reference,
                    Blend::Add if !look.sprite_shader && !texture_alpha(texture.facts.alpha) => -1.0,
                    _ => 0.0,
                },
            ],
        };
        let a = corner(left, bottom, min_u, max_v);
        let b = corner(left, top, min_u, min_v);
        let c = corner(right_edge, top, max_u, min_v);
        let d = corner(right_edge, bottom, max_u, max_v);
        let vertices = vec![a, b, c, a, c, d];
        let (min, max) = vertices
            .iter()
            .fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(min, max), vertex| {
                let point = Vec3::from_array(vertex.position);
                (min.min(point), max.max(point))
            });
        Some(SpriteQuad {
            sight,
            center: origin.as_vec3(),
            min,
            max,
            key,
            vertices,
            depth_test,
            gamma,
        })
    }

    /// Whether a glow's proxy point is out of the camera's sight, behind a face the game
    /// draws, so the glow does not show, as `StandardGlowBlend` tests it.
    fn glow_hidden(&self, scene: &Scene, models: &ModelCache, camera: &Camera3D, proxy: DVec3) -> bool {
        let drawn = |face: &SceneFace| {
            face.material.as_deref().is_none_or(|material| {
                let texture = self.texture_for(&material_key(material));
                !matches!(texture.surface, Surface::Tool | Surface::Nodraw) && !texture.alpha.see_through()
            })
        };
        scene.line_blocked(camera.position, proxy, models, drawn)
    }

    /// Uploads sorted sprites into one vertex buffer, in runs that share a texture and a
    /// pipeline.
    fn sprite_draws(&self, quads: &[SpriteQuad]) -> Option<SpriteDraws> {
        if quads.is_empty() {
            return None;
        }
        let mut vertices: Vec<SpriteVertex> = Vec::new();
        let mut batches: Vec<(String, bool, Range<u32>)> = Vec::new();
        for quad in quads {
            let start = vertices.len() as u32;
            vertices.extend_from_slice(&quad.vertices);
            let end = vertices.len() as u32;
            match batches.last_mut() {
                Some((key, depth_test, range)) if *key == quad.key && *depth_test == quad.depth_test => range.end = end,
                _ => batches.push((quad.key.clone(), quad.depth_test, start..end)),
            }
        }
        let buffer = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mallet sprites"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        Some(SpriteDraws { buffer, batches })
    }

    /// Draws sprites into a pass on the camera image; `gamma` picks the pipelines for the
    /// image's gamma view.
    pub(super) fn draw_sprites(&self, pass: &mut wgpu::RenderPass<'_>, draws: &SpriteDraws, gamma: bool) {
        let pipelines = match (gamma, &self.sprite_pipelines.gamma) {
            (true, Some(gamma)) => gamma,
            _ => &self.sprite_pipelines.linear,
        };
        pass.set_vertex_buffer(0, draws.buffer.slice(..));
        for (key, depth_test, range) in &draws.batches {
            pass.set_pipeline(&pipelines[usize::from(!*depth_test)]);
            pass.set_bind_group(1, &self.texture_for(key).bind_group, &[]);
            pass.draw(range.clone(), 0..1);
        }
    }
}

/// Whether an added material is weighted by its texture's alpha (`$translucent` too).
fn texture_alpha(alpha: AlphaMode) -> bool {
    match alpha {
        AlphaMode::Add { texture_alpha, .. } => texture_alpha,
        _ => true,
    }
}

/// What the texture is multiplied by, how it blends, and whether what is in front hides
/// it, for a sprite of `look` drawn in render mode `mode` with the entity's colour and
/// opacity `vertex`. The `Sprite` shader picks its blend by the render mode; other
/// shaders blend as their material says and take the entity's colour only with
/// `$vertexcolor` and `$vertexalpha`, in linear light.
fn sprite_shading(look: SpriteLook, alpha: AlphaMode, mode: u8, vertex: [f32; 4]) -> ([f32; 4], Blend, bool) {
    if !look.sprite_shader {
        let mut tint = [1.0; 4];
        if look.vertex_color {
            for channel in 0..3 {
                tint[channel] = to_linear(vertex[channel]);
            }
        }
        if look.vertex_alpha {
            tint[3] = vertex[3];
        }
        let blend = match alpha {
            AlphaMode::Opaque => Blend::Opaque,
            AlphaMode::Test(reference) => Blend::Test(reference),
            AlphaMode::Blend(opacity) => {
                tint[3] *= opacity;
                Blend::Mix
            }
            AlphaMode::Add { opacity, .. } => {
                tint[3] *= opacity;
                Blend::Add
            }
        };
        return (tint, blend, true);
    }
    let constant = |color: [f32; 3], opacity: f32| {
        let tint = [color[0], color[1], color[2], opacity];
        if look.ignore_vertex_colors {
            tint
        } else {
            std::array::from_fn(|channel| tint[channel] * vertex[channel])
        }
    };
    let tint = |tint: [f32; 4]| -> [f32; 4] {
        if look.gamma {
            tint
        } else {
            [to_linear(tint[0]), to_linear(tint[1]), to_linear(tint[2]), tint[3]]
        }
    };
    match mode {
        NORMAL => ([1.0; 4], Blend::Opaque, true),
        TRANS_COLOR | TRANS_TEXTURE | TRANS_ALPHA => (tint(vertex), Blend::Mix, true),
        GLOW | WORLD_GLOW => (tint(vertex), Blend::Add, false),
        TRANS_ADD => (tint(constant(look.color, look.alpha)), Blend::Add, true),
        TRANS_ADD_FRAME_BLEND => (tint(constant([look.alpha; 3], 1.0)), Blend::Add, true),
        TRANS_ALPHA_ADD => (tint(vertex), Blend::AlphaAdd, true),
        _ => ([1.0; 4], Blend::Opaque, true),
    }
}
