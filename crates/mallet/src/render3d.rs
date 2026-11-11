//! The viewports drawn with wgpu into offscreen images: the textured, wireframe and
//! lighting preview 3D camera views and the three 2D views.
//!
//! The camera image is shown in the window and read back for MCP screenshots, so both
//! always match. Faces are shaded by direction like Hammer's textured mode, or lit by
//! the map's lights in the lighting preview (`lit`); selected brushes are tinted red with
//! red outlines, and tool previews are drawn as lines on top. Geometry is cached on the
//! GPU until it changes, so moving the camera or panning a 2D view costs almost nothing
//! on the CPU.

mod lit;

pub use lit::{LightingInput, LightingStats};

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use eframe::wgpu::{self, util::DeviceExt};
use glam::{DVec3, Mat4, Vec3, Vec4};
use mallet_content::vtf::{Encoding, Level, Mips};
use mallet_content::{AlphaMode, Materials, Surface};

use crate::lighting::TextureLights;
use crate::scene::{
    box_edges, entity_marker_edges, ModelCache, Overlay, Owner, Remap, Scene, SceneFace, WireMesh, AGENT_CAMERA_COLOR,
};
use crate::style::{self as view_style, ViewStyle};
use crate::view2d::View2D;
use crate::view3d::Camera3D;

const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const NEAR: f32 = 1.0;
/// Texture size assumed for texture coordinates when a material cannot be found.
const MISSING_TEXTURE_SIZE: f32 = 128.0;
/// How strongly selected faces are tinted red.
const SELECTION_TINT: f32 = 0.4;
/// Key for the plain white texture used by point entity boxes.
const WHITE_KEY: &str = "";
/// Side length of the world cubes faces are grouped into for culling.
const CHUNK_SIZE: f64 = 1024.0;
/// Most texture bytes sent to the graphics card in one frame. A map's textures arrive
/// over many frames, so the copies on their way to the card never add up to much memory.
const UPLOAD_BYTES_PER_FRAME: usize = 24 << 20;
/// Longest a frame spends sending textures to the graphics card.
const UPLOAD_TIME_PER_FRAME: Duration = Duration::from_millis(6);
/// How many loaded textures may wait to be sent to the graphics card. The loading threads
/// pause while it is full, so decoded pictures never pile up in memory.
const LOADED_QUEUE: usize = 16;
/// Most threads loading textures at once.
const MAX_LOADING_THREADS: usize = 8;
/// Shortest time between swapping arrived textures into the picture while more load.
const SWAP_INTERVAL: Duration = Duration::from_millis(200);
/// Longest time between swapping arrived textures into the picture while more load.
const MAX_SWAP_INTERVAL: Duration = Duration::from_secs(2);
/// How many times longer than rebuilding the cached faces took the next swap waits, so
/// rebuilding takes a small share of the time while a large map's textures stream in.
const SWAP_COST_FACTOR: u32 = 8;

/// How eframe sets up the graphics card: as it does by itself, and asking for DXT texture
/// sampling where the card has it, so textures can stay compressed in its memory.
pub fn wgpu_setup() -> eframe::egui_wgpu::WgpuSetup {
    let defaults = eframe::egui_wgpu::WgpuSetupCreateNew::default();
    let describe = Arc::clone(&defaults.device_descriptor);
    eframe::egui_wgpu::WgpuSetup::CreateNew(eframe::egui_wgpu::WgpuSetupCreateNew {
        device_descriptor: Arc::new(move |adapter| {
            let mut descriptor = describe(adapter);
            descriptor.required_features |= adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC;
            descriptor
        }),
        ..defaults
    })
}

/// One corner of a brush face on the GPU. The textured view shades it by its normal, as
/// Hammer does; the lighting preview lights it.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FaceVertex {
    position: [f32; 3],
    /// The face's outward normal.
    normal: [f32; 3],
    uv: [f32; 2],
    /// The face's tint: white, or an entity box's colour.
    color: [f32; 4],
    highlight: f32,
    /// Alpha-test reference (0 for none) and blend opacity (0 for opaque).
    alpha: [f32; 2],
    /// The face's number among all the scene's faces, so the lighting preview can find
    /// its luxels; [`NO_FACE`] when it has none, as for a face being dragged.
    face: u32,
}

/// The face number of a face the lighting preview keeps nothing for.
const NO_FACE: u32 = u32::MAX;
/// How far the pieces of overlays and decals are lifted off the faces they lie on, in
/// world units. The blended pipelines' depth bias does the rest at a distance.
const DECAL_LIFT: f64 = 0.0625;

/// Where each [`FaceVertex`] field goes in the shaders, in field order.
const FACE_ATTRIBUTES: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![0 => Float32x3, 5 => Float32x3, 1 => Float32x2, 2 => Float32x4, 3 => Float32, 4 => Float32x2, 6 => Uint32];
const MODEL_ATTRIBUTES: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2];
const INSTANCE_ATTRIBUTES: [wgpu::VertexAttribute; 5] =
    wgpu::vertex_attr_array![3 => Float32x4, 4 => Float32x4, 5 => Float32x4, 6 => Float32x4, 7 => Float32x4];

/// The vertex buffer layout of brush faces.
fn face_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<FaceVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &FACE_ATTRIBUTES,
    }
}

/// The vertex buffer layouts of props: model vertices, then one placement per instance.
fn model_buffer_layouts() -> [wgpu::VertexBufferLayout<'static>; 2] {
    [
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<ModelGpuVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &MODEL_ATTRIBUTES,
        },
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<ModelInstanceData>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &INSTANCE_ATTRIBUTES,
        },
    ]
}

/// One vertex of a prop model on the GPU, in model space.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ModelGpuVertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
}

/// Per-instance data for drawing a prop: its model-to-world matrix, and a colour whose
/// alpha is the selection tint.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ModelInstanceData {
    transform: [f32; 16],
    tint: [f32; 4],
}

/// A prop model uploaded once and drawn for every instance.
struct GpuModel {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    /// Index ranges sharing one material, by material key.
    meshes: Vec<(String, Range<u32>)>,
    triangles: usize,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LineVertex {
    position: [f32; 3],
    color: [f32; 4],
}

/// A texture uploaded to the GPU, with the full size used for texture coordinates and
/// what its material is: how it uses alpha, what the compilers make of it, and whether it
/// glows or ignores light.
struct GpuTexture {
    bind_group: wgpu::BindGroup,
    width: f32,
    height: f32,
    alpha: AlphaMode,
    surface: Surface,
    facts: MaterialFacts,
    /// The material's uniform buffer, rewritten when its texture light changes.
    material: wgpu::Buffer,
}

/// What a material is, besides its texture.
#[derive(Clone, Copy, Debug)]
struct MaterialFacts {
    alpha: AlphaMode,
    surface: Surface,
    self_illum: Option<[f32; 3]>,
    unlit: bool,
}

impl MaterialFacts {
    fn opaque() -> Self {
        Self {
            alpha: AlphaMode::Opaque,
            surface: Surface::Normal,
            self_illum: None,
            unlit: false,
        }
    }

    /// The material uniform the shaders read: the alpha mode as [`alpha_uniform`] puts it,
    /// lighting flags (1 self-illum, 2 sky, 4 unlit, 8 texture light), the self-illum
    /// tint, and the colour of the texture light it is, if any.
    fn uniform(&self, emissive: Option<DVec3>) -> [f32; 12] {
        let [test, blend] = alpha_uniform(self.alpha);
        let flags = u32::from(self.self_illum.is_some())
            | (u32::from(self.surface == Surface::Sky) << 1)
            | (u32::from(self.unlit) << 2)
            | (u32::from(emissive.is_some()) << 3);
        let [r, g, b] = self.self_illum.unwrap_or([1.0; 3]);
        let glow = emissive.map_or([0.0; 3], |color| {
            (color / color.max_element().max(1e-6)).as_vec3().to_array()
        });
        [
            test,
            blend,
            flags as f32,
            0.0,
            r,
            g,
            b,
            0.0,
            glow[0],
            glow[1],
            glow[2],
            0.0,
        ]
    }
}

/// How the shaders read a material's alpha mode. The first number is the alpha-test
/// reference: 0 for none, and below 0 when the texture's alpha is ignored. The second
/// is the opacity: 0 for opaque, above 0 for blended and below 0 for added.
fn alpha_uniform(alpha: AlphaMode) -> [f32; 2] {
    match alpha {
        AlphaMode::Opaque => [0.0, 0.0],
        AlphaMode::Test(reference) => [reference, 0.0],
        AlphaMode::Blend(opacity) => [0.0, opacity.max(f32::EPSILON)],
        AlphaMode::Add { opacity, texture_alpha } => {
            [if texture_alpha { 0.0 } else { -1.0 }, -opacity.max(f32::EPSILON)]
        }
    }
}

/// A translucent face, kept on the CPU so it can be sorted back to front every frame.
#[derive(Clone)]
struct TranslucentFace {
    center: Vec3,
    /// The corners of the box around the face's triangles, for culling: a face shows
    /// while any of it is in view, even when its centre is not.
    min: Vec3,
    max: Vec3,
    key: String,
    vertices: Vec<FaceVertex>,
    /// What the face belongs to, so a selected brush's cached copy can be left out while
    /// its tinted copy is drawn.
    owner: Option<Owner>,
}

impl TranslucentFace {
    fn new(center: Vec3, key: String, vertices: Vec<FaceVertex>, owner: Option<Owner>) -> Self {
        let (min, max) = vertices.iter().fold((center, center), |(min, max), vertex| {
            let point = Vec3::from_array(vertex.position);
            (min.min(point), max.max(point))
        });
        Self {
            center,
            min,
            max,
            key,
            vertices,
            owner,
        }
    }
}

/// Every face of the map: opaque and alpha-tested faces in chunked GPU buffers, and
/// translucent faces for sorting.
struct StaticFaces {
    generation: u64,
    opaque: FaceMesh,
    translucent: Vec<TranslucentFace>,
    /// Each chunk's triangles, kept so a retexture can rebuild just its chunks.
    cells: HashMap<[i32; 3], CellFaces>,
}

/// The triangles of one chunk before upload, by texture, and its blended faces.
#[derive(Clone)]
struct CellFaces {
    min: Vec3,
    max: Vec3,
    by_texture: HashMap<String, Vec<FaceVertex>>,
    translucent: Vec<TranslucentFace>,
}

/// How the camera viewport draws the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CameraStyle {
    Textured,
    Wireframe,
    /// Textured and lit by the map's lights, as the game would light it.
    Lit,
}

