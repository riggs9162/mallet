//! The texture tool's Displacement tab and painting displacements in the 3D view, as
//! Hammer's Face Edit sheet does them: Create, Destroy, Paint Geometry, Paint Alpha,
//! Invert Alpha, Sculpt, Subdivide, Noise and Sew, with the settings of the tool in use,
//! and the vertices Displacement Edge Collapse shows.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, PointerButton, Pos2, Rect, Response, RichText, Stroke};
use glam::{DVec2, DVec3};
use mallet_vmf::displacement::{Effect, Falloff, StrokeMemory, CARVE_PROFILE, MAX_POWER, MIN_POWER};

use super::{MalletApp, PointerState};
use crate::editor::{
    effect_label, AxisChoice, DisplacementMode, Editor, SculptDirection, SculptSettings, SculptTool, Tool,
};
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
/// How far, in points, a sculpt brush moves before it applies again.
const SCULPT_SPACING: f32 = 3.0;
/// The smallest and largest sculpt brush radius, in points.
const SCULPT_RADII: std::ops::RangeInclusive<f64> = 4.0..=1000.0;
/// The colour of the vertices Displacement Edge Collapse shows, Hammer's pink.
const COLLAPSED_COLOR: Color32 = Color32::from_rgb(255, 0, 255);
/// Half the size, in points, of a collapsed vertex's square.
const COLLAPSED_HALF_SIZE: f32 = 3.0;

/// A sculpt stroke in progress in a 3D view.
#[derive(Debug)]
pub(super) struct SculptStroke {
    slot: usize,
    /// The undo group of the stroke's steps.
    group: u64,
    /// Ctrl was held when it began: pull, or take the texture away.
    reverse: bool,
    /// Shift was held when it began: smooth.
    smooth: bool,
    /// Where the brush last applied, and when.
    last: Option<(Instant, Pos2)>,
    /// How far Attenuated has moved each vertex this stroke.
    memory: StrokeMemory,
}

/// A right-button drag resizing the sculpt brush: where it began and the radius then.
#[derive(Clone, Copy, Debug)]
pub(super) struct BrushResize {
    slot: usize,
    start: Pos2,
    radius: f64,
}

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

