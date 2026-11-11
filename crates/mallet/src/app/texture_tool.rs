//! The texture tool's Face Edit Sheet, laid out like Hammer++'s: the selected faces'
//! scale, shift, luxel scale, rotation and UV vectors, justify and alignment, the click
//! mode, and Hammer's Replace Textures dialog. Values apply as you change them.

use std::collections::BTreeSet;

use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icon;
use glam::DVec3;
use mallet_vmf::texture::Justify;
use mallet_vmf::{Side, TexAxis};

use super::chrome::material_preview;
use super::texture_browser::Pick;
use super::MalletApp;
use crate::editor::{Editor, Tool};
use crate::faces::{FaceInfo, FaceValues, ReplaceAction, ReplaceOptions, TextureMode};

const MUTED: Color32 = Color32::from_gray(140);
const MIXED: Color32 = Color32::from_rgb(230, 170, 60);

/// What Face Edit remembers between frames.
#[derive(Default)]
pub(super) struct FaceEditState {
    tab: FaceTab,
    /// The selected faces, cached until the map or the selection changes.
    cache: Option<((u64, u64), Vec<FaceInfo>)>,
    treat_as_one: bool,
    /// The Replace Textures dialog.
    pub(super) replace_open: bool,
    pub(super) find: String,
    pub(super) replace: String,
    marked_only: bool,
    action: ReplaceAction,
    mark_only: bool,
    rescale: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum FaceTab {
    #[default]
    Material,
    Displacement,
}

impl MalletApp {
    /// Face Edit, shown while the texture tool is active. Closing it leaves the tool.
    pub(super) fn face_edit_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        self.replace_window(ctx, editor);
        if editor.tool != Tool::Texture {
            self.face_edit.cache = None;
            return;
        }
        let key = (editor.scene.geometry_generation, editor.scene.selection_generation);
        if self.face_edit.cache.as_ref().is_none_or(|(cached, _)| *cached != key) {
            let faces = editor.faces(&editor.face_selection);
            self.face_edit.cache = Some((key, faces));
        }
        let faces = self
            .face_edit
            .cache
            .as_ref()
            .map(|(_, faces)| faces.clone())
            .unwrap_or_default();
        let current = editor.material.clone();
        let preview = material_preview(ctx, &mut self.texture_previews, editor, &current);
        let hotspots = editor.hotspots(&current).len();
        let mut open = true;
        let mut browse = false;
        egui::Window::new(format!("{} Face Edit Sheet", icon::PAINT_BRUSH))
            .id(egui::Id::new("face_edit"))
            .open(&mut open)
            .default_pos(ctx.screen_rect().right_top() + egui::vec2(-600.0, 110.0))
            .resizable(false)
            .collapsible(true)
            .show(ctx, |ui| {
                let state = &mut self.face_edit;
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut state.tab, FaceTab::Material, "Material");
                    ui.selectable_value(&mut state.tab, FaceTab::Displacement, "Displacement");
                });
                ui.separator();
                match state.tab {
                    FaceTab::Material => {
                        values_section(ui, editor, &faces);
                        ui.add_space(4.0);
                        uv_section(ui, editor, &faces);
                        ui.add_space(6.0);
                        browse = actions_section(ui, editor, state, &faces, preview.as_ref());
                        ui.add_space(4.0);
                        current_texture_row(ui, editor, preview.as_ref());
                        if hotspots > 0 {
                            ui.label(
                                RichText::new(format!(
                                    "{} Hotspot texture with {hotspots} rectangles: apply it, then press Fit.",
                                    icon::SQUARES_FOUR
                                ))
                                .small(),
                            );
                        }
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(
                                "Right-click applies the current texture; Alt+right-click also applies the lifted \
                                 alignment. Ctrl+click adds faces, Shift+click takes the whole brush, Alt+click lifts.",
                            )
                            .small()
                            .color(MUTED),
                        );
                    }
                    FaceTab::Displacement => displacement_section(ui),
                }
            });
        if browse {
            self.open_texture_browser(editor, Pick::Current);
        }
        if !open {
            editor.set_tool(Tool::Select);
        }
    }

    /// Hammer's Replace Textures dialog.
    fn replace_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if !self.face_edit.replace_open {
            return;
        }
        let find = self.face_edit.find.clone();
        let replace = self.face_edit.replace.clone();
        let find_preview = material_preview(ctx, &mut self.texture_previews, editor, &find);
        let replace_preview = material_preview(ctx, &mut self.texture_previews, editor, &replace);
        let mut open = true;
        let mut browse = None;
        let mut finished = false;
        egui::Window::new(format!("{} Replace Textures", icon::SWAP))
            .id(egui::Id::new("replace_textures"))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                let state = &mut self.face_edit;
                ui.horizontal_top(|ui| {
                    for (label, text, preview, pick) in [
                        ("Find", &mut state.find, &find_preview, Pick::Find),
                        ("Replace", &mut state.replace, &replace_preview, Pick::Replace),
                    ] {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(label).strong());
                            ui.add(egui::TextEdit::singleline(text).desired_width(200.0));
                            ui.horizontal(|ui| {
                                swatch(ui, preview.as_ref(), 72.0);
                                if ui.button("Browse").clicked() {
                                    browse = Some(pick);
                                }
                            });
                        });
                        ui.add_space(8.0);
                    }
                });
                ui.add_space(6.0);
                ui.horizontal_top(|ui| {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new("Replace in").strong());
                            ui.radio_value(&mut state.marked_only, true, "Selected objects")
                                .on_hover_text("The selected faces, or every face of the selected brushes");
                            ui.radio_value(&mut state.marked_only, false, "Everything");
                            ui.add_enabled(false, egui::Checkbox::new(&mut false, "Hidden objects too"))
                                .on_disabled_hover_text("Hidden objects are left out. Show them to replace textures on them.");
                        });
                    });
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new("Action").strong());
                            ui.radio_value(&mut state.action, ReplaceAction::Exact, "Replace exact matches");
                            ui.radio_value(&mut state.action, ReplaceAction::Partial, "Replace partial matches")
                                .on_hover_text("Materials containing the text become the replacement");
                            ui.radio_value(
                                &mut state.action,
                                ReplaceAction::Substitute,
                                "Substitute partial matches",
                            )
                            .on_hover_text("Only the matching part of each name is swapped, such as brick to concrete");
                        });
                    });
                });
                ui.checkbox(&mut state.mark_only, "Do not replace textures (mark found faces)");
                ui.checkbox(
                    &mut state.rescale,
                    "Rescale texture coordinates for the new texture size",
                )
                .on_hover_text("Keep the texture the same size in the world when the new one has another resolution");
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("OK").strong()).clicked() {
                        let within = state.marked_only.then(|| editor.texture_targets());
                        let options = ReplaceOptions {
                            action: state.action,
                            within,
                            mark_only: state.mark_only,
                            rescale: state.rescale,
                        };
                        let replacement = if state.replace.trim().is_empty() {
                            editor.material.clone()
                        } else {
                            state.replace.clone()
                        };
                        match editor.replace_materials(&state.find, &replacement, &options) {
                            Ok(_) => finished = true,
                            Err(error) => editor.message = error,
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        finished = true;
                    }
                });
            });
        if let Some(pick) = browse {
            self.open_texture_browser(editor, pick);
        }
        if !open || finished {
            self.face_edit.replace_open = false;
        }
    }

    /// Opens Replace Textures, searching for a material.
    pub(super) fn open_replace(&mut self, find: &str) {
        self.face_edit.replace_open = true;
        if !find.is_empty() {
            self.face_edit.find = find.to_owned();
        }
    }
}

