//! The lighting preview: the camera view lit the way Source lights a map, from every
//! light entity, the sun and sky light, texture lights, self-illuminated materials,
//! projected textures, glows, the skybox and fog.
//!
//! Lights are gathered from the scene's lighting and the texture lights of `lights.rad`
//! whenever the map, the settings or a dragged light change, and are sorted into a grid
//! of world-space cells, so each pixel only adds up the lights that reach it. Shadows
//! come from shadow maps: one per sun covering the whole map, one per direction sky light
//! is traced from, and cube or spot maps for the lights nearest and brightest around the
//! camera, with small shadow maps in an atlas for many more lights past them, drawn a
//! few a frame. Lights stay where they are, so a shadow map is only drawn again when what
//! it shows changes. A map's 3D skybox gets its own sun shadows, so the map and the
//! skybox never shadow each other.
//!
//! With luxel detail, brush faces are lit the way the game lights them: once per luxel of
//! their lightmaps, at each face's lightmap scale. Every face's luxels sit in one atlas
//! texture, and lights are baked into it a batch each frame, every one with its own small
//! shadow maps, into a second texture shown once the bake is done. Moving the camera
//! needs no new lighting; faces are lit per pixel until the first bake is ready.

use std::collections::{HashMap, HashSet};
use std::f32::consts::FRAC_PI_2;
use std::hash::Hash;
use std::ops::Range;
use std::sync::{mpsc, Arc};
use std::time::Instant;

use eframe::egui::{self, Color32};
use eframe::wgpu::{self, util::DeviceExt};
use glam::{DVec3, Mat4, Vec3, Vec4};
use mallet_content::{AlphaMode, MaterialLoader, Surface};
use mallet_vmf::geometry::Bounds;
use mallet_vmf::light::{
    pattern_brightness, texture_light_scale, Falloff, Light, LightKind, Receivers, MAX_REACH, QUADRATIC,
};
use mallet_vmf::lightmap::{pack_rows, LuxelGrid};

use super::{
    box_visible, frustum_planes, material_key, pipeline, signature, Batches, Depth, FaceMesh, FaceVertex, GpuTexture,
    ModelInstanceData, RenderStats, Renderer3D, Translucent, COLOR_FORMAT, WHITE_KEY,
};
use crate::config::LightingOptions;
use crate::lighting::{HelperMode, LightingDetail, SkyMode, TextureLights, Tonemap};
use crate::scene::{Overlay, Owner, Scene, SceneFace};
use crate::view3d::Camera3D;

/// Depth format of shadow maps.
const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Pixels across each shadow map sky light is traced with.
const SKY_SHADOW_SIZE: u32 = 1024;
/// Pixels across the textures projected textures project, once scaled.
const COOKIE_SIZE: u32 = 256;
/// Pixels across each skybox face, once scaled.
const SKY_FACE_SIZE: u32 = 512;
/// Skybox face names, in the order the shader reads them: +X, -X, +Y, -Y, up, down.
const SKY_SUFFIXES: [&str; 6] = ["rt", "lf", "bk", "ft", "up", "dn"];
/// Texture lights are split into patches about this far apart, as VRAD chops faces.
const PATCH_SPACING: f64 = 128.0;
/// The most patches along one side of a texture light face.
const MAX_PATCH_SIDE: f64 = 8.0;
/// The widest spotlight cone, in degrees from its middle, that one shadow map covers;
/// wider ones get a cube of six.
const SINGLE_MAP_CONE: f64 = 60.0;
/// The most suns with shadows.
const MAX_SUNS: usize = 4;
/// The most light grid cells along one side.
const MAX_GRID_SIDE: f64 = 64.0;
/// The smallest light grid cell, in units.
const MIN_GRID_CELL: f64 = 64.0;
/// Stands for "no cap" in the shader, which has no infinity.
const NO_CAP: f32 = 1.0e30;
/// How much more a light that already has shadow maps counts when choosing which lights
/// get them, so the choice does not flicker between two lights.
const KEEP_BONUS: f64 = 1.5;
/// Depth format of the atlas of small shadow maps: half the memory of the others, and
/// enough to keep light from shining through walls.
const ATLAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth16Unorm;
/// Pixels along each side of a layer of the small shadow map atlas.
const ATLAS_SIDE: u32 = 2048;
/// The most small shadow maps drawn in one frame, so a map full of lights fills them in
/// over a few frames instead of stalling one.
const ATLAS_TILES_PER_FRAME: usize = 96;
/// Shadow casters are grouped into cubes this many units across, so the shadow maps of
/// small lights only draw the few around them.
const CASTER_CHUNK: f32 = 512.0;
/// How shadow caster positions are laid out for the shadow map shaders.
const POSITION_ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x3];
/// Format of baked luxel light.
const LUXEL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// The most luxels along one side of a face; bigger faces get coarser luxels.
const MAX_FACE_LUXELS: u32 = 256;
/// The most luxels the atlas holds; maps with more get coarser luxels everywhere.
const MAX_LUXELS: usize = 1 << 22;
/// Texels along each side of the largest luxel atlas.
const MAX_LUXEL_SIDE: u32 = 8192;
/// Faces with luxels are found by the cubes of this many units they touch.
const LUXEL_CELL: f32 = 1024.0;
/// Small shadow map tiles kept for the lights being baked into luxels.
const BAKE_TILES: u32 = 96;
/// The most lights baked into luxels in one frame.
const BAKE_LIGHTS: usize = 48;
/// How the rectangles of luxels a bake lights are laid out for the bake shader.
const RECT_ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x4];
/// The six faces of a cube shadow map: the way each looks and its up direction, in the
/// order the shader picks them: +X, -X, +Y, -Y, +Z, -Z.
const CUBE_FACES: [(Vec3, Vec3); 6] = [
    (Vec3::X, Vec3::Z),
    (Vec3::NEG_X, Vec3::Z),
    (Vec3::Y, Vec3::Z),
    (Vec3::NEG_Y, Vec3::Z),
    (Vec3::Z, Vec3::X),
    (Vec3::NEG_Z, Vec3::X),
];

/// What the lighting preview needs from the editor besides the scene.
#[derive(Clone, Copy)]
pub struct LightingInput<'a> {
    pub options: &'a LightingOptions,
    pub texture_lights: &'a Arc<TextureLights>,
}

/// What the latest lighting preview frame used, for the status and AI agents.
#[derive(Clone, Copy, Debug, Default)]
pub struct LightingStats {
    /// Lights lighting the preview, texture light patches included.
    pub lights: usize,
    pub texture_lights: usize,
    /// Suns and dragged lights, which every pixel adds up.
    pub everywhere: usize,
    pub grid_cells: usize,
    /// The most lights any grid cell holds.
    pub busiest_cell: usize,
    /// Lights with their own shadow maps, suns included.
    pub shadowed: usize,
    /// Lights with small shadow maps.
    pub small_shadowed: usize,
    /// Lights still waiting for their small shadow maps to be drawn.
    pub waiting: usize,
    /// Shadow map layers and small shadow maps there is room for.
    pub shadow_layers: usize,
    /// Shadow maps drawn for the latest frame, small ones included.
    pub shadow_maps_drawn: usize,
    pub glows: usize,
    /// Where the map's 3D skybox is, which gets its own sun shadows.
    pub skybox: Option<Bounds>,
    /// Luxels of the faces' lightmaps, 0 when faces are lit per pixel.
    pub luxels: usize,
    /// How many times coarser than the faces ask the luxels are, to fit the atlas.
    pub luxel_scale: f64,
    /// How much of a bake of luxel lighting is done, from 0 to 1, while one runs.
    pub baking: Option<f32>,
    /// Whether faces show their baked luxels.
    pub luxels_shown: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuLight {
    position: [f32; 4],
    direction: [f32; 4],
    color: [f32; 4],
    falloff: [f32; 4],
    fade: [f32; 4],
    shadow: [f32; 4],
    extra: [f32; 4],
    routing: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuFrame {
    eye: [f32; 4],
    fill: [f32; 4],
    sky_light: [f32; 4],
    fog_color: [f32; 4],
    fog_color2: [f32; 4],
    fog_dir: [f32; 4],
    fog_range: [f32; 4],
    sky_color: [f32; 4],
    sky_top: [f32; 4],
    sky_bottom: [f32; 4],
    sky_dusk: [f32; 4],
    sun_glow: [f32; 4],
    sun_glow_color: [f32; 4],
    grid_origin: [f32; 4],
    grid_size: [u32; 4],
    options: [u32; 4],
    shadow: [f32; 4],
    sky_shadow: [f32; 4],
    atlas: [u32; 4],
    skybox_min: [f32; 4],
    skybox_max: [f32; 4],
    luxels: [u32; 4],
}

/// Bytes in one glow vertex.
pub(super) const GLOW_VERTEX_SIZE: usize = std::mem::size_of::<GlowVertex>();

/// One corner of a glow or beam, drawn additively.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GlowVertex {
    position: [f32; 3],
    color: [f32; 4],
    shape: [f32; 3],
}

/// A light ready for the preview: its colour this frame, how far it reaches, and
/// whether every pixel adds it up instead of finding it through the grid.
#[derive(Clone)]
struct PreviewLight {
    light: Light,
    color: DVec3,
    reach: f64,
    everywhere: bool,
    casts: bool,
    /// Identifies the light's placement and shape, so its shadow maps are kept while it
    /// does not change.
    key: u64,
}

/// Lights sorted into world-space cells, each listing the lights that reach it.
#[derive(Default)]
struct Grid {
    origin: Vec3,
    cell: f32,
    size: [u32; 3],
    cells: Vec<[u32; 2]>,
    indices: Vec<u32>,
    busiest: usize,
}

/// A storage buffer that grows as needed.
struct Storage {
    buffer: wgpu::Buffer,
    size: u64,
    label: &'static str,
}

impl Storage {
    fn new(device: &wgpu::Device, label: &'static str) -> Self {
        Self {
            buffer: storage_buffer(device, label, 256),
            size: 256,
            label,
        }
    }

    /// Writes `bytes` at the start, replacing the buffer with a larger one when they do
    /// not fit. Returns whether it was replaced.
    fn write(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, bytes: &[u8]) -> bool {
        let needed = (bytes.len() as u64).max(16);
        let replaced = needed > self.size;
        if replaced {
            self.size = needed.next_power_of_two();
            self.buffer = storage_buffer(device, self.label, self.size);
        }
        if !bytes.is_empty() {
            queue.write_buffer(&self.buffer, 0, bytes);
        }
        replaced
    }
}

fn storage_buffer(device: &wgpu::Device, label: &str, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// One shadow map of a [`ShadowSet`], with the camera matrix it is drawn from.
struct ShadowLayer {
    target: wgpu::TextureView,
    buffer: wgpu::Buffer,
    group: wgpu::BindGroup,
    /// What the layer was last drawn for.
    content: Option<u64>,
}

/// Shadow maps of one size in one texture array.
struct ShadowSet {
    size: u32,
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    layers: Vec<ShadowLayer>,
}

impl ShadowSet {
    fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, size: u32, count: u32, label: &str) -> Self {
        let count = count.max(1);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: count,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SHADOW_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let layers = (0..count)
            .map(|layer| {
                let target = texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: layer,
                    array_layer_count: Some(1),
                    ..Default::default()
                });
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: std::mem::size_of::<[f32; 16]>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(label),
                    layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                });
                ShadowLayer {
                    target,
                    buffer,
                    group,
                    content: None,
                }
            })
            .collect();
        Self {
            size,
            _texture: texture,
            view,
            layers,
        }
    }
}

/// An RGBA texture array sampled by the shader: projected textures or skybox faces.
struct ImageArray {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl ImageArray {
    /// Uploads square layers of `size` pixels; at least one white layer when there are none.
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, size: u32, layers: &[Vec<u8>], label: &str) -> Self {
        let white = vec![255u8; (size * size * 4) as usize];
        let layers: Vec<&[u8]> = if layers.is_empty() {
            vec![&white]
        } else {
            layers.iter().map(Vec::as_slice).collect()
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: layers.len() as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: COLOR_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (layer, pixels) in layers.iter().enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size * 4),
                    rows_per_image: Some(size),
                },
                wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        Self {
            _texture: texture,
            view,
        }
    }
}

/// Small shadow maps for the lights past the nearest few: square tiles packed into the
/// layers of one depth texture array, each drawn from its own camera.
struct ShadowAtlas {
    /// Pixels across one tile; 0 for an atlas with no tiles.
    tile: u32,
    /// Tiles along each side of a layer.
    per_row: u32,
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Each layer on its own, to draw into.
    targets: Vec<wgpu::TextureView>,
    tiles: Vec<AtlasTile>,
}

/// One small shadow map: its camera and what it was last drawn for.
struct AtlasTile {
    buffer: wgpu::Buffer,
    group: wgpu::BindGroup,
    content: Option<u64>,
}

impl ShadowAtlas {
    /// An atlas of `layers` layers of tiles `tile` pixels across; with no tiles or no
    /// layers, a tiny one that holds nothing.
    fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, tile: u32, layers: u32) -> Self {
        let (tile, per_row, layers) = if tile == 0 || layers == 0 {
            (0, 1, 1)
        } else {
            (tile, (ATLAS_SIDE / tile).max(1), layers)
        };
        let side = (per_row * tile).max(1);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mallet small shadows"),
            size: wgpu::Extent3d {
                width: side,
                height: side,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: ATLAS_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let targets = (0..layers)
            .map(|layer| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: layer,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let count = if tile == 0 { 0 } else { per_row * per_row * layers };
        let tiles = (0..count)
            .map(|_| {
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("mallet small shadow"),
                    size: std::mem::size_of::<[f32; 16]>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("mallet small shadow"),
                    layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                });
                AtlasTile {
                    buffer,
                    group,
                    content: None,
                }
            })
            .collect();
        Self {
            tile,
            per_row,
            _texture: texture,
            view,
            targets,
            tiles,
        }
    }

    /// Tiles in one layer.
    fn per_layer(&self) -> u32 {
        self.per_row * self.per_row
    }

    /// The tile size and layer count it was made with: zeros when it holds nothing.
    fn shape(&self) -> (u32, u32) {
        if self.tile == 0 {
            (0, 0)
        } else {
            (self.tile, self.targets.len() as u32)
        }
    }
}

/// Which part of the map something is in: the map itself, or its 3D skybox, which the
/// game draws behind the sky and lights on its own, so neither shadows the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Zone {
    Map,
    Skybox,
}

/// Where the 3D skybox is, and how far the rest of the map reaches.
#[derive(Clone, Copy, Debug)]
struct Zones {
    skybox: Option<Bounds>,
    map: Bounds,
}

/// The faces that block light, in their own buffers for drawing shadow maps: opaque faces
/// as bare positions drawn a chunk at a time, and alpha-tested faces with their textures.
/// Faces of the 3D skybox are kept apart from the map's.
struct Casters {
    key: u64,
    positions: wgpu::Buffer,
    tested: wgpu::Buffer,
    chunks: Vec<CasterChunk>,
}

/// The casters of one zone in one cube of the world. Its sky faces follow its opaque
/// faces, so both draw at once when sky blocks the light.
struct CasterChunk {
    min: Vec3,
    max: Vec3,
    zone: Zone,
    solid: Range<u32>,
    sky: Range<u32>,
    tested: Vec<(String, Range<u32>)>,
}

