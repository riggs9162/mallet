//! Tie to Entity and Move to World from the window: Hammer's question when the selection
//! already holds a brush entity, and the toEntity and toWorld buttons of the side panel.

use eframe::egui::{self, RichText};

use super::MalletApp;
use crate::editor::{Editor, DEFAULT_SOLID_ENTITY};
use crate::keymap::Action;

/// A brush entity the selected brushes could join instead of making a new one.
pub(super) struct TiePrompt {
    entity: u32,
    classname: String,
}

impl MalletApp {
    /// Hammer's Tie to Entity: with only world brushes selected, they become a new
    /// `func_detail` and Object Properties opens to pick its class. When the selection
    /// also holds a brush entity, asks whether to add the brushes to it instead. A lone
    /// brush entity just opens its properties.
    pub(super) fn tie_to_entity(&mut self, editor: &mut Editor) {
        let existing = editor.selected_brush_entities();
        match existing.first() {
            Some((id, _)) if existing.len() == 1 && editor.selection.len() == 1 => {
                editor.properties_open = true;
                editor.message = format!("Entity {id} is a brush entity already");
            }
            Some((entity, classname)) => {
                self.tie_prompt = Some(TiePrompt {
                    entity: *entity,
                    classname: classname.clone(),
                });
            }
            None => match editor.tie_to_new_entity(DEFAULT_SOLID_ENTITY) {
                Ok(_) => editor.properties_open = true,
                Err(error) => editor.message = error,
            },
        }
    }

    /// The question Tie to Entity asks when the selection holds a brush entity already.
    pub(super) fn tie_prompt_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        let Some(prompt) = &self.tie_prompt else { return };
        let mut answer = None;
        let mut cancel = false;
        egui::Window::new("Tie to Entity")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(format!(
                    "The selection already holds a {} (entity {}).",
                    prompt.classname, prompt.entity
                ));
                ui.label("Add the other selected brushes to it, or make a new entity of all of them?");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button(format!("Add to the {}", prompt.classname)).clicked() {
                        answer = Some(true);
                    }
                    if ui.button("New entity").clicked() {
                        answer = Some(false);
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel {
            self.tie_prompt = None;
        }
        let Some(add) = answer else { return };
        let Some(prompt) = self.tie_prompt.take() else { return };
        let result = if add {
            editor.tie_to_existing_entity(prompt.entity)
        } else {
            editor.tie_to_new_entity(DEFAULT_SOLID_ENTITY).map(drop)
        };
        match result {
            Ok(()) => editor.properties_open = true,
            Err(error) => editor.message = error,
        }
    }
}

/// The side panel's row of Hammer's toEntity and toWorld buttons. Returns the action of
/// the button clicked.
pub(super) fn brush_entity_buttons(ui: &mut egui::Ui, editor: &Editor, keymap: &crate::keymap::KeyMap) -> Option<Action> {
    let mut chosen = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new("Move selected").small());
        let usable = !editor.selection.is_empty();
        let tie = format!(
            "Tie the selected brushes to an entity, such as func_detail or func_door ({})",
            keymap.label(Action::TieToEntity)
        );
        if ui
            .add_enabled(usable, egui::Button::new("toEntity"))
            .on_hover_text(tie)
            .clicked()
        {
            chosen = Some(Action::TieToEntity);
        }
        let world = format!(
            "Turn the selected brush entities back into world brushes ({})",
            keymap.label(Action::MoveToWorld)
        );
        if ui
            .add_enabled(usable, egui::Button::new("toWorld"))
            .on_hover_text(world)
            .clicked()
        {
            chosen = Some(Action::MoveToWorld);
        }
    });
    chosen
}