/// A square texture preview.
fn swatch(ui: &mut egui::Ui, preview: Option<&(egui::TextureHandle, [u32; 2])>, size: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::click());
    match preview {
        Some((texture, _)) => {
            let full = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
            ui.painter().image(texture.id(), rect, full, Color32::WHITE);
        }
        None => {
            ui.painter().rect_filled(rect, 2, Color32::from_gray(20));
        }
    }
    response
}

/// A number field with spin arrows. Dragging, typing, the arrows and the mouse wheel all
/// change it. An orange dot shows that the selected faces disagree. Returns whether the
/// value changed.
fn spin(ui: &mut egui::Ui, value: &mut f64, step: f64, speed: f64, decimals: usize, mixed: bool) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 1.0;
        let drag = egui::DragValue::new(value).speed(speed).max_decimals(decimals);
        let response = ui.add_sized([58.0, 20.0], drag);
        changed |= response.changed();
        if response.hovered() {
            let scroll = ui.input(|input| input.raw_scroll_delta.y);
            if scroll != 0.0 {
                *value += step * f64::from(scroll.signum());
                changed = true;
            }
        }
        if mixed {
            ui.painter()
                .circle_filled(response.rect.right_top() + egui::vec2(-3.0, 3.0), 2.5, MIXED);
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let arrow = |glyph| egui::Button::new(RichText::new(glyph).size(8.0)).min_size(egui::vec2(14.0, 10.0));
            if ui.add(arrow(icon::CARET_UP)).clicked() {
                *value += step;
                changed = true;
            }
            if ui.add(arrow(icon::CARET_DOWN)).clicked() {
                *value -= step;
                changed = true;
            }
        });
    });
    changed
}