/// The triangles of one [`CasterChunk`] before upload.
struct CasterBucket {
    min: Vec3,
    max: Vec3,
    solid: Vec<[f32; 3]>,
    sky: Vec<[f32; 3]>,
    tested: HashMap<String, Vec<FaceVertex>>,
}

/// What a shadow map is drawn for: just one zone's casters or all of them, and whether
/// sky faces block its light. VRAD stops every light at the sky but the sun and the sky
/// light, which come in through it.
#[derive(Clone, Copy)]
struct Blockers {
    zone: Option<Zone>,
    sky: bool,
}

impl Casters {
    /// Sorts the map's opaque, sky and alpha-tested faces into chunks, by zone and by which
    /// cube the middle of each triangle falls in. Tool, translucent and entity box faces
    /// never block light, and are left out.
    fn build(renderer: &Renderer3D, skybox: Option<&Bounds>, key: u64) -> Self {
        let mut buckets: HashMap<(Zone, [i32; 3]), CasterBucket> = HashMap::new();
        for cell in renderer.faces.iter().flat_map(|faces| faces.cells.values()) {
            for (texture_key, vertices) in &cell.by_texture {
                let texture = renderer.texture_for(texture_key);
                let passes = texture.surface == Surface::Tool || texture.alpha.see_through();
                if texture_key == WHITE_KEY || passes {
                    continue;
                }
                let sky = texture.surface == Surface::Sky;
                let tested = !sky && matches!(texture.alpha, AlphaMode::Test(_));
                for triangle in vertices.as_chunks::<3>().0 {
                    let corners = triangle.iter().map(|vertex| Vec3::from(vertex.position));
                    let middle = corners.clone().sum::<Vec3>() / 3.0;
                    let zone = zone_of(skybox, middle.as_dvec3());
                    let cube = (middle / CASTER_CHUNK).floor().as_ivec3().to_array();
                    let bucket = buckets.entry((zone, cube)).or_insert_with(|| CasterBucket {
                        min: Vec3::splat(f32::INFINITY),
                        max: Vec3::splat(f32::NEG_INFINITY),
                        solid: Vec::new(),
                        sky: Vec::new(),
                        tested: HashMap::new(),
                    });
                    for corner in corners {
                        bucket.min = bucket.min.min(corner);
                        bucket.max = bucket.max.max(corner);
                    }
                    if tested {
                        bucket
                            .tested
                            .entry(texture_key.clone())
                            .or_default()
                            .extend_from_slice(triangle);
                    } else if sky {
                        bucket.sky.extend(triangle.iter().map(|vertex| vertex.position));
                    } else {
                        bucket.solid.extend(triangle.iter().map(|vertex| vertex.position));
                    }
                }
            }
        }
        let mut positions: Vec<[f32; 3]> = Vec::new();
        let mut tested_vertices: Vec<FaceVertex> = Vec::new();
        let mut chunks = Vec::with_capacity(buckets.len());
        for ((zone, _), bucket) in buckets {
            let start = positions.len() as u32;
            positions.extend_from_slice(&bucket.solid);
            let solid = start..positions.len() as u32;
            positions.extend_from_slice(&bucket.sky);
            let sky = solid.end..positions.len() as u32;
            let mut tested = Vec::with_capacity(bucket.tested.len());
            for (texture_key, vertices) in bucket.tested {
                let start = tested_vertices.len() as u32;
                tested_vertices.extend_from_slice(&vertices);
                tested.push((texture_key, start..tested_vertices.len() as u32));
            }
            chunks.push(CasterChunk {
                min: bucket.min,
                max: bucket.max,
                zone,
                solid,
                sky,
                tested,
            });
        }
        let upload = |label: &str, bytes: &[u8]| {
            renderer.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: if bytes.is_empty() { &[0u8; 16] } else { bytes },
                usage: wgpu::BufferUsages::VERTEX,
            })
        };
        Self {
            key,
            positions: upload("mallet shadow casters", bytemuck::cast_slice(&positions)),
            tested: upload(
                "mallet alpha-tested shadow casters",
                bytemuck::cast_slice(&tested_vertices),
            ),
            chunks,
        }
    }
}

/// The pipelines that draw shadow casters into one depth format: opaque faces as bare
/// positions, alpha-tested faces with their textures, and props.
struct CasterPipelines {
    solid: wgpu::RenderPipeline,
    tested: wgpu::RenderPipeline,
    model: wgpu::RenderPipeline,
}

/// What the shader reads to find a face's luxels: its luxel grid directions, each with
/// the texel position of its first luxel in w, and its normal, whose w is 1 when the face
/// has luxels.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuLuxelFace {
    s: [f32; 4],
    t: [f32; 4],
    normal: [f32; 4],
}

/// One face's luxels, for baking lights into just the faces they reach: the space the
/// face covers, and its luxels' left, top, right and bottom in the atlas's clip space.
struct LuxelRect {
    min: Vec3,
    max: Vec3,
    clip: [f32; 4],
}

/// Where the luxels of every face the game lightmaps sit in one atlas texture, and the
/// point each luxel lights.
struct LuxelLayout {
    key: u64,
    width: u32,
    height: u32,
    /// What the shader needs of each face of the scene, by face number.
    faces: Vec<GpuLuxelFace>,
    rects: Vec<LuxelRect>,
    /// The rectangles touching each cube of [`LUXEL_CELL`] units.
    cells: HashMap<[i32; 3], Vec<u32>>,
    /// Each texel's point as float bits, and its face's number plus one, 0 for texels no
    /// face uses. Emptied once uploaded.
    points: Vec<[u32; 4]>,
    luxels: usize,
    /// How many times coarser than the faces ask the luxels had to be to fit.
    coarser: f64,
}

/// A face to lay luxels out for, copied from the scene for the worker thread that lays
/// them out: its number, corners, normal, texture axes and units per luxel.
struct LuxelFace {
    id: u32,
    corners: Vec<DVec3>,
    normal: DVec3,
    axes: [DVec3; 2],
    scale: f64,
}

impl LuxelFace {
    /// Every face of the scene the game lightmaps: faces with a lit material that is not
    /// sky, nodraw or a tool texture. Displacements are left out: their luxels would lie
    /// on the flat face, so they are lit per pixel where their surface is. Returns them
    /// and one more than the highest face number the scene uses.
    fn gather(scene: &Scene, renderer: &Renderer3D) -> (Vec<Self>, usize) {
        let total = scene
            .meshes
            .iter()
            .filter_map(|mesh| Some(mesh.first_face? as usize + mesh.faces.len()))
            .max()
            .unwrap_or(0);
        let mut faces = Vec::new();
        for mesh in &scene.meshes {
            let Some(first) = mesh.first_face else {
                continue;
            };
            for (index, face) in mesh.faces.iter().enumerate() {
                let lightmapped = !face.decal && face.displacement.is_none();
                let Some(material) = face.material.as_ref().filter(|_| lightmapped) else {
                    continue;
                };
                let texture = renderer.texture_for(&material_key(material));
                if texture.surface != Surface::Normal || texture.facts.unlit {
                    continue;
                }
                faces.push(Self::copy(first.saturating_add(index as u32), face));
            }
        }
        (faces, total)
    }

    fn copy(id: u32, face: &SceneFace) -> Self {
        Self {
            id,
            corners: face.vertices.clone(),
            normal: face.normal,
            axes: [face.uaxis.axis, face.vaxis.axis],
            scale: face.lightmap_scale,
        }
    }
}

impl LuxelLayout {
    /// Lays out the luxels of `lit`, of a scene with `total` faces. Luxels get coarser
    /// everywhere when the map has too many for the atlas.
    fn build(lit: &[LuxelFace], total: usize, key: u64) -> Self {
        let mut coarser = 1.0;
        loop {
            let grids: Vec<Option<LuxelGrid>> = lit
                .iter()
                .map(|face| {
                    LuxelGrid::new(
                        &face.corners,
                        face.normal,
                        face.axes,
                        face.scale * coarser,
                        MAX_FACE_LUXELS,
                    )
                })
                .collect();
            let sizes: Vec<(u32, u32)> = grids
                .iter()
                .map(|grid| grid.as_ref().map_or((0, 0), |grid| (grid.width, grid.height)))
                .collect();
            let luxels: usize = sizes.iter().map(|(w, h)| *w as usize * *h as usize).sum();
            if luxels > MAX_LUXELS {
                coarser *= (luxels as f64 / MAX_LUXELS as f64).sqrt().max(1.1);
                continue;
            }
            let area: f64 = sizes.iter().map(|(w, h)| f64::from(w + 1) * f64::from(h + 1)).sum();
            let strip = ((area.sqrt() * 1.1).ceil() as u32)
                .max(MAX_FACE_LUXELS)
                .next_power_of_two()
                .min(MAX_LUXEL_SIDE);
            let (places, height) = pack_rows(&sizes, strip, 1);
            let lost = places
                .iter()
                .zip(&sizes)
                .any(|(place, (w, _))| place.is_none() && *w > 0);
            if height > MAX_LUXEL_SIDE || lost {
                coarser *= 1.25;
                continue;
            }
            let (width, height) = (strip, height.max(1));
            let mut faces = vec![GpuLuxelFace::default(); total.max(1)];
            let mut points = vec![[0u32; 4]; width as usize * height as usize];
            let mut rects = Vec::new();
            let mut cells: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
            let clip_x = |x: u32| x as f32 / width as f32 * 2.0 - 1.0;
            let clip_y = |y: u32| 1.0 - y as f32 / height as f32 * 2.0;
            for ((face, grid), place) in lit.iter().zip(&grids).zip(&places) {
                let (Some(grid), Some((x, y))) = (grid, place) else {
                    continue;
                };
                let id = &face.id;
                for j in 0..grid.height {
                    for i in 0..grid.width {
                        let point = grid.point(i, j).as_vec3();
                        let at = (y + j) as usize * width as usize + (x + i) as usize;
                        points[at] = [point.x.to_bits(), point.y.to_bits(), point.z.to_bits(), id + 1];
                    }
                }
                faces[*id as usize] = GpuLuxelFace {
                    s: grid
                        .s
                        .as_vec3()
                        .extend((f64::from(*x) + 0.5 - grid.s_min) as f32)
                        .to_array(),
                    t: grid
                        .t
                        .as_vec3()
                        .extend((f64::from(*y) + 0.5 - grid.t_min) as f32)
                        .to_array(),
                    normal: face.normal.as_vec3().extend(1.0).to_array(),
                };
                let mut bounds = Bounds::EMPTY;
                for corner in &face.corners {
                    bounds.add_point(*corner);
                }
                let (min, max) = (bounds.min.as_vec3(), bounds.max.as_vec3());
                let (low, high) = (luxel_cell(min), luxel_cell(max));
                for cx in low[0]..=high[0] {
                    for cy in low[1]..=high[1] {
                        for cz in low[2]..=high[2] {
                            cells.entry([cx, cy, cz]).or_default().push(rects.len() as u32);
                        }
                    }
                }
                rects.push(LuxelRect {
                    min,
                    max,
                    clip: [clip_x(*x), clip_y(*y), clip_x(x + grid.width), clip_y(y + grid.height)],
                });
            }
            return Self {
                key,
                width,
                height,
                faces,
                rects,
                cells,
                points,
                luxels,
                coarser,
            };
        }
    }

    /// The rectangles of the faces a light reaching `radius` units from `center` may
    /// light, each once, using `seen` to remember which were taken with `stamp`.
    fn reached(&self, center: Vec3, radius: f32, (seen, stamp): (&mut [u32], u32), out: &mut Vec<u32>) {
        let low = luxel_cell(center - Vec3::splat(radius));
        let high = luxel_cell(center + Vec3::splat(radius));
        let span = (0..3)
            .map(|axis| i64::from(high[axis] - low[axis]) + 1)
            .product::<i64>();
        let check = |rect: &LuxelRect| {
            let nearest = center.clamp(rect.min, rect.max);
            nearest.distance_squared(center) <= radius * radius
        };
        if span > self.cells.len() as i64 {
            for (index, rect) in self.rects.iter().enumerate() {
                if seen[index] != stamp && check(rect) {
                    seen[index] = stamp;
                    out.push(index as u32);
                }
            }
            return;
        }
        for cx in low[0]..=high[0] {
            for cy in low[1]..=high[1] {
                for cz in low[2]..=high[2] {
                    for &index in self.cells.get(&[cx, cy, cz]).into_iter().flatten() {
                        if seen[index as usize] != stamp && check(&self.rects[index as usize]) {
                            seen[index as usize] = stamp;
                            out.push(index);
                        }
                    }
                }
            }
        }
    }
}

/// The cube of [`LUXEL_CELL`] units a point is in.
fn luxel_cell(point: Vec3) -> [i32; 3] {
    (point / LUXEL_CELL).floor().as_ivec3().to_array()
}

/// The luxel lighting of the faces: their layout, the points the luxels light, and two
/// light textures, one shown while the other is baked.
struct Luxels {
    layout: LuxelLayout,
    _points: wgpu::Texture,
    points: wgpu::TextureView,
    lights: [(wgpu::Texture, wgpu::TextureView); 2],
    faces: Storage,
    /// Which of `lights` faces show, and what it was baked from.
    front: usize,
    shown: Option<u64>,
    /// The bake under way into the other texture: what it is baked from, and how many
    /// of its lights are in.
    bake: Option<(u64, usize)>,
    /// Marks which rectangles a batch of lights already takes.
    seen: Vec<u32>,
    stamp: u32,
}

impl Luxels {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, mut layout: LuxelLayout) -> Self {
        let size = wgpu::Extent3d {
            width: layout.width,
            height: layout.height,
            depth_or_array_layers: 1,
        };
        let texture = |label: &str, format: wgpu::TextureFormat, usage: wgpu::TextureUsages| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            (texture, view)
        };
        let (points_texture, points) = texture(
            "mallet luxel points",
            wgpu::TextureFormat::Rgba32Uint,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        );
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &points_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&layout.points),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(layout.width * 16),
                rows_per_image: Some(layout.height),
            },
            size,
        );
        layout.points = Vec::new();
        let light_usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        let lights = [
            texture("mallet luxel light", LUXEL_FORMAT, light_usage),
            texture("mallet luxel light", LUXEL_FORMAT, light_usage),
        ];
        let mut faces = Storage::new(device, "mallet luxel faces");
        faces.write(device, queue, bytemuck::cast_slice(&layout.faces));
        let seen = vec![0; layout.rects.len()];
        Self {
            layout,
            _points: points_texture,
            points,
            lights,
            faces,
            front: 0,
            shown: None,
            bake: None,
            seen,
            stamp: 0,
        }
    }
}

/// Props that cast shadows: one instance buffer of every prop, sorted by model and then by
/// place so neighbouring props of a model draw together, with each prop's model, bounds
/// and zone. Rebuilt when the geometry changes or more models have loaded.
struct PropCasters {
    generation: u64,
    models: usize,
    buffer: Option<wgpu::Buffer>,
    props: Vec<PropCaster>,
}

/// One prop of [`PropCasters`].
struct PropCaster {
    key: String,
    min: Vec3,
    max: Vec3,
    zone: Zone,
}