/// How a pipeline uses the depth buffer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Depth {
    /// Normal 3D geometry: the nearest surface wins.
    Test,
    /// Drawn over geometry already at the same depth, for the selection tint.
    Overdraw,
    /// Always on top, in a pass that has a depth buffer.
    Ignore,
    /// For passes without a depth buffer.
    None,
    /// Blended translucent faces: tested against the depth buffer but not written to it.
    /// Their shaders give colour already multiplied by opacity, and an alpha of 0 for
    /// added materials, so one blend serves both and the image stays opaque.
    Translucent,
}

/// Faces in one vertex buffer, split into spatial chunks for culling, each drawn in runs
/// that share a texture.
struct FaceMesh {
    buffer: wgpu::Buffer,
    chunks: Vec<Chunk>,
}

/// The chunks that hold selected brushes, rebuilt without them, so a drag does not leave a
/// copy of the selection behind where it started.
struct DragFaces {
    /// The geometry and selection generations these were built from.
    key: (u64, u64),
    /// The cells of [`StaticFaces`] that `mesh` stands in for.
    cells: HashSet<[i32; 3]>,
    mesh: FaceMesh,
}

/// The faces of the brushes whose centres fall in one cube of the world.
struct Chunk {
    /// Which cube of the world this is, as [`chunk_cell`] numbers them.
    cell: [i32; 3],
    min: Vec3,
    max: Vec3,
    batches: Vec<(String, Range<u32>)>,
    triangles: usize,
}

/// A vertex or instance buffer and the runs of it that share a texture or model.
type Batches = (wgpu::Buffer, Vec<(String, Range<u32>)>);

/// Lines in one vertex buffer.
struct LineMesh {
    buffer: wgpu::Buffer,
    count: u32,
}

/// An image a viewport is drawn into, with its own camera matrix.
struct Target {
    size: [u32; 2],
    color: wgpu::Texture,
    color_view: wgpu::TextureView,
    depth_view: Option<wgpu::TextureView>,
    uniform_buffer: wgpu::Buffer,
    uniform_group: wgpu::BindGroup,
}

/// Draw counts from the latest camera frame, to show what culling saves.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderStats {
    pub chunks: usize,
    pub chunks_drawn: usize,
    pub triangles_drawn: usize,
    pub models: usize,
    pub models_drawn: usize,
}

/// A material loaded on a worker thread, by its key: its texture ready to upload and what
/// the material is, or `None` when it has no texture that can be read.
type Loaded = (String, Option<(Mips, MaterialFacts)>);

/// The materials of one request arriving from the loading threads, one at a time.
type Incoming = mpsc::Receiver<Loaded>;

/// Renders the camera viewport and the three 2D viewports with wgpu.
pub struct Renderer3D {
    device: wgpu::Device,
    queue: wgpu::Queue,
    face_pipeline: wgpu::RenderPipeline,
    overdraw_pipeline: wgpu::RenderPipeline,
    translucent_pipeline: wgpu::RenderPipeline,
    model_pipeline: wgpu::RenderPipeline,
    translucent_model_pipeline: wgpu::RenderPipeline,
    /// Uploaded prop models by model key; `None` when the model could not be loaded.
    gpu_models: HashMap<String, Option<GpuModel>>,
    line_pipeline: wgpu::RenderPipeline,
    flat_line_pipeline: wgpu::RenderPipeline,
    uniform_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Uploaded textures by material key; `None` when the material could not be loaded.
    textures: HashMap<String, Option<GpuTexture>>,
    missing: GpuTexture,
    /// Flat grey stand-in drawn while a material's texture is still loading.
    loading_texture: GpuTexture,
    /// Materials asked for whose textures are not drawn yet: still loading in the
    /// background, or uploaded and waiting in [`Self::arrived`].
    loading: HashSet<String>,
    /// The requests whose materials are still arriving.
    incoming: Vec<Incoming>,
    /// Textures uploaded but not drawn yet. They are swapped in together a few times a
    /// second, so the cached map faces are not rebuilt every frame while textures stream in.
    arrived: Vec<(String, Option<GpuTexture>)>,
    /// When arrived textures were last swapped in, and how long to wait before the next.
    last_swap: Instant,
    swap_interval: Duration,
    /// Materials whose textures were swapped in since the cached map faces were built.
    stale: HashSet<String>,
    /// Whether the graphics card samples DXT blocks, so textures can stay compressed.
    blocks: bool,
    /// Counts texture uploads, so cached viewport images know when to redraw.
    textures_version: u64,
    /// What each slot's image was last drawn from; an unchanged signature means the image
    /// can be shown again without drawing.
    signatures: [Option<u64>; 4],
    /// Which slots reused their image in their latest draw.
    pub reused: [bool; 4],
    /// Every face, cached until the geometry changes.
    faces: Option<StaticFaces>,
    /// Every edge, cached until the geometry changes; shared by the 2D views.
    edges: Option<(u64, LineMesh)>,
    /// Every edge but the selection's, for while a drag shows the selection elsewhere;
    /// keyed by the geometry and selection generations it was built from.
    drag_edges: Option<((u64, u64), Option<LineMesh>)>,
    /// The chunks of [`Self::faces`] that hold selected brushes, rebuilt without them,
    /// for while a drag shows the selection elsewhere.
    drag_faces: Option<DragFaces>,
    /// The geometry generation whose materials have been loaded.
    textures_checked: u64,
    /// One image per viewport slot.
    targets: [Option<Target>; 4],
    pub stats: RenderStats,
    /// The lighting preview's lights, shadow maps and pipelines, made when first used.
    lit: Option<lit::LitState>,
    /// The texture lights materials glow with in the lighting preview.
    texture_lights: Arc<TextureLights>,
}

