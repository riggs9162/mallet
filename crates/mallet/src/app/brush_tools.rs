//! Dialogs of Hammer's brush commands, starting with Make Hollow's wall width.

use eframe::egui;

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
}