/// A button of the Displacement tab's tool grid that picks the tool clicks in the 3D
/// view use.
fn mode_button(ui: &mut egui::Ui, editor: &mut Editor, mode: DisplacementMode, name: &str, hint: &str) {
    let on = editor.displacement.mode == mode;
    let button = egui::Button::new(name).selected(on).min_size(BUTTON_SIZE);
    if ui.add(button).on_hover_text(hint).clicked() {
        editor.displacement.mode = mode;
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
                mode_button(ui, editor, mode, name, hint);
            }
            if tool_button(
                ui,
                "Invert\nAlpha",
                any_displacement,
                "Swap the two textures of a blended material on the selected displacements",
            ) {
                result = Some(editor.invert_displacement_alpha(&sides));
            }
            mode_button(
                ui,
                editor,
                DisplacementMode::Sculpt,
                "Sculpt",
                "Push, carve, smooth or blend the selected displacements with a brush that is a circle on the screen",
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
        DisplacementMode::Sculpt => sculpt_settings(ui, &mut editor.displacement.sculpt),
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

/// The Sculpt panel, laid out like Hammer's: the tool, the brush, how far it moves
/// vertices and which way, and Push's falloff, Carve's profile or Blend's texture.
fn sculpt_settings(ui: &mut egui::Ui, settings: &mut SculptSettings) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Sculpt").strong());
        for (tool, name, hint) in [
            (
                SculptTool::Push,
                "Push",
                "Push the vertices under the brush out, most in the middle",
            ),
            (
                SculptTool::Carve,
                "Carve",
                "Carve the profile below along the stroke, as for a road or a ditch",
            ),
            (
                SculptTool::Blend,
                "Blend",
                "Paint one of the textures of a four-texture blend material, such as a Lightmapped_4WayBlend",
            ),
        ] {
            ui.selectable_value(&mut settings.tool, tool, name).on_hover_text(hint);
        }
    });
    egui::Grid::new("sculpt_settings")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            ui.label("Brush size");
            ui.add(
                egui::Slider::new(&mut settings.radius, SCULPT_RADII)
                    .logarithmic(true)
                    .suffix(" px"),
            )
            .on_hover_text("The brush's radius on the screen; right-drag sideways in the 3D view to change it");
            ui.end_row();
            if settings.tool == SculptTool::Blend {
                ui.label("Texture");
                ui.horizontal(|ui| {
                    for texture in 0..4 {
                        ui.radio_value(&mut settings.texture, texture, (texture + 1).to_string());
                    }
                });
                ui.end_row();
                ui.label("Amount");
                ui.add(egui::Slider::new(&mut settings.blend_amount, 0.0..=100.0).suffix(" %"))
                    .on_hover_text("How much of the texture each step paints in the middle of the brush");
                ui.end_row();
                return;
            }
            ui.label("Offset mode");
            ui.horizontal(|ui| {
                ui.radio_value(&mut settings.adaptive, false, "Absolute")
                    .on_hover_text("Move vertices by the distance below");
                ui.radio_value(&mut settings.adaptive, true, "Adaptive").on_hover_text(
                    "Move vertices by a share of the brush's size in the world, so near and far feel alike",
                );
            });
            ui.end_row();
            if settings.adaptive {
                ui.label("Offset percent");
                ui.add(egui::Slider::new(&mut settings.percent, 0.0..=100.0).suffix(" %"));
            } else {
                ui.label("Offset distance");
                ui.add(egui::Slider::new(&mut settings.distance, 0.0..=128.0).logarithmic(true));
            }
            ui.end_row();
            ui.label("Smoothing");
            ui.add(egui::Slider::new(&mut settings.smoothing, 0.0..=100.0).suffix(" %"))
                .on_hover_text("How much the moved vertices are evened out with their neighbours");
            ui.end_row();
            ui.label("Density");
            ui.horizontal(|ui| {
                ui.radio_value(&mut settings.attenuated, false, "Additive")
                    .on_hover_text("Every pass of the brush moves the vertices again");
                ui.radio_value(&mut settings.attenuated, true, "Attenuated")
                    .on_hover_text("A stroke moves each vertex once, however often the brush passes over it");
            });
            ui.end_row();
            ui.label("Normal direction");
            egui::ComboBox::from_id_salt("sculpt_direction")
                .selected_text(settings.direction.label())
                .show_ui(ui, |ui| {
                    for direction in SculptDirection::ALL {
                        ui.selectable_value(&mut settings.direction, direction, direction.label());
                    }
                })
                .response
                .on_hover_text(
                    "Brush Center follows the surface under the middle of the brush, Screen moves towards the \
                     camera, and Selected uses the face picked with Alt+right-click",
                );
            ui.end_row();
            if settings.tool == SculptTool::Push {
                ui.label("Falloff position");
                ui.add(egui::Slider::new(&mut settings.falloff_position, 0.0..=100.0).suffix(" %"))
                    .on_hover_text("How much of the brush, from its middle, moves vertices the whole way");
                ui.end_row();
                ui.label("Falloff final");
                ui.add(egui::Slider::new(&mut settings.falloff_final, 0.0..=100.0).suffix(" %"))
                    .on_hover_text("How much of the way vertices at the brush's edge still move");
                ui.end_row();
            }
        });
    if settings.tool == SculptTool::Carve {
        ui.label("Profile across the stroke, left to right; drag the points:");
        profile_graph(ui, &mut settings.profile);
        ui.horizontal(|ui| {
            for (name, profile) in [
                ("Trench", CARVE_PROFILE.to_vec()),
                ("V", vec![0.0, 0.25, 0.5, 0.75, 1.0, 0.75, 0.5, 0.25, 0.0]),
                (
                    "Round",
                    (0..9)
                        .map(|step| (std::f64::consts::PI * f64::from(step) / 8.0).sin())
                        .collect(),
                ),
                ("Ledge", vec![0.0, 0.0, 0.0, 0.0, 0.5, 1.0, 1.0, 1.0, 1.0]),
            ] {
                if ui.small_button(name).clicked() {
                    settings.profile = profile;
                }
            }
        });
    }
    if settings.tool != SculptTool::Blend {
        ui.checkbox(&mut settings.autosew, "Autosew")
            .on_hover_text("Keep the edges of neighbouring selected displacements joined while sculpting");
    }
    ui.label(
        RichText::new(
            "Drag over the selected displacements in the 3D view. Ctrl pulls (or takes the texture away), Shift \
             smooths, right-drag sideways resizes the brush, and Alt+right-click picks the Selected direction.",
        )
        .small()
        .color(MUTED),
    );
}