impl Renderer3D {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mallet viewport shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("render3d.wgsl").into()),
        });
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mallet uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mallet texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("mallet sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let face_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mallet faces"),
            bind_group_layouts: &[&uniform_layout, &texture_layout],
            push_constant_ranges: &[],
        });
        let line_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mallet lines"),
            bind_group_layouts: &[&uniform_layout],
            push_constant_ranges: &[],
        });
        let line_attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];
        let face_buffer = face_buffer_layout();
        let line_buffer = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<LineVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &line_attributes,
        };
        let model_buffers = model_buffer_layouts();
        let faces = |depth| {
            let topology = wgpu::PrimitiveTopology::TriangleList;
            let buffers = [face_buffer.clone()];
            pipeline(
                device,
                &face_layout,
                &shader,
                ("vs_face", "fs_face"),
                &buffers,
                topology,
                depth,
                true,
            )
        };
        let lines = |depth| {
            let topology = wgpu::PrimitiveTopology::LineList;
            let buffers = [line_buffer.clone()];
            pipeline(
                device,
                &line_layout,
                &shader,
                ("vs_line", "fs_line"),
                &buffers,
                topology,
                depth,
                false,
            )
        };
        let models = |depth| {
            let topology = wgpu::PrimitiveTopology::TriangleList;
            pipeline(
                device,
                &face_layout,
                &shader,
                ("vs_model", "fs_face"),
                &model_buffers,
                topology,
                depth,
                false,
            )
        };
        let model_pipeline = models(Depth::Test);
        let translucent_model_pipeline = models(Depth::Translucent);
        let face_pipeline = faces(Depth::Test);
        let overdraw_pipeline = faces(Depth::Overdraw);
        let translucent_pipeline = faces(Depth::Translucent);
        let line_pipeline = lines(Depth::Ignore);
        let flat_line_pipeline = lines(Depth::None);

        let upload = |mips: Mips, facts| upload(device, queue, &texture_layout, &sampler, &mips, facts, None);
        let missing = upload(checkerboard(), MaterialFacts::opaque());
        let white = upload(
            flat(Color32::WHITE, 1),
            MaterialFacts {
                unlit: true,
                ..MaterialFacts::opaque()
            },
        );
        let loading_texture = upload(
            flat(Color32::from_gray(90), MISSING_TEXTURE_SIZE as u32),
            MaterialFacts::opaque(),
        );
        Self {
            device: device.clone(),
            queue: queue.clone(),
            face_pipeline,
            overdraw_pipeline,
            translucent_pipeline,
            model_pipeline,
            translucent_model_pipeline,
            gpu_models: HashMap::new(),
            line_pipeline,
            flat_line_pipeline,
            uniform_layout,
            texture_layout,
            sampler,
            textures: HashMap::from([(WHITE_KEY.to_owned(), Some(white))]),
            missing,
            loading_texture,
            loading: HashSet::new(),
            incoming: Vec::new(),
            arrived: Vec::new(),
            last_swap: Instant::now(),
            swap_interval: SWAP_INTERVAL,
            stale: HashSet::new(),
            blocks: device.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
            textures_version: 0,
            signatures: [None; 4],
            reused: [false; 4],
            faces: None,
            edges: None,
            drag_edges: None,
            drag_faces: None,
            textures_checked: 0,
            targets: [None, None, None, None],
            stats: RenderStats::default(),
            lit: None,
            texture_lights: Arc::new(TextureLights::new()),
        }
    }

    /// Forgets every uploaded texture, for example after the game folder changed, and the
    /// lighting preview's skybox, projected textures and shadow maps with them.
    pub fn clear_textures(&mut self) {
        self.textures.retain(|key, _| key == WHITE_KEY);
        self.lit = None;
        self.gpu_models.clear();
        self.loading.clear();
        self.incoming.clear();
        self.arrived.clear();
        self.stale.clear();
        self.faces = None;
        self.textures_checked = 0;
        self.textures_version += 1;
    }

    /// How many materials, and skyboxes and projected textures of the lighting preview,
    /// are still loading in the background.
    pub fn textures_loading(&self) -> usize {
        self.loading.len() + self.lit.as_ref().map_or(0, lit::LitState::loading)
    }

    /// Waits for every background texture load and uploads the results, so a screenshot
    /// shows real textures instead of loading placeholders, and has the lighting preview
    /// draw all its small shadow maps in its next frame.
    pub fn finish_loading(&mut self) {
        let mut waiting = 0;
        for batch in std::mem::take(&mut self.incoming) {
            while let Ok(loaded) = batch.recv() {
                waiting += self.upload_loaded(loaded);
                if waiting >= UPLOAD_BYTES_PER_FRAME {
                    waiting = 0;
                    self.queue.submit([]);
                    let _ = self.device.poll(wgpu::Maintain::Poll);
                }
            }
        }
        self.swap_in();
        if let Some(lit) = &mut self.lit {
            lit.poll(&self.device, &self.queue, true);
            lit.draw_all_shadows();
        }
    }

    /// Whether the lighting preview still has work to do a little each frame: small
    /// shadow maps to draw or luxels to bake.
    pub fn lighting_pending(&self) -> bool {
        self.lit.as_ref().is_some_and(lit::LitState::pending)
    }

    /// What the latest lighting preview frame used, once one has been drawn.
    pub fn lighting_stats(&self) -> Option<LightingStats> {
        self.lit.as_ref().map(|lit| lit.stats)
    }

    /// Makes the materials `lights.rad` lists glow in the lighting preview, when the list
    /// changed.
    fn sync_texture_lights(&mut self, table: &Arc<TextureLights>) {
        if Arc::ptr_eq(&self.texture_lights, table) {
            return;
        }
        self.texture_lights = Arc::clone(table);
        for (key, texture) in &self.textures {
            if let Some(texture) = texture {
                let emissive = table.get(key).map(|light| light.hdr.max(light.ldr));
                self.queue.write_buffer(
                    &texture.material,
                    0,
                    bytemuck::cast_slice(&texture.facts.uniform(emissive)),
                );
            }
        }
        self.textures_version += 1;
    }

    /// Uploads textures the background threads have finished, as many as one frame may
    /// take, without waiting for the rest, and swaps what has arrived into the picture
    /// when that is due.
    fn poll_loading(&mut self) {
        let started = Instant::now();
        let mut sent = 0;
        let mut pending = Vec::new();
        for batch in std::mem::take(&mut self.incoming) {
            let mut open = true;
            while sent < UPLOAD_BYTES_PER_FRAME && started.elapsed() < UPLOAD_TIME_PER_FRAME {
                match batch.try_recv() {
                    Ok(loaded) => sent += self.upload_loaded(loaded),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        open = false;
                        break;
                    }
                }
            }
            if open {
                pending.push(batch);
            }
        }
        self.incoming = pending;
        if self.incoming.is_empty() || self.last_swap.elapsed() >= self.swap_interval {
            self.swap_in();
        }
    }

    /// Sends one loaded material's texture to the graphics card, where it waits in
    /// [`Self::arrived`]. Returns how many bytes that took.
    fn upload_loaded(&mut self, (key, loaded): Loaded) -> usize {
        let emissive = self.texture_lights.get(&key).map(|light| light.hdr.max(light.ldr));
        let bytes = loaded.as_ref().map_or(0, |(mips, _)| mips.bytes());
        let texture = loaded.and_then(|(mips, facts)| self.upload_guarded(&mips, facts, emissive));
        self.arrived.push((key, texture));
        bytes
    }

    /// Uploads a texture, or gives `None` when the graphics card refuses it, as it does
    /// when its memory is full. The material then shows as missing; without the error
    /// scopes wgpu would stop the editor instead.
    fn upload_guarded(&self, mips: &Mips, facts: MaterialFacts, emissive: Option<DVec3>) -> Option<GpuTexture> {
        self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let texture = upload(
            &self.device,
            &self.queue,
            &self.texture_layout,
            &self.sampler,
            mips,
            facts,
            emissive,
        );
        let refused = [self.device.pop_error_scope(), self.device.pop_error_scope()]
            .into_iter()
            .find_map(|scope| ready(scope).flatten());
        match refused {
            Some(error) => {
                eprintln!("texture upload failed: {error}");
                None
            }
            None => Some(texture),
        }
    }

    /// Starts drawing the textures that have arrived. Once nothing is on its way any
    /// more, a material whose load was lost stops counting as loading.
    fn swap_in(&mut self) {
        if !self.arrived.is_empty() {
            for (key, texture) in self.arrived.drain(..) {
                self.loading.remove(&key);
                self.stale.insert(key.clone());
                self.textures.insert(key, texture);
            }
            self.textures_version += 1;
            self.last_swap = Instant::now();
        }
        if self.incoming.is_empty() {
            self.loading.clear();
        }
    }

    /// Draws the scene from the camera into viewport slot `slot`'s image of `size`
    /// pixels. Returns the image's view and whether the image was recreated, so callers
    /// holding a reference to the old one can refresh it.
    ///
    /// Textured: static geometry comes from a cache split into chunks, and chunks outside
    /// the camera's view are skipped. Faces are one-sided, so brushes look solid from
    /// outside and open from inside, as in Hammer. Translucent faces are drawn after the
    /// rest, sorted back to front. The selection is drawn again on top, tinted, at its
    /// live position. Lit: the same, lit by the map's lights with shadows, the skybox,
    /// glows and fog, as `lighting` sets it up. Wireframe: every brush edge in its editor
    /// colour.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        slot: usize,
        style: CameraStyle,
        scene: &Scene,
        overlay: &Overlay,
        camera: &Camera3D,
        size: [u32; 2],
        materials: Option<&mut Materials>,
        models: &ModelCache,
        lighting: LightingInput<'_>,
    ) -> (&wgpu::TextureView, bool) {
        let size = [size[0].max(1), size[1].max(1)];
        let recreated = ensure_target(&self.device, &self.uniform_layout, &mut self.targets[slot], size, true);
        self.poll_loading();
        let lit = style == CameraStyle::Lit;
        let textured = style != CameraStyle::Wireframe;
        if lit {
            self.sync_texture_lights(lighting.texture_lights);
            let state = self.lit.get_or_insert_with(|| {
                lit::LitState::new(&self.device, &self.queue, &self.uniform_layout, &self.texture_layout)
            });
            state.poll(&self.device, &self.queue, false);
        }
        let lit_fingerprint = self
            .lit
            .as_ref()
            .filter(|_| lit)
            .map(|state| state.fingerprint(scene, lighting));
        let look = view_style::current();
        let signature = signature(|hasher| {
            (slot, style, size).hash(hasher);
            look.hash(hasher);
            (
                scene.geometry_generation,
                scene.selection_generation,
                self.textures_version,
                lit_fingerprint,
            )
                .hash(hasher);
            for value in [
                camera.position.x,
                camera.position.y,
                camera.position.z,
                camera.yaw,
                camera.pitch,
                camera.fov,
                camera.far,
            ] {
                value.to_bits().hash(hasher);
            }
            format!("{overlay:?}").hash(hasher);
        });
        self.reused[slot] = !recreated && self.signatures[slot] == Some(signature);
        if self.reused[slot] {
            return (&self.targets[slot].as_ref().expect("target exists").color_view, false);
        }
        self.signatures[slot] = Some(signature);
        let loader = materials.as_deref().map(Materials::loader);
        if textured {
            if self.textures_checked != scene.geometry_generation {
                self.upload_models(scene, models);
                self.load_textures(scene, materials, models);
                self.textures_checked = scene.geometry_generation;
            }
            self.refresh_stale_faces(scene);
            if self
                .faces
                .as_ref()
                .is_none_or(|faces| faces.generation != scene.geometry_generation)
            {
                self.update_static_faces(scene);
            }
            self.update_drag_faces(scene, overlay);
        } else {
            self.update_edges(scene, overlay);
        }
        let selected: Vec<&WireMesh> = scene.meshes.iter().filter(|mesh| mesh.selected).collect();
        let selected_faces = selected_face_meshes(scene);
        let (tinted, tinted_translucent) = if textured {
            let (opaque, translucent) = self.face_mesh(
                selected.iter().copied(),
                overlay.transform.as_ref(),
                SELECTION_TINT,
                false,
            );
            (Some(opaque), translucent)
        } else {
            (None, Vec::new())
        };
        // Faces picked with the texture tool get a lighter tint, so the texture being
        // lined up stays easy to see.
        let face_tinted = (textured && !selected_faces.is_empty() && !overlay.hide_face_mask)
            .then(|| self.face_mesh(selected_faces.iter(), None, FACE_TINT, false).0);
        let selected_owners: HashSet<Owner> = selected.iter().filter_map(|mesh| mesh.owner).collect();
        let mut outline = Vec::new();
        // Lines that belong to nothing, such as a leak line, show in the textured view too.
        for mesh in scene.meshes.iter().filter(|mesh| mesh.owner.is_none()) {
            push_edges(&mut outline, &mesh.edges, None, mesh.color);
        }
        for mesh in &selected {
            push_edges(&mut outline, &mesh.edges, overlay.transform.as_ref(), look.selection);
        }
        for mesh in &selected_faces {
            push_edges(&mut outline, &mesh.edges, None, look.selected_faces);
        }
        if let Some(block) = &overlay.block {
            push_edges(&mut outline, &box_edges(block), None, look.tool);
        }
        if let Some(point) = overlay.entity {
            push_edges(&mut outline, &entity_marker_edges(point), None, look.tool);
        }
        push_edges(&mut outline, &overlay.agent_camera, None, AGENT_CAMERA_COLOR);
        if let Some(clip) = &overlay.clip {
            for (edge, color) in clip.edges_3d() {
                push_edges(&mut outline, &[edge], None, color);
            }
        }

        let aspect = size[0] as f32 / size[1] as f32;
        let matrix = view_projection(camera, aspect);
        let planes = frustum_planes(matrix);
        let helpers = !lit || lighting.options.entity_boxes;
        let sprites = if textured && helpers {
            self.sprite_faces(scene, overlay, camera)
        } else {
            Vec::new()
        };
        let translucent = match (&self.faces, textured) {
            (Some(faces), true) => {
                // A selected brush's cached faces give way to its tinted copy, except in a
                // clone drag, which leaves the original where it is.
                let keep_originals = overlay.transform.is_some() && overlay.cloning;
                let unselected = faces.translucent.iter().filter(|face| {
                    keep_originals || face.owner.is_none_or(|owner| !selected_owners.contains(&owner))
                });
                self.sorted_translucent(unselected.chain(&tinted_translucent).chain(&sprites), camera, &planes)
            }
            _ => None,
        };
        let mut stats = RenderStats::default();
        let instances = if textured {
            self.model_instances(scene, overlay, &planes, helpers, &mut stats)
        } else {
            None
        };
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mallet camera"),
        });
        let mut lit_state = if lit { self.lit.take() } else { None };
        let mut glows = None;
        if let Some(state) = &mut lit_state {
            state.prepare(
                self,
                &mut encoder,
                scene,
                overlay,
                camera,
                &planes,
                loader.as_ref(),
                lighting,
            );
            for (edge, color) in state.helper_lines(scene, lighting.options) {
                push_edges(&mut outline, &[edge], None, color);
            }
            let bytes = state.glow_vertices(scene, camera, lighting.options);
            glows = (!bytes.is_empty()).then(|| {
                let buffer = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("mallet glows"),
                    contents: &bytes,
                    usage: wgpu::BufferUsages::VERTEX,
                });
                (buffer, (bytes.len() / lit::GLOW_VERTEX_SIZE) as u32)
            });
        }
        let outline = self.line_mesh(&outline);
        let target = self.targets[slot].as_ref().expect("target was just created");
        self.queue
            .write_buffer(&target.uniform_buffer, 0, bytemuck::cast_slice(&matrix.to_cols_array()));
        {
            let mut pass = begin_pass(&mut encoder, target, &look);
            pass.set_bind_group(0, &target.uniform_group, &[]);
            match (style, &self.faces) {
                (CameraStyle::Textured, Some(faces)) => {
                    pass.set_pipeline(&self.face_pipeline);
                    stats.chunks = faces.opaque.chunks.len();
                    for (mesh, skipped) in self.opaque_faces() {
                        pass.set_vertex_buffer(0, mesh.buffer.slice(..));
                        for chunk in mesh.chunks.iter().filter(|chunk| {
                            skipped.is_none_or(|cells| !cells.contains(&chunk.cell))
                                && box_visible(&planes, chunk.min, chunk.max)
                        }) {
                            stats.chunks_drawn += 1;
                            stats.triangles_drawn += chunk.triangles;
                            for (key, range) in &chunk.batches {
                                pass.set_bind_group(1, &self.texture_for(key).bind_group, &[]);
                                pass.draw(range.clone(), 0..1);
                            }
                        }
                    }
                    if let Some((buffer, ranges)) = &instances {
                        self.draw_models(&mut pass, buffer, ranges, false, &mut stats);
                    }
                    for tinted in [&tinted, &face_tinted].into_iter().flatten() {
                        pass.set_pipeline(&self.overdraw_pipeline);
                        pass.set_vertex_buffer(0, tinted.buffer.slice(..));
                        for (key, range) in tinted.chunks.iter().flat_map(|chunk| &chunk.batches) {
                            pass.set_bind_group(1, &self.texture_for(key).bind_group, &[]);
                            pass.draw(range.clone(), 0..1);
                        }
                    }
                    if let Some((buffer, batches)) = &translucent {
                        pass.set_pipeline(&self.translucent_pipeline);
                        pass.set_vertex_buffer(0, buffer.slice(..));
                        for (key, range) in batches {
                            stats.triangles_drawn += range.len() / 3;
                            pass.set_bind_group(1, &self.texture_for(key).bind_group, &[]);
                            pass.draw(range.clone(), 0..1);
                        }
                    }
                    if let Some((buffer, ranges)) = &instances {
                        self.draw_models(&mut pass, buffer, ranges, true, &mut stats);
                    }
                }
                (CameraStyle::Lit, Some(_)) => {
                    if let Some(state) = &lit_state {
                        let tinted_meshes: Vec<&FaceMesh> = [&tinted, &face_tinted].into_iter().flatten().collect();
                        state.draw(
                            self,
                            &mut pass,
                            &planes,
                            lighting.options,
                            &tinted_meshes,
                            translucent.as_ref(),
                            instances.as_ref(),
                            glows.as_ref(),
                            &mut stats,
                        );
                    }
                }
                (CameraStyle::Wireframe, _) => {
                    if let Some(edges) = self.static_edges() {
                        pass.set_pipeline(&self.line_pipeline);
                        pass.set_vertex_buffer(0, edges.buffer.slice(..));
                        pass.draw(0..edges.count, 0..1);
                    }
                }
                (_, None) => {}
            }
            if let Some(lines) = &outline {
                pass.set_pipeline(&self.line_pipeline);
                pass.set_vertex_buffer(0, lines.buffer.slice(..));
                pass.draw(0..lines.count, 0..1);
            }
        }
        self.queue.submit([encoder.finish()]);
        if textured {
            self.stats = stats;
        }
        if let Some(state) = lit_state {
            self.lit = Some(state);
        }
        (
            &self.targets[slot].as_ref().expect("target exists").color_view,
            recreated,
        )
    }

    /// Builds this frame's buffer of translucent faces in view, farthest first, with runs
    /// of the same texture merged into one draw.
    #[allow(clippy::type_complexity)]
    fn sorted_translucent<'a>(
        &self,
        faces: impl Iterator<Item = &'a TranslucentFace>,
        camera: &Camera3D,
        planes: &[Vec4; 6],
    ) -> Option<(wgpu::Buffer, Vec<(String, Range<u32>)>)> {
        let eye = camera.position.as_vec3();
        let mut visible: Vec<&TranslucentFace> = faces.filter(|face| box_visible(planes, face.min, face.max)).collect();
        if visible.is_empty() {
            return None;
        }
        visible.sort_by(|a, b| {
            b.center
                .distance_squared(eye)
                .total_cmp(&a.center.distance_squared(eye))
        });
        let mut vertices = Vec::new();
        let mut batches: Vec<(String, Range<u32>)> = Vec::new();
        for face in visible {
            let start = vertices.len() as u32;
            vertices.extend_from_slice(&face.vertices);
            let end = vertices.len() as u32;
            match batches.last_mut() {
                Some((key, range)) if *key == face.key => range.end = end,
                _ => batches.push((face.key.clone(), start..end)),
            }
        }
        let buffer = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mallet translucent faces"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        Some((buffer, batches))
    }

    /// Uploads every model the scene places that is not on the GPU yet.
    fn upload_models(&mut self, scene: &Scene, models: &ModelCache) {
        for instance in scene.meshes.iter().filter_map(|mesh| mesh.model.as_ref()) {
            if self.gpu_models.contains_key(&instance.key) {
                continue;
            }
            let uploaded = models.get(&instance.key).and_then(Option::as_ref).map(|model| {
                let vertices: Vec<ModelGpuVertex> = model
                    .vertices
                    .iter()
                    .map(|vertex| ModelGpuVertex {
                        position: vertex.position,
                        normal: vertex.normal,
                        uv: vertex.uv,
                    })
                    .collect();
                let mut indices = Vec::new();
                let mut meshes = Vec::new();
                for mesh in &model.meshes {
                    let start = indices.len() as u32;
                    indices.extend_from_slice(&mesh.indices);
                    meshes.push((material_key(&mesh.material), start..indices.len() as u32));
                }
                GpuModel {
                    vertices: self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("mallet model vertices"),
                        contents: bytemuck::cast_slice(&vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
                    indices: self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("mallet model indices"),
                        contents: bytemuck::cast_slice(&indices),
                        usage: wgpu::BufferUsages::INDEX,
                    }),
                    meshes,
                    triangles: model.triangle_count(),
                }
            });
            self.gpu_models.insert(instance.key.clone(), uploaded);
        }
    }

    /// Builds this frame's instance buffer: every prop in view, grouped by model, with
    /// selected props tinted and shown where a move or resize drag puts them. Editor
    /// models of point entities are left out unless `helpers` is set. Returns the buffer
    /// and each model's range of instances in it.
    #[allow(clippy::type_complexity)]
    fn model_instances(
        &self,
        scene: &Scene,
        overlay: &Overlay,
        planes: &[Vec4; 6],
        helpers: bool,
        stats: &mut RenderStats,
    ) -> Option<(wgpu::Buffer, Vec<(String, Range<u32>)>)> {
        let mut by_model: HashMap<&str, Vec<ModelInstanceData>> = HashMap::new();
        for mesh in &scene.meshes {
            let Some(instance) = mesh.model.as_ref().filter(|instance| helpers || !instance.helper) else {
                continue;
            };
            if self.gpu_models.get(&instance.key).is_none_or(Option::is_none) {
                continue;
            }
            stats.models += 1;
            // A clone drag leaves the original where it is, so draw it there as well.
            let original = mesh.selected && overlay.transform.is_some() && overlay.cloning;
            if original && box_visible(planes, mesh.bounds.min.as_vec3(), mesh.bounds.max.as_vec3()) {
                by_model
                    .entry(instance.key.as_str())
                    .or_default()
                    .push(ModelInstanceData {
                        transform: instance.transform.as_mat4().to_cols_array(),
                        tint: [1.0, 1.0, 1.0, 0.0],
                    });
            }
            let (_, remap) = mesh.appearance(overlay);
            let bounds = mesh.mapped_bounds(remap.as_ref());
            if !box_visible(planes, bounds.min.as_vec3(), bounds.max.as_vec3()) {
                continue;
            }
            stats.models_drawn += 1;
            let transform = remap.map_or(instance.transform, |remap| remap.matrix() * instance.transform);
            let highlight = if mesh.selected { SELECTION_TINT } else { 0.0 };
            by_model
                .entry(instance.key.as_str())
                .or_default()
                .push(ModelInstanceData {
                    transform: transform.as_mat4().to_cols_array(),
                    tint: [1.0, 1.0, 1.0, highlight],
                });
        }
        if by_model.is_empty() {
            return None;
        }
        let mut all = Vec::new();
        let mut ranges = Vec::with_capacity(by_model.len());
        for (key, instances) in by_model {
            let start = all.len() as u32;
            all.extend(instances);
            ranges.push((key.to_owned(), start..all.len() as u32));
        }
        let buffer = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mallet model instances"),
            contents: bytemuck::cast_slice(&all),
            usage: wgpu::BufferUsages::VERTEX,
        });
        Some((buffer, ranges))
    }

    /// Draws every visible prop's opaque meshes, or with `translucent` its blended ones.
    fn draw_models<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        instances: &'a wgpu::Buffer,
        ranges: &[(String, Range<u32>)],
        translucent: bool,
        stats: &mut RenderStats,
    ) {
        pass.set_pipeline(if translucent {
            &self.translucent_model_pipeline
        } else {
            &self.model_pipeline
        });
        pass.set_vertex_buffer(1, instances.slice(..));
        for (key, instance_range) in ranges {
            let Some(Some(model)) = self.gpu_models.get(key) else {
                continue;
            };
            pass.set_vertex_buffer(0, model.vertices.slice(..));
            pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
            for (material, range) in &model.meshes {
                let texture = self.texture_for(material);
                if texture.alpha.see_through() != translucent {
                    continue;
                }
                pass.set_bind_group(1, &texture.bind_group, &[]);
                pass.draw_indexed(range.clone(), 0, instance_range.clone());
            }
            if !translucent {
                stats.triangles_drawn += model.triangles * instance_range.len();
            }
        }
    }

    /// Rebuilds the cached buffer of every brush edge when the geometry changed.
    fn ensure_edges(&mut self, scene: &Scene) {
        if self
            .edges
            .as_ref()
            .is_none_or(|(generation, _)| *generation != scene.geometry_generation)
        {
            let mut vertices = Vec::new();
            for mesh in &scene.meshes {
                push_edges(&mut vertices, &mesh.edges, None, mesh.color);
            }
            self.edges = self.line_mesh(&vertices).map(|mesh| (scene.geometry_generation, mesh));
        }
    }

    /// Draws one 2D view into viewport slot `slot`'s image of `pixels` size; `points` is the viewport's size
    /// in layout points, which the view's zoom is measured in. Returns the image's view
    /// and whether it was recreated.
    ///
    /// Brush edges come from a cache shared by all three views, so panning and zooming
    /// only change a matrix. Handles' squares, the marquee and the label are left for the
    /// caller to draw on top.
    #[allow(clippy::too_many_arguments)]
    pub fn render_2d(
        &mut self,
        slot: usize,
        view: &View2D,
        points: Vec2,
        pixels: [u32; 2],
        scene: &Scene,
        overlay: &Overlay,
        grid: f64,
    ) -> (&wgpu::TextureView, bool) {
        let pixels = [pixels[0].max(1), pixels[1].max(1)];
        let recreated = ensure_target(
            &self.device,
            &self.uniform_layout,
            &mut self.targets[slot],
            pixels,
            false,
        );
        let look = view_style::current();
        let signature = signature(|hasher| {
            (slot, view.axis.axes(), pixels).hash(hasher);
            look.hash(hasher);
            (scene.geometry_generation, scene.selection_generation).hash(hasher);
            for value in [
                view.center.x,
                view.center.y,
                view.scale,
                grid,
                f64::from(points.x),
                f64::from(points.y),
            ] {
                value.to_bits().hash(hasher);
            }
            format!("{overlay:?}").hash(hasher);
        });
        self.reused[slot] = !recreated && self.signatures[slot] == Some(signature);
        if self.reused[slot] {
            return (&self.targets[slot].as_ref().expect("target exists").color_view, false);
        }
        self.signatures[slot] = Some(signature);
        self.update_edges(scene, overlay);

        let local = Rect::from_min_size(Pos2::ZERO, points);
        let mut under = Vec::new();
        for ([a, b], color) in view.grid_lines(local, grid) {
            push_edges(&mut under, &[[view.axis.lift(a), view.axis.lift(b)]], None, color);
        }
        let mut over = Vec::new();
        if let Some(handles) = &overlay.handles {
            for [a, b] in view.handle_outline(handles) {
                push_edges(&mut over, &[[view.axis.lift(a), view.axis.lift(b)]], None, look.handles);
            }
        }
        for ([a, b], color) in view.centre_crosses(local, scene, overlay) {
            push_edges(&mut over, &[[view.axis.lift(a), view.axis.lift(b)]], None, color);
        }
        for mesh in scene.meshes.iter().filter(|mesh| mesh.selected) {
            push_edges(&mut over, &mesh.edges, overlay.transform.as_ref(), look.selection);
        }
        if let Some(block) = &overlay.block {
            push_edges(&mut over, &box_edges(block), None, look.tool);
        }
        if let Some(point) = overlay.entity {
            push_edges(&mut over, &entity_marker_edges(point), None, look.tool);
        }
        push_edges(&mut over, &overlay.agent_camera, None, AGENT_CAMERA_COLOR);
        if let Some(clip) = &overlay.clip {
            for (edge, color) in clip.edges_2d() {
                push_edges(&mut over, &[edge], None, color);
            }
        }
        let under = self.line_mesh(&under);
        let over = self.line_mesh(&over);

        let target = self.targets[slot].as_ref().expect("target was just created");
        let matrix = orthographic(view, points);
        self.queue
            .write_buffer(&target.uniform_buffer, 0, bytemuck::cast_slice(&matrix.to_cols_array()));
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mallet 2d"),
        });
        {
            let mut pass = begin_pass(&mut encoder, target, &look);
            pass.set_pipeline(&self.flat_line_pipeline);
            pass.set_bind_group(0, &target.uniform_group, &[]);
            for lines in [under.as_ref(), self.static_edges(), over.as_ref()].into_iter().flatten() {
                pass.set_vertex_buffer(0, lines.buffer.slice(..));
                pass.draw(0..lines.count, 0..1);
            }
        }
        self.queue.submit([encoder.finish()]);
        (
            &self.targets[slot].as_ref().expect("target exists").color_view,
            recreated,
        )
    }

    /// Copies the last rendered image of a viewport slot back from the GPU as RGBA bytes,
    /// row by row.
    pub fn read_pixels(&self, slot: usize) -> Option<(u32, u32, Vec<u8>)> {
        let target = self.targets.get(slot)?.as_ref()?;
        let [width, height] = target.size;
        let row = width * 4;
        let padded_row = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mallet readback"),
            size: u64::from(padded_row * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mallet readback"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target.color,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        let _ = self.device.poll(wgpu::Maintain::Wait);
        receiver.recv().ok()?.ok()?;
        let mapped = slice.get_mapped_range();
        let mut pixels = Vec::with_capacity((row * height) as usize);
        for line in mapped.chunks(padded_row as usize) {
            pixels.extend_from_slice(&line[..row as usize]);
        }
        drop(mapped);
        buffer.unmap();
        Some((width, height, pixels))
    }

    /// Starts loading, on background threads, every material in the scene that has not
    /// been seen yet. Faces show a grey stand-in until their texture arrives. The threads
    /// hand over one texture at a time and wait while [`LOADED_QUEUE`] of them are still
    /// to be uploaded, so loading a large map never holds more than a few in memory.
    fn load_textures(&mut self, scene: &Scene, materials: Option<&mut Materials>, models: &ModelCache) {
        let model_materials = scene
            .meshes
            .iter()
            .filter_map(|mesh| models.get(&mesh.model.as_ref()?.key)?.as_ref())
            .flat_map(|model| model.meshes.iter().map(|mesh| mesh.material.as_str()));
        let wanted: HashSet<String> = scene
            .meshes
            .iter()
            .flat_map(|mesh| &mesh.faces)
            .filter_map(|face| face.material.as_deref())
            .chain(model_materials)
            .chain(scene.meshes.iter().filter_map(|mesh| mesh.sprite.as_deref()))
            .map(material_key)
            .filter(|key| !self.textures.contains_key(key))
            .collect();
        if wanted.is_empty() {
            return;
        }
        let mut keys: Vec<String> = wanted.into_iter().filter(|key| !self.loading.contains(key)).collect();
        if keys.is_empty() {
            return;
        }
        keys.sort();
        let Some(materials) = materials else {
            for key in keys {
                self.textures.insert(key, None);
            }
            return;
        };
        self.loading.extend(keys.iter().cloned());
        let (sender, receiver) = mpsc::sync_channel(LOADED_QUEUE);
        self.incoming.push(receiver);
        let cores = std::thread::available_parallelism().map_or(4, usize::from);
        let threads = cores.saturating_sub(1).clamp(1, MAX_LOADING_THREADS).min(keys.len());
        let keys = Arc::new(keys);
        let next = Arc::new(AtomicUsize::new(0));
        for _ in 0..threads {
            let (keys, next, sender) = (Arc::clone(&keys), Arc::clone(&next), sender.clone());
            let (loader, blocks) = (materials.loader(), self.blocks);
            let _ = std::thread::Builder::new()
                .name("mallet textures".to_owned())
                .spawn(move || {
                    while let Some(key) = keys.get(next.fetch_add(1, Ordering::Relaxed)) {
                        let (material, mips) = loader.load_mips(key, blocks);
                        let facts = MaterialFacts {
                            alpha: material.alpha,
                            surface: material.surface,
                            self_illum: material.self_illum,
                            unlit: material.unlit,
                        };
                        if sender.send((key.clone(), mips.map(|mips| (mips, facts)))).is_err() {
                            return;
                        }
                    }
                });
        }
    }

    /// The loaded texture of a mesh's editor sprite; `None` while it loads, when it could
    /// not be found, or when the mesh has no sprite, so the entity keeps its box.
    fn sprite_texture(&self, mesh: &WireMesh) -> Option<&GpuTexture> {
        let key = material_key(mesh.sprite.as_deref()?);
        if self.loading.contains(&key) {
            return None;
        }
        self.textures.get(&key)?.as_ref()
    }

    /// Builds the editor sprites of point entities as quads facing the camera, parallel
    /// to the view plane as Hammer draws them. Each sprite is as tall as the entity's box
    /// and keeps its texture's shape; selected ones are tinted and follow a move drag.
    /// They are blended by the texture's alpha and face the textured view's light, so
    /// they are drawn at full brightness.
    fn sprite_faces(&self, scene: &Scene, overlay: &Overlay, camera: &Camera3D) -> Vec<TranslucentFace> {
        let forward = camera.forward();
        let right = camera.right();
        let up = right.cross(forward).normalize_or(DVec3::Z);
        let normal = Vec3::new(0.3, 0.5, 1.0).normalize().to_array();
        scene
            .meshes
            .iter()
            .filter_map(|mesh| {
                let texture = self.sprite_texture(mesh)?;
                let (_, remap) = mesh.appearance(overlay);
                let bounds = mesh.mapped_bounds(remap.as_ref());
                let center = bounds.center();
                let half_height = bounds.size().max_element() * 0.5;
                let half_width = half_height * f64::from(texture.width / texture.height.max(1.0));
                let highlight = if mesh.selected { SELECTION_TINT } else { 0.0 };
                let corner = |x: f64, y: f64| FaceVertex {
                    position: (center + right * (x * half_width) + up * (y * half_height))
                        .as_vec3()
                        .to_array(),
                    normal,
                    uv: [((x + 1.0) * 0.5) as f32, ((1.0 - y) * 0.5) as f32],
                    color: [1.0; 4],
                    highlight,
                    alpha: [0.0, 1.0],
                    face: NO_FACE,
                };
                let quad = [
                    corner(-1.0, -1.0),
                    corner(1.0, -1.0),
                    corner(1.0, 1.0),
                    corner(-1.0, 1.0),
                ];
                Some(TranslucentFace::new(
                    center.as_vec3(),
                    material_key(mesh.sprite.as_deref()?),
                    vec![quad[0], quad[1], quad[2], quad[0], quad[2], quad[3]],
                    mesh.owner,
                ))
            })
            .collect()
    }

    fn texture_for(&self, key: &str) -> &GpuTexture {
        if self.loading.contains(key) {
            return &self.loading_texture;
        }
        self.textures.get(key).and_then(Option::as_ref).unwrap_or(&self.missing)
    }

    /// Builds a vertex buffer for the faces of `meshes`. With `chunked`, brushes are
    /// grouped by which world cube their centre is in, so whole chunks can be culled.
    /// Translucent faces are always returned separately, for sorting.
    /// Brings the cached map faces up to the scene's geometry: just the chunks of
    /// retextured brushes when that is all that changed, otherwise everything.
    fn update_static_faces(&mut self, scene: &Scene) {
        if let Some(mut faces) = self.faces.take() {
            // Follow the retexture steps from what was last drawn to now.
            let mut generation = faces.generation;
            let mut changed = Vec::new();
            for step in &scene.retextures {
                if step.from == generation {
                    changed.extend_from_slice(&step.meshes);
                    generation = step.to;
                }
            }
            if generation == scene.geometry_generation && !changed.is_empty() {
                let dirty: HashSet<[i32; 3]> = changed
                    .iter()
                    .filter_map(|at| scene.meshes.get(*at))
                    .map(|mesh| chunk_cell(&mesh.bounds))
                    .collect();
                self.rebuild_cells(&mut faces, scene, &dirty);
                faces.generation = scene.geometry_generation;
                self.faces = Some(faces);
                return;
            }
        }
        let cells = self.cell_faces(scene.meshes.iter(), None, 0.0, true);
        let opaque = self.upload_cells(&cells);
        let translucent = cells
            .values()
            .flat_map(|cell| cell.translucent.iter().cloned())
            .collect();
        self.faces = Some(StaticFaces {
            generation: scene.geometry_generation,
            opaque,
            translucent,
            cells,
        });
    }

    /// Triangulates the chunks in `dirty` again and uploads the cached map faces anew.
    fn rebuild_cells(&self, faces: &mut StaticFaces, scene: &Scene, dirty: &HashSet<[i32; 3]>) {
        let rebuilt = self.cell_faces(
            scene
                .meshes
                .iter()
                .filter(|mesh| dirty.contains(&chunk_cell(&mesh.bounds))),
            None,
            0.0,
            true,
        );
        faces.cells.retain(|cell, _| !dirty.contains(cell));
        faces.cells.extend(rebuilt);
        faces.opaque = self.upload_cells(&faces.cells);
        faces.translucent = faces
            .cells
            .values()
            .flat_map(|cell| cell.translucent.iter().cloned())
            .collect();
    }

    /// Brings the cached map faces up to the textures swapped in since they were built:
    /// a texture's size and the way its material uses alpha are baked into the faces
    /// drawn with it. Only the chunks using those materials are rebuilt while the cache
    /// still matches the map; otherwise it is dropped to be built anew. The next swap
    /// waits longer the longer this took.
    fn refresh_stale_faces(&mut self, scene: &Scene) {
        if self.stale.is_empty() {
            return;
        }
        let started = Instant::now();
        let stale = std::mem::take(&mut self.stale);
        self.drag_faces = None;
        let current = self
            .faces
            .take()
            .filter(|faces| faces.generation == scene.geometry_generation);
        let Some(mut faces) = current else {
            return;
        };
        let uses_stale = |cell: &CellFaces| {
            cell.by_texture.keys().any(|key| stale.contains(key))
                || cell.translucent.iter().any(|face| stale.contains(&face.key))
        };
        let mut dirty: HashSet<[i32; 3]> = faces
            .cells
            .iter()
            .filter(|(_, cell)| uses_stale(cell))
            .map(|(at, _)| *at)
            .collect();
        let sprites = scene.meshes.iter().filter(|mesh| {
            mesh.sprite
                .as_deref()
                .is_some_and(|sprite| stale.contains(&material_key(sprite)))
        });
        dirty.extend(sprites.map(|mesh| chunk_cell(&mesh.bounds)));
        if !dirty.is_empty() {
            self.rebuild_cells(&mut faces, scene, &dirty);
        }
        self.faces = Some(faces);
        self.swap_interval = (started.elapsed() * SWAP_COST_FACTOR).clamp(SWAP_INTERVAL, MAX_SWAP_INTERVAL);
    }

    /// Keeps [`Self::drag_faces`] matching the scene while a drag hides the selection's
    /// own copy, and drops it otherwise. Only the chunks holding selected brushes are
    /// triangulated again, once per drag.
    fn update_drag_faces(&mut self, scene: &Scene, overlay: &Overlay) {
        if !overlay.hides_selection() {
            self.drag_faces = None;
            return;
        }
        let key = (scene.geometry_generation, scene.selection_generation);
        if self.drag_faces.as_ref().is_some_and(|faces| faces.key == key) {
            return;
        }
        let cells: HashSet<[i32; 3]> = scene
            .meshes
            .iter()
            .filter(|mesh| mesh.selected)
            .map(|mesh| chunk_cell(&mesh.bounds))
            .collect();
        let kept = scene
            .meshes
            .iter()
            .filter(|mesh| !mesh.selected && cells.contains(&chunk_cell(&mesh.bounds)));
        let rebuilt = self.cell_faces(kept, None, 0.0, true);
        let mesh = self.upload_cells(&rebuilt);
        self.drag_faces = Some(DragFaces { key, cells, mesh });
    }

    /// The opaque map faces to draw, each with the chunks of it to leave out: all of
    /// [`Self::faces`], or while a drag hides the selection, the chunks holding it swapped
    /// for copies without it.
    fn opaque_faces(&self) -> Vec<(&FaceMesh, Option<&HashSet<[i32; 3]>>)> {
        let Some(faces) = &self.faces else {
            return Vec::new();
        };
        match &self.drag_faces {
            Some(drag) => vec![(&faces.opaque, Some(&drag.cells)), (&drag.mesh, None)],
            None => vec![(&faces.opaque, None)],
        }
    }

    /// Keeps the edge caches matching the scene: [`Self::edges`] always, and while a drag
    /// hides the selection's own copy, [`Self::drag_edges`] too.
    fn update_edges(&mut self, scene: &Scene, overlay: &Overlay) {
        self.ensure_edges(scene);
        if !overlay.hides_selection() {
            self.drag_edges = None;
            return;
        }
        let key = (scene.geometry_generation, scene.selection_generation);
        if self.drag_edges.as_ref().is_none_or(|(built, _)| *built != key) {
            let mut vertices = Vec::new();
            for mesh in scene.meshes.iter().filter(|mesh| !mesh.selected) {
                push_edges(&mut vertices, &mesh.edges, None, mesh.color);
            }
            self.drag_edges = Some((key, self.line_mesh(&vertices)));
        }
    }

    /// The map's edges to draw: every edge, or while a drag hides the selection, every
    /// edge but the selection's, which the drag draws where it is going instead.
    fn static_edges(&self) -> Option<&LineMesh> {
        match &self.drag_edges {
            Some((_, mesh)) => mesh.as_ref(),
            None => self.edges.as_ref().map(|(_, mesh)| mesh),
        }
    }

    fn face_mesh<'a>(
        &self,
        meshes: impl Iterator<Item = &'a WireMesh>,
        remap: Option<&Remap>,
        highlight: f32,
        chunked: bool,
    ) -> (FaceMesh, Vec<TranslucentFace>) {
        let cells = self.cell_faces(meshes, remap, highlight, chunked);
        let translucent = cells
            .values()
            .flat_map(|cell| cell.translucent.iter().cloned())
            .collect();
        (self.upload_cells(&cells), translucent)
    }

    /// Triangulates meshes into chunks by where their centres fall, or into one chunk.
    fn cell_faces<'a>(
        &self,
        meshes: impl Iterator<Item = &'a WireMesh>,
        remap: Option<&Remap>,
        highlight: f32,
        chunked: bool,
    ) -> HashMap<[i32; 3], CellFaces> {
        let mut chunks: HashMap<[i32; 3], CellFaces> = HashMap::new();
        for mesh in meshes {
            let bounds = mesh.mapped_bounds(remap);
            let cell = if chunked { chunk_cell(&bounds) } else { [0; 3] };
            let chunk = chunks.entry(cell).or_insert_with(|| CellFaces {
                min: Vec3::splat(f32::INFINITY),
                max: Vec3::splat(f32::NEG_INFINITY),
                by_texture: HashMap::new(),
                translucent: Vec::new(),
            });
            chunk.min = chunk.min.min(bounds.min.as_vec3());
            chunk.max = chunk.max.max(bounds.max.as_vec3());
            if self.sprite_texture(mesh).is_some() {
                continue;
            }
            let tint = if mesh.faces.iter().any(|face| face.material.is_none()) {
                let [r, g, b, _] = egui::Rgba::from(mesh.color).to_array();
                [r, g, b]
            } else {
                [1.0, 1.0, 1.0]
            };
            for (index, face) in mesh.faces.iter().enumerate() {
                let id = match (remap, mesh.first_face) {
                    (None, Some(first)) => first.saturating_add(index as u32),
                    _ => NO_FACE,
                };
                let key = face
                    .material
                    .as_deref()
                    .map_or_else(|| WHITE_KEY.to_owned(), material_key);
                let texture = self.texture_for(&key);
                if face.decal || texture.alpha.see_through() {
                    let mut vertices = Vec::new();
                    triangulate(face, (id, texture), tint, highlight, remap, &mut vertices);
                    let center = face.vertices.iter().sum::<DVec3>() / face.vertices.len().max(1) as f64;
                    let center = remap.map_or(center, |remap| remap.apply(center));
                    chunk
                        .translucent
                        .push(TranslucentFace::new(center.as_vec3(), key, vertices, mesh.owner));
                } else {
                    triangulate(
                        face,
                        (id, texture),
                        tint,
                        highlight,
                        remap,
                        chunk.by_texture.entry(key).or_default(),
                    );
                }
            }
        }
        chunks
    }

    /// Puts every chunk's triangles into one vertex buffer, batched by texture. They are
    /// written straight into the buffer, so a large map's faces are not copied once more
    /// in memory on the way.
    fn upload_cells(&self, cells: &HashMap<[i32; 3], CellFaces>) -> FaceMesh {
        const VERTEX_SIZE: usize = std::mem::size_of::<FaceVertex>();
        let count: usize = cells
            .values()
            .flat_map(|cell| cell.by_texture.values())
            .map(Vec::len)
            .sum();
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mallet faces"),
            size: (count.max(1) * VERTEX_SIZE) as u64,
            usage: wgpu::BufferUsages::VERTEX,
            mapped_at_creation: true,
        });
        let mut built = Vec::with_capacity(cells.len());
        {
            let mut mapped = buffer.slice(..).get_mapped_range_mut();
            let mut written = 0;
            for (at, cell) in cells {
                let mut keys: Vec<&String> = cell.by_texture.keys().collect();
                keys.sort();
                let mut batches = Vec::with_capacity(keys.len());
                let first = written;
                for key in keys {
                    let bytes: &[u8] = bytemuck::cast_slice(&cell.by_texture[key]);
                    let start = written * VERTEX_SIZE;
                    mapped[start..start + bytes.len()].copy_from_slice(bytes);
                    let end = written + bytes.len() / VERTEX_SIZE;
                    batches.push((key.clone(), written as u32..end as u32));
                    written = end;
                }
                built.push(Chunk {
                    cell: *at,
                    min: cell.min,
                    max: cell.max,
                    batches,
                    triangles: (written - first) / 3,
                });
            }
        }
        buffer.unmap();
        FaceMesh { buffer, chunks: built }
    }

    fn line_mesh(&self, vertices: &[LineVertex]) -> Option<LineMesh> {
        if vertices.is_empty() {
            return None;
        }
        let buffer = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mallet lines"),
            contents: bytemuck::cast_slice(vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        Some(LineMesh {
            buffer,
            count: vertices.len() as u32,
        })
    }
}

