//! The texture tool's Displacement tab and painting displacements in the 3D view, as
//! Hammer's Face Edit sheet does them: Create, Destroy, Paint Geometry, Paint Alpha,
//! Invert Alpha, Subdivide, Noise and Sew, with the settings of the tool in use.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, PointerButton, Pos2, Rect, Response, RichText, Stroke};
use glam::DVec3;
use mallet_vmf::displacement::{Effect, Falloff, MAX_POWER, MIN_POWER};

use super::{MalletApp, PointerState};
use crate::editor::{effect_label, AxisChoice, DisplacementMode, Editor, Tool};
use crate::faces::FaceInfo;
use crate::scene::next_generation;
use crate::view3d::Camera3D;

/// How often a brush held still applies again.
const PAINT_INTERVAL: Duration = Duration::from_millis(60);
/// How far a dragged brush moves, as a share of its radius, before it applies again.
const PAINT_SPACING: f64 = 0.25;
const BRUSH_COLOR: Color32 = Color32::from_rgb(255, 220, 60);
const BRUSH_SEGMENTS: usize = 48;
const MUTED: Color32 = Color32::from_gray(140);
const BUTTON_SIZE: egui::Vec2 = egui::vec2(76.0, 34.0);

/// A paint stroke in progress in a 3D view.
#[derive(Clone, Copy, Debug)]
pub(super) struct PaintStroke {
    slot: usize,
    /// The undo group of the stroke's dabs.
    group: u64,
    /// Painted with the right button, so raising lowers.
    reverse: bool,
    /// When and where the brush last applied.
    last: Option<(Instant, DVec3)>,
}

/// Where the paint brush sits on the selected displacements under the mouse.
#[derive(Clone, Copy, Debug)]
pub(super) struct BrushSpot {
    slot: usize,
    point: DVec3,
    normal: DVec3,
}

/// What the Displacement tab keeps between frames: the numbers typed for the
/// Elevation and Scale attributes.
#[derive(Clone, Debug)]
pub(super) struct DisplacementDrafts {
    elevation: f64,
    scale: f64,
}

impl Default for DisplacementDrafts {
    fn default() -> Self {
        Self {
            elevation: 0.0,
            scale: 1.0,
        }
    }
}

/// A button of the Displacement tab's tool grid.
fn tool_button(ui: &mut egui::Ui, name: &str, enabled: bool, hint: &str) -> bool {
    ui.add_enabled(enabled, egui::Button::new(name).min_size(BUTTON_SIZE))
        .on_hover_text(hint)
        .on_disabled_hover_text(hint)
        .clicked()
}