/// Something made on a worker thread for the preview, by default textures, with what it
/// is for.
type Loading<T, R = Vec<Option<Vec<u8>>>> = Option<(T, mpsc::Receiver<R>)>;

/// Everything the lighting preview keeps between frames.
pub(super) struct LitState {
    layout: wgpu::BindGroupLayout,
    face: wgpu::RenderPipeline,
    overdraw: wgpu::RenderPipeline,
    translucent: wgpu::RenderPipeline,
    model: wgpu::RenderPipeline,
    translucent_model: wgpu::RenderPipeline,
    /// Draw shadow casters into full shadow maps, and into the small ones of the atlas.
    layer_casters: CasterPipelines,
    tile_casters: CasterPipelines,
    /// Clears one tile of the atlas.
    tile_clear: wgpu::RenderPipeline,
    /// Start a bake of luxel lighting with sky light and sunlight, and add a batch of
    /// lights to one.
    bake_sky: wgpu::RenderPipeline,
    bake_lights: wgpu::RenderPipeline,
    glow: wgpu::RenderPipeline,
    frame: wgpu::Buffer,
    lights: Storage,
    cells: Storage,
    indices: Storage,
    matrices: Storage,
    sky_samples: Storage,
    local: ShadowSet,
    suns: ShadowSet,
    sky: ShadowSet,
    shadow_sampler: wgpu::Sampler,
    clamp_sampler: wgpu::Sampler,
    cookies: ImageArray,
    cookie_names: Vec<String>,
    cookie_loading: Loading<Vec<String>>,
    skybox: ImageArray,
    /// The skybox last loaded, and whether all six of its faces were found.
    sky_name: Option<String>,
    sky_ready: bool,
    sky_loading: Loading<String>,
    group: Option<wgpu::BindGroup>,
    /// What the gathered lights were made from, and what the light grid was sorted from;
    /// a dragged light changes the first but not the second, as it lights everywhere.
    lights_key: Option<u64>,
    grid_key: Option<u64>,
    prepared: Vec<PreviewLight>,
    everywhere: usize,
    grid: Grid,
    surface_key: Option<u64>,
    surface: Vec<Light>,
    /// Shadow map layers of the lights that have them: first layer and count, by key.
    slots: HashMap<u64, (u32, u32)>,
    atlas: ShadowAtlas,
    /// Tiles of the atlas of the lights with small shadow maps: first tile and count, by
    /// key.
    atlas_slots: HashMap<u64, (u32, u32)>,
    casters: Option<Casters>,
    props: Option<PropCasters>,
    /// The zones, with the geometry generation they were found in.
    zones: Option<(u64, Zones)>,
    /// Draw every small shadow map still waiting in the next frame, as screenshots need.
    draw_all: bool,
    luxels: Option<Luxels>,
    /// A luxel layout being made on a worker thread, and what it is for.
    luxel_loading: Loading<u64, LuxelLayout>,
    /// Stand-ins bound while faces are lit per pixel.
    blank_light: (wgpu::Texture, wgpu::TextureView),
    blank_points: (wgpu::Texture, wgpu::TextureView),
    blank_faces: Storage,
    started: Instant,
    /// Counts loads and settings that change what the preview shows, for cached images.
    pub version: u64,
    pub stats: LightingStats,
}