/// The value of a future that is ready at once, as wgpu's are outside a browser.
fn ready<T>(future: impl Future<Output = T>) -> Option<T> {
    let mut future = std::pin::pin!(future);
    match future.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

/// Hashes whatever `feed` writes, to fingerprint what a viewport image was drawn from.
fn signature(feed: impl FnOnce(&mut DefaultHasher)) -> u64 {
    let mut hasher = DefaultHasher::new();
    feed(&mut hasher);
    hasher.finish()
}

/// Adds line-list vertices for edges, optionally remapped, in one colour.
fn push_edges(out: &mut Vec<LineVertex>, edges: &[[DVec3; 2]], remap: Option<&Remap>, color: Color32) {
    let color = egui::Rgba::from(color).to_array();
    for [a, b] in edges {
        for point in [a, b] {
            let point = remap.map_or(*point, |remap| remap.apply(*point));
            out.push(LineVertex {
                position: point.as_vec3().to_array(),
                color,
            });
        }
    }
}

/// Creates a viewport's image, or recreates it when its size changed or the slot switched
/// between 3D (with a depth buffer) and 2D. Returns whether it was recreated.
fn ensure_target(
    device: &wgpu::Device,
    uniform_layout: &wgpu::BindGroupLayout,
    slot: &mut Option<Target>,
    size: [u32; 2],
    with_depth: bool,
) -> bool {
    if slot
        .as_ref()
        .is_some_and(|target| target.size == size && target.depth_view.is_some() == with_depth)
    {
        return false;
    }
    let extent = wgpu::Extent3d {
        width: size[0],
        height: size[1],
        depth_or_array_layers: 1,
    };
    let color = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mallet viewport color"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let depth_view = with_depth.then(|| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("mallet viewport depth"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    });
    let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mallet viewport matrix"),
        size: std::mem::size_of::<[[f32; 4]; 4]>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let uniform_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mallet viewport matrix"),
        layout: uniform_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform_buffer.as_entire_binding(),
        }],
    });
    *slot = Some(Target {
        size,
        color_view: color.create_view(&wgpu::TextureViewDescriptor::default()),
        color,
        depth_view,
        uniform_buffer,
        uniform_group,
    });
    true
}

