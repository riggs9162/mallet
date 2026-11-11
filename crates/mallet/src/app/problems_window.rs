//! Hammer's Check for Problems dialog (Alt+P): the problems found, with Go to Error, Fix
//! and Fix All, which fixes every problem of the same kind.

use eframe::egui::{self, RichText};

use super::MalletApp;
use crate::editor::{Editor, SelectMode};
use crate::problems::Problem;

/// The dialog's list while it is open, and the row picked.
#[derive(Default)]
pub(super) struct ProblemsDialog {
    problems: Option<Vec<Problem>>,
    selected: Option<usize>,
}

impl MalletApp {
    /// Map > Check for Problems: checks the map and shows what it found.
    pub(super) fn check_for_problems(&mut self, editor: &mut Editor) {
        let problems = editor.check_for_problems();
        editor.message = match problems.len() {
            0 => "No problems were found".to_owned(),
            1 => "1 problem was found".to_owned(),
            count => format!("{count} problems were found"),
        };
        self.problems.selected = (!problems.is_empty()).then_some(0);
        self.problems.problems = Some(problems);
    }

    /// Draws the Check for Problems dialog while it is open.
    pub(super) fn problems_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        let Some(problems) = &self.problems.problems else { return };
        let mut open = true;
        let mut go_to = false;
        let mut fix_one = false;
        let mut fix_all = false;
        let mut check_again = false;
        let selected = &mut self.problems.selected;
        egui::Window::new("Check for Problems")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([560.0, 320.0])
            .show(ctx, |ui| {
                if problems.is_empty() {
                    ui.label("No problems were found.");
                } else {
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        for (index, problem) in problems.iter().enumerate() {
                            let text = format!("{}: {}", problem.kind.label(), problem.detail);
                            let row = ui.selectable_label(*selected == Some(index), text);
                            if row.clicked() {
                                *selected = Some(index);
                            }
                            if row.double_clicked() {
                                go_to = true;
                            }
                        }
                    });
                }
                ui.separator();
                let picked = selected.and_then(|index| problems.get(index));
                ui.horizontal(|ui| {
                    let has_object = picked.is_some_and(|problem| problem.object.is_some());
                    go_to |= ui.add_enabled(has_object, egui::Button::new("Go to Error")).clicked();
                    let fixable = picked.is_some_and(|problem| problem.kind.fixable());
                    fix_one = ui.add_enabled(fixable, egui::Button::new("Fix")).clicked();
                    fix_all = ui
                        .add_enabled(fixable, egui::Button::new("Fix All"))
                        .on_hover_text("Fix every problem of this kind")
                        .clicked();
                    check_again = ui.button("Check Again").clicked();
                });
                if let Some(problem) = picked.filter(|problem| !problem.kind.fixable()) {
                    ui.label(
                        RichText::new(format!("{} has to be fixed by hand.", problem.kind.label()))
                            .small()
                            .weak(),
                    );
                }
            });
        let picked = self.problems.selected.and_then(|index| problems.get(index)).cloned();
        if go_to {
            if let Some(object) = picked.as_ref().and_then(|problem| problem.object) {
                editor.select([object], SelectMode::Replace);
                editor.center_2d_on_selection();
                editor.center_3d_on_selection();
            }
        }
        if fix_one || fix_all {
            if let Some(picked) = &picked {
                let chosen: Vec<Problem> = if fix_all {
                    problems.iter().filter(|problem| problem.kind == picked.kind).cloned().collect()
                } else {
                    vec![picked.clone()]
                };
                if let Err(error) = editor.fix_problems(&chosen) {
                    editor.message = error;
                }
                check_again = true;
            }
        }
        if !open {
            self.problems = ProblemsDialog::default();
        } else if check_again {
            self.check_for_problems(editor);
        }
    }
}
