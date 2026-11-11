//! The side panel's new objects: the full list of entity classes for the Entity tool, like
//! Hammer's New Objects bar.

use eframe::egui::{self, RichText};
use egui_phosphor::regular as icon;
use mallet_content::ClassKind;

use crate::editor::{Editor, ENTITY_CLASSES};

/// Most classes the class list shows at once; typing narrows it.
const MAX_CLASS_ITEMS: usize = 400;

/// A drop-down beside the class field listing every point class of the game's FGD, or a
/// few common ones without one. Typing in the field narrows the list, as typing into
/// Hammer's Objects box does.
pub(super) fn entity_class_menu(ui: &mut egui::Ui, editor: &mut Editor) {
    ui.menu_button(icon::CARET_DOWN, |ui| {
        ui.set_max_height(380.0);
        ui.set_min_width(200.0);
        let names: Vec<String> = match editor.game_data.as_deref() {
            Some(data) => data.class_names(ClassKind::Point).into_iter().map(str::to_owned).collect(),
            None => Vec::new(),
        };
        let names = if names.is_empty() {
            ENTITY_CLASSES.iter().map(|name| (*name).to_owned()).collect()
        } else {
            names
        };
        let typed = editor.entity_class.trim().to_ascii_lowercase();
        let exact = names.iter().any(|name| name.eq_ignore_ascii_case(&typed));
        let matching: Vec<&String> = names
            .iter()
            .filter(|name| exact || typed.is_empty() || name.to_ascii_lowercase().contains(&typed))
            .collect();
        if matching.is_empty() {
            ui.label(RichText::new("No class matches what is typed").small());
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for name in matching.into_iter().take(MAX_CLASS_ITEMS) {
                let chosen = name.eq_ignore_ascii_case(&editor.entity_class);
                if ui.selectable_label(chosen, name.as_str()).clicked() {
                    editor.entity_class.clone_from(name);
                    ui.close_menu();
                }
            }
        });
    })
    .response
    .on_hover_text("Every point entity class; type in the box to narrow the list");
}