impl LitState {
    pub(super) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        uniform_layout: &wgpu::BindGroupLayout,
        texture_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mallet lighting preview shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("lit.wgsl").into()),
        });
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: fragment,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let depth_array = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: fragment,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Depth,
                view_dimension: wgpu::TextureViewDimension::D2Array,
                multisampled: false,
            },
            count: None,
        };
        let color_array = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: fragment,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2Array,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mallet lighting"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: fragment,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage(1),
                storage(2),
                storage(3),
                storage(4),
                depth_array(5),
                depth_array(6),
                depth_array(7),
                wgpu::BindGroupLayoutEntry {
                    binding: 8,
                    visibility: fragment,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
                color_array(9),
                color_array(10),
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: fragment,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                storage(12),
                depth_array(13),
                wgpu::BindGroupLayoutEntry {
                    binding: 14,
                    visibility: fragment,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                storage(15),
                wgpu::BindGroupLayoutEntry {
                    binding: 16,
                    visibility: fragment,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let lit_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mallet lit faces"),
            bind_group_layouts: &[uniform_layout, texture_layout, &layout],
            push_constant_ranges: &[],
        });
        let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mallet shadows"),
            bind_group_layouts: &[uniform_layout, texture_layout],
            push_constant_ranges: &[],
        });
        let glow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mallet glows"),
            bind_group_layouts: &[uniform_layout],
            push_constant_ranges: &[],
        });
        let face_buffers = [super::face_buffer_layout()];
        let model_buffers = super::model_buffer_layouts();
        let faces = |depth| {
            pipeline(
                device,
                &lit_layout,
                &shader,
                ("vs_lit_face", "fs_lit"),
                &face_buffers,
                wgpu::PrimitiveTopology::TriangleList,
                depth,
                true,
            )
        };
        let models = |depth| {
            pipeline(
                device,
                &lit_layout,
                &shader,
                ("vs_lit_model", "fs_lit"),
                &model_buffers,
                wgpu::PrimitiveTopology::TriangleList,
                depth,
                false,
            )
        };
        let solid_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mallet shadow casters"),
            bind_group_layouts: &[uniform_layout],
            push_constant_ranges: &[],
        });
        let position_buffers = [wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<[f32; 3]>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &POSITION_ATTRIBUTES,
        }];
        let casters = |format| CasterPipelines {
            solid: depth_pipeline(
                device,
                &solid_layout,
                &shader,
                ("vs_shadow_solid", false),
                &position_buffers,
                format,
            ),
            tested: depth_pipeline(
                device,
                &shadow_layout,
                &shader,
                ("vs_shadow_face", true),
                &face_buffers,
                format,
            ),
            model: depth_pipeline(
                device,
                &shadow_layout,
                &shader,
                ("vs_shadow_model", true),
                &model_buffers,
                format,
            ),
        };
        let glow = glow_pipeline(device, &glow_layout, &shader);
        let rect_buffers = [wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<[f32; 4]>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &RECT_ATTRIBUTES,
        }];
        let add = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        let bake_sky = bake_pipeline(
            device,
            &lit_layout,
            &shader,
            ("vs_bake_all", "fs_bake_sky"),
            &[],
            (wgpu::PrimitiveTopology::TriangleList, None),
        );
        let bake_lights = bake_pipeline(
            device,
            &lit_layout,
            &shader,
            ("vs_bake_rect", "fs_bake_lights"),
            &rect_buffers,
            (
                wgpu::PrimitiveTopology::TriangleStrip,
                Some(wgpu::BlendState { color: add, alpha: add }),
            ),
        );
        let blank = |label: &str, format: wgpu::TextureFormat| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            (texture, view)
        };
        let frame = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mallet lighting frame"),
            size: std::mem::size_of::<GpuFrame>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("mallet shadow sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let clamp_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("mallet clamp sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            face: faces(Depth::Test),
            overdraw: faces(Depth::Overdraw),
            translucent: faces(Depth::Translucent),
            model: models(Depth::Test),
            translucent_model: models(Depth::Translucent),
            layer_casters: casters(SHADOW_FORMAT),
            tile_casters: casters(ATLAS_FORMAT),
            tile_clear: clear_pipeline(device, &solid_layout, &shader),
            bake_sky,
            bake_lights,
            glow,
            frame,
            lights: Storage::new(device, "mallet lights"),
            cells: Storage::new(device, "mallet light grid"),
            indices: Storage::new(device, "mallet light grid lights"),
            matrices: Storage::new(device, "mallet shadow matrices"),
            sky_samples: Storage::new(device, "mallet sky directions"),
            local: ShadowSet::new(device, uniform_layout, 1, 1, "mallet light shadows"),
            suns: ShadowSet::new(device, uniform_layout, 1, 1, "mallet sun shadows"),
            sky: ShadowSet::new(device, uniform_layout, 1, 1, "mallet sky shadows"),
            shadow_sampler,
            clamp_sampler,
            cookies: ImageArray::new(device, queue, 1, &[], "mallet projected textures"),
            cookie_names: Vec::new(),
            cookie_loading: None,
            skybox: ImageArray::new(device, queue, 1, &[], "mallet skybox"),
            sky_name: None,
            sky_ready: false,
            sky_loading: None,
            group: None,
            layout,
            lights_key: None,
            grid_key: None,
            prepared: Vec::new(),
            everywhere: 0,
            grid: Grid::default(),
            surface_key: None,
            surface: Vec::new(),
            slots: HashMap::new(),
            atlas: ShadowAtlas::new(device, uniform_layout, 0, 0),
            atlas_slots: HashMap::new(),
            casters: None,
            props: None,
            zones: None,
            draw_all: false,
            luxels: None,
            luxel_loading: None,
            blank_light: blank("mallet blank luxel light", LUXEL_FORMAT),
            blank_points: blank("mallet blank luxel points", wgpu::TextureFormat::Rgba32Uint),
            blank_faces: Storage::new(device, "mallet blank luxel faces"),
            started: Instant::now(),
            version: 0,
            stats: LightingStats::default(),
        }
    }

    /// How many texture loads of the preview are still running, a luxel layout being made
    /// included.
    pub(super) fn loading(&self) -> usize {
        usize::from(self.cookie_loading.is_some())
            + usize::from(self.sky_loading.is_some())
            + usize::from(self.luxel_loading.is_some())
    }

    /// Whether lights are still waiting for their small shadow maps, or luxels are being
    /// laid out or baked.
    pub(super) fn pending(&self) -> bool {
        self.stats.waiting > 0 || self.stats.baking.is_some() || self.luxel_loading.is_some()
    }

    /// Draws every small shadow map still waiting in the next frame, as screenshots need.
    pub(super) fn draw_all_shadows(&mut self) {
        self.draw_all = true;
        self.version += 1;
    }

    /// A fingerprint of what the preview shows besides the scene and camera, for cached
    /// viewport images: the settings, the texture lights, loads that finished and, while
    /// lights of `scene` play their appearances, the moment.
    pub(super) fn fingerprint(&self, scene: &Scene, input: LightingInput<'_>) -> u64 {
        let animated = input.options.animate && scene.lighting.lights.iter().any(|light| light.pattern.is_some());
        signature(|hasher| {
            format!("{:?}", input.options).hash(hasher);
            (Arc::as_ptr(input.texture_lights) as usize, self.version).hash(hasher);
            if animated {
                ((self.started.elapsed().as_secs_f64() * mallet_vmf::light::PATTERN_RATE) as u64).hash(hasher);
            }
        })
    }

    /// Takes in projected textures and skybox faces that finished loading; with `wait`,
    /// waits for those still loading, as screenshots do.
    pub(super) fn poll(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, wait: bool) {
        let receive = |receiver: &mpsc::Receiver<Vec<Option<Vec<u8>>>>| {
            if wait {
                receiver.recv().map_err(|_| mpsc::TryRecvError::Disconnected)
            } else {
                receiver.try_recv()
            }
        };
        if let Some((names, receiver)) = self.cookie_loading.take() {
            match receive(&receiver) {
                Ok(images) => {
                    let layers: Vec<Vec<u8>> = images
                        .into_iter()
                        .map(|image| image.unwrap_or_else(|| vec![255u8; (COOKIE_SIZE * COOKIE_SIZE * 4) as usize]))
                        .collect();
                    self.cookies = ImageArray::new(device, queue, COOKIE_SIZE, &layers, "mallet projected textures");
                    self.cookie_names = names;
                    self.group = None;
                    self.version += 1;
                }
                Err(mpsc::TryRecvError::Empty) => self.cookie_loading = Some((names, receiver)),
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }
        if let Some((name, receiver)) = self.sky_loading.take() {
            match receive(&receiver) {
                Ok(faces) => {
                    self.sky_ready = faces.iter().all(Option::is_some);
                    let layers: Vec<Vec<u8>> = faces.into_iter().flatten().collect();
                    self.skybox = if self.sky_ready {
                        ImageArray::new(device, queue, SKY_FACE_SIZE, &layers, "mallet skybox")
                    } else {
                        ImageArray::new(device, queue, 1, &[], "mallet skybox")
                    };
                    self.sky_name = Some(name);
                    self.group = None;
                    self.version += 1;
                }
                Err(mpsc::TryRecvError::Empty) => self.sky_loading = Some((name, receiver)),
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }
    }

    /// Starts loading the projected textures and skybox the map asks for, when they are
    /// not loaded or loading already.
    fn request_textures(&mut self, scene: &Scene, loader: Option<&MaterialLoader>, options: &LightingOptions) {
        let Some(loader) = loader else {
            return;
        };
        let mut names: Vec<String> = scene
            .lighting
            .lights
            .iter()
            .filter_map(|light| light.projection.as_ref())
            .map(|projection| projection.texture.to_ascii_lowercase())
            .collect();
        names.sort();
        names.dedup();
        let cookies_wanted = names != self.cookie_names
            && self
                .cookie_loading
                .as_ref()
                .is_none_or(|(loading, _)| *loading != names);
        if cookies_wanted && !names.is_empty() {
            let (sender, receiver) = mpsc::channel();
            let loader = loader.clone();
            let wanted = names.clone();
            let spawned = std::thread::Builder::new()
                .name("mallet projected textures".to_owned())
                .spawn(move || {
                    let images = wanted
                        .iter()
                        .map(|name| {
                            loader
                                .texture_image(name, COOKIE_SIZE * 2)
                                .map(|image| square(&image, COOKIE_SIZE, false))
                        })
                        .collect();
                    let _ = sender.send(images);
                });
            if spawned.is_ok() {
                self.cookie_loading = Some((names, receiver));
            }
        }
        let sky = scene.lighting.sky.name.trim().to_ascii_lowercase();
        let sky_wanted = options.sky == SkyMode::Skybox
            && !sky.is_empty()
            && sky != "painted"
            && self.sky_name.as_deref() != Some(sky.as_str())
            && self.sky_loading.as_ref().is_none_or(|(loading, _)| *loading != sky);
        if sky_wanted {
            let (sender, receiver) = mpsc::channel();
            let loader = loader.clone();
            let name = sky.clone();
            let spawned = std::thread::Builder::new()
                .name("mallet skybox".to_owned())
                .spawn(move || {
                    let faces = SKY_SUFFIXES
                        .iter()
                        .map(|suffix| {
                            let material = loader.load(&format!("skybox/{name}{suffix}"));
                            let image = material.texture?;
                            let half = image.pixel_width >= image.pixel_height * 2;
                            Some(square(&image, SKY_FACE_SIZE, half))
                        })
                        .collect();
                    let _ = sender.send(faces);
                });
            if spawned.is_ok() {
                self.sky_loading = Some((sky, receiver));
            }
        }
    }

    /// Gets the lights, the light grid and the shadow maps ready for a frame, drawing the
    /// shadow maps that changed into `encoder`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare(
        &mut self,
        renderer: &Renderer3D,
        encoder: &mut wgpu::CommandEncoder,
        scene: &Scene,
        overlay: &Overlay,
        camera: &Camera3D,
        planes: &[Vec4; 6],
        loader: Option<&MaterialLoader>,
        input: LightingInput<'_>,
    ) {
        let options = input.options;
        let device = &renderer.device;
        let queue = &renderer.queue;
        self.request_textures(scene, loader, options);
        let settled = if renderer.loading.is_empty() {
            renderer.textures_version
        } else {
            0
        };
        let surface_key = signature(|hasher| {
            (
                scene.geometry_generation,
                settled,
                Arc::as_ptr(input.texture_lights) as usize,
                options.texture_lights,
            )
                .hash(hasher);
        });
        if self.surface_key != Some(surface_key) {
            self.surface = if options.texture_lights {
                surface_lights(scene, renderer, input.texture_lights)
            } else {
                Vec::new()
            };
            self.surface_key = Some(surface_key);
        }
        let moving = moving_entities(scene, overlay);
        let seconds = self.started.elapsed().as_secs_f64();
        let grid_key = signature(|hasher| {
            (scene.geometry_generation, surface_key).hash(hasher);
            let mut moving: Vec<u32> = moving.iter().copied().collect();
            moving.sort_unstable();
            moving.hash(hasher);
            (
                options.hdr,
                options.sun,
                options.dynamic_lights,
                options.dark_lights,
                options.sun_shadows,
                options.light_shadows,
                options.light_cutoff().to_bits(),
                options.lights_per_spot(),
            )
                .hash(hasher);
        });
        let lights_key = signature(|hasher| {
            grid_key.hash(hasher);
            format!("{:?}", overlay.transform).hash(hasher);
        });
        if self.lights_key != Some(lights_key) {
            self.prepared = gather(scene, overlay, &moving, &self.surface, options);
            self.everywhere = self.prepared.iter().take_while(|light| light.everywhere).count();
            self.lights_key = Some(lights_key);
        }
        if self.grid_key != Some(grid_key) {
            self.grid = build_grid(
                &scene.bounds,
                &self.prepared,
                self.everywhere,
                options.light_cutoff(),
                options.lights_per_spot(),
            );
            let mut replaced = self.cells.write(device, queue, bytemuck::cast_slice(&self.grid.cells));
            replaced |= self
                .indices
                .write(device, queue, bytemuck::cast_slice(&self.grid.indices));
            if replaced {
                self.group = None;
            }
            self.grid_key = Some(grid_key);
        }
        self.update_zones(scene);
        self.update_casters(renderer, scene, settled);
        self.update_props(scene, renderer, options);
        let luxel_mode = options.detail == LightingDetail::Luxels && !scene.bounds.is_empty();
        self.update_luxels(scene, renderer, (settled, self.draw_all), luxel_mode);
        let appearance = |light: &PreviewLight| match (&light.light.pattern, options.animate) {
            (Some(pattern), true) => pattern_brightness(pattern, seconds),
            _ => 1.0,
        };
        let mut gpu: Vec<GpuLight> = self
            .prepared
            .iter()
            .map(|light| pack(light, appearance(light)))
            .collect();
        let mut matrices: Vec<Mat4> = Vec::new();
        let mut drawn = 0;
        let has_bounds = !scene.bounds.is_empty();
        let zones = self.zones.map_or(
            Zones {
                skybox: None,
                map: scene.bounds,
            },
            |(_, zones)| zones,
        );
        let draw_all = std::mem::take(&mut self.draw_all);
        let bake_list = if luxel_mode {
            self.bake_list(options)
        } else {
            Vec::new()
        };
        let bake_key = self.bake_key(&bake_list, options, settled);
        if luxel_mode {
            for &index in &bake_list {
                gpu[index].routing[2] = 1.0;
            }
            for (index, light) in self.prepared.iter().enumerate().take(self.everywhere) {
                if light.light.kind == LightKind::Directional {
                    gpu[index].routing[2] = 1.0;
                }
            }
        }
        let luxels_shown = self
            .luxels
            .as_ref()
            .is_some_and(|luxels| luxels.shown == Some(bake_key));
        let casters = self.casters.as_ref();
        let props = self.props.as_ref().filter(|_| options.prop_shadows);

        let (chosen, small) = if options.light_shadows && has_bounds {
            self.choose_shadowed(
                camera,
                planes,
                (options.shadowed_light_count(), options.more_shadowed_count()),
                max_layers(device),
            )
        } else {
            (Vec::new(), Vec::new())
        };
        let needed: u32 = chosen.iter().map(|&index| layers_for(&self.prepared[index])).sum();
        let local_size = options.light_map_size();
        let (want_size, want_count) = if needed == 0 {
            if options.light_shadows {
                (self.local.size, self.local.layers.len() as u32)
            } else {
                (1, 1)
            }
        } else {
            let count = needed.next_multiple_of(6).min(max_layers(device));
            let keep = self.local.size == local_size && self.local.layers.len() as u32 >= count;
            if keep {
                (self.local.size, self.local.layers.len() as u32)
            } else {
                (
                    local_size,
                    count.max(self.local.layers.len() as u32).min(max_layers(device)),
                )
            }
        };
        if self.local.size != want_size || self.local.layers.len() as u32 != want_count {
            self.local = ShadowSet::new(
                device,
                &renderer.uniform_layout,
                want_size,
                want_count,
                "mallet light shadows",
            );
            self.slots.clear();
            self.group = None;
        }
        let wanted = self.runs(&chosen);
        allocate_runs(&mut self.slots, self.local.layers.len(), &wanted);
        matrices.resize(self.local.layers.len(), Mat4::IDENTITY);
        for &index in &chosen {
            let light = &self.prepared[index];
            let Some(&(first, count)) = self.slots.get(&light.key) else {
                continue;
            };
            let (views, offset) = local_views(light, count, self.local.size);
            for (face, matrix) in views.iter().enumerate() {
                let layer = first as usize + face;
                matrices[layer] = *matrix;
                let content = signature(|hasher| {
                    (
                        light.key,
                        face,
                        scene.geometry_generation,
                        options.prop_shadows,
                        settled,
                    )
                        .hash(hasher);
                    self.local.size.hash(hasher);
                });
                if self.local.layers[layer].content != Some(content) {
                    draw_layer(
                        renderer,
                        encoder,
                        &self.layer_casters,
                        &self.local.layers[layer],
                        *matrix,
                        (casters, props),
                        Blockers { zone: None, sky: true },
                    );
                    self.local.layers[layer].content = Some(content);
                    drawn += 1;
                }
            }
            gpu[index].shadow[0] = first as f32;
            gpu[index].shadow[1] = count as f32;
            gpu[index].extra[0] = offset;
            if light.light.kind == LightKind::Projected {
                gpu[index].extra[2] = first as f32;
            }
        }

        let reserve = if luxel_mode && options.light_shadows {
            BAKE_TILES
        } else {
            0
        };
        let small_needed: u32 = small
            .iter()
            .map(|&index| layers_for(&self.prepared[index]))
            .sum::<u32>()
            + reserve;
        let small_size = options.small_map_size();
        let want_shape = if small_needed == 0 {
            if options.light_shadows && options.more_shadowed_count() > 0 && has_bounds {
                self.atlas.shape()
            } else {
                (0, 0)
            }
        } else {
            let per_layer = (ATLAS_SIDE / small_size).max(1).pow(2);
            let layers = small_needed.div_ceil(per_layer).min(max_layers(device));
            let (tile, held) = self.atlas.shape();
            if tile == small_size && held >= layers {
                (tile, held)
            } else if tile == small_size {
                (small_size, layers.max(held))
            } else {
                (small_size, layers)
            }
        };
        if self.atlas.shape() != want_shape {
            self.atlas = ShadowAtlas::new(device, &renderer.uniform_layout, want_shape.0, want_shape.1);
            self.atlas_slots.clear();
            self.group = None;
        }
        let wanted = self.runs(&small);
        let room = self.atlas.tiles.len().saturating_sub(reserve as usize);
        allocate_runs(&mut self.atlas_slots, room, &wanted);
        let atlas_base = matrices.len() as u32;
        matrices.resize(matrices.len() + self.atlas.tiles.len(), Mat4::IDENTITY);
        let mut budget = if draw_all { usize::MAX } else { ATLAS_TILES_PER_FRAME };
        let mut pending: Vec<(u32, Mat4, u64)> = Vec::new();
        let (mut small_ready, mut waiting) = (0, 0);
        for &index in &small {
            let light = &self.prepared[index];
            let Some(&(first, count)) = self.atlas_slots.get(&light.key) else {
                continue;
            };
            let (views, offset) = local_views(light, count, self.atlas.tile);
            let mut ready = true;
            for (face, matrix) in views.iter().enumerate() {
                let tile = first + face as u32;
                matrices[(atlas_base + tile) as usize] = *matrix;
                let content = signature(|hasher| {
                    (
                        light.key,
                        face,
                        scene.geometry_generation,
                        options.prop_shadows,
                        settled,
                        self.atlas.tile,
                    )
                        .hash(hasher);
                });
                if self.atlas.tiles[tile as usize].content == Some(content) {
                    continue;
                }
                if budget == 0 {
                    ready = false;
                    continue;
                }
                budget -= 1;
                pending.push((tile, *matrix, content));
            }
            if !ready {
                waiting += 1;
                continue;
            }
            small_ready += 1;
            gpu[index].shadow[0] = first as f32;
            gpu[index].shadow[1] = count as f32;
            gpu[index].extra[0] = offset;
            gpu[index].routing[1] = 1.0;
            if light.light.kind == LightKind::Projected {
                gpu[index].extra[2] = (atlas_base + first) as f32;
            }
        }
        let first_bake = gpu.len() as u32;
        let mut bake_tiles: Vec<(u32, Mat4, u64)> = Vec::new();
        let mut bake_work: Option<(bool, Vec<u32>, u32)> = None;
        if let Some(luxels) = self.luxels.as_mut().filter(|_| luxel_mode && !luxels_shown) {
            let start = luxels.bake.is_none_or(|(key, _)| key != bake_key);
            if start {
                luxels.bake = Some((bake_key, 0));
            }
            let mut next = luxels.bake.map_or(0, |(_, next)| next);
            let mut tile = room as u32;
            let mut batch = Vec::new();
            while next < bake_list.len() && batch.len() < BAKE_LIGHTS {
                let light = &self.prepared[bake_list[next]];
                let count = if light.casts { layers_for(light) } else { 0 };
                if count > 0 && tile + count > self.atlas.tiles.len() as u32 && !batch.is_empty() {
                    break;
                }
                let mut packed = pack(light, 1.0);
                if count > 0 && tile + count <= self.atlas.tiles.len() as u32 {
                    let (views, offset) = local_views(light, count, self.atlas.tile);
                    for (face, matrix) in views.iter().enumerate() {
                        let at = tile + face as u32;
                        matrices[(atlas_base + at) as usize] = *matrix;
                        bake_tiles.push((at, *matrix, 0));
                    }
                    packed.shadow[0] = tile as f32;
                    packed.shadow[1] = count as f32;
                    packed.extra[0] = offset;
                    packed.routing[1] = 1.0;
                    tile += count;
                }
                gpu.push(packed);
                batch.push(bake_list[next]);
                next += 1;
            }
            luxels.bake = Some((bake_key, next));
            luxels.stamp = luxels.stamp.wrapping_add(1).max(1);
            let mut rects = Vec::new();
            for &index in &batch {
                let light = &self.prepared[index];
                luxels.layout.reached(
                    light.light.origin.as_vec3(),
                    light.reach as f32,
                    (&mut luxels.seen, luxels.stamp),
                    &mut rects,
                );
            }
            bake_work = Some((start, rects, batch.len() as u32));
        }
        if !pending.is_empty() || !bake_tiles.is_empty() {
            let mut tiles = pending.clone();
            tiles.extend_from_slice(&bake_tiles);
            draw_tiles(
                renderer,
                encoder,
                (&self.tile_casters, &self.tile_clear),
                &self.atlas,
                &tiles,
                (casters, props),
            );
            for (tile, _, content) in &pending {
                self.atlas.tiles[*tile as usize].content = Some(*content);
            }
            for (tile, ..) in &bake_tiles {
                self.atlas.tiles[*tile as usize].content = None;
            }
            drawn += tiles.len();
        }
        if waiting > 0 {
            self.version += 1;
        }

        let suns: Vec<usize> = if options.sun_shadows && has_bounds {
            (0..self.everywhere)
                .filter(|&index| {
                    self.prepared[index].light.kind == LightKind::Directional && self.prepared[index].casts
                })
                .take(MAX_SUNS)
                .collect()
        } else {
            Vec::new()
        };
        let places: Vec<(Zone, Bounds)> = std::iter::once((Zone::Map, zones.map))
            .chain(zones.skybox.map(|skybox| (Zone::Skybox, skybox)))
            .collect();
        let sun_size = if suns.is_empty() { 1 } else { options.sun_map_size() };
        let sun_count = (suns.len() * places.len()).max(1) as u32;
        if self.suns.size != sun_size || self.suns.layers.len() as u32 != sun_count {
            self.suns = ShadowSet::new(
                device,
                &renderer.uniform_layout,
                sun_size,
                sun_count,
                "mallet sun shadows",
            );
            self.group = None;
        }
        let sun_base = matrices.len() as u32;
        matrices.resize(matrices.len() + self.suns.layers.len(), Mat4::IDENTITY);
        let mut sun_offsets = [0.0f32; 2];
        for (number, &index) in suns.iter().enumerate() {
            let light = &self.prepared[index];
            for (place, (zone, bounds)) in places.iter().enumerate() {
                let layer = number * places.len() + place;
                let (matrix, texel) = directional_view(light.light.direction.as_vec3(), bounds, sun_size);
                matrices[sun_base as usize + layer] = matrix;
                sun_offsets[place] = texel * 1.5;
                let content = signature(|hasher| {
                    (
                        light.key,
                        scene.geometry_generation,
                        options.prop_shadows,
                        settled,
                        sun_size,
                        *zone,
                    )
                        .hash(hasher);
                    for value in [bounds.min, bounds.max] {
                        value.to_array().map(f64::to_bits).hash(hasher);
                    }
                });
                if self.suns.layers[layer].content != Some(content) {
                    draw_layer(
                        renderer,
                        encoder,
                        &self.layer_casters,
                        &self.suns.layers[layer],
                        matrix,
                        (casters, props),
                        Blockers {
                            zone: Some(*zone),
                            sky: false,
                        },
                    );
                    self.suns.layers[layer].content = Some(content);
                    drawn += 1;
                }
            }
            gpu[index].shadow[0] = number as f32;
        }

        let ambient = scene.lighting.ambient.map(|ambient| ambient.color(options.hdr));
        let sky_count = if options.sky_light && ambient.is_some() && has_bounds {
            options.sky_sample_count()
        } else {
            0
        };
        let sky_size = if sky_count == 0 { 1 } else { SKY_SHADOW_SIZE };
        if self.sky.size != sky_size || self.sky.layers.len() as u32 != sky_count.max(1) {
            self.sky = ShadowSet::new(
                device,
                &renderer.uniform_layout,
                sky_size,
                sky_count,
                "mallet sky shadows",
            );
            self.group = None;
        }
        let sky_base = matrices.len() as u32;
        let directions = sky_directions(sky_count);
        matrices.resize(matrices.len() + self.sky.layers.len(), Mat4::IDENTITY);
        let mut sky_offset = 0.0;
        for (layer, toward) in directions.iter().enumerate() {
            let (matrix, texel) = directional_view(-*toward, &zones.map, sky_size);
            matrices[sky_base as usize + layer] = matrix;
            sky_offset = texel * 1.5;
            let content = signature(|hasher| {
                (
                    layer,
                    sky_count,
                    scene.geometry_generation,
                    options.prop_shadows,
                    settled,
                )
                    .hash(hasher);
                for value in [zones.map.min, zones.map.max] {
                    value.to_array().map(f64::to_bits).hash(hasher);
                }
            });
            if self.sky.layers[layer].content != Some(content) {
                draw_layer(
                    renderer,
                    encoder,
                    &self.layer_casters,
                    &self.sky.layers[layer],
                    matrix,
                    (casters, props),
                    Blockers {
                        zone: Some(Zone::Map),
                        sky: false,
                    },
                );
                self.sky.layers[layer].content = Some(content);
                drawn += 1;
            }
        }

        for (index, light) in self.prepared.iter().enumerate() {
            if light.light.kind == LightKind::Projected && gpu[index].shadow[0] < 0.0 {
                let (views, _) = local_views(light, 1, 256);
                gpu[index].extra[2] = matrices.len() as f32;
                matrices.push(views[0]);
            }
            if let Some(projection) = &light.light.projection {
                let texture = projection.texture.to_ascii_lowercase();
                gpu[index].shadow[2] = self
                    .cookie_names
                    .iter()
                    .position(|name| *name == texture)
                    .map_or(-1.0, |layer| layer as f32);
            }
        }

        let mut replaced = self.lights.write(device, queue, bytemuck::cast_slice(&gpu));
        let matrix_data: Vec<[f32; 16]> = matrices.iter().map(Mat4::to_cols_array).collect();
        replaced |= self.matrices.write(device, queue, bytemuck::cast_slice(&matrix_data));
        let samples: Vec<[f32; 4]> = directions.iter().map(|toward| toward.extend(0.0).to_array()).collect();
        replaced |= self.sky_samples.write(device, queue, bytemuck::cast_slice(&samples));
        if replaced {
            self.group = None;
        }
        let normalisation = 1.0 / directions.iter().map(|toward| toward.z).sum::<f32>().max(1e-3);
        let mut frame = self.frame_data(
            scene,
            camera,
            options,
            ambient,
            [sun_base, sky_base, sky_count],
            [sun_offsets[0], sky_offset, normalisation],
        );
        frame.atlas = [
            self.atlas.tile.max(1),
            self.atlas.per_row,
            self.atlas.per_layer(),
            atlas_base,
        ];
        if let Some(skybox) = zones.skybox {
            frame.skybox_min = (skybox.min - DVec3::ONE).as_vec3().extend(1.0).to_array();
            frame.skybox_max = (skybox.max + DVec3::ONE).as_vec3().extend(sun_offsets[1]).to_array();
        }
        let batch = bake_work.as_ref().map_or(0, |(_, _, count)| *count);
        frame.luxels = [first_bake, batch, u32::from(luxels_shown), 0];
        queue.write_buffer(&self.frame, 0, bytemuck::bytes_of(&frame));
        if self.group.is_none() {
            self.group = Some(self.bind_group(device));
        }
        if let Some((start, rects, _)) = bake_work {
            self.bake(renderer, encoder, start, &rects, bake_list.len());
        }
        self.stats = LightingStats {
            lights: self.prepared.len(),
            texture_lights: self
                .prepared
                .iter()
                .filter(|light| light.light.kind == LightKind::Surface)
                .count(),
            everywhere: self.everywhere,
            grid_cells: self.grid.cells.len(),
            busiest_cell: self.grid.busiest,
            shadowed: chosen.len() + suns.len(),
            small_shadowed: small_ready,
            waiting,
            shadow_layers: self.local.layers.len()
                + self.suns.layers.len()
                + self.sky.layers.len()
                + self.atlas.tiles.len(),
            shadow_maps_drawn: drawn,
            glows: 0,
            skybox: zones.skybox,
            luxels: self.luxels.as_ref().map_or(0, |luxels| luxels.layout.luxels),
            luxel_scale: self.luxels.as_ref().map_or(1.0, |luxels| luxels.layout.coarser),
            baking: self
                .luxels
                .as_ref()
                .and_then(|luxels| luxels.bake)
                .map(|(_, next)| next as f32 / bake_list.len().max(1) as f32),
            luxels_shown,
        };
        if self.stats.baking.is_some() {
            self.version += 1;
        }
    }

    /// Records this frame's part of a bake into the luxel texture faces do not show: sky
    /// light and sunlight first when the bake starts, then the batch of lights over the
    /// faces they reach. Once every light of the bake is in, faces show it from the next
    /// frame on; the bind group is made again at once, so this frame still draws.
    fn bake(
        &mut self,
        renderer: &Renderer3D,
        encoder: &mut wgpu::CommandEncoder,
        start: bool,
        rects: &[u32],
        total: usize,
    ) {
        let (Some(luxels), Some(group)) = (self.luxels.as_mut(), self.group.as_ref()) else {
            return;
        };
        let target = &luxels.lights[1 - luxels.front].1;
        let view_group = &self.local.layers[0].group;
        let texture_group = &renderer.texture_for(WHITE_KEY).bind_group;
        if start {
            let mut sky = luxel_pass(encoder, target, wgpu::LoadOp::Clear(wgpu::Color::BLACK));
            sky.set_pipeline(&self.bake_sky);
            sky.set_bind_group(0, view_group, &[]);
            sky.set_bind_group(1, texture_group, &[]);
            sky.set_bind_group(2, group, &[]);
            sky.draw(0..3, 0..1);
        }
        if !rects.is_empty() {
            let clips: Vec<[f32; 4]> = rects
                .iter()
                .map(|&index| luxels.layout.rects[index as usize].clip)
                .collect();
            let buffer = renderer.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mallet luxel rectangles"),
                contents: bytemuck::cast_slice(&clips),
                usage: wgpu::BufferUsages::VERTEX,
            });
            let mut lights = luxel_pass(encoder, target, wgpu::LoadOp::Load);
            lights.set_pipeline(&self.bake_lights);
            lights.set_bind_group(0, view_group, &[]);
            lights.set_bind_group(1, texture_group, &[]);
            lights.set_bind_group(2, group, &[]);
            lights.set_vertex_buffer(0, buffer.slice(..));
            lights.draw(0..4, 0..clips.len() as u32);
        }
        if let Some((key, next)) = luxels.bake {
            if next >= total {
                luxels.front = 1 - luxels.front;
                luxels.shown = Some(key);
                luxels.bake = None;
                self.group = Some(self.bind_group(&renderer.device));
                self.version += 1;
            }
        }
    }

    /// Lays the luxels out again on a worker thread when the faces or their textures
    /// changed, faces being lit per pixel until it is done, and takes in a finished layout;
    /// with `wait`, waits for it, as screenshots do. Lets the luxels go when faces are lit
    /// per pixel.
    fn update_luxels(&mut self, scene: &Scene, renderer: &Renderer3D, (settled, wait): (u64, bool), wanted: bool) {
        if !wanted {
            self.luxel_loading = None;
            if self.luxels.take().is_some() {
                self.group = None;
            }
            return;
        }
        let key = signature(|hasher| (scene.geometry_generation, settled).hash(hasher));
        if self.luxels.as_ref().is_some_and(|luxels| luxels.layout.key == key) {
            return;
        }
        if self.luxels.take().is_some() {
            self.group = None;
        }
        if self.luxel_loading.as_ref().is_none_or(|(loading, _)| *loading != key) {
            let (faces, total) = LuxelFace::gather(scene, renderer);
            let (sender, receiver) = mpsc::channel();
            let spawned = std::thread::Builder::new()
                .name("mallet luxel layout".to_owned())
                .spawn(move || {
                    let _ = sender.send(LuxelLayout::build(&faces, total, key));
                });
            self.luxel_loading = spawned.ok().map(|_| (key, receiver));
        }
        let Some((loading, receiver)) = self.luxel_loading.take() else {
            return;
        };
        let received = if wait {
            receiver.recv().map_err(|_| mpsc::TryRecvError::Disconnected)
        } else {
            receiver.try_recv()
        };
        match received {
            Ok(layout) => {
                self.luxels = Some(Luxels::new(&renderer.device, &renderer.queue, layout));
                self.group = None;
                self.version += 1;
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.luxel_loading = Some((loading, receiver));
                self.version += 1;
            }
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
    }

    /// The lights baked into luxels: all but suns, which are baked with the sky light,
    /// lights being dragged, projected textures, lights only for models and, while
    /// appearances play, lights with one. Those stay live, lit per pixel.
    fn bake_list(&self, options: &LightingOptions) -> Vec<usize> {
        self.prepared
            .iter()
            .enumerate()
            .filter(|(_, light)| {
                let source = &light.light;
                !light.everywhere
                    && source.kind != LightKind::Projected
                    && source.receivers != Receivers::Models
                    && !(options.animate && source.pattern.is_some())
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// What a bake of luxel lighting comes from: the layout, what casts shadows, the
    /// baked lights and suns, and the settings that change how they light.
    fn bake_key(&self, list: &[usize], options: &LightingOptions, settled: u64) -> u64 {
        signature(|hasher| {
            self.luxels.as_ref().map(|luxels| luxels.layout.key).hash(hasher);
            self.casters.as_ref().map(|casters| casters.key).hash(hasher);
            self.props
                .as_ref()
                .filter(|_| options.prop_shadows)
                .map(|props| (props.generation, props.models))
                .hash(hasher);
            let suns = self
                .prepared
                .iter()
                .take(self.everywhere)
                .filter(|light| light.light.kind == LightKind::Directional);
            for light in list.iter().map(|&index| &self.prepared[index]).chain(suns) {
                (light.key, light.casts).hash(hasher);
                light.color.to_array().map(f64::to_bits).hash(hasher);
            }
            (
                options.sky_light,
                options.sky_sample_count(),
                options.sun_shadows,
                options.light_shadows,
                options.prop_shadows,
                options.bounce_share().to_bits(),
                options.small_map_size(),
                options.hdr,
                settled,
            )
                .hash(hasher);
        })
    }

    /// Picks the lights that get shadow maps, the brightest around the camera and those in
    /// view first: the first of `budgets` get their own maps, and the next small ones in
    /// the atlas. Lights keep the maps they have unless another is clearly brighter.
    fn choose_shadowed(
        &self,
        camera: &Camera3D,
        planes: &[Vec4; 6],
        (budget, more): (usize, usize),
        max_layers: u32,
    ) -> (Vec<usize>, Vec<usize>) {
        if budget + more == 0 {
            return (Vec::new(), Vec::new());
        }
        let scored: Vec<(f64, usize)> = self
            .prepared
            .iter()
            .enumerate()
            .filter(|(_, light)| light.casts && light.light.kind != LightKind::Directional)
            .map(|(index, light)| {
                let distance = light.light.origin.distance(camera.position);
                let mut score = light.color.max_element() * light.light.falloff.at(distance.max(64.0));
                let reach = DVec3::splat(light.reach);
                let (low, high) = (light.light.origin - reach, light.light.origin + reach);
                if !box_visible(planes, low.as_vec3(), high.as_vec3()) {
                    score *= 0.1;
                }
                (score, index)
            })
            .collect();
        let ranked = |slots: &HashMap<u64, (u32, u32)>, skip: &HashSet<usize>| {
            let mut order: Vec<(f64, usize)> = scored
                .iter()
                .filter(|(_, index)| !skip.contains(index))
                .map(|&(score, index)| {
                    let kept = slots.contains_key(&self.prepared[index].key);
                    (if kept { score * KEEP_BONUS } else { score }, index)
                })
                .collect();
            order.sort_by(|a, b| b.0.total_cmp(&a.0));
            order
        };
        let mut chosen = Vec::new();
        let mut layers = 0;
        for (_, index) in ranked(&self.slots, &HashSet::new()) {
            if chosen.len() >= budget {
                break;
            }
            let count = layers_for(&self.prepared[index]);
            if layers + count > max_layers {
                continue;
            }
            layers += count;
            chosen.push(index);
        }
        let taken: HashSet<usize> = chosen.iter().copied().collect();
        let small = ranked(&self.atlas_slots, &taken)
            .into_iter()
            .take(more)
            .map(|(_, index)| index)
            .collect();
        (chosen, small)
    }

    /// The keys of lights and how many shadow maps each needs.
    fn runs(&self, lights: &[usize]) -> Vec<(u64, u32)> {
        lights
            .iter()
            .map(|&index| (self.prepared[index].key, layers_for(&self.prepared[index])))
            .collect()
    }

    /// Finds the 3D skybox, and how far the rest of the map reaches, when the geometry
    /// changed.
    fn update_zones(&mut self, scene: &Scene) {
        if self
            .zones
            .is_some_and(|(generation, _)| generation == scene.geometry_generation)
        {
            return;
        }
        let skybox = skybox_bounds(scene);
        let mut map = Bounds::EMPTY;
        for mesh in scene.meshes.iter().filter(|mesh| !mesh.bounds.is_empty()) {
            if zone_of(skybox.as_ref(), mesh.bounds.center()) == Zone::Map {
                map.add_bounds(&mesh.bounds);
            }
        }
        if map.is_empty() {
            map = scene.bounds;
        }
        self.zones = Some((scene.geometry_generation, Zones { skybox, map }));
    }

    /// Rebuilds the shadow caster buffers when the faces, their textures or the zones
    /// changed.
    fn update_casters(&mut self, renderer: &Renderer3D, scene: &Scene, settled: u64) {
        let skybox = self.zones.and_then(|(_, zones)| zones.skybox);
        let key = signature(|hasher| {
            (scene.geometry_generation, settled).hash(hasher);
            if let Some(skybox) = skybox {
                for value in [skybox.min, skybox.max] {
                    value.to_array().map(f64::to_bits).hash(hasher);
                }
            }
        });
        if self.casters.as_ref().is_some_and(|casters| casters.key == key) {
            return;
        }
        self.casters = Some(Casters::build(renderer, skybox.as_ref(), key));
    }

    /// Rebuilds the instance buffer of every prop that blocks light when the geometry
    /// changed or more models have loaded.
    fn update_props(&mut self, scene: &Scene, renderer: &Renderer3D, options: &LightingOptions) {
        let models = renderer.gpu_models.values().filter(|model| model.is_some()).count();
        if !options.prop_shadows
            || self
                .props
                .as_ref()
                .is_some_and(|props| props.generation == scene.geometry_generation && props.models == models)
        {
            return;
        }
        let skybox = self.zones.and_then(|(_, zones)| zones.skybox);
        let mut found: Vec<(PropCaster, ModelInstanceData, [i32; 3])> = Vec::new();
        for mesh in &scene.meshes {
            let Some(instance) = mesh.model.as_ref().filter(|instance| instance.casts_shadows) else {
                continue;
            };
            if renderer.gpu_models.get(&instance.key).is_none_or(Option::is_none) {
                continue;
            }
            let center = mesh.bounds.center();
            found.push((
                PropCaster {
                    key: instance.key.clone(),
                    min: mesh.bounds.min.as_vec3(),
                    max: mesh.bounds.max.as_vec3(),
                    zone: zone_of(skybox.as_ref(), center),
                },
                ModelInstanceData {
                    transform: instance.transform.as_mat4().to_cols_array(),
                    tint: [1.0, 1.0, 1.0, 0.0],
                },
                (center.as_vec3() / CASTER_CHUNK).floor().as_ivec3().to_array(),
            ));
        }
        found.sort_by(|a, b| a.0.key.cmp(&b.0.key).then(a.2.cmp(&b.2)));
        let data: Vec<ModelInstanceData> = found.iter().map(|(_, data, _)| *data).collect();
        let buffer = (!data.is_empty()).then(|| {
            renderer.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mallet shadow props"),
                contents: bytemuck::cast_slice(&data),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        self.props = Some(PropCasters {
            generation: scene.geometry_generation,
            models,
            buffer,
            props: found.into_iter().map(|(prop, ..)| prop).collect(),
        });
    }

    fn frame_data(
        &self,
        scene: &Scene,
        camera: &Camera3D,
        options: &LightingOptions,
        ambient: Option<DVec3>,
        [sun_base, sky_base, sky_count]: [u32; 3],
        [sun_offset, sky_offset, normalisation]: [f32; 3],
    ) -> GpuFrame {
        let lighting = &scene.lighting;
        let rgb = |color: DVec3| color.as_vec3().to_array();
        let four = |[r, g, b]: [f32; 3], w: f32| [r, g, b, w];
        let mut frame = GpuFrame {
            eye: four(rgb(camera.position), options.exposure_scale() as f32),
            fill: four([options.fill_light() as f32; 3], options.bounce_share() as f32),
            sky_light: four(rgb(ambient.filter(|_| options.sky_light).unwrap_or(DVec3::ZERO)), 0.0),
            sun_glow: [0.0, 0.0, 1.0, 2.0],
            grid_origin: four(self.grid.origin.to_array(), self.grid.cell),
            grid_size: [
                self.grid.size[0],
                self.grid.size[1],
                self.grid.size[2],
                self.everywhere as u32,
            ],
            options: [
                u32::from(options.tonemap == Tonemap::Filmic)
                    | (u32::from(options.self_illum) << 1)
                    | (u32::from(options.texture_lights) << 2),
                sky_count,
                sun_base,
                sky_base,
            ],
            shadow: [
                options.softness() as f32,
                options.softness() as f32,
                1.0 / self.local.size as f32,
                1.0 / self.suns.size as f32,
            ],
            sky_shadow: [1.0 / self.sky.size as f32, normalisation, sun_offset, sky_offset],
            ..GpuFrame::default()
        };
        if let Some(fog) = lighting.fog.filter(|_| options.fog) {
            frame.fog_color = four(rgb(fog.color), fog.max_density as f32);
            if let Some((second, direction)) = fog.blend {
                frame.fog_color2 = four(rgb(second), 1.0);
                frame.fog_dir = four(rgb(direction), 0.0);
            }
            frame.fog_range = [fog.start as f32, fog.end as f32, 0.0, 0.0];
        }
        let flat = ambient
            .filter(|ambient| ambient.max_element() > 0.0)
            .map_or(DVec3::new(0.35, 0.5, 0.75), |ambient| {
                ambient / ambient.max_element() * 0.7
            });
        let painted = lighting.sky.name.eq_ignore_ascii_case("painted") && lighting.sky.paint.is_some();
        let skybox_ready = self.sky_ready
            && self
                .sky_name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(lighting.sky.name.trim()));
        let mode = match options.sky {
            SkyMode::Black => 0.0,
            SkyMode::Color => 1.0,
            SkyMode::Skybox if painted => 3.0,
            SkyMode::Skybox if skybox_ready => 2.0,
            SkyMode::Skybox => 1.0,
        };
        frame.sky_color = four(rgb(flat), mode);
        if let Some(paint) = &lighting.sky.paint {
            frame.sky_top = four(rgb(paint.top), paint.fade_bias as f32);
            frame.sky_bottom = four(rgb(paint.bottom), paint.dusk_scale as f32);
            frame.sky_dusk = four(rgb(paint.dusk_color), paint.dusk_intensity as f32);
        }
        let glow = lighting.sky.sun.or_else(|| {
            lighting
                .sky
                .paint
                .filter(|_| painted)
                .map(|paint| (paint.sun_normal, paint.sun_color * 4.0, paint.sun_size.max(0.5)))
        });
        if let (Some((toward, color, size)), true) = (glow, options.glows && options.sky != SkyMode::Black) {
            let toward = toward.normalize_or(DVec3::Z);
            frame.sun_glow = four(rgb(toward), size.clamp(0.1, 30.0).to_radians().cos() as f32);
            frame.sun_glow_color = four(rgb(color), 0.0);
        }
        frame
    }

    fn bind_group(&self, device: &wgpu::Device) -> wgpu::BindGroup {
        let buffer = buffer_entry;
        let view = view_entry;
        let (luxel_light, luxel_faces, luxel_points) = match &self.luxels {
            Some(luxels) => (&luxels.lights[luxels.front].1, &luxels.faces.buffer, &luxels.points),
            None => (&self.blank_light.1, &self.blank_faces.buffer, &self.blank_points.1),
        };
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mallet lighting"),
            layout: &self.layout,
            entries: &[
                buffer(0, &self.frame),
                buffer(1, &self.lights.buffer),
                buffer(2, &self.cells.buffer),
                buffer(3, &self.indices.buffer),
                buffer(4, &self.matrices.buffer),
                view(5, &self.local.view),
                view(6, &self.suns.view),
                view(7, &self.sky.view),
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: wgpu::BindingResource::Sampler(&self.shadow_sampler),
                },
                view(9, &self.cookies.view),
                view(10, &self.skybox.view),
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::Sampler(&self.clamp_sampler),
                },
                buffer(12, &self.sky_samples.buffer),
                view(13, &self.atlas.view),
                view(14, luxel_light),
                buffer(15, luxel_faces),
                view(16, luxel_points),
            ],
        })
    }

    /// The lines that show lights' reach, cones and directions, in each light's colour:
    /// for selected lights or all of them, as the settings say.
    pub(super) fn helper_lines(&self, scene: &Scene, options: &LightingOptions) -> Vec<([DVec3; 2], Color32)> {
        if options.helpers == HelperMode::Off {
            return Vec::new();
        }
        let selected: HashSet<u32> = scene
            .meshes
            .iter()
            .filter(|mesh| mesh.selected)
            .filter_map(|mesh| match mesh.owner {
                Some(Owner::Entity(id)) => Some(id),
                _ => None,
            })
            .collect();
        let mut lines = Vec::new();
        for light in &self.prepared {
            let Some(entity) = light.light.entity else {
                continue;
            };
            if options.helpers == HelperMode::Selected && !selected.contains(&entity) {
                continue;
            }
            let color = helper_color(light.color);
            for edge in helper_edges(light) {
                lines.push((edge, color));
            }
        }
        lines
    }

    /// Glows, the halos and beams of spotlights, as camera-facing quads.
    pub(super) fn glow_vertices(&mut self, scene: &Scene, camera: &Camera3D, options: &LightingOptions) -> Vec<u8> {
        if !options.glows {
            self.stats.glows = 0;
            return Vec::new();
        }
        let forward = camera.forward();
        let right = camera.right();
        let up = right.cross(forward);
        let mut vertices: Vec<GlowVertex> = Vec::new();
        let mut quad = |corners: [DVec3; 4], colors: [[f32; 4]; 2], shapes: [[f32; 3]; 4]| {
            for corner in [0, 1, 2, 0, 2, 3] {
                vertices.push(GlowVertex {
                    position: corners[corner].as_vec3().to_array(),
                    color: colors[usize::from(corner >= 2)],
                    shape: shapes[corner],
                });
            }
        };
        let mut count = 0;
        for glow in &scene.lighting.glows {
            let distance = glow.origin.distance(camera.position);
            let strength = glow.fade.map_or(1.0, |(near, far)| {
                ((distance - near) / (far - near).max(1.0)).clamp(0.0, 1.0)
            });
            if strength <= 0.0 {
                continue;
            }
            let (r, u) = (right * glow.radius, up * glow.radius);
            let color = glow.color.as_vec3().extend(strength as f32).to_array();
            quad(
                [
                    glow.origin - r - u,
                    glow.origin + r - u,
                    glow.origin + r + u,
                    glow.origin - r + u,
                ],
                [color, color],
                [[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0]],
            );
            count += 1;
        }
        for beam in &scene.lighting.beams {
            if beam.starts_dark && !options.dark_lights {
                continue;
            }
            let color = if options.hdr { beam.hdr } else { beam.ldr };
            let end = beam.origin + beam.direction * beam.length;
            let toward = (camera.position - beam.origin).normalize_or(DVec3::Z);
            let side = beam
                .direction
                .cross(toward)
                .try_normalize()
                .unwrap_or_else(|| beam.direction.any_orthonormal_vector());
            let (near, far) = (side * beam.width * 0.25, side * beam.width);
            let bright = color.as_vec3().extend(0.6).to_array();
            quad(
                [beam.origin - near, beam.origin + near, end + far, end - far],
                [bright, bright],
                [[-1.0, 0.0, 1.0], [1.0, 0.0, 1.0], [1.0, 1.0, 1.0], [-1.0, 1.0, 1.0]],
            );
            let (r, u) = (right * beam.width * 0.6, up * beam.width * 0.6);
            let halo = color.as_vec3().extend(0.8).to_array();
            quad(
                [
                    beam.origin - r - u,
                    beam.origin + r - u,
                    beam.origin + r + u,
                    beam.origin - r + u,
                ],
                [halo, halo],
                [[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0]],
            );
            count += 1;
        }
        self.stats.glows = count;
        bytemuck::cast_slice(&vertices).to_vec()
    }

    /// Draws the lit map into the camera's pass: faces, props, the selection's tint,
    /// translucent faces and props, then glows.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw<'a>(
        &'a self,
        renderer: &'a Renderer3D,
        pass: &mut wgpu::RenderPass<'a>,
        planes: &[Vec4; 6],
        options: &LightingOptions,
        tinted: &[&'a FaceMesh],
        translucent: Option<&'a Translucent>,
        instances: Option<&'a Batches>,
        glows: Option<&'a (wgpu::Buffer, u32)>,
        stats: &mut RenderStats,
    ) {
        let Some(group) = &self.group else {
            return;
        };
        pass.set_bind_group(2, group, &[]);
        let hidden = |key: &str| {
            (key == WHITE_KEY && !options.entity_boxes)
                || (options.hide_tools && matches!(renderer.texture_for(key).surface, Surface::Tool | Surface::Nodraw))
        };
        if let Some(faces) = &renderer.faces {
            pass.set_pipeline(&self.face);
            stats.chunks = faces.opaque.chunks.len();
            for (mesh, skipped) in renderer.opaque_faces() {
                for chunk in mesh.chunks.iter().filter(|chunk| {
                    skipped.is_none_or(|cells| !cells.contains(&chunk.cell))
                        && box_visible(planes, chunk.min, chunk.max)
                }) {
                    pass.set_vertex_buffer(0, mesh.buffer_of(chunk).slice(..));
                    stats.chunks_drawn += 1;
                    stats.triangles_drawn += chunk.triangles;
                    for (key, range) in &chunk.batches {
                        if hidden(key) {
                            continue;
                        }
                        pass.set_bind_group(1, &renderer.texture_for(key).bind_group, &[]);
                        pass.draw(range.clone(), 0..1);
                    }
                }
            }
        }
        if let Some((buffer, ranges)) = instances {
            self.draw_models(renderer, pass, buffer, ranges, &hidden, stats);
        }
        for mesh in tinted {
            pass.set_pipeline(&self.overdraw);
            pass.set_vertex_buffer(0, mesh.buffer.slice(..));
            for (key, range) in mesh.chunks.iter().flat_map(|chunk| &chunk.batches) {
                if hidden(key) {
                    continue;
                }
                pass.set_bind_group(1, &renderer.texture_for(key).bind_group, &[]);
                pass.draw(range.clone(), 0..1);
            }
        }
        if let Some(translucent) = translucent {
            renderer.draw_translucent(
                pass,
                translucent,
                instances.map(|(buffer, _)| buffer),
                (&self.translucent, &self.translucent_model),
                &hidden,
                stats,
            );
        }
        if let Some((buffer, count)) = glows {
            pass.set_pipeline(&self.glow);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..*count, 0..1);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_models<'a>(
        &'a self,
        renderer: &'a Renderer3D,
        pass: &mut wgpu::RenderPass<'a>,
        instances: &'a wgpu::Buffer,
        ranges: &[(String, Range<u32>)],
        hidden: &dyn Fn(&str) -> bool,
        stats: &mut RenderStats,
    ) {
        pass.set_pipeline(&self.model);
        pass.set_vertex_buffer(1, instances.slice(..));
        for (key, instance_range) in ranges {
            let Some(Some(model)) = renderer.gpu_models.get(key) else {
                continue;
            };
            pass.set_vertex_buffer(0, model.vertices.slice(..));
            pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
            for (material, range) in &model.meshes {
                let texture = renderer.texture_for(material);
                if texture.alpha.see_through() || hidden(material) {
                    continue;
                }
                pass.set_bind_group(1, &texture.bind_group, &[]);
                pass.draw_indexed(range.clone(), 0, instance_range.clone());
            }
            stats.triangles_drawn += model.triangles * instance_range.len();
        }
    }
}