/// Carve's profile as a graph whose points can be dragged up and down: the middle of
/// the stroke in the middle, its edges at the sides, full depth at the top.
fn profile_graph(ui: &mut egui::Ui, profile: &mut Vec<f64>) {
    if profile.len() < 2 {
        *profile = CARVE_PROFILE.to_vec();
    }
    let (rect, response) = ui.allocate_exact_size(egui::vec2(260.0, 90.0), egui::Sense::click_and_drag());
    let last = (profile.len() - 1) as f32;
    let point = |index: usize, value: f64| {
        egui::pos2(
            rect.left() + rect.width() * index as f32 / last,
            rect.bottom() - rect.height() * value as f32,
        )
    };
    if response.clicked() || response.dragged() {
        if let Some(position) = response.interact_pointer_pos() {
            let index = ((position.x - rect.left()) / rect.width() * last)
                .round()
                .clamp(0.0, last) as usize;
            profile[index] = f64::from(((rect.bottom() - position.y) / rect.height()).clamp(0.0, 1.0));
        }
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, Color32::from_gray(24));
    painter.line_segment(
        [
            egui::pos2(rect.center().x, rect.top()),
            egui::pos2(rect.center().x, rect.bottom()),
        ],
        Stroke::new(1.0_f32, Color32::from_gray(60)),
    );
    let points: Vec<Pos2> = profile
        .iter()
        .enumerate()
        .map(|(index, value)| point(index, *value))
        .collect();
    painter.add(egui::Shape::line(points.clone(), Stroke::new(1.5_f32, BRUSH_COLOR)));
    for at in points {
        painter.circle_filled(at, 3.5, Color32::WHITE);
    }
}