/// Starts drawing into a viewport target, cleared to the style's background.
fn begin_pass<'a>(encoder: &'a mut wgpu::CommandEncoder, target: &'a Target, look: &ViewStyle) -> wgpu::RenderPass<'a> {
    let background = egui::Rgba::from(look.background);
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("mallet viewport"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &target.color_view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: f64::from(background.r()),
                    g: f64::from(background.g()),
                    b: f64::from(background.b()),
                    a: 1.0,
                }),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: target
            .depth_view
            .as_ref()
            .map(|view| wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
        timestamp_writes: None,
        occlusion_query_set: None,
    })
}

/// The six clipping planes of a view-projection matrix, each as `normal.xyz, distance`,
/// with normals pointing into the visible volume. wgpu's depth runs from 0 to 1.
fn frustum_planes(matrix: Mat4) -> [Vec4; 6] {
    let row = |index| matrix.row(index);
    [
        row(3) + row(0),
        row(3) - row(0),
        row(3) + row(1),
        row(3) - row(1),
        row(2),
        row(3) - row(2),
    ]
}

/// Whether any part of a box can be inside the frustum.
fn box_visible(planes: &[Vec4; 6], min: Vec3, max: Vec3) -> bool {
    planes.iter().all(|plane| {
        let farthest = Vec3::select(plane.truncate().cmpge(Vec3::ZERO), max, min);
        plane.truncate().dot(farthest) + plane.w >= 0.0
    })
}