fn buffer_entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

fn view_entry(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
    }
}

/// The most shadow map layers this graphics card allows for lights.
fn max_layers(device: &wgpu::Device) -> u32 {
    device.limits().max_texture_array_layers.clamp(6, 256)
}

/// Scales an image to a square of `side` pixels, nearest pixel. With `upper_half`, the
/// image fills the upper half and its bottom row is repeated below, as half-height
/// skybox sides are drawn.
fn square(image: &mallet_content::vtf::Image, side: u32, upper_half: bool) -> Vec<u8> {
    let (width, height) = (image.pixel_width.max(1), image.pixel_height.max(1));
    let mut pixels = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        let source_y = if upper_half {
            ((y * 2 * height) / side).min(height - 1)
        } else {
            (y * height) / side
        };
        for x in 0..side {
            let source_x = (x * width) / side;
            let at = ((source_y * width + source_x) * 4) as usize;
            pixels.extend_from_slice(image.rgba.get(at..at + 4).unwrap_or(&[255, 255, 255, 255]));
        }
    }
    pixels
}

/// The entities being dragged, whose lights move with the drag.
fn moving_entities(scene: &Scene, overlay: &Overlay) -> HashSet<u32> {
    if overlay.transform.is_none() {
        return HashSet::new();
    }
    scene
        .meshes
        .iter()
        .filter(|mesh| mesh.selected)
        .filter_map(|mesh| match mesh.owner {
            Some(Owner::Entity(id)) => Some(id),
            _ => None,
        })
        .collect()
}