/// Whether the selected faces disagree on a value.
fn mixed(faces: &[FaceInfo], value: impl Fn(&FaceInfo) -> f64) -> bool {
    let Some(first) = faces.first() else { return false };
    let first = value(first);
    faces.iter().any(|face| (value(face) - first).abs() > 1e-6)
}

/// A small fixed-width button.
fn small(ui: &mut egui::Ui, text: &str, hint: &str) -> bool {
    ui.add(egui::Button::new(RichText::new(text).size(11.0)).min_size(egui::vec2(22.0, 20.0)))
        .on_hover_text(hint)
        .clicked()
}

/// Texture scale (with halve and double), texture shift (with random shift), luxel scale
/// and rotation.
fn values_section(ui: &mut egui::Ui, editor: &mut Editor, faces: &[FaceInfo]) {
    let selected: BTreeSet<u32> = faces.iter().map(|face| face.side.id).collect();
    let side = faces.first().map_or_else(empty_side, |face| face.side.clone());
    let mut values = FaceValues::default();
    let mut group = None;
    let mut random = None;
    let mut rescale = None;
    ui.add_enabled_ui(!faces.is_empty(), |ui| {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.label("Texture scale:");
                for (index, name) in ["X", "Y"].into_iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(name);
                        let mut scale = [side.uaxis.scale, side.vaxis.scale][index];
                        let differ = mixed(faces, |face| [face.side.uaxis.scale, face.side.vaxis.scale][index]);
                        if spin(ui, &mut scale, 0.05, 0.005, 4, differ) {
                            values.scale[index] = Some(scale);
                            group = Some(["scale_x", "scale_y"][index]);
                        }
                        let axis = if index == 0 { "width" } else { "height" };
                        if small(
                            ui,
                            "½",
                            &format!(
                                "Halve the {axis}: the texture repeats twice as often, pinned at the face's corner"
                            ),
                        ) {
                            rescale = Some((index == 0, 0.5));
                        }
                        if small(
                            ui,
                            "×2",
                            &format!(
                                "Double the {axis}: the texture repeats half as often, pinned at the face's corner"
                            ),
                        ) {
                            rescale = Some((index == 0, 2.0));
                        }
                    });
                }
            });
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.label("Texture shift:");
                for (index, name) in ["X", "Y"].into_iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(name);
                        let mut shift = [side.uaxis.offset, side.vaxis.offset][index];
                        let differ = mixed(faces, |face| [face.side.uaxis.offset, face.side.vaxis.offset][index]);
                        if spin(ui, &mut shift, 1.0, 1.0, 2, differ) {
                            values.shift[index] = Some(shift);
                            group = Some(["shift_x", "shift_y"][index]);
                        }
                        if small(
                            ui,
                            icon::DICE_FIVE,
                            "Shift each selected face by a random amount, so repeated faces such as stair steps differ",
                        ) {
                            random = Some(index == 0);
                        }
                    });
                }
            });
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.label("Luxel scale:");
                let mut luxels = f64::from(side.lightmap_scale.unwrap_or(16));
                let differ = mixed(faces, |face| f64::from(face.side.lightmap_scale.unwrap_or(16)));
                if spin(ui, &mut luxels, 1.0, 0.2, 0, differ) {
                    values.lightmap_scale = Some(luxels.round().clamp(1.0, 1024.0) as u32);
                    group = Some("lightmap");
                }
                ui.label("Rotation:");
                let mut rotation = side.rotation.unwrap_or(0.0);
                let differ = mixed(faces, |face| face.side.rotation.unwrap_or(0.0));
                if spin(ui, &mut rotation, 1.0, 1.0, 2, differ) {
                    values.rotation = Some(rotation);
                    group = Some("rotation");
                }
            });
        });
    });
    if let Some(group) = group {
        if let Err(error) = editor.set_face_values(&selected, values, Some(group)) {
            editor.message = error;
        }
    }
    if let Some(horizontal) = random {
        if let Err(error) = editor.random_shift(&selected, horizontal) {
            editor.message = error;
        }
    }
    if let Some((horizontal, factor)) = rescale {
        if let Err(error) = editor.scale_axis(&selected, horizontal, factor) {
            editor.message = error;
        }
    }
}