impl MalletApp {
    /// Sculpts the selected displacements while the Sculpt tool of the Displacement tab
    /// is in use, as Hammer's: a left-button stroke applies the brush as it moves and
    /// again every [`PAINT_INTERVAL`] while held still, all as one undo step, with Ctrl
    /// reversing it and Shift smoothing; a right-button drag sideways resizes the brush;
    /// and Alt+right-click takes the face under the mouse as the Selected direction.
    fn sculpt_displacements_3d(
        &mut self,
        slot: usize,
        rect: Rect,
        response: &Response,
        editor: &mut Editor,
        pointer: &PointerState,
    ) -> bool {
        let hovered = response
            .hover_pos()
            .or(pointer.position.filter(|_| response.is_pointer_button_down_on()));
        if response.secondary_clicked() && pointer.modifiers.alt {
            if let Some(position) = hovered {
                let (origin, direction) = editor.camera.ray(rect, position);
                match editor.scene.surface_face(origin, direction) {
                    Some((_, normal)) => {
                        editor.displacement.sculpt.direction = SculptDirection::Selected;
                        editor.displacement.sculpt.selected = normal;
                        editor.message = "Sculpting along the normal of the face picked".to_owned();
                    }
                    None => editor.message = "Alt+right-click a face to sculpt along its normal".to_owned(),
                }
            }
        }
        if response.drag_started_by(PointerButton::Secondary) && !pointer.modifiers.alt {
            self.brush_resize = pointer.press_origin.map(|start| BrushResize {
                slot,
                start,
                radius: editor.displacement.sculpt.radius,
            });
        }
        if let Some(resize) = self.brush_resize.filter(|resize| resize.slot == slot) {
            if response.dragged_by(PointerButton::Secondary) {
                if let Some(position) = pointer.position {
                    let radius = resize.radius + f64::from(position.x - resize.start.x);
                    editor.displacement.sculpt.radius = radius.clamp(*SCULPT_RADII.start(), *SCULPT_RADII.end());
                }
                self.sculpt_brush = Some((slot, resize.start));
                return true;
            }
            self.brush_resize = None;
        }
        if let Some(position) = hovered {
            self.sculpt_brush = Some((slot, position));
            response.ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        let held = response.is_pointer_button_down_on()
            && response
                .ctx
                .input(|input| input.pointer.button_down(PointerButton::Primary));
        if !held {
            if self.sculpt_stroke.as_ref().is_some_and(|stroke| stroke.slot == slot) {
                self.sculpt_stroke = None;
            }
            return true;
        }
        let Some(position) = hovered else {
            return true;
        };
        let stroke = self.sculpt_stroke.get_or_insert_with(|| SculptStroke {
            slot,
            group: next_generation(),
            reverse: pointer.modifiers.ctrl,
            smooth: pointer.modifiers.shift,
            last: None,
            memory: StrokeMemory::default(),
        });
        if stroke.slot != slot {
            return true;
        }
        response.ctx.request_repaint_after(PAINT_INTERVAL);
        let settings = editor.displacement.sculpt.clone();
        let carving = settings.tool == SculptTool::Carve && !stroke.smooth;
        let previous = stroke.last.map_or(position, |(_, at)| at);
        let moved = previous.distance(position) >= SCULPT_SPACING;
        let due = match stroke.last {
            None => !carving,
            Some((when, _)) => moved || (!carving && when.elapsed() >= PAINT_INTERVAL),
        };
        if !due {
            if stroke.last.is_none() {
                stroke.last = Some((Instant::now(), position));
            }
            return true;
        }
        stroke.last = Some((Instant::now(), position));
        let to_point = |at: Pos2| DVec2::new(f64::from(at.x), f64::from(at.y));
        let sides = editor.displacement_sides();
        let result = editor.sculpt_step(
            &sides,
            &settings,
            rect,
            to_point(position),
            to_point(previous),
            stroke.reverse,
            stroke.smooth,
            &mut stroke.memory,
            stroke.group,
        );
        if let Err(error) = result {
            editor.message = error;
        }
        true
    }

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
        if self.sculpt_brush.is_some_and(|(at, _)| at == slot) {
            self.sculpt_brush = None;
        }
        let painting = editor.tool == Tool::Texture && editor.displacement.painting() && !editor.camera.mouse_look();
        let sculpting = painting && editor.displacement.mode == DisplacementMode::Sculpt;
        if !sculpting && self.sculpt_stroke.as_ref().is_some_and(|stroke| stroke.slot == slot) {
            self.sculpt_stroke = None;
        }
        if !painting || sculpting {
            if self.paint_stroke.is_some_and(|stroke| stroke.slot == slot) {
                self.paint_stroke = None;
            }
            return sculpting && self.sculpt_displacements_3d(slot, rect, response, editor, pointer);
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

/// The sculpt brush under the mouse in viewport `slot`: a circle of its radius on the
/// screen.
pub(super) fn draw_sculpt_brush(painter: &egui::Painter, slot: usize, spot: Option<(usize, Pos2)>, radius: f64) {
    if let Some((_, center)) = spot.filter(|(at, _)| *at == slot) {
        painter.circle_stroke(center, radius as f32, Stroke::new(1.5_f32, BRUSH_COLOR));
        painter.circle_filled(center, 2.0, BRUSH_COLOR);
    }
}

/// The vertices the compiler collapses where displacements meet neighbours of a lower
/// power, as small pink squares, as Hammer's Displacement Edge Collapse shows them.
pub(super) fn draw_collapsed_vertices(painter: &egui::Painter, camera: &Camera3D, rect: Rect, points: &[DVec3]) {
    for point in points {
        if let Some(at) = camera.project(rect, *point) {
            let square = Rect::from_center_size(at, egui::Vec2::splat(COLLAPSED_HALF_SIZE * 2.0));
            painter.rect_filled(square, 0.0, COLLAPSED_COLOR);
        }
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