/// The lights of the preview, suns and dragged lights first: the map's lights the
/// settings let through, moved with a drag, then texture light patches. Appearances are
/// played when the lights are packed for the shader, so the order and the grid stay put.
fn gather(
    scene: &Scene,
    overlay: &Overlay,
    moving: &HashSet<u32>,
    surface: &[Light],
    options: &LightingOptions,
) -> Vec<PreviewLight> {
    let cutoff = options.light_cutoff();
    let mut lights = Vec::new();
    for light in &scene.lighting.lights {
        let directional = light.kind == LightKind::Directional;
        if (directional && !options.sun)
            || (light.dynamic && !options.dynamic_lights)
            || (light.starts_dark && !options.dark_lights)
        {
            continue;
        }
        let color = light.color(options.hdr);
        if color.max_element() <= 0.0 {
            continue;
        }
        let mut light = light.clone();
        let dragged = light.entity.is_some_and(|id| moving.contains(&id));
        if let (true, Some(remap)) = (dragged, overlay.transform) {
            light.origin = remap.apply(light.origin);
            light.direction = remap
                .matrix
                .transform_vector3(light.direction)
                .normalize_or(light.direction);
        }
        let reach = if directional {
            MAX_REACH
        } else {
            light.falloff.reach(color.max_element(), cutoff)
        };
        if reach <= 1.0 {
            continue;
        }
        let casts = light.shadows
            && if directional {
                options.sun_shadows
            } else {
                options.light_shadows
            };
        lights.push(PreviewLight {
            key: light_key(&light, reach),
            light,
            color,
            reach,
            everywhere: directional || dragged,
            casts,
        });
    }
    for light in surface {
        let color = light.color(options.hdr);
        let reach = light.falloff.reach(color.max_element(), cutoff);
        if reach <= 1.0 {
            continue;
        }
        lights.push(PreviewLight {
            key: light_key(light, reach),
            light: light.clone(),
            color,
            reach,
            everywhere: false,
            casts: options.light_shadows,
        });
    }
    lights.sort_by_key(|light| !light.everywhere);
    lights
}

