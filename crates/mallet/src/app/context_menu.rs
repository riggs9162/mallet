//! The menus that open on a right-click in a viewport. With the selection tool, like
//! Hammer's: what can be done to the selection, the clipboard, plus Mesh to Geometry for
//! props. With the block tool: creating or cancelling the box being drawn.

use eframe::egui::{self, Response};

use super::{menu_item, MalletApp};
use crate::editor::{Editor, PasteAt, PasteOptions, SelectMode};
use crate::keymap::Action;
use crate::prop_brushes::{PropBrushOptions, PropShape};
use crate::scene::Owner;

/// What the menu's Mesh to Geometry entries remember between uses.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ContextMenuState {
    pub options: PropBrushOptions,
    /// Where the menu's Paste puts objects: where the menu was opened.
    pub paste_at: Option<PasteAt>,
}

impl MalletApp {
    /// Opens the selection menu on a right-click in a viewport. Right-clicking something
    /// that is not selected selects it first, as Hammer does; right-clicking empty space
    /// keeps the selection.
    pub(super) fn selection_context_menu(&mut self, response: &Response, hit: Option<Owner>, editor: &mut Editor) {
        if response.secondary_clicked() {
            self.context_menu.paste_at = Some(editor.paste_point());
            if let Some(owner) = hit.filter(|owner| !editor.selection.contains(owner)) {
                editor.select([owner], SelectMode::Replace);
            }
        }
        let mut chosen = None;
        let mut convert = None;
        let mut paste = false;
        let can_paste = !editor.clipboard.is_empty();
        let selected = !editor.selection.is_empty();
        let options = &mut self.context_menu.options;
        let keymap = &self.keymap;
        response.context_menu(|ui| {
            ui.set_min_width(180.0);
            let props = editor.selected_props().len();
            let mesh_menu = |ui: &mut egui::Ui| {
                let collision = ui
                    .button("From Collision Model")
                    .on_hover_text("One brush per convex piece of the prop's collision model (.phy): few brushes, shaped like what players bump into.");
                if collision.clicked() {
                    convert = Some(PropShape::Collision);
                    ui.close_menu();
                }
                let mesh = ui
                    .button("From Visual Mesh")
                    .on_hover_text("Splits the model you see into nearly convex parts: closer to its look, with more brushes.");
                if mesh.clicked() {
                    convert = Some(PropShape::Mesh);
                    ui.close_menu();
                }
                ui.separator();
                ui.checkbox(&mut options.detail, "Put in func_detail");
                ui.checkbox(&mut options.keep_props, "Keep the props");
            };
            ui.add_enabled_ui(props > 0, |ui| {
                let label = if props > 1 {
                    format!("Mesh to Geometry ({props} props)")
                } else {
                    "Mesh to Geometry".to_owned()
                };
                ui.menu_button(label, mesh_menu)
                    .response
                    .on_disabled_hover_text("Select a prop with a model to rebuild it as brushes.");
            });
            ui.separator();
            ui.add_enabled_ui(selected, |ui| {
                menu_item(ui, keymap, "Cut", Action::Cut, &mut chosen);
                menu_item(ui, keymap, "Copy", Action::Copy, &mut chosen);
            });
            ui.add_enabled_ui(can_paste, |ui| {
                let button = egui::Button::new("Paste").shortcut_text(keymap.label(Action::Paste));
                if ui.add(button).on_hover_text("Paste where the menu was opened").clicked() {
                    paste = true;
                    ui.close_menu();
                }
                menu_item(ui, keymap, "Paste Special...", Action::PasteSpecial, &mut chosen);
            });
            ui.separator();
            ui.add_enabled_ui(selected, |ui| {
                menu_item(ui, keymap, "Transform...", Action::Transform, &mut chosen);
                menu_item(ui, keymap, "Snap Selected to Grid", Action::SnapToGrid, &mut chosen);
                menu_item(ui, keymap, "Group", Action::Group, &mut chosen);
                menu_item(ui, keymap, "Ungroup", Action::Ungroup, &mut chosen);
                menu_item(ui, keymap, "Move to VisGroup...", Action::MoveToVisGroup, &mut chosen);
                menu_item(ui, keymap, "Carve", Action::Carve, &mut chosen);
                menu_item(ui, keymap, "Make Hollow...", Action::MakeHollow, &mut chosen);
                menu_item(ui, keymap, "Tie to Entity", Action::TieToEntity, &mut chosen);
                menu_item(ui, keymap, "Move to World", Action::MoveToWorld, &mut chosen);
                menu_item(ui, keymap, "Delete", Action::DeleteSelection, &mut chosen);
                menu_item(ui, keymap, "Select None", Action::ClearSelection, &mut chosen);
                ui.separator();
                menu_item(ui, keymap, "Properties", Action::Properties, &mut chosen);
            });
        });
        if let Some(shape) = convert {
            if let Err(error) = editor.props_to_brushes(shape, self.context_menu.options) {
                editor.message = format!("Mesh to Geometry: {error}");
            }
        }
        if paste {
            let at = self.context_menu.paste_at.unwrap_or_else(|| editor.paste_point());
            if let Err(error) = editor.paste(at, &PasteOptions::default()) {
                editor.message = error;
            }
        }
        if let Some(action) = chosen {
            self.dispatch(action, editor);
        }
    }

    /// Opens the block tool's menu on a right-click in a 2D view, before the box is made:
    /// Create Object makes the pending box (as Enter does) and Cancel Box drops it (as
    /// Escape does). Both need a box drawn first.
    pub(super) fn block_context_menu(&mut self, response: &Response, editor: &mut Editor) {
        let pending = editor.overlay.block.is_some();
        let mut chosen = None;
        let keymap = &self.keymap;
        response.context_menu(|ui| {
            ui.set_min_width(180.0);
            ui.add_enabled_ui(pending, |ui| {
                menu_item(ui, keymap, "Create Object", Action::CreateBlock, &mut chosen);
                menu_item(ui, keymap, "Cancel Box", Action::ClearSelection, &mut chosen);
            });
            if !pending {
                ui.label("Drag out a box in a 2D view first.");
            }
        });
        if let Some(action) = chosen {
            self.dispatch(action, editor);
        }
    }
}
