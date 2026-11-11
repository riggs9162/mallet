//! Dialogs of Hammer's brush commands: Make Hollow's wall width and the Block tool's Arch
//! Properties.

use eframe::egui;
use glam::DVec2;
use mallet_vmf::geometry::Bounds;
use mallet_vmf::primitive::{Arch, MAX_SIDES, MIN_SIDES};

use super::MalletApp;
use crate::editor::{Editor, DEFAULT_HOLLOW_WIDTH};

/// The Make Hollow dialog's state: whether it shows, and the wall width, kept between
/// uses as Hammer keeps it.
pub(super) struct HollowDialog {
    open: bool,
    width: f64,
    /// Set when the dialog opens, so the width field takes the keyboard once.
    focus: bool,
}

impl Default for HollowDialog {
    fn default() -> Self {
        Self {
            open: false,
            width: DEFAULT_HOLLOW_WIDTH,
            focus: false,
        }
    }
}

impl MalletApp {
    /// Hammer's Make Hollow: asks for the wall width, then turns the selected brushes into
    /// walls of that width. Nothing selected tells the user so instead.
    pub(super) fn open_hollow_dialog(&mut self, editor: &mut Editor) {
        if editor.selection.is_empty() {
            editor.message = "Select the brushes to hollow first".to_owned();
            return;
        }
        self.hollow.open = true;
        self.hollow.focus = true;
    }

    /// The Make Hollow dialog. Enter or OK hollows; Escape or Cancel closes it.
    pub(super) fn hollow_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if !self.hollow.open {
            return;
        }
        let mut ok = false;
        let mut cancel = false;
        egui::Window::new("Make Hollow")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("Wall width; a negative width builds the walls outside the brush:");
                let field = ui.add(egui::DragValue::new(&mut self.hollow.width).speed(1.0).range(-4096.0..=4096.0));
                if std::mem::take(&mut self.hollow.focus) {
                    field.request_focus();
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ok = ui.button("OK").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        let (enter, escape) = ctx.input(|input| (input.key_pressed(egui::Key::Enter), input.key_pressed(egui::Key::Escape)));
        if ok || enter {
            self.hollow.open = false;
            if let Err(error) = editor.make_hollow(self.hollow.width) {
                editor.message = error;
            }
        } else if cancel || escape {
            self.hollow.open = false;
        }
    }

    /// The Block tool's Enter, or Create Object, with the arch shape: opens Hammer's Arch
    /// Properties dialog for the box drawn.
    pub(super) fn open_arch_dialog(&mut self, editor: &mut Editor) {
        match editor.overlay.block {
            Some(bounds) if self.arch.bounds.is_none() => {
                self.arch.bounds = Some(bounds);
                self.arch.opened = true;
            }
            Some(_) => {}
            None => editor.message = "Drag out the arch's box in a 2D view first".to_owned(),
        }
    }

    /// Hammer's Arch Properties dialog: wall width, number of sides, arc (Circle makes it
    /// 360), start angle and added height, with a preview of the pieces from above. OK
    /// or Enter builds the arch in the box; Cancel or Escape keeps the box.
    pub(super) fn arch_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        let Some(bounds) = self.arch.bounds else { return };
        let mut ok = false;
        let mut cancel = false;
        let arch = &mut self.arch.settings;
        egui::Window::new("Arch Properties")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    arch_preview(ui, arch, &bounds);
                    ui.vertical(|ui| {
                        egui::Grid::new("arch_fields").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                            ui.label("Wall width");
                            ui.add(egui::DragValue::new(&mut arch.wall_width).speed(1.0).range(-4096.0..=4096.0));
                            ui.end_row();
                            ui.label("Number of sides");
                            ui.add(egui::DragValue::new(&mut arch.sides).range(MIN_SIDES..=MAX_SIDES));
                            ui.end_row();
                            ui.label("Arc");
                            ui.horizontal(|ui| {
                                let (low, high) = Arch::ARC_RANGE;
                                ui.add(egui::DragValue::new(&mut arch.arc).range(low..=high).suffix("\u{b0}"));
                                if ui.button("Circle").on_hover_text("Go all the way round").clicked() {
                                    arch.arc = high;
                                }
                            });
                            ui.end_row();
                            ui.label("Start angle");
                            ui.add(egui::DragValue::new(&mut arch.start_angle).range(0.0..=360.0).suffix("\u{b0}"));
                            ui.end_row();
                            ui.label("Add height");
                            ui.add(egui::DragValue::new(&mut arch.add_height).speed(1.0))
                                .on_hover_text("How much higher each piece is than the last, for spiral stairs; negative goes down");
                            ui.end_row();
                        });
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ok = ui.button("OK").clicked();
                            cancel = ui.button("Cancel").clicked();
                        });
                    });
                });
            });
        let typing = ctx.wants_keyboard_input() || std::mem::take(&mut self.arch.opened);
        let (enter, escape) = ctx.input(|input| (input.key_pressed(egui::Key::Enter), input.key_pressed(egui::Key::Escape)));
        if ok || (enter && !typing) {
            self.arch.bounds = None;
            let settings = self.arch.settings;
            if let Err(error) = editor.create_arch(bounds.min, bounds.max, &settings) {
                editor.message = error;
            }
        } else if cancel || (escape && !typing) {
            self.arch.bounds = None;
        }
    }
}

/// The Arch Properties dialog's state: the box the arch fills while it shows, and the
/// settings, kept between arches as Hammer keeps them.
#[derive(Default)]
pub(super) struct ArchDialog {
    bounds: Option<Bounds>,
    settings: Arch,
    /// Set on the frame the dialog opens, so the Enter that opened it does not also
    /// build the arch.
    opened: bool,
}

/// Draws the arch's pieces from above, scaled to fit, as Hammer's dialog previews it.
fn arch_preview(ui: &mut egui::Ui, arch: &Arch, bounds: &Bounds) {
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(180.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, egui::Color32::BLACK);
    let (min, max) = (bounds.min.truncate(), bounds.max.truncate());
    let size = (max - min).max(DVec2::ONE);
    let scale = (f64::from(rect.width()) - 16.0) / size.x.max(size.y);
    let centre = (min + max) * 0.5;
    let to_screen = |point: DVec2| {
        let offset = (point - centre) * scale;
        rect.center() + egui::vec2(offset.x as f32, -offset.y as f32)
    };
    let stroke = egui::Stroke::new(1.0_f32, egui::Color32::WHITE);
    for outline in arch.outlines(min, max) {
        let corners: Vec<egui::Pos2> = outline.iter().map(|point| to_screen(*point)).collect();
        for index in 0..corners.len() {
            painter.line_segment([corners[index], corners[(index + 1) % corners.len()]], stroke);
        }
    }
}
