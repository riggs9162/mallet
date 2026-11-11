//! Hammer's Run Map dialog (F9) in its normal mode. Expert... switches to the compile
//! window with its presets, and the compile window's Normal... button switches back; F9
//! opens whichever was used last, as in Hammer.

use eframe::egui::{self, RichText};

use super::{save_as_dialog, MalletApp};
use crate::compile::{RunBsp, RunPass};
use crate::editor::Editor;

impl MalletApp {
    /// F9 and File > Run Map: the normal Run Map dialog, or the compile window when the
    /// expert one was used last.
    pub(super) fn run_map(&mut self, editor: &mut Editor) {
        if editor.config.compile.expert {
            self.open_compile_window(editor);
        } else {
            self.run_map_open = true;
        }
    }

    /// The compile window's Normal... button: back to the normal Run Map dialog.
    pub(super) fn run_map_normal(&mut self, editor: &mut Editor) {
        editor.compile_open = false;
        editor.config.compile.expert = false;
        editor.save_config();
        self.run_map_open = true;
    }

    /// Draws the Run Map dialog while it is open.
    pub(super) fn run_map_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if !self.run_map_open {
            return;
        }
        let mut ok = false;
        let mut cancel = false;
        let mut expert = false;
        let options = &mut editor.config.compile.run_map;
        egui::Window::new("Run Map")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                egui::Grid::new("run_map").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Run BSP:");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut options.bsp, RunBsp::No, "No");
                        ui.radio_value(&mut options.bsp, RunBsp::Normal, "Normal");
                        ui.radio_value(&mut options.bsp, RunBsp::OnlyEntities, "Only entities");
                    });
                    ui.end_row();
                    ui.label("Run VIS:");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut options.vis, RunPass::No, "No");
                        ui.radio_value(&mut options.vis, RunPass::Normal, "Normal");
                        ui.radio_value(&mut options.vis, RunPass::Fast, "Fast");
                    });
                    ui.end_row();
                    ui.label("Run RAD:");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut options.rad, RunPass::No, "No");
                        ui.radio_value(&mut options.rad, RunPass::Normal, "Normal");
                        ui.radio_value(&mut options.rad, RunPass::Fast, "Fast");
                        ui.checkbox(&mut options.hdr, "HDR")
                            .on_hover_text("Light the map for HDR as well as LDR; the lighting takes about twice as long");
                    });
                    ui.end_row();
                });
                ui.add_space(4.0);
                ui.checkbox(&mut options.dont_run_game, "Don't run the game after compiling");
                ui.label("Additional game parameters:");
                ui.add(
                    egui::TextEdit::singleline(&mut options.game_parameters)
                        .hint_text("such as -toconsole -dev -console +sv_lan 1")
                        .desired_width(360.0),
                );
                ui.label(
                    RichText::new("The compiled map is copied into the game's maps folder.")
                        .small()
                        .weak(),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    expert = ui
                        .button("Expert...")
                        .on_hover_text("The compile window, with presets of steps and every option")
                        .clicked();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        cancel = ui.button("Cancel").clicked();
                        ok = ui.button("OK").clicked();
                    });
                });
            });
        if expert {
            self.run_map_open = false;
            editor.config.compile.expert = true;
            editor.save_config();
            self.open_compile_window(editor);
        } else if cancel || (ctx.input(|input| input.key_pressed(egui::Key::Escape)) && !ctx.wants_keyboard_input()) {
            self.run_map_open = false;
            editor.save_config();
        } else if ok {
            self.run_map_open = false;
            editor.save_config();
            if editor.is_untitled() {
                save_as_dialog(editor);
            }
            if editor.is_untitled() {
                editor.message = "Save the map first, then run it".to_owned();
                return;
            }
            match editor.run_map() {
                Ok(()) => {
                    editor.compile_open = true;
                    self.compile.tab = super::compile_window::Tab::Output;
                }
                Err(error) => editor.message = error,
            }
        }
    }
}