/// Maps a 2D view's visible area onto clip space: its two axes become x and y, and the
/// depth axis is ignored.
fn orthographic(view: &View2D, points: Vec2) -> Mat4 {
    let [horizontal, vertical, _] = view.axis.axes();
    let scale_x = (2.0 * view.scale / f64::from(points.x.max(1.0))) as f32;
    let scale_y = (2.0 * view.scale / f64::from(points.y.max(1.0))) as f32;
    let mut columns = [[0.0f32; 4]; 4];
    columns[horizontal][0] = scale_x;
    columns[vertical][1] = scale_y;
    columns[3] = [
        -(view.center.x as f32) * scale_x,
        -(view.center.y as f32) * scale_y,
        0.5,
        1.0,
    ];
    Mat4::from_cols_array_2d(&columns)
}

/// A one-pixel picture of a single colour, for a texture `size` units wide and high.
fn flat(color: Color32, size: u32) -> Mips {
    Mips::from_rgba(
        (size, size),
        Level {
            width: 1,
            height: 1,
            data: color.to_array().to_vec(),
        },
    )
}

/// Uploads a texture's mip levels, as RGBA pixels or as the DXT blocks its file stores.
/// `emissive` is the light it gives off when `lights.rad` lists its material.
fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    mips: &Mips,
    facts: MaterialFacts,
    emissive: Option<DVec3>,
) -> GpuTexture {
    let (format, block_side, block_bytes) = match mips.encoding {
        Encoding::Rgba => (COLOR_FORMAT, 1, 4),
        Encoding::Bc1 => (wgpu::TextureFormat::Bc1RgbaUnormSrgb, 4, 8),
        Encoding::Bc2 => (wgpu::TextureFormat::Bc2RgbaUnormSrgb, 4, 16),
        Encoding::Bc3 => (wgpu::TextureFormat::Bc3RgbaUnormSrgb, 4, 16),
    };
    let (width, height) = (mips.width as f32, mips.height as f32);
    let image = &mips.levels[0];
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mallet material"),
        size: wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: mips.levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (level, mip) in mips.levels.iter().enumerate() {
        let (columns, rows) = (mip.width.div_ceil(block_side), mip.height.div_ceil(block_side));
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &mip.data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(columns * block_bytes),
                rows_per_image: Some(rows),
            },
            wgpu::Extent3d {
                width: columns * block_side,
                height: rows * block_side,
                depth_or_array_layers: 1,
            },
        );
    }
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let material = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mallet material"),
        contents: bytemuck::cast_slice(&facts.uniform(emissive)),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mallet material"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: material.as_entire_binding(),
            },
        ],
    });
    GpuTexture {
        bind_group,
        width,
        height,
        alpha: facts.alpha,
        surface: facts.surface,
        facts,
        material,
    }
}