/// The U and V axis vectors, which can skew or turn a texture freely.
fn uv_section(ui: &mut egui::Ui, editor: &mut Editor, faces: &[FaceInfo]) {
    let Some(first) = faces.first() else { return };
    let selected: BTreeSet<u32> = faces.iter().map(|face| face.side.id).collect();
    let mut values = FaceValues::default();
    let mut group = None;
    ui.label("UV vectors (X Y Z):");
    egui::Grid::new("uv_vectors")
        .num_columns(3)
        .spacing([4.0, 4.0])
        .show(ui, |ui| {
            for (row, axis) in [first.side.uaxis.axis, first.side.vaxis.axis].into_iter().enumerate() {
                let mut vector = axis.to_array();
                let mut row_changed = false;
                for component in &mut vector {
                    let drag = egui::DragValue::new(component).speed(0.01).max_decimals(4);
                    row_changed |= ui.add_sized([84.0, 20.0], drag).changed();
                }
                if row_changed {
                    values.axes[row] = Some(DVec3::from_array(vector));
                    group = Some(["u_vector", "v_vector"][row]);
                }
                ui.end_row();
            }
        });
    if let Some(group) = group {
        if let Err(error) = editor.set_face_values(&selected, values, Some(group)) {
            editor.message = error;
        }
    }
}