/// The Displacement tab: Hammer's tools for the faces Face Edit has selected, then the
/// settings of the paint tool in use, or of Create, Noise and the attributes.
pub(super) fn displacement_tab(
    ui: &mut egui::Ui,
    editor: &mut Editor,
    faces: &[FaceInfo],
    drafts: &mut DisplacementDrafts,
) {
    let sides: BTreeSet<u32> = faces.iter().map(|face| face.side.id).collect();
    let displaced: Vec<&FaceInfo> = faces.iter().filter(|face| face.side.dispinfo().is_some()).collect();
    let any_face = !faces.is_empty();
    let any_displacement = !displaced.is_empty();
    let mut result: Option<Result<usize, String>> = None;
    ui.label(RichText::new("Tools").strong());
    egui::Grid::new("displacement_tools")
        .num_columns(5)
        .spacing([4.0, 4.0])
        .show(ui, |ui| {
            for (mode, name, hint) in [
                (
                    DisplacementMode::Select,
                    "Select",
                    "Click faces to select them, as the texture tool does",
                ),
                (
                    DisplacementMode::PaintGeometry,
                    "Paint\nGeometry",
                    "Drag over the selected displacements in the 3D view to raise them; the right button lowers",
                ),
                (
                    DisplacementMode::PaintAlpha,
                    "Paint\nAlpha",
                    "Drag over the selected displacements in the 3D view to blend in a two-texture material's \
                     second texture; the right button takes it away",
                ),
            ] {
                let on = editor.displacement.mode == mode;
                let button = egui::Button::new(name).selected(on).min_size(BUTTON_SIZE);
                if ui.add(button).on_hover_text(hint).clicked() {
                    editor.displacement.mode = mode;
                }
            }
            if tool_button(
                ui,
                "Invert\nAlpha",
                any_displacement,
                "Swap the two textures of a blended material on the selected displacements",
            ) {
                result = Some(editor.invert_displacement_alpha(&sides));
            }
            tool_button(
                ui,
                "Sculpt",
                false,
                "Sculpting is not in Mallet yet; use Paint Geometry",
            );
            ui.end_row();
            if tool_button(
                ui,
                "Create",
                any_face,
                "Make each selected four-sided face a flat displacement of the power below",
            ) {
                result = Some(editor.create_displacements(&sides, editor.displacement.power));
            }
            if tool_button(
                ui,
                "Destroy",
                any_displacement,
                "Turn the selected displacements back into plain faces",
            ) {
                result = Some(editor.destroy_displacements(&sides));
            }
            if tool_button(
                ui,
                "Subdivide",
                any_displacement,
                "Round the selected displacements off as one smooth surface, such as a box into a boulder",
            ) {
                result = Some(editor.subdivide_displacements(&sides));
            }
            if tool_button(
                ui,
                "Noise",
                any_displacement,
                "Raise the selected displacements by a random amount between Min and Max below",
            ) {
                let tool = editor.displacement.clone();
                result = Some(editor.displacement_noise(&sides, tool.noise_min, tool.noise_max, tool.rockiness));
            }
            if tool_button(
                ui,
                "Sew",
                any_displacement,
                "Join the edges and corners the selected displacements share, so no gaps open between them",
            ) {
                result = Some(editor.sew_displacements(&sides));
            }
            ui.end_row();
        });
    if let Some(Err(error)) = result {
        editor.message = error;
    }
    ui.add_space(4.0);
    ui.separator();
    match editor.displacement.mode {
        DisplacementMode::Select => {
            create_and_noise(ui, editor);
            ui.add_space(4.0);
            attributes(ui, editor, &sides, &displaced, drafts);
        }
        DisplacementMode::PaintGeometry => paint_settings(ui, editor, false),
        DisplacementMode::PaintAlpha => paint_settings(ui, editor, true),
    }
    ui.add_space(4.0);
    let summary = if faces.is_empty() {
        "No faces selected. Click faces in the 3D view; Shift+click takes a whole brush.".to_owned()
    } else {
        format!(
            "{} faces selected, {} of them displacements.",
            faces.len(),
            displaced.len()
        )
    };
    ui.label(RichText::new(summary).small().color(MUTED));
}

/// Create's power and the values of Noise.
fn create_and_noise(ui: &mut egui::Ui, editor: &mut Editor) {
    ui.horizontal(|ui| {
        ui.label("Create with power");
        for power in MIN_POWER..=MAX_POWER {
            let size = (1 << power) + 1;
            ui.radio_value(&mut editor.displacement.power, power, power.to_string())
                .on_hover_text(format!("{size} by {size} vertices"));
        }
    });
    ui.horizontal(|ui| {
        ui.label("Noise: min");
        ui.add(egui::DragValue::new(&mut editor.displacement.noise_min).speed(0.5));
        ui.label("max");
        ui.add(egui::DragValue::new(&mut editor.displacement.noise_max).speed(0.5));
        ui.label("rockiness");
        ui.add(
            egui::DragValue::new(&mut editor.displacement.rockiness)
                .speed(0.01)
                .range(0.0..=1.0),
        )
        .on_hover_text("From 0, broad and gentle, to 1, small and rough");
    });
}