/// Hammer's pink and black checkerboard for missing textures.
fn checkerboard() -> Mips {
    const SIDE: u32 = 64;
    const CELL: u32 = 16;
    let data = (0..SIDE * SIDE)
        .flat_map(|index| {
            let (x, y) = (index % SIDE, index / SIDE);
            if (x / CELL + y / CELL).is_multiple_of(2) {
                [255, 0, 255, 255]
            } else {
                [0, 0, 0, 255]
            }
        })
        .collect();
    let size = MISSING_TEXTURE_SIZE as u32;
    Mips::from_rgba(
        (size, size),
        Level {
            width: SIDE,
            height: SIDE,
            data,
        },
    )
}

/// Fans a convex face into triangles wound counter-clockwise as seen from outside, so
/// back-face culling hides faces seen from behind, as in Hammer. Texture coordinates follow
/// Source:
/// `u = (point · axis / scale + offset) / texture width`, and likewise for `v`. The normal
/// turns with a remap, so a rotated preview is lit the way it faces. Every vertex carries
/// the face's number `id`. The pieces of overlays and decals have axes that already run
/// from 0 to 1 across the picture, and are lifted a little off the face they lie on.
fn triangulate(
    face: &SceneFace,
    (id, texture): (u32, &GpuTexture),
    tint: [f32; 3],
    highlight: f32,
    remap: Option<&Remap>,
    out: &mut Vec<FaceVertex>,
) {
    let color = [tint[0], tint[1], tint[2], 1.0];
    let normal = remap
        .map_or(face.normal, |remap| {
            remap.matrix.transform_vector3(face.normal).normalize_or(face.normal)
        })
        .as_vec3()
        .to_array();
    let coordinate = |point: DVec3, axis: &mallet_vmf::TexAxis, size: f32| {
        let scale = if axis.scale == 0.0 { 1.0 } else { axis.scale };
        let size = if face.decal { 1.0 } else { size };
        ((point.dot(axis.axis) / scale + axis.offset) / f64::from(size)) as f32
    };
    let lift = if face.decal {
        face.normal * DECAL_LIFT
    } else {
        DVec3::ZERO
    };
    let vertex = |point: DVec3| FaceVertex {
        position: remap
            .map_or(point + lift, |remap| remap.apply(point + lift))
            .as_vec3()
            .to_array(),
        normal,
        uv: [
            coordinate(point, &face.uaxis, texture.width),
            coordinate(point, &face.vaxis, texture.height),
        ],
        color,
        highlight,
        alpha: alpha_uniform(texture.alpha),
        face: if face.decal { NO_FACE } else { id },
    };
    let points = &face.vertices;
    if points.len() < 3 {
        return;
    }
    let counter_clockwise = (points[1] - points[0]).cross(points[2] - points[0]).dot(face.normal) >= 0.0;
    for index in 1..points.len() - 1 {
        let (second, third) = if counter_clockwise {
            (index, index + 1)
        } else {
            (index + 1, index)
        };
        out.push(vertex(points[0]));
        out.push(vertex(points[second]));
        out.push(vertex(points[third]));
    }
}