/// The thumbnail with Mark, Browse and Replace, the justify cross, alignment, Apply
/// Nodraw and Apply, Treat as one, the click mode, smoothing groups and Hide mask.
/// Returns whether the texture browser should open.
fn actions_section(
    ui: &mut egui::Ui,
    editor: &mut Editor,
    state: &mut FaceEditState,
    faces: &[FaceInfo],
    preview: Option<&(egui::TextureHandle, [u32; 2])>,
) -> bool {
    let mut browse = false;
    let selected: BTreeSet<u32> = faces.iter().map(|face| face.side.id).collect();
    let any = !faces.is_empty();
    ui.horizontal_top(|ui| {
        if swatch(ui, preview, 88.0).on_hover_text("Browse materials").clicked() {
            browse = true;
        }
        ui.vertical(|ui| {
            let wide = |text: &str| egui::Button::new(text).min_size(egui::vec2(78.0, 22.0));
            if ui
                .add(wide("Mark..."))
                .on_hover_text("Select every face that uses the current texture")
                .clicked()
            {
                let material = editor.material.clone();
                editor.mark_material(&material);
            }
            if ui.add(wide("Browse...")).clicked() {
                browse = true;
            }
            if ui.add(wide("Replace...")).clicked() {
                state.replace_open = true;
                if let Some(face) = faces.first() {
                    state.find = face.side.material.clone();
                }
            }
        });
        ui.add_space(4.0);
        // The justify cross, as in Hammer.
        ui.add_enabled_ui(any, |ui| {
            ui.vertical(|ui| {
                ui.label("Justify");
                let mut justify = None;
                ui.horizontal(|ui| {
                    if small(
                        ui,
                        "Fit",
                        "Fit the texture once across the face. On a hotspot texture, each face gets the \
                         rectangle that suits it best; press again for another pick, or hold Alt for the \
                         alternate rectangles",
                    ) {
                        justify = Some(Justify::Fit);
                    }
                    if small(ui, "T", "Top") {
                        justify = Some(Justify::Top);
                    }
                });
                ui.horizontal(|ui| {
                    if small(ui, "L", "Left") {
                        justify = Some(Justify::Left);
                    }
                    if small(ui, "C", "Center") {
                        justify = Some(Justify::Center);
                    }
                    if small(ui, "R", "Right") {
                        justify = Some(Justify::Right);
                    }
                });
                ui.horizontal(|ui| {
                    ui.add_space(26.0);
                    if small(ui, "B", "Bottom") {
                        justify = Some(Justify::Bottom);
                    }
                });
                ui.horizontal(|ui| {
                    if small(
                        ui,
                        "Fit W",
                        "Fit the texture once across the width, keeping its proportions",
                    ) {
                        justify = Some(Justify::FitWidth);
                    }
                    if small(
                        ui,
                        "Fit H",
                        "Fit the texture once down the height, keeping its proportions",
                    ) {
                        justify = Some(Justify::FitHeight);
                    }
                });
                if let Some(how) = justify {
                    let alternates = ui.input(|input| input.modifiers.alt);
                    if let Err(error) = editor.justify_faces(&selected, how, state.treat_as_one, alternates) {
                        editor.message = error;
                    }
                }
            });
        });
        ui.add_space(4.0);
        // Alignment checkboxes show how the faces are aligned now.
        ui.add_enabled_ui(any, |ui| {
            ui.vertical(|ui| {
                ui.label("Align");
                let (world, face) = alignment_of(faces);
                let mut world_checked = world;
                if ui.checkbox(&mut world_checked, "World").changed() {
                    if let Err(error) = editor.align_faces(&selected, false) {
                        editor.message = error;
                    }
                }
                let mut face_checked = face && !world;
                if ui.checkbox(&mut face_checked, "Face").changed() {
                    if let Err(error) = editor.align_faces(&selected, true) {
                        editor.message = error;
                    }
                }
            });
        });
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_enabled_ui(any, |ui| {
            if ui
                .button("Apply Nodraw")
                .on_hover_text("Put tools/toolsnodraw on the selected faces, keeping the current texture")
                .clicked()
            {
                if let Err(error) = editor.apply_material(&selected, "TOOLS/TOOLSNODRAW") {
                    editor.message = error;
                }
            }
            if ui
                .button(RichText::new("Apply").strong())
                .on_hover_text("Put the current texture on the selected faces")
                .clicked()
            {
                let material = editor.material.clone();
                if let Err(error) = editor.apply_material(&selected, &material) {
                    editor.message = error;
                }
            }
        });
        ui.checkbox(&mut state.treat_as_one, "Treat as one")
            .on_hover_text("Justify the selected faces together, so the texture runs across them");
    });
    ui.horizontal(|ui| {
        ui.label("Mode:");
        egui::ComboBox::from_id_salt("texture_mode")
            .selected_text(editor.texture_mode.label())
            .width(170.0)
            .show_ui(ui, |ui| {
                for mode in TextureMode::ALL {
                    ui.selectable_value(&mut editor.texture_mode, mode, mode.label())
                        .on_hover_text(mode.description());
                }
            })
            .response
            .on_hover_text(editor.texture_mode.description());
    });
    ui.horizontal(|ui| {
        ui.add_enabled_ui(any, |ui| smoothing_groups(ui, editor, faces));
        ui.toggle_value(&mut editor.overlay.hide_face_mask, "Hide mask")
            .on_hover_text("Draw the selected faces without their red tint");
    });
    browse
}

/// Which alignments the faces have now: (all world aligned, all face aligned).
fn alignment_of(faces: &[FaceInfo]) -> (bool, bool) {
    if faces.is_empty() {
        return (false, false);
    }
    let matches = |face: &FaceInfo, to_face: bool| {
        let mut aligned = face.side.clone();
        aligned.align(to_face);
        aligned
            .uaxis
            .axis
            .abs_diff_eq(face.side.uaxis.axis.normalize_or_zero(), 1e-3)
            && aligned
                .vaxis
                .axis
                .abs_diff_eq(face.side.vaxis.axis.normalize_or_zero(), 1e-3)
    };
    (
        faces.iter().all(|face| matches(face, false)),
        faces.iter().all(|face| matches(face, true)),
    )
}

