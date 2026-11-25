//! Hammer++'s selection glow: selected props are not tinted but drawn with a glowing band
//! just outside their silhouette. The selected props are drawn into a one-channel mask the
//! size of the viewport, then one full-screen pass paints the band around the mask's edge.

use eframe::egui::{self, Color32};
use eframe::wgpu::{self, util::DeviceExt};

use super::{model_buffer_layouts, Batches, ModelInstanceData, Renderer3D, COLOR_FORMAT};
use crate::scene::{Overlay, Scene};

/// The format of the mask the selected props are drawn into.
const MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
/// How many pixels the glow reaches beyond a prop's silhouette.
const GLOW_PIXELS: f32 = 3.0;

/// The pipelines and settings shared by every viewport's glow.
pub(super) struct GlowPipelines {
    /// Draws selected props into the mask.
    mask: wgpu::RenderPipeline,
    /// Paints the band around the mask onto the viewport.
    glow: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    /// The glow's colour and reach, as `glow.wgsl` reads them.
    settings: wgpu::Buffer,
}

/// One viewport's mask and the bind group that reads it.
pub(super) struct GlowTarget {
    size: [u32; 2],
    mask_view: wgpu::TextureView,
    group: wgpu::BindGroup,
}

impl GlowPipelines {
    /// Builds the pipelines. `face_layout` and `shader` are the textured view's, whose
    /// model vertex shader places the props in the mask exactly where they are drawn.
    pub(super) fn new(device: &wgpu::Device, face_layout: &wgpu::PipelineLayout, shader: &wgpu::ShaderModule) -> Self {
        let model_buffers = model_buffer_layouts();
        let mask = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mallet selection mask"),
            layout: Some(face_layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vs_model"),
                buffers: &model_buffers,
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fs_mask"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: MASK_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::RED,
                })],
                compilation_options: Default::default(),
            }),
            multiview: None,
            cache: None,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mallet selection glow"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let glow_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mallet selection glow"),
            source: wgpu::ShaderSource::Wgsl(include_str!("glow.wgsl").into()),
        });
        let glow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mallet selection glow"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let glow = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mallet selection glow"),
            layout: Some(&glow_layout),
            vertex: wgpu::VertexState {
                module: &glow_shader,
                entry_point: Some("vs_glow"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &glow_shader,
                entry_point: Some("fs_glow"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COLOR_FORMAT,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview: None,
            cache: None,
        });
        let settings = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mallet selection glow"),
            contents: bytemuck::cast_slice(&[1.0f32, 1.0, 0.0, GLOW_PIXELS]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        Self {
            mask,
            glow,
            layout,
            settings,
        }
    }

    /// A viewport's mask, made again when the viewport changed size.
    fn target(&self, device: &wgpu::Device, slot: &mut Option<GlowTarget>, size: [u32; 2]) {
        if slot.as_ref().is_some_and(|target| target.size == size) {
            return;
        }
        let mask = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mallet selection mask"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: MASK_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let mask_view = mask.create_view(&wgpu::TextureViewDescriptor::default());
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mallet selection glow"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.settings.as_entire_binding(),
                },
            ],
        });
        *slot = Some(GlowTarget { size, mask_view, group });
    }
}

impl Renderer3D {
    /// The selected props, placed where the view draws them, for the selection glow.
    /// Editor models of point entities are left out unless `helpers` is set, as they are
    /// when drawing.
    pub(super) fn selected_model_instances(&self, scene: &Scene, overlay: &Overlay, helpers: bool) -> Option<Batches> {
        let mut by_model: Vec<(&str, Vec<ModelInstanceData>)> = Vec::new();
        for mesh in scene.meshes.iter().filter(|mesh| mesh.selected) {
            let Some(instance) = mesh.model.as_ref().filter(|instance| helpers || !instance.helper) else {
                continue;
            };
            if self.gpu_models.get(&instance.key).is_none_or(Option::is_none) {
                continue;
            }
            let (_, remap) = mesh.appearance(overlay);
            let transform = remap.map_or(instance.transform, |remap| remap.matrix() * instance.transform);
            let data = ModelInstanceData {
                transform: transform.as_mat4().to_cols_array(),
                tint: [1.0, 1.0, 1.0, 0.0],
            };
            match by_model.iter_mut().find(|(key, _)| *key == instance.key) {
                Some((_, instances)) => instances.push(data),
                None => by_model.push((instance.key.as_str(), vec![data])),
            }
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
            label: Some("mallet selected props"),
            contents: bytemuck::cast_slice(&all),
            usage: wgpu::BufferUsages::VERTEX,
        });
        Some((buffer, ranges))
    }

    /// Makes sure a 3D viewport has a mask of its size for the selection glow.
    pub(super) fn ensure_glow_target(&mut self, slot: usize, size: [u32; 2]) {
        self.glow.target(&self.device, &mut self.glow_targets[slot], size);
    }

    /// Draws the glow around the selected props in `selected` over a 3D viewport's image,
    /// in `color`. The viewport's uniforms must already hold this frame's camera.
    pub(super) fn draw_selection_glow(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        slot: usize,
        (instances, ranges): &Batches,
        color: Color32,
    ) {
        let (Some(target), Some(glow)) = (self.targets[slot].as_ref(), self.glow_targets[slot].as_ref()) else {
            return;
        };
        let rgba = egui::Rgba::from(color);
        self.queue.write_buffer(
            &self.glow.settings,
            0,
            bytemuck::cast_slice(&[rgba.r(), rgba.g(), rgba.b(), GLOW_PIXELS]),
        );
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mallet selection mask"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &glow.mask_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.glow.mask);
            pass.set_bind_group(0, &target.uniform_group, &[]);
            pass.set_vertex_buffer(1, instances.slice(..));
            for (key, range) in ranges {
                let Some(Some(model)) = self.gpu_models.get(key) else {
                    continue;
                };
                pass.set_vertex_buffer(0, model.vertices.slice(..));
                pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
                for (material, indices) in &model.meshes {
                    pass.set_bind_group(1, &self.texture_for(material).bind_group, &[]);
                    pass.draw_indexed(indices.clone(), 0, range.clone());
                }
            }
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mallet selection glow"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.color_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.glow.glow);
        pass.set_bind_group(0, &glow.group, &[]);
        pass.draw(0..3, 0..1);
    }
}