/// The combined view and projection matrix for the camera, with the camera's horizontal
/// field of view. Nothing beyond the camera's draw distance is drawn, and the frustum's
/// far plane makes culling skip it too.
fn view_projection(camera: &Camera3D, aspect: f32) -> Mat4 {
    let far = (camera.far as f32).max(NEAR * 2.0);
    let horizontal = (camera.fov as f32).to_radians();
    let vertical = 2.0 * ((horizontal * 0.5).tan() / aspect).atan();
    let projection = Mat4::perspective_rh(vertical, aspect, NEAR, far);
    let view = Mat4::look_to_rh(camera.position.as_vec3(), camera.forward().as_vec3(), Vec3::Z);
    projection * view
}

/// The chunk a mesh's faces go into, by where its centre falls.
fn chunk_cell(bounds: &mallet_vmf::geometry::Bounds) -> [i32; 3] {
    (bounds.center() / CHUNK_SIZE).floor().as_ivec3().to_array()
}

/// How strongly faces the texture tool has selected are tinted red.
const FACE_TINT: f32 = 0.18;

/// The faces the texture tool has selected, as meshes holding just those faces and their
/// edges, so the selection tint and outline can draw them.
fn selected_face_meshes(scene: &Scene) -> Vec<WireMesh> {
    scene
        .meshes
        .iter()
        .filter(|mesh| !mesh.selected && mesh.faces.iter().any(|face| face.selected))
        .map(|mesh| {
            let faces: Vec<SceneFace> = mesh.faces.iter().filter(|face| face.selected).cloned().collect();
            let edges = faces
                .iter()
                .flat_map(|face| {
                    let count = face.vertices.len();
                    (0..count).map(move |index| [face.vertices[index], face.vertices[(index + 1) % count]])
                })
                .collect();
            WireMesh {
                faces,
                edges,
                selected: true,
                first_face: None,
                ..mesh.clone()
            }
        })
        .collect()
}

fn material_key(material: &str) -> String {
    material.replace('\\', "/").to_ascii_lowercase()
}

/// Pulls blended faces a few depth steps towards the camera, so overlays and decals
/// never flicker against the brush face they lie on, however far away it is.
const TRANSLUCENT_BIAS: wgpu::DepthBiasState = wgpu::DepthBiasState {
    constant: -8,
    slope_scale: -1.0,
    clamp: 0.0,
};

#[allow(clippy::too_many_arguments)]
fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    (vertex_entry, fragment_entry): (&str, &str),
    buffers: &[wgpu::VertexBufferLayout<'_>],
    topology: wgpu::PrimitiveTopology,
    depth: Depth,
    cull_back_faces: bool,
) -> wgpu::RenderPipeline {
    let depth_stencil = match depth {
        Depth::None => None,
        Depth::Test | Depth::Overdraw | Depth::Ignore | Depth::Translucent => Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: matches!(depth, Depth::Test | Depth::Overdraw),
            depth_compare: match depth {
                Depth::Test | Depth::Translucent => wgpu::CompareFunction::Less,
                Depth::Overdraw => wgpu::CompareFunction::LessEqual,
                _ => wgpu::CompareFunction::Always,
            },
            stencil: wgpu::StencilState::default(),
            bias: if depth == Depth::Translucent {
                TRANSLUCENT_BIAS
            } else {
                wgpu::DepthBiasState::default()
            },
        }),
    };
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
            topology,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: cull_back_faces.then_some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment_entry),
            targets: &[Some(wgpu::ColorTargetState {
                format: COLOR_FORMAT,
                blend: (depth == Depth::Translucent).then_some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        multiview: None,
        cache: None,
    })
}