/// Hammer's attributes: the selection's power, and its elevation and scale with Apply.
fn attributes(
    ui: &mut egui::Ui,
    editor: &mut Editor,
    sides: &BTreeSet<u32>,
    displaced: &[&FaceInfo],
    drafts: &mut DisplacementDrafts,
) {
    let powers: BTreeSet<String> = displaced
        .iter()
        .filter_map(|face| face.side.dispinfo()?.get("power").map(str::to_owned))
        .collect();
    let power = match powers.len() {
        0 => "none".to_owned(),
        1 => powers.into_iter().next().unwrap_or_default(),
        _ => "mixed".to_owned(),
    };
    ui.label(RichText::new("Attributes").strong());
    ui.add_enabled_ui(!displaced.is_empty(), |ui| {
        egui::Grid::new("displacement_attributes")
            .num_columns(3)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Power");
                ui.label(power);
                ui.end_row();
                ui.label("Elevation");
                ui.add(egui::DragValue::new(&mut drafts.elevation).speed(0.5));
                if ui
                    .button("Apply")
                    .on_hover_text("Lift the selected displacements this far off their faces")
                    .clicked()
                {
                    if let Err(error) = editor.set_displacement_elevation(sides, drafts.elevation) {
                        editor.message = error;
                    }
                }
                ui.end_row();
                ui.label("Scale");
                ui.add(egui::DragValue::new(&mut drafts.scale).speed(0.01));
                if ui
                    .button("Apply")
                    .on_hover_text("Multiply how far every vertex of the selected displacements is moved")
                    .clicked()
                {
                    if let Err(error) = editor.scale_displacements(sides, drafts.scale) {
                        editor.message = error;
                    }
                }
                ui.end_row();
            });
    });
}

/// The settings of Paint Geometry, or of Paint Alpha with `alpha`, as Hammer's dialogs
/// lay them out.
fn paint_settings(ui: &mut egui::Ui, editor: &mut Editor, alpha: bool) {
    let tool = &mut editor.displacement;
    ui.label(RichText::new(if alpha { "Paint Alpha" } else { "Paint Geometry" }).strong());
    egui::Grid::new("displacement_paint")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            ui.label("Effect");
            egui::ComboBox::from_id_salt("paint_effect")
                .selected_text(effect_label(tool.effect))
                .show_ui(ui, |ui| {
                    for effect in [Effect::Raise, Effect::RaiseTo, Effect::Smooth] {
                        ui.selectable_value(&mut tool.effect, effect, effect_label(effect));
                    }
                });
            ui.end_row();
            if !alpha {
                ui.label("Axis");
                egui::ComboBox::from_id_salt("paint_axis")
                    .selected_text(tool.axis.label())
                    .show_ui(ui, |ui| {
                        for axis in AxisChoice::ALL {
                            ui.selectable_value(&mut tool.axis, axis, axis.label());
                        }
                    });
                ui.end_row();
            }
            ui.label("Brush");
            ui.horizontal(|ui| {
                ui.radio_value(&mut tool.falloff, Falloff::Soft, "Soft edge");
                ui.radio_value(&mut tool.falloff, Falloff::Hard, "Hard edge");
            });
            ui.end_row();
            if alpha {
                ui.label("Value");
                ui.add(egui::Slider::new(&mut tool.alpha, 0.0..=255.0).step_by(1.0));
            } else {
                ui.label("Distance");
                ui.add(egui::Slider::new(&mut tool.distance, 0.0..=256.0).logarithmic(true));
            }
            ui.end_row();
            ui.label("Radius");
            ui.add(egui::Slider::new(&mut tool.radius, 1.0..=1024.0).logarithmic(true));
            ui.end_row();
        });
    if !alpha {
        ui.checkbox(&mut tool.autosew, "Autosew")
            .on_hover_text("Keep the edges of neighbouring selected displacements joined while painting");
    }
    let hint = match (alpha, tool.effect) {
        (_, Effect::Smooth) => "Drag over the selected displacements in the 3D view to smooth them.",
        (false, Effect::RaiseTo) => {
            "Drag over the selected displacements in the 3D view to move them to the distance from their faces."
        }
        (true, Effect::RaiseTo) => {
            "Drag over the selected displacements in the 3D view to set their alpha to the value."
        }
        (false, Effect::Raise) => {
            "Drag over the selected displacements in the 3D view: the left button raises them, the right lowers them."
        }
        (true, Effect::Raise) => {
            "Drag over the selected displacements in the 3D view: the left button blends in the second texture, \
             the right takes it away."
        }
    };
    ui.label(RichText::new(hint).small().color(MUTED));
}

