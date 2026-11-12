//! Hammer's displacement Sculpt tool on the editor's side: the Sculpt panel's settings,
//! and one step of a stroke from a point of a 3D view, which the window and the MCP
//! tools share.

use std::collections::BTreeSet;

use eframe::egui::{Pos2, Rect};
use glam::{DVec2, DVec3};
use mallet_vmf::displacement::{self, SculptBrush, SculptMode, StrokeMemory, CARVE_PROFILE};

use super::displacements::split;
use super::Editor;

/// The sculpt brush's radius on the screen until another is chosen, in points.
pub const DEFAULT_SCULPT_RADIUS: f64 = 60.0;

/// Which of the Sculpt panel's tools a stroke uses; Shift smooths with any of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SculptTool {
    /// Hammer's Push: moves the vertices under the brush, most in the middle.
    #[default]
    Push,
    /// Hammer's Carve: moves them by a profile across the stroke.
    Carve,
    /// Hammer's Blend: paints a four-texture material's textures.
    Blend,
}

/// Which way Push and Carve move vertices: Hammer's Normal Direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SculptDirection {
    /// The surface's normal under the middle of the brush.
    #[default]
    BrushCenter,
    /// Towards the camera.
    Screen,
    X,
    Y,
    Z,
    /// The normal of the face picked with Alt+right-click.
    Selected,
}

impl SculptDirection {
    pub const ALL: [SculptDirection; 6] = [
        SculptDirection::BrushCenter,
        SculptDirection::Screen,
        SculptDirection::X,
        SculptDirection::Y,
        SculptDirection::Z,
        SculptDirection::Selected,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SculptDirection::BrushCenter => "Brush Center",
            SculptDirection::Screen => "Screen",
            SculptDirection::X => "X",
            SculptDirection::Y => "Y",
            SculptDirection::Z => "Z",
            SculptDirection::Selected => "Selected",
        }
    }
}

/// The Sculpt panel's settings, as Hammer's Sculpt dialog has them.
#[derive(Clone, Debug, PartialEq)]
pub struct SculptSettings {
    pub tool: SculptTool,
    /// The brush's radius on the screen, in points.
    pub radius: f64,
    /// Hammer's Offset Mode: Adaptive moves vertices by `percent` of the brush's size in
    /// the world, Absolute by `distance` units.
    pub adaptive: bool,
    pub distance: f64,
    pub percent: f64,
    /// How much moved vertices are evened out, from 0 to 100.
    pub smoothing: f64,
    /// Hammer's density: Attenuated moves each vertex once a stroke, however often the
    /// brush passes, and Additive every step.
    pub attenuated: bool,
    pub direction: SculptDirection,
    /// The direction Alt+right-click picked, for [`SculptDirection::Selected`].
    pub selected: DVec3,
    /// Push's Falloff Position and Falloff Final, from 0 to 100.
    pub falloff_position: f64,
    pub falloff_final: f64,
    /// Carve's profile across the stroke, heights from 0 to 1 from its left edge to
    /// its right.
    pub profile: Vec<f64>,
    /// The texture Blend paints, from 0 to 3, and how much each step adds of it, from
    /// 0 to 100.
    pub texture: usize,
    pub blend_amount: f64,
    /// Sew the edges of sculpted displacements to their selected neighbours after every
    /// step.
    pub autosew: bool,
}

impl Default for SculptSettings {
    fn default() -> Self {
        Self {
            tool: SculptTool::Push,
            radius: DEFAULT_SCULPT_RADIUS,
            adaptive: false,
            distance: 8.0,
            percent: 10.0,
            smoothing: 0.0,
            attenuated: false,
            direction: SculptDirection::BrushCenter,
            selected: DVec3::Z,
            falloff_position: 25.0,
            falloff_final: 0.0,
            profile: CARVE_PROFILE.to_vec(),
            texture: 1,
            blend_amount: 25.0,
            autosew: true,
        }
    }
}

impl Editor {
    /// One step of a sculpt stroke on the displacements among `sides`, with the brush at
    /// `center` of the 3D view `rect` after being at `previous` the step before.
    /// `reverse` (Ctrl) pulls instead of pushing and takes a texture away instead of
    /// adding it; `smooth` (Shift) evens the surface out instead. Steps with the same
    /// `stroke` undo together. Returns how many displacements changed; nothing changes
    /// when Brush Center or Adaptive need a displacement under the middle of the brush
    /// and there is none.
    #[allow(clippy::too_many_arguments)]
    pub fn sculpt_step(
        &mut self,
        sides: &BTreeSet<u32>,
        settings: &SculptSettings,
        rect: Rect,
        center: DVec2,
        previous: DVec2,
        reverse: bool,
        smooth: bool,
        memory: &mut StrokeMemory,
        stroke: u64,
    ) -> Result<usize, String> {
        let (owners, mut patches) = split(self.displacement_patches(sides));
        if patches.is_empty() {
            return Err("select the displacements to sculpt first".to_owned());
        }
        let camera = &self.camera;
        let (origin, ray) = camera.ray(rect, Pos2::new(center.x as f32, center.y as f32));
        let hit = self.displacement_hit(sides, origin, ray);
        let mode = match (smooth, settings.tool) {
            (true, _) => SculptMode::Smooth,
            (false, SculptTool::Push) => SculptMode::Push,
            (false, SculptTool::Carve) => SculptMode::Carve,
            (false, SculptTool::Blend) => SculptMode::Blend,
        };
        let direction = match settings.direction {
            SculptDirection::BrushCenter => hit.map(|(_, normal)| normal),
            SculptDirection::Screen => Some(-camera.forward()),
            SculptDirection::X => Some(DVec3::X),
            SculptDirection::Y => Some(DVec3::Y),
            SculptDirection::Z => Some(DVec3::Z),
            SculptDirection::Selected => Some(settings.selected),
        };
        let amount = match mode {
            SculptMode::Blend => Some(settings.blend_amount / 100.0),
            _ if settings.adaptive => hit.map(|(point, _)| {
                let units_per_point = camera.depth(point).max(1.0) / camera.focal(rect);
                settings.percent / 100.0 * settings.radius * units_per_point
            }),
            _ => Some(settings.distance),
        };
        let (Some(direction), Some(amount)) = (direction.or((mode == SculptMode::Blend).then_some(DVec3::Z)), amount)
        else {
            return Ok(0);
        };
        let brush = SculptBrush {
            mode,
            center,
            previous,
            radius: settings.radius.max(1.0),
            direction: if reverse { -direction } else { direction },
            amount,
            falloff_start: settings.falloff_position / 100.0,
            falloff_end: settings.falloff_final / 100.0,
            smoothing: settings.smoothing / 100.0,
            attenuated: settings.attenuated,
            profile: settings.profile.clone(),
            texture: settings.texture,
            erase: reverse,
        };
        let project = |point: DVec3| {
            camera
                .project(rect, point)
                .map(|at| DVec2::new(f64::from(at.x), f64::from(at.y)))
        };
        let mut changed = displacement::sculpt(&mut patches, &brush, &project, memory);
        if settings.autosew && mode != SculptMode::Blend && !changed.is_empty() {
            changed.extend(displacement::sew(&mut patches));
        }
        self.store_patches(&owners, &patches, &changed, Some(stroke))
    }
}
