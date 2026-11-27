//! The 3D skybox in the camera views, as the game shows it: the room around the
//! sky_camera, drawn behind the map from where the camera would stand in it, so it looks
//! the sky camera's scale times larger than it is built. The map's sky faces open onto
//! it, as they do in the game, and the 2D skybox shows behind it.

use glam::{DMat4, DQuat, DVec3, Mat4, Vec3, Vec4};
use mallet_content::Surface;
use mallet_vmf::geometry::Bounds;

use super::{box_visible, frustum_planes, lit, FaceMesh, Renderer3D, RenderStats, TranslucentFace, WHITE_KEY};
use crate::scene::Scene;
use crate::view3d::Camera3D;

/// The scale a sky_camera without one of its own uses.
const DEFAULT_SCALE: f64 = 16.0;

/// The 3D skybox's faces, kept apart from the map's so they can be drawn from the sky
/// camera, rebuilt when the map or a texture changes.
pub(super) struct SkyboxFaces {
    /// The geometry generation and textures version these were built from.
    key: (u64, u64),
    /// The room the sky_camera stands in; `None` without a sky_camera or a room around it.
    bounds: Option<Bounds>,
    pub(super) opaque: Option<FaceMesh>,
    pub(super) translucent: Vec<TranslucentFace>,
}

/// Where the 3D skybox is seen from in one frame.
pub(super) struct SkyboxView {
    /// The camera's view and projection, with the skybox moved and scaled to where the
    /// game shows it.
    pub(super) matrix: Mat4,
    /// Where the camera stands in the skybox.
    pub(super) eye: Vec3,
    /// The clipping planes of `matrix`, in the skybox's own coordinates.
    pub(super) planes: [Vec4; 6],
    pub(super) bounds: Bounds,
}

impl Renderer3D {
    /// Keeps [`Self::skybox`] matching the map: finds the room around the sky_camera
    /// and triangulates the brushes whose middles are in it.
    pub(super) fn update_skybox_faces(&mut self, scene: &Scene) {
        let key = (scene.geometry_generation, self.textures_version);
        if self.skybox.as_ref().is_some_and(|faces| faces.key == key) {
            return;
        }
        let bounds = lit::skybox_bounds(scene);
        let (opaque, translucent) = match &bounds {
            Some(room) => {
                let inside = scene
                    .meshes
                    .iter()
                    .filter(|mesh| mesh.model.is_none() && !mesh.bounds.is_empty() && contains(room, mesh.bounds.center()));
                let cells = self.cell_faces(inside, None, 0.0, true);
                let translucent = cells
                    .values()
                    .flat_map(|cell| cell.translucent.iter().cloned())
                    .collect();
                (Some(self.upload_cells(&cells)), translucent)
            }
            None => (None, Vec::new()),
        };
        self.skybox = Some(SkyboxFaces {
            key,
            bounds,
            opaque,
            translucent,
        });
    }

    /// Where the 3D skybox is seen from by `camera`, or `None` when the map has none or
    /// the camera is inside it, where the skybox is drawn as it is built.
    pub(super) fn skybox_view(&self, scene: &Scene, camera: &Camera3D, aspect: f32) -> Option<SkyboxView> {
        let bounds = self.skybox.as_ref()?.bounds?;
        let origin = scene.lighting.sky.camera?;
        if contains(&bounds, camera.position) {
            return None;
        }
        let scale = Some(scene.lighting.sky.camera_scale)
            .filter(|scale| *scale > 0.0)
            .unwrap_or(DEFAULT_SCALE);
        let to_map = DMat4::from_scale_rotation_translation(DVec3::splat(scale), DQuat::IDENTITY, -origin * scale);
        let matrix = (camera.view_projection(f64::from(aspect)) * to_map).as_mat4();
        Some(SkyboxView {
            matrix,
            eye: (origin + camera.position / scale).as_vec3(),
            planes: frustum_planes(matrix),
            bounds,
        })
    }

    /// Whether faces of material `key` are left out of the 3D skybox, as the game leaves
    /// them out: sky, tool and nodraw faces and editor helpers.
    pub(super) fn hidden_in_skybox(&self, key: &str) -> bool {
        key == WHITE_KEY
            || matches!(
                self.texture_for(key).surface,
                Surface::Sky | Surface::Tool | Surface::Nodraw
            )
    }

    /// Draws the 3D skybox's opaque faces with the textured view's pipeline.
    pub(super) fn draw_skybox_faces<'a>(
        &'a self,
        pass: &mut eframe::wgpu::RenderPass<'a>,
        planes: &[Vec4; 6],
        stats: &mut RenderStats,
    ) {
        let Some(mesh) = self.skybox.as_ref().and_then(|faces| faces.opaque.as_ref()) else {
            return;
        };
        pass.set_pipeline(&self.face_pipeline);
        draw_chunks(self, pass, mesh, planes, stats);
    }

    /// Writes the depth of the map's sky faces without drawing them, so the 3D skybox
    /// drawn before shows through them while anything behind them stays hidden.
    pub(super) fn draw_sky_masks<'a>(&'a self, pass: &mut eframe::wgpu::RenderPass<'a>, planes: &[Vec4; 6]) {
        pass.set_pipeline(&self.sky_mask_pipeline);
        for (mesh, skipped) in self.opaque_faces() {
            for chunk in mesh.chunks.iter().filter(|chunk| {
                skipped.is_none_or(|cells| !cells.contains(&chunk.cell)) && box_visible(planes, chunk.min, chunk.max)
            }) {
                let sky = chunk
                    .batches
                    .iter()
                    .filter(|(key, _)| key != WHITE_KEY && self.texture_for(key).surface == Surface::Sky);
                let mut bound = false;
                for (key, range) in sky {
                    if !bound {
                        pass.set_vertex_buffer(0, mesh.buffer_of(chunk).slice(..));
                        bound = true;
                    }
                    pass.set_bind_group(1, &self.texture_for(key).bind_group, &[]);
                    pass.draw(range.clone(), 0..1);
                }
            }
        }
    }
}

/// Draws the chunks of `mesh` inside `planes` with the pipeline already set, leaving out
/// what the 3D skybox does not show.
pub(super) fn draw_chunks<'a>(
    renderer: &'a Renderer3D,
    pass: &mut eframe::wgpu::RenderPass<'a>,
    mesh: &'a FaceMesh,
    planes: &[Vec4; 6],
    stats: &mut RenderStats,
) {
    for chunk in mesh.chunks.iter().filter(|chunk| box_visible(planes, chunk.min, chunk.max)) {
        pass.set_vertex_buffer(0, mesh.buffer_of(chunk).slice(..));
        stats.chunks_drawn += 1;
        stats.triangles_drawn += chunk.triangles;
        for (key, range) in chunk.batches.iter().filter(|(key, _)| !renderer.hidden_in_skybox(key)) {
            pass.set_bind_group(1, &renderer.texture_for(key).bind_group, &[]);
            pass.draw(range.clone(), 0..1);
        }
    }
}

/// Whether `point` is in `bounds`, give or take a unit.
pub(super) fn contains(bounds: &Bounds, point: DVec3) -> bool {
    point.cmpge(bounds.min - DVec3::ONE).all() && point.cmple(bounds.max + DVec3::ONE).all()
}
