//! The 2D skybox behind everything in the 3D camera views, as Hammer++ shows it: the six
//! images worldspawn's sky name picks, or Garry's Mod's painted sky, in every direction
//! no face covers. The textured view and the lighting preview both draw it; only the
//! lighting preview adds the sun's glow.

use std::sync::mpsc;

use eframe::wgpu::{self, util::DeviceExt};
use glam::DVec3;
use mallet_content::MaterialLoader;

use super::lit::{square, ImageArray, Loading};
use super::{pipeline, Depth};
use crate::scene::Scene;

/// The suffixes of a skybox's six images, in the order the shader's layers use.
const SUFFIXES: [&str; 6] = ["rt", "lf", "bk", "ft", "up", "dn"];
/// Pixels across each skybox image once loaded.
const FACE_SIZE: u32 = 512;

/// What [`SkyBackground::draw`] shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Nothing: the view keeps its background colour.
    None,
    Skybox,
    Painted,
}

/// The skybox images, their loading and the pipeline that draws them.
pub(super) struct SkyBackground {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    images: ImageArray,
    /// The sky name last loaded, and whether all six of its images were found.
    name: Option<String>,
    ready: bool,
    loading: Loading<String>,
    group: Option<wgpu::BindGroup>,
    mode: Mode,
    /// Counts finished loads, so cached viewport images know when to redraw.
    version: u64,
}

impl SkyBackground {
    /// Makes the pipeline, drawing into views whose camera uniforms use `view_layout`.
    pub(super) fn new(device: &wgpu::Device, queue: &wgpu::Queue, view_layout: &wgpu::BindGroupLayout) -> Self {
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mallet sky"),
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
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: fragment,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: fragment,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mallet sky"),
            bind_group_layouts: &[view_layout, &layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mallet sky shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("sky.wgsl").into()),
        });
        let pipeline = pipeline(
            device,
            &pipeline_layout,
            &shader,
            ("vs_sky", "fs_sky"),
            &[],
            wgpu::PrimitiveTopology::TriangleList,
            Depth::Ignore,
            false,
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("mallet sky"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mallet sky"),
            contents: bytemuck::cast_slice(&[0.0f32; 24]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        Self {
            pipeline,
            layout,
            sampler,
            uniform,
            images: ImageArray::new(device, queue, 1, &[], "mallet skybox"),
            name: None,
            ready: false,
            loading: None,
            group: None,
            mode: Mode::None,
            version: 0,
        }
    }

    /// Counts finished loads, for the signatures of cached viewport images.
    pub(super) fn version(&self) -> u64 {
        self.version
    }

    /// Whether a skybox is still loading.
    pub(super) fn loading(&self) -> bool {
        self.loading.is_some()
    }

    /// Starts loading `scene`'s skybox images when its sky name changed, and takes in the
    /// images that finished loading.
    pub(super) fn update(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, scene: &Scene, loader: Option<&MaterialLoader>) {
        let name = scene.lighting.sky.name.trim().to_ascii_lowercase();
        let wanted = !name.is_empty()
            && name != "painted"
            && self.name.as_deref() != Some(name.as_str())
            && self.loading.as_ref().is_none_or(|(loading, _)| *loading != name);
        if let (true, Some(loader)) = (wanted, loader) {
            let (sender, receiver) = mpsc::channel();
            let loader = loader.clone();
            let sky = name.clone();
            let spawned = std::thread::Builder::new()
                .name("mallet skybox".to_owned())
                .spawn(move || {
                    let faces = SUFFIXES
                        .iter()
                        .map(|suffix| {
                            let material = loader.load(&format!("skybox/{sky}{suffix}"));
                            let image = material.texture?;
                            let half = image.pixel_width >= image.pixel_height * 2;
                            Some(square(&image, FACE_SIZE, half))
                        })
                        .collect();
                    let _ = sender.send(faces);
                });
            if spawned.is_ok() {
                self.loading = Some((name.clone(), receiver));
            }
        }
        self.poll(device, queue, false);
        let painted = name == "painted" && scene.lighting.sky.paint.is_some();
        let loaded = self.ready && self.name.as_deref() == Some(name.as_str());
        self.mode = match (painted, loaded) {
            (true, _) => Mode::Painted,
            (false, true) => Mode::Skybox,
            (false, false) => Mode::None,
        };
    }

    /// Waits for the skybox images still loading and takes them in, as screenshots do.
    pub(super) fn finish(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        self.poll(device, queue, true);
    }

    /// Takes in the skybox images that finished loading; with `wait`, waits for them.
    fn poll(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, wait: bool) {
        if let Some((loaded, receiver)) = self.loading.take() {
            let received = if wait {
                receiver.recv().map_err(|_| mpsc::TryRecvError::Disconnected)
            } else {
                receiver.try_recv()
            };
            match received {
                Ok(faces) => {
                    self.ready = faces.iter().all(Option::is_some);
                    let layers: Vec<Vec<u8>> = faces.into_iter().flatten().collect();
                    self.images = if self.ready {
                        ImageArray::new(device, queue, FACE_SIZE, &layers, "mallet skybox")
                    } else {
                        ImageArray::new(device, queue, 1, &[], "mallet skybox")
                    };
                    self.name = Some(loaded);
                    self.group = None;
                    self.version += 1;
                }
                Err(mpsc::TryRecvError::Empty) => self.loading = Some((loaded, receiver)),
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }
        if self.group.is_none() {
            self.group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mallet sky"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&self.images.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            }));
        }
    }

    /// Writes what the next draw shows: the sky of `scene`, with the sun's glow when `glow`
    /// is set, as the lighting preview shows it.
    pub(super) fn prepare(&self, queue: &wgpu::Queue, scene: &Scene, glow: bool) {
        let sky = &scene.lighting.sky;
        let rgb = |color: DVec3| color.as_vec3().to_array();
        let four = |[r, g, b]: [f32; 3], w: f32| [r, g, b, w];
        let mode = match self.mode {
            Mode::None => 0.0,
            Mode::Skybox => 2.0,
            Mode::Painted => 3.0,
        };
        let mut data = [[0.0f32; 4]; 6];
        data[0] = [mode, 0.0, 0.0, 0.0];
        data[4] = [0.0, 0.0, 1.0, 2.0];
        if let Some(paint) = &sky.paint {
            data[1] = four(rgb(paint.top), paint.fade_bias as f32);
            data[2] = four(rgb(paint.bottom), paint.dusk_scale as f32);
            data[3] = four(rgb(paint.dusk_color), paint.dusk_intensity as f32);
        }
        let sun = sky.sun.or_else(|| {
            sky.paint
                .filter(|_| self.mode == Mode::Painted)
                .map(|paint| (paint.sun_normal, paint.sun_color * 4.0, paint.sun_size.max(0.5)))
        });
        if let (Some((toward, color, size)), true) = (sun, glow) {
            let toward = toward.normalize_or(DVec3::Z);
            data[4] = four(rgb(toward), size.clamp(0.1, 30.0).to_radians().cos() as f32);
            data[5] = four(rgb(color), 0.0);
        }
        queue.write_buffer(&self.uniform, 0, bytemuck::cast_slice(&data));
    }

    /// Draws the sky into `pass`, whose camera uniforms are bound to group 0, before
    /// anything else, so every face covers it. Draws nothing when the map has no sky.
    pub(super) fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        let (Mode::Skybox | Mode::Painted, Some(group)) = (self.mode, &self.group) else {
            return;
        };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(1, group, &[]);
        pass.draw(0..36, 0..1);
    }
}