/// Identifies a light's placement and shape for keeping its shadow maps.
fn light_key(light: &Light, reach: f64) -> u64 {
    signature(|hasher| {
        light.kind.hash(hasher);
        for value in [light.origin, light.direction] {
            value.to_array().map(f64::to_bits).hash(hasher);
        }
        reach.to_bits().hash(hasher);
        if let Some(cone) = light.cone {
            [cone.inner, cone.outer].map(f64::to_bits).hash(hasher);
        }
        if let Some(projection) = &light.projection {
            [projection.fov, projection.near].map(f64::to_bits).hash(hasher);
        }
    })
}

/// VRAD's texture lights for every face whose material `lights.rad` lists: the face's
/// light, times the times its texture repeats across it, split over patches about 128
/// units apart that shine out of the face.
fn surface_lights(scene: &Scene, renderer: &Renderer3D, table: &TextureLights) -> Vec<Light> {
    if table.is_empty() {
        return Vec::new();
    }
    let mut lights = Vec::new();
    for face in scene.meshes.iter().flat_map(|mesh| &mesh.faces) {
        let Some(material) = face.material.as_ref().filter(|_| !face.decal) else {
            continue;
        };
        let key = material_key(material);
        let Some(emission) = table.get(&key) else {
            continue;
        };
        let Some(Some(texture)) = renderer.textures.get(&key) else {
            continue;
        };
        let tile =
            f64::from(texture.width) * face.uaxis.scale.abs() * f64::from(texture.height) * face.vaxis.scale.abs();
        let strength = texture_light_scale(polygon_area(&face.vertices, face.normal), tile);
        if strength <= 0.0 {
            continue;
        }
        let points = patch_points(&face.vertices, face.normal);
        let share = strength / points.len() as f64;
        for point in points {
            lights.push(Light {
                entity: None,
                classname: "texture light".to_owned(),
                kind: LightKind::Surface,
                origin: point + face.normal,
                direction: face.normal,
                ldr: emission.ldr * share,
                hdr: emission.hdr * share,
                falloff: QUADRATIC,
                cone: None,
                pattern: None,
                starts_dark: false,
                shadows: true,
                dynamic: false,
                projection: None,
                spread: 0.0,
                receivers: Receivers::All,
            });
        }
    }
    lights
}

/// The area of a convex polygon.
fn polygon_area(points: &[DVec3], normal: DVec3) -> f64 {
    let Some(first) = points.first() else {
        return 0.0;
    };
    let twice: f64 = points
        .windows(2)
        .skip(1)
        .map(|pair| (pair[0] - *first).cross(pair[1] - *first).dot(normal))
        .sum();
    twice.abs() * 0.5
}

/// Points spread over a convex face about [`PATCH_SPACING`] apart, or its centre for a
/// small face.
fn patch_points(points: &[DVec3], normal: DVec3) -> Vec<DVec3> {
    let center = points.iter().sum::<DVec3>() / points.len().max(1) as f64;
    if points.len() < 3 {
        return vec![center];
    }
    let u = (points[1] - points[0]).normalize_or(normal.any_orthonormal_vector());
    let v = normal.cross(u);
    let flat: Vec<(f64, f64)> = points
        .iter()
        .map(|point| ((*point - center).dot(u), (*point - center).dot(v)))
        .collect();
    let (mut low, mut high) = ((f64::INFINITY, f64::INFINITY), (f64::NEG_INFINITY, f64::NEG_INFINITY));
    for (x, y) in &flat {
        low = (low.0.min(*x), low.1.min(*y));
        high = (high.0.max(*x), high.1.max(*y));
    }
    let columns = ((high.0 - low.0) / PATCH_SPACING).ceil().clamp(1.0, MAX_PATCH_SIDE);
    let rows = ((high.1 - low.1) / PATCH_SPACING).ceil().clamp(1.0, MAX_PATCH_SIDE);
    if columns * rows <= 1.0 {
        return vec![center];
    }
    let inside = |x: f64, y: f64| {
        let mut sign = 0.0f64;
        for (index, a) in flat.iter().enumerate() {
            let b = flat[(index + 1) % flat.len()];
            let cross = (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0);
            if cross.abs() < 1e-6 {
                continue;
            }
            if sign == 0.0 {
                sign = cross.signum();
            } else if cross.signum() != sign {
                return false;
            }
        }
        true
    };
    let mut patches = Vec::new();
    for column in 0..columns as usize {
        for row in 0..rows as usize {
            let x = low.0 + (column as f64 + 0.5) * (high.0 - low.0) / columns;
            let y = low.1 + (row as f64 + 0.5) * (high.1 - low.1) / rows;
            if inside(x, y) {
                patches.push(center + u * x + v * y);
            }
        }
    }
    if patches.is_empty() {
        vec![center]
    } else {
        patches
    }
}

/// Sorts the lights from `first` on into world cells: a cell lists every light that
/// reaches its nearest corner brighter than `threshold`, the brightest `cap` of them.
fn build_grid(bounds: &Bounds, lights: &[PreviewLight], first: usize, threshold: f64, cap: usize) -> Grid {
    let (low, high) = if bounds.is_empty() {
        (DVec3::splat(-64.0), DVec3::splat(64.0))
    } else {
        (bounds.min - DVec3::splat(8.0), bounds.max + DVec3::splat(8.0))
    };
    let extent = (high - low).max(DVec3::ONE);
    let cell = (extent.max_element() / MAX_GRID_SIDE).max(MIN_GRID_CELL);
    let size = (extent / cell).ceil().clamp(DVec3::ONE, DVec3::splat(MAX_GRID_SIDE));
    let [sx, sy, sz] = size.to_array().map(|side| side as usize);
    let mut lists: Vec<Vec<(f32, u32)>> = vec![Vec::new(); sx * sy * sz];
    for (index, light) in lights.iter().enumerate().skip(first) {
        let origin = light.light.origin;
        let reach = light.reach;
        if (origin + reach).cmplt(low).any() || (origin - reach).cmpgt(high).any() {
            continue;
        }
        let brightest = light.color.max_element();
        let cell_of = |point: DVec3| ((point - low) / cell).floor().clamp(DVec3::ZERO, size - DVec3::ONE);
        let (from, to) = (cell_of(origin - reach), cell_of(origin + reach));
        for z in from.z as usize..=to.z as usize {
            for y in from.y as usize..=to.y as usize {
                for x in from.x as usize..=to.x as usize {
                    let corner = low + DVec3::new(x as f64, y as f64, z as f64) * cell;
                    let nearest = origin.clamp(corner, corner + DVec3::splat(cell));
                    let distance = origin.distance(nearest);
                    if distance > reach {
                        continue;
                    }
                    let weight = brightest * light.light.falloff.at(distance);
                    if weight < threshold {
                        continue;
                    }
                    lists[x + sx * (y + sy * z)].push((weight as f32, index as u32));
                }
            }
        }
    }
    let mut grid = Grid {
        origin: low.as_vec3(),
        cell: cell as f32,
        size: [sx as u32, sy as u32, sz as u32],
        ..Grid::default()
    };
    for mut list in lists {
        if list.len() > cap {
            list.sort_by(|a, b| b.0.total_cmp(&a.0));
            list.truncate(cap);
        }
        grid.busiest = grid.busiest.max(list.len());
        grid.cells.push([grid.indices.len() as u32, list.len() as u32]);
        grid.indices.extend(list.into_iter().map(|(_, index)| index));
    }
    grid
}

/// How many shadow map layers a light needs: one for a projected texture, a narrow
/// spotlight or a texture light, six for everything else.
fn layers_for(light: &PreviewLight) -> u32 {
    match light.light.kind {
        LightKind::Projected | LightKind::Surface => 1,
        LightKind::Spot
            if light
                .light
                .cone
                .is_some_and(|cone| cone.outer_degrees() <= SINGLE_MAP_CONE) =>
        {
            1
        }
        _ => 6,
    }
}

/// The camera matrices of a light's shadow maps, and how far to push surfaces out along
/// their normal, per unit of distance from the light, to keep them from shadowing
/// themselves.
fn local_views(light: &PreviewLight, count: u32, size: u32) -> (Vec<Mat4>, f32) {
    let origin = light.light.origin.as_vec3();
    let far = (light.reach as f32).clamp(16.0, MAX_REACH as f32);
    let near = light
        .light
        .projection
        .as_ref()
        .map_or(2.0, |projection| projection.near as f32)
        .clamp(1.0, far * 0.5);
    if count == 6 {
        let projection = Mat4::perspective_rh(FRAC_PI_2, 1.0, near, far);
        let views = CUBE_FACES
            .iter()
            .map(|(direction, up)| projection * Mat4::look_to_rh(origin, *direction, *up))
            .collect();
        return (views, 3.0 / size as f32);
    }
    let fov = match light.light.kind {
        LightKind::Projected => light
            .light
            .projection
            .as_ref()
            .map_or(90.0, |projection| projection.fov),
        LightKind::Surface => 150.0,
        _ => light.light.cone.map_or(90.0, |cone| cone.outer_degrees() * 2.0 + 10.0),
    }
    .clamp(5.0, 160.0) as f32;
    let direction = light.light.direction.as_vec3().normalize_or(Vec3::NEG_Z);
    let up = if direction.z.abs() > 0.99 { Vec3::Y } else { Vec3::Z };
    let matrix = Mat4::perspective_rh(fov.to_radians(), 1.0, near, far) * Mat4::look_to_rh(origin, direction, up);
    (vec![matrix], 3.0 * (fov.to_radians() * 0.5).tan() / size as f32)
}

/// An orthographic shadow map looking along `direction` that covers the whole map, and
/// the size of one of its pixels in units.
fn directional_view(direction: Vec3, bounds: &Bounds, size: u32) -> (Mat4, f32) {
    let center = bounds.center().as_vec3();
    let radius = (bounds.size().length() * 0.5) as f32 + 64.0;
    let direction = direction.normalize_or(Vec3::NEG_Z);
    let up = if direction.z.abs() > 0.99 { Vec3::Y } else { Vec3::Z };
    let view = Mat4::look_to_rh(center - direction * radius * 2.0, direction, up);
    let mut low = Vec3::splat(f32::INFINITY);
    let mut high = Vec3::splat(f32::NEG_INFINITY);
    for corner in 0..8 {
        let pick = |bit: usize, axis: usize| {
            if corner & bit == 0 {
                bounds.min[axis]
            } else {
                bounds.max[axis]
            }
        };
        let point = Vec3::new(pick(1, 0) as f32, pick(2, 1) as f32, pick(4, 2) as f32);
        let seen = view.transform_point3(point);
        low = low.min(seen);
        high = high.max(seen);
    }
    let projection = Mat4::orthographic_rh(low.x, high.x, low.y, high.y, -high.z - 16.0, -low.z + 16.0);
    let texel = (high.x - low.x).max(high.y - low.y) / size as f32;
    (projection * view, texel)
}

/// Directions sky light is traced from, spread evenly over the sky, from just above the
/// horizon to straight up.
fn sky_directions(count: u32) -> Vec<Vec3> {
    (0..count)
        .map(|index| {
            let z = 1.0 - (index as f32 + 0.5) / count as f32 * 0.88;
            let across = (1.0 - z * z).max(0.0).sqrt();
            let angle = index as f32 * 2.399_963;
            Vec3::new(across * angle.cos(), across * angle.sin(), z)
        })
        .collect()
}

/// A light as the shader reads it, at `brightness` times its colour, without shadows yet.
fn pack(light: &PreviewLight, brightness: f64) -> GpuLight {
    let source = &light.light;
    let kind = match source.kind {
        LightKind::Point => 0.0,
        LightKind::Spot => 1.0,
        LightKind::Directional => 2.0,
        LightKind::Projected => 3.0,
        LightKind::Surface => 4.0,
    };
    let (outer, inner, focus) = match (source.kind, source.cone) {
        (LightKind::Spot, Some(cone)) => (cone.outer as f32, cone.inner as f32, cone.exponent as f32),
        _ => (-2.0, -1.5, 1.0),
    };
    let (falloff, fade) = match source.falloff {
        Falloff::Inverse {
            constant,
            linear,
            quadratic,
            cap,
            fade_start,
            fade_end,
            max_distance,
        } => (
            [
                constant as f32,
                linear as f32,
                quadratic as f32,
                (cap.min(f64::from(NO_CAP))) as f32,
            ],
            [fade_start as f32, fade_end as f32, max_distance as f32, 0.0],
        ),
        Falloff::Projected { far } => ([far as f32, 0.0, 0.0, 0.0], [0.0, -1.0, 0.0, 1.0]),
        Falloff::Radius { radius } => ([radius as f32, 0.0, 0.0, 0.0], [0.0, -1.0, 0.0, 2.0]),
        Falloff::None => ([0.0; 4], [0.0, -1.0, 0.0, 3.0]),
    };
    let origin = source.origin.as_vec3();
    let direction = source.direction.as_vec3().normalize_or(Vec3::NEG_Z);
    let color = (light.color * brightness).as_vec3();
    let receivers = match source.receivers {
        Receivers::All => 0.0,
        Receivers::World => 1.0,
        Receivers::Models => 2.0,
    };
    let bounces = source.kind != LightKind::Directional && !source.dynamic;
    GpuLight {
        position: origin.extend(kind).to_array(),
        direction: direction.extend(outer).to_array(),
        color: color.extend(inner).to_array(),
        falloff,
        fade,
        shadow: [-1.0, 0.0, -1.0, focus],
        extra: [0.0, if bounces { 1.0 } else { 0.0 }, -1.0, 0.0],
        routing: [receivers, 0.0, 0.0, 0.0],
    }
}