/// A menu of the 24 smoothing groups, set on every selected face.
fn smoothing_groups(ui: &mut egui::Ui, editor: &mut Editor, faces: &[FaceInfo]) {
    let groups = faces.first().and_then(|face| face.side.smoothing_groups).unwrap_or(0);
    let mut changed = None;
    ui.menu_button("Smoothing Groups", |ui| {
        ui.label(
            RichText::new("Faces in a shared group get smooth lighting across their edges.")
                .small()
                .color(MUTED),
        );
        egui::Grid::new("smoothing_groups")
            .num_columns(6)
            .spacing([6.0, 4.0])
            .show(ui, |ui| {
                for group in 0..24u32 {
                    let mut on = groups & (1 << group) != 0;
                    if ui.checkbox(&mut on, format!("{}", group + 1)).changed() {
                        changed = Some(if on {
                            groups | (1 << group)
                        } else {
                            groups & !(1 << group)
                        });
                    }
                    if group % 6 == 5 {
                        ui.end_row();
                    }
                }
            });
        if ui.button("Clear all").clicked() {
            changed = Some(0);
        }
    });
    if let Some(groups) = changed {
        let selected: BTreeSet<u32> = faces.iter().map(|face| face.side.id).collect();
        let values = FaceValues {
            smoothing_groups: Some(groups),
            ..FaceValues::default()
        };
        if let Err(error) = editor.set_face_values(&selected, values, None) {
            editor.message = error;
        }
    }
}

/// The current texture: its size, and a field with the recently used ones.
fn current_texture_row(ui: &mut egui::Ui, editor: &mut Editor, preview: Option<&(egui::TextureHandle, [u32; 2])>) {
    ui.horizontal(|ui| {
        ui.label("Current texture:");
        if let Some((_, [width, height])) = preview {
            ui.label(RichText::new(format!("{width}x{height}")).color(MUTED));
        }
    });
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut editor.material).desired_width(300.0));
        ui.add_enabled_ui(!editor.recent_materials.is_empty(), |ui| {
            ui.menu_button(icon::CARET_DOWN, |ui| {
                for material in editor.recent_materials.clone() {
                    if ui
                        .selectable_label(material.eq_ignore_ascii_case(&editor.material), &material)
                        .clicked()
                    {
                        editor.material = material;
                        ui.close_menu();
                    }
                }
            })
            .response
            .on_hover_text("Recently used textures");
        });
    });
}

/// The Displacement tab: Hammer's tools, waiting for displacement support.
fn displacement_section(ui: &mut egui::Ui) {
    ui.label(RichText::new("Tools").strong());
    ui.add_enabled_ui(false, |ui| {
        egui::Grid::new("displacement_tools")
            .num_columns(5)
            .spacing([4.0, 4.0])
            .show(ui, |ui| {
                for (index, name) in [
                    "Select",
                    "Paint Geometry",
                    "Paint Alpha",
                    "Invert Alpha",
                    "Sculpt",
                    "Create",
                    "Destroy",
                    "Subdivide",
                    "Noise",
                    "Sew",
                ]
                .into_iter()
                .enumerate()
                {
                    let _ = ui.add(egui::Button::new(name).min_size(egui::vec2(64.0, 40.0)));
                    if index % 5 == 4 {
                        ui.end_row();
                    }
                }
            });
        ui.add_space(6.0);
        ui.label(RichText::new("Attributes").strong());
        egui::Grid::new("displacement_attributes")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                for name in ["Power", "Elevation", "Scale"] {
                    ui.label(name);
                    ui.add(egui::TextEdit::singleline(&mut String::new()).desired_width(60.0));
                    ui.end_row();
                }
            });
    });
    ui.add_space(6.0);
    ui.label(
        RichText::new("Displacements are next on the roadmap: these tools turn on once Mallet can draw and edit them.")
            .color(MUTED),
    );
}

/// A side with default values, so the fields can show while no face is selected.
fn empty_side() -> Side {
    let axis = |axis: DVec3| TexAxis {
        axis,
        offset: 0.0,
        scale: 0.25,
    };
    Side {
        id: 0,
        plane: [DVec3::ZERO; 3],
        material: String::new(),
        uaxis: axis(DVec3::X),
        vaxis: axis(DVec3::NEG_Y),
        rotation: Some(0.0),
        lightmap_scale: Some(16),
        smoothing_groups: Some(0),
        extra: Vec::new(),
        extra_slots: Vec::new(),
    }
}
