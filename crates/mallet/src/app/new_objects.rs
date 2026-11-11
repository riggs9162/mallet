//! The side panel's new objects: the Block tool's shape and faces, and the full list of
//! entity classes for the Entity tool, like Hammer's New Objects bar.

use eframe::egui::{self, RichText};
use egui_phosphor::regular as icon;
use mallet_content::ClassKind;
use mallet_vmf::primitive::{Shape, MAX_SIDES, MIN_SIDES};

use crate::editor::{Editor, ENTITY_CLASSES};

/// Most classes the class list shows at once; typing narrows it.
const MAX_CLASS_ITEMS: usize = 400;

/// The Block tool's row: the shape it makes and, for round shapes, how many faces they
/// get around, as Hammer's Objects and Faces boxes.
pub(super) fn primitive_row(ui: &mut egui::Ui, editor: &mut Editor) {
    ui.horizontal(|ui| {
        ui.label("Block tool")
            .on_hover_text("The shape the Block tool makes: drag a box in a 2D view and press Enter");
        egui::ComboBox::from_id_salt("block_shape")
            .selected_text(editor.block_shape.label())
            .width(82.0)
            .show_ui(ui, |ui| {
                for shape in Shape::ALL {
                    ui.selectable_value(&mut editor.block_shape, shape, shape.label());
                }
            })
            .response
            .on_hover_text("Objects: block, wedge, cylinder, spike, sphere, or an arch set up in its own dialog");
        let round = editor.block_shape.uses_faces();
        ui.add_enabled(round, egui::Label::new("Faces"));
        ui.add_enabled(
            round,
            egui::DragValue::new(&mut editor.block_faces).range(MIN_SIDES..=MAX_SIDES),
        )
        .on_hover_text("How many sides cylinders, spikes and spheres get around");
    });
}

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
