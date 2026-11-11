//! Small dialogs of Hammer's View and Map menus: Go to Brush Number, Go to Coordinates
//! and Show Information.

use eframe::egui::{self, RichText};
use glam::DVec3;

use super::MalletApp;
use crate::editor::Editor;

/// Which of the small dialogs show, and what is typed into them.
#[derive(Default)]
pub(super) struct MapDialogs {
    /// The brush number typed into Go to Brush Number, while it shows.
    brush: Option<String>,
    /// The point typed into Go to Coordinates, while it shows.
    point: Option<[f64; 3]>,
    /// Whether Show Information shows.
    information: bool,
    /// Set when a dialog opens, so its first field takes the keyboard once.
    focus: bool,
}

impl MalletApp {
    /// View > Go to Brush Number (Ctrl+Shift+G).
    pub(super) fn open_go_to_brush(&mut self) {
        self.map_dialogs.brush = Some(String::new());
        self.map_dialogs.focus = true;
    }

    /// View > Go to Coordinates, starting from where the camera is.
    pub(super) fn open_go_to_point(&mut self, editor: &Editor) {
        self.map_dialogs.point = Some(editor.camera.position.round().to_array());
        self.map_dialogs.focus = true;
    }

    /// Map > Show Information.
    pub(super) fn open_map_information(&mut self) {
        self.map_dialogs.information = true;
    }

    /// Draws whichever of the small dialogs are open.
    pub(super) fn map_dialogs(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        self.go_to_brush_window(ctx, editor);
        self.go_to_point_window(ctx, editor);
        self.information_window(ctx, editor);
    }

    fn go_to_brush_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        let Some(text) = &mut self.map_dialogs.brush else { return };
        let mut ok = false;
        let mut cancel = false;
        let focus = std::mem::take(&mut self.map_dialogs.focus);
        egui::Window::new("Go to Brush Number")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("Brush number:");
                let field = ui.add(egui::TextEdit::singleline(text).desired_width(120.0));
                if focus {
                    field.request_focus();
                }
                let entered = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ok = ui.button("OK").clicked() || entered;
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if ok {
            let result = text
                .trim()
                .parse::<u32>()
                .map_err(|_| format!("\"{}\" is not a brush number", text.trim()))
                .and_then(|id| editor.go_to_brush(id));
            match result {
                Ok(()) => self.map_dialogs.brush = None,
                Err(error) => editor.message = error,
            }
        } else if cancel || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.map_dialogs.brush = None;
        }
    }

    fn go_to_point_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        let Some(point) = &mut self.map_dialogs.point else { return };
        let mut ok = false;
        let mut cancel = false;
        let focus = std::mem::take(&mut self.map_dialogs.focus);
        egui::Window::new("Go to Coordinates")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (index, (label, value)) in ["X", "Y", "Z"].into_iter().zip(point.iter_mut()).enumerate() {
                        ui.label(label);
                        let field = ui.add(egui::DragValue::new(value).speed(8.0));
                        if focus && index == 0 {
                            field.request_focus();
                        }
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ok = ui.button("OK").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if ok {
            let [x, y, z] = *point;
            editor.go_to_point(DVec3::new(x, y, z));
            self.map_dialogs.point = None;
        } else if cancel || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.map_dialogs.point = None;
        }
    }

    fn information_window(&mut self, ctx: &egui::Context, editor: &Editor) {
        if !self.map_dialogs.information {
            return;
        }
        let info = editor.map_information();
        let mut open = true;
        egui::Window::new("Map Information")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(320.0)
            .show(ctx, |ui| {
                egui::Grid::new("map_information").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                    for (label, count) in [
                        ("Solids", info.solids),
                        ("Faces", info.faces),
                        ("Point entities", info.point_entities),
                        ("Solid entities", info.solid_entities),
                        ("Unique textures", info.unique_textures),
                    ] {
                        ui.label(label);
                        ui.label(count.to_string());
                        ui.end_row();
                    }
                });
                ui.separator();
                ui.label(RichText::new("Textures used").strong());
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for texture in &info.textures {
                        ui.label(texture);
                    }
                });
            });
        self.map_dialogs.information = open;
    }
}