impl MalletApp {
    /// Paints the selected displacements while a paint tool of the Displacement tab is
    /// in use: pressing the left or right button over them starts a stroke, which
    /// applies the brush as it moves and again every [`PAINT_INTERVAL`] while held still,
    /// all as one undo step. Returns whether the 3D view's clicks belong to painting.
    pub(super) fn paint_displacements_3d(
        &mut self,
        slot: usize,
        rect: Rect,
        response: &Response,
        editor: &mut Editor,
        pointer: &PointerState,
    ) -> bool {
        if self.brush_spot.is_some_and(|spot| spot.slot == slot) {
            self.brush_spot = None;
        }
        let painting = editor.tool == Tool::Texture && editor.displacement.painting() && !editor.camera.mouse_look();
        if !painting {
            if self.paint_stroke.is_some_and(|stroke| stroke.slot == slot) {
                self.paint_stroke = None;
            }
            return false;
        }
        let sides = editor.displacement_sides();
        let hovered = response
            .hover_pos()
            .or(pointer.position.filter(|_| response.is_pointer_button_down_on()));
        let spot = hovered.and_then(|position| {
            let (origin, direction) = editor.camera.ray(rect, position);
            editor.displacement_hit(&sides, origin, direction)
        });
        if let Some((point, normal)) = spot {
            self.brush_spot = Some(BrushSpot { slot, point, normal });
            response.ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        let (left, right) = response.ctx.input(|input| {
            (
                input.pointer.button_down(PointerButton::Primary),
                input.pointer.button_down(PointerButton::Secondary),
            )
        });
        let held = response.is_pointer_button_down_on() && (left || right);
        if !held {
            if self.paint_stroke.is_some_and(|stroke| stroke.slot == slot) {
                self.paint_stroke = None;
            }
            return true;
        }
        let stroke = self.paint_stroke.get_or_insert(PaintStroke {
            slot,
            group: next_generation(),
            reverse: right && !left,
            last: None,
        });
        if stroke.slot != slot {
            return true;
        }
        response.ctx.request_repaint_after(PAINT_INTERVAL);
        let Some((point, _)) = spot else {
            return true;
        };
        let due = stroke.last.is_none_or(|(when, at)| {
            when.elapsed() >= PAINT_INTERVAL || at.distance(point) >= editor.displacement.radius * PAINT_SPACING
        });
        if !due {
            return true;
        }
        stroke.last = Some((Instant::now(), point));
        let dab = editor.displacement.dab(point, stroke.reverse);
        let autosew = editor.displacement.autosew && editor.displacement.mode == DisplacementMode::PaintGeometry;
        if let Err(error) = editor.paint_displacements(&sides, &dab, autosew, stroke.group) {
            editor.message = error;
        }
        true
    }
}

/// The paint brush on the displacement under the mouse in viewport `slot`: a circle of
/// its radius around the spot, square to the surface there.
pub(super) fn draw_brush(
    painter: &egui::Painter,
    camera: &Camera3D,
    rect: Rect,
    slot: usize,
    spot: Option<BrushSpot>,
    radius: f64,
) {
    let Some(spot) = spot.filter(|spot| spot.slot == slot) else {
        return;
    };
    let normal = spot.normal.normalize_or(DVec3::Z);
    let side = if normal.z.abs() < 0.9 { DVec3::Z } else { DVec3::X };
    let first = normal.cross(side).normalize_or(DVec3::X);
    let second = normal.cross(first);
    let points: Vec<Option<Pos2>> = (0..=BRUSH_SEGMENTS)
        .map(|step| {
            let angle = std::f64::consts::TAU * step as f64 / BRUSH_SEGMENTS as f64;
            let point = spot.point + (first * angle.cos() + second * angle.sin()) * radius;
            camera.project(rect, point)
        })
        .collect();
    let stroke = Stroke::new(1.5_f32, BRUSH_COLOR);
    for pair in points.windows(2) {
        if let [Some(a), Some(b)] = pair {
            painter.line_segment([*a, *b], stroke);
        }
    }
    if let (Some(center), Some(tip)) = (
        camera.project(rect, spot.point),
        camera.project(rect, spot.point + normal * radius * 0.5),
    ) {
        painter.line_segment([center, tip], stroke);
        painter.circle_filled(center, 2.5, BRUSH_COLOR);
    }
}