/// A light's colour for its helper lines: its colour at full brightness.
fn helper_color(color: DVec3) -> Color32 {
    let brightest = color.max_element().max(1e-6);
    let [r, g, b] = (color / brightest)
        .to_array()
        .map(|channel| channel.clamp(0.0, 1.0) as f32);
    let linear = egui::Rgba::from_rgb(r.max(0.15), g.max(0.15), b.max(0.15));
    Color32::from(linear)
}

/// Lines showing a light: a sphere where it reaches, its cones, its frustum or, for a
/// sun, an arrow along its light.
fn helper_edges(light: &PreviewLight) -> Vec<[DVec3; 2]> {
    let source = &light.light;
    let origin = source.origin;
    let direction = source.direction.normalize_or(DVec3::NEG_Z);
    let mut edges = Vec::new();
    match source.kind {
        LightKind::Directional => {
            let tip = origin + direction * 128.0;
            edges.push([origin, tip]);
            let side = direction.any_orthonormal_vector();
            let other = direction.cross(side);
            for spoke in [side, -side, other, -other] {
                edges.push([tip, tip - direction * 24.0 + spoke * 12.0]);
            }
        }
        LightKind::Spot | LightKind::Projected => {
            let length = light.reach.min(512.0);
            let angles: Vec<f64> = match (source.kind, source.cone, &source.projection) {
                (LightKind::Projected, _, Some(projection)) => vec![projection.fov * 0.5],
                (_, Some(cone), _) => vec![cone.inner_degrees(), cone.outer_degrees()],
                _ => Vec::new(),
            };
            let side = direction.any_orthonormal_vector();
            let other = direction.cross(side);
            for angle in angles {
                let radius = angle.to_radians().tan().min(8.0) * length;
                let center = origin + direction * length;
                let ring: Vec<DVec3> = (0..24)
                    .map(|step| {
                        let turn = f64::from(step) / 24.0 * std::f64::consts::TAU;
                        center + (side * turn.cos() + other * turn.sin()) * radius
                    })
                    .collect();
                for (index, point) in ring.iter().enumerate() {
                    edges.push([*point, ring[(index + 1) % ring.len()]]);
                    if index % 6 == 0 {
                        edges.push([origin, *point]);
                    }
                }
            }
        }
        LightKind::Point | LightKind::Surface => {
            let radius = light.reach.min(MAX_REACH);
            for (a, b) in [(DVec3::X, DVec3::Y), (DVec3::X, DVec3::Z), (DVec3::Y, DVec3::Z)] {
                let ring: Vec<DVec3> = (0..32)
                    .map(|step| {
                        let turn = f64::from(step) / 32.0 * std::f64::consts::TAU;
                        origin + (a * turn.cos() + b * turn.sin()) * radius
                    })
                    .collect();
                for (index, point) in ring.iter().enumerate() {
                    edges.push([*point, ring[(index + 1) % ring.len()]]);
                }
            }
        }
    }
    edges
}

/// Draws everything that blocks light into a shadow map pass whose camera is bound:
/// opaque faces a chunk at a time, alpha-tested faces with their textures, then props,
/// all culled to `planes`. Tool faces, translucent faces and entity boxes let light
/// through; nodraw faces block it, and sky faces when `blockers` says so.
fn draw_casters(
    pass: &mut wgpu::RenderPass<'_>,
    renderer: &Renderer3D,
    pipelines: &CasterPipelines,
    (casters, props): (Option<&Casters>, Option<&PropCasters>),
    planes: &[Vec4; 6],
    blockers: Blockers,
) {
    let wanted = |of: Zone| blockers.zone.is_none_or(|zone| zone == of);
    if let Some(casters) = casters {
        let visible: Vec<&CasterChunk> = casters
            .chunks
            .iter()
            .filter(|chunk| wanted(chunk.zone) && box_visible(planes, chunk.min, chunk.max))
            .collect();
        pass.set_pipeline(&pipelines.solid);
        pass.set_vertex_buffer(0, casters.positions.slice(..));
        for chunk in &visible {
            let end = if blockers.sky { chunk.sky.end } else { chunk.solid.end };
            if end > chunk.solid.start {
                pass.draw(chunk.solid.start..end, 0..1);
            }
        }
        if visible.iter().any(|chunk| !chunk.tested.is_empty()) {
            pass.set_pipeline(&pipelines.tested);
            pass.set_vertex_buffer(0, casters.tested.slice(..));
            for (key, range) in visible.iter().flat_map(|chunk| &chunk.tested) {
                pass.set_bind_group(1, &renderer.texture_for(key).bind_group, &[]);
                pass.draw(range.clone(), 0..1);
            }
        }
    }
    let Some(PropCasters {
        buffer: Some(buffer),
        props,
        ..
    }) = props
    else {
        return;
    };
    let seen = |prop: &PropCaster| wanted(prop.zone) && box_visible(planes, prop.min, prop.max);
    pass.set_pipeline(&pipelines.model);
    pass.set_vertex_buffer(1, buffer.slice(..));
    let mut first = 0;
    while first < props.len() {
        if !seen(&props[first]) {
            first += 1;
            continue;
        }
        let key = &props[first].key;
        let mut end = first + 1;
        while end < props.len() && props[end].key == *key && seen(&props[end]) {
            end += 1;
        }
        if let Some(Some(model)) = renderer.gpu_models.get(key) {
            pass.set_vertex_buffer(0, model.vertices.slice(..));
            pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
            for (material, range) in &model.meshes {
                let texture = renderer.texture_for(material);
                if !blocks(texture) {
                    continue;
                }
                pass.set_bind_group(1, &texture.bind_group, &[]);
                pass.draw_indexed(range.clone(), 0, first as u32..end as u32);
            }
        }
        first = end;
    }
}

/// Whether a texture's faces block light: all but sky, tool and translucent ones.
fn blocks(texture: &GpuTexture) -> bool {
    !matches!(texture.surface, Surface::Sky | Surface::Tool) && !texture.alpha.see_through()
}

/// Draws one whole shadow map layer from `matrix`.
#[allow(clippy::too_many_arguments)]
fn draw_layer(
    renderer: &Renderer3D,
    encoder: &mut wgpu::CommandEncoder,
    pipelines: &CasterPipelines,
    layer: &ShadowLayer,
    matrix: Mat4,
    casters: (Option<&Casters>, Option<&PropCasters>),
    blockers: Blockers,
) {
    renderer
        .queue
        .write_buffer(&layer.buffer, 0, bytemuck::cast_slice(&matrix.to_cols_array()));
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("mallet shadow map"),
        color_attachments: &[],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: &layer.target,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(1.0),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
    });
    pass.set_bind_group(0, &layer.group, &[]);
    draw_casters(
        &mut pass,
        renderer,
        pipelines,
        casters,
        &frustum_planes(matrix),
        blockers,
    );
}

/// Draws small shadow maps into their tiles of the atlas, one pass for each layer they
/// are on: each tile is cleared to the far plane, then drawn from its camera.
fn draw_tiles(
    renderer: &Renderer3D,
    encoder: &mut wgpu::CommandEncoder,
    (pipelines, clear): (&CasterPipelines, &wgpu::RenderPipeline),
    atlas: &ShadowAtlas,
    tiles: &[(u32, Mat4, u64)],
    casters: (Option<&Casters>, Option<&PropCasters>),
) {
    let per_layer = atlas.per_layer();
    let mut sorted: Vec<&(u32, Mat4, u64)> = tiles.iter().collect();
    sorted.sort_by_key(|(tile, ..)| *tile);
    for group in sorted.chunk_by(|a, b| a.0 / per_layer == b.0 / per_layer) {
        let layer = (group[0].0 / per_layer) as usize;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mallet small shadow maps"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &atlas.targets[layer],
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        for (tile, matrix, _) in group {
            let slot = &atlas.tiles[*tile as usize];
            renderer
                .queue
                .write_buffer(&slot.buffer, 0, bytemuck::cast_slice(&matrix.to_cols_array()));
            let within = tile % per_layer;
            let x = (within % atlas.per_row) * atlas.tile;
            let y = (within / atlas.per_row) * atlas.tile;
            let side = atlas.tile as f32;
            pass.set_viewport(x as f32, y as f32, side, side, 0.0, 1.0);
            pass.set_scissor_rect(x, y, atlas.tile, atlas.tile);
            pass.set_bind_group(0, &slot.group, &[]);
            pass.set_pipeline(clear);
            pass.draw(0..3, 0..1);
            let blockers = Blockers { zone: None, sky: true };
            draw_casters(
                &mut pass,
                renderer,
                pipelines,
                casters,
                &frustum_planes(*matrix),
                blockers,
            );
        }
    }
}

/// Gives every wanted light its run of `count` layers or tiles out of `capacity`, keeping
/// the runs of lights that already have them, and packing them all again when the free
/// ones are too scattered.
fn allocate_runs(slots: &mut HashMap<u64, (u32, u32)>, capacity: usize, wanted: &[(u64, u32)]) {
    slots.retain(|key, (_, count)| wanted.iter().any(|(wanted, layers)| wanted == key && layers == count));
    let mut used = vec![false; capacity];
    for (first, count) in slots.values() {
        for layer in *first..*first + *count {
            if let Some(slot) = used.get_mut(layer as usize) {
                *slot = true;
            }
        }
    }
    let place = |used: &mut Vec<bool>, count: u32| -> Option<u32> {
        let count = count as usize;
        let start =
            (0..=capacity.checked_sub(count)?).find(|&start| used[start..start + count].iter().all(|taken| !taken))?;
        used[start..start + count].iter_mut().for_each(|taken| *taken = true);
        Some(start as u32)
    };
    for (key, count) in wanted {
        if slots.contains_key(key) {
            continue;
        }
        if let Some(first) = place(&mut used, *count) {
            slots.insert(*key, (first, *count));
            continue;
        }
        slots.clear();
        used.iter_mut().for_each(|taken| *taken = false);
        for (key, count) in wanted {
            if let Some(first) = place(&mut used, *count) {
                slots.insert(*key, (first, *count));
            }
        }
        break;
    }
}

/// Which zone a point is in, given the 3D skybox's bounds.
fn zone_of(skybox: Option<&Bounds>, point: DVec3) -> Zone {
    match skybox {
        Some(skybox) if point.cmpge(skybox.min - DVec3::ONE).all() && point.cmple(skybox.max + DVec3::ONE).all() => {
            Zone::Skybox
        }
        _ => Zone::Map,
    }
}

/// The 3D skybox: the room the sky_camera stands in, found as the world brushes that touch
/// the one under the camera, and the ones touching those. `None` without a sky_camera, or
/// when that group reaches over half the map, as when the skybox touches the map itself.
fn skybox_bounds(scene: &Scene) -> Option<Bounds> {
    let camera = scene.lighting.sky.camera?;
    let brushes: Vec<Bounds> = scene
        .meshes
        .iter()
        .filter(|mesh| matches!(mesh.owner, Some(Owner::Solid(_))) && !mesh.bounds.is_empty())
        .map(|mesh| mesh.bounds)
        .collect();
    let under = |bounds: &Bounds| {
        camera.x >= bounds.min.x
            && camera.x <= bounds.max.x
            && camera.y >= bounds.min.y
            && camera.y <= bounds.max.y
            && bounds.max.z <= camera.z + 1.0
    };
    let seed = brushes
        .iter()
        .enumerate()
        .filter(|(_, bounds)| under(bounds))
        .max_by(|a, b| a.1.max.z.total_cmp(&b.1.max.z))?
        .0;
    let limit = volume(&scene.bounds) * 0.5;
    let mut room = brushes[seed];
    let mut found = vec![false; brushes.len()];
    found[seed] = true;
    let mut queue = vec![seed];
    while let Some(at) = queue.pop() {
        for (index, other) in brushes.iter().enumerate() {
            if found[index] || !touching(&brushes[at], other) {
                continue;
            }
            found[index] = true;
            room.add_bounds(other);
            if volume(&room) > limit {
                return None;
            }
            queue.push(index);
        }
    }
    let holds = camera.cmpge(room.min - DVec3::ONE).all() && camera.cmple(room.max + DVec3::ONE).all();
    holds.then_some(room)
}

/// Whether two boxes touch or overlap, give or take a unit.
fn touching(a: &Bounds, b: &Bounds) -> bool {
    (a.min - DVec3::ONE).cmple(b.max).all() && (b.min - DVec3::ONE).cmple(a.max).all()
}

fn volume(bounds: &Bounds) -> f64 {
    let size = bounds.size().max(DVec3::ZERO);
    size.x * size.y * size.z
}

/// A depth-only pipeline for shadow maps, with a bias that keeps surfaces from shadowing
/// themselves. Both sides of faces are drawn, so thin walls still block light. Without
/// a fragment stage nothing is alpha-tested.
fn depth_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    (vertex_entry, tested): (&str, bool),
    buffers: &[wgpu::VertexBufferLayout<'_>],
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(vertex_entry),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vertex_entry),
            buffers,
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState {
                constant: 2,
                slope_scale: 2.0,
                clamp: 0.0,
            },
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: tested.then_some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_shadow"),
            targets: &[],
            compilation_options: Default::default(),
        }),
        multiview: None,
        cache: None,
    })
}

/// A render pass into a luxel light texture.
fn luxel_pass<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    target: &'a wgpu::TextureView,
    load: wgpu::LoadOp<wgpu::Color>,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("mallet luxel bake"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            resolve_target: None,
            ops: wgpu::Operations {
                load,
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    })
}

/// A pipeline that bakes light into the luxel atlas, from vertices covering whole faces'
/// luxels or the whole atlas, replacing what is there or, with a blend, adding to it.
fn bake_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    (vertex_entry, fragment_entry): (&str, &str),
    buffers: &[wgpu::VertexBufferLayout<'_>],
    (topology, blend): (wgpu::PrimitiveTopology, Option<wgpu::BlendState>),
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(fragment_entry),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vertex_entry),
            buffers,
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment_entry),
            targets: &[Some(wgpu::ColorTargetState {
                format: LUXEL_FORMAT,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        multiview: None,
        cache: None,
    })
}

/// The pipeline that clears one tile of the small shadow map atlas to the far plane.
fn clear_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("mallet small shadow clear"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_shadow_clear"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: ATLAS_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Always,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: None,
        multiview: None,
        cache: None,
    })
}

/// The pipeline for glows and beams: added onto what is behind, hidden by what is in
/// front, never hiding anything.
fn glow_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
) -> wgpu::RenderPipeline {
    let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4, 2 => Float32x3];
    let buffers = [wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<GlowVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attributes,
    }];
    let add = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    };
    let keep = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::Zero,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("mallet glows"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_glow"),
            buffers: &buffers,
            compilation_options: Default::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: super::DEPTH_FORMAT,
            depth_write_enabled: false,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_glow"),
            targets: &[Some(wgpu::ColorTargetState {
                format: COLOR_FORMAT,
                blend: Some(wgpu::BlendState {
                    color: add,
                    alpha: keep,
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        multiview: None,
        cache: None,
    })
}
