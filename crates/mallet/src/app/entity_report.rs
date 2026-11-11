//! Hammer's Entity Report (Map menu), which Edit > Find Entities (Ctrl+Shift+F) opens with
//! the name filter ready: the map's entities, narrowed by class, name and keyvalue, with
//! Go to, Select All, Properties and Delete.

use eframe::egui::{self, RichText};

use super::MalletApp;
use crate::editor::{Editor, SelectMode};
use crate::scene::Owner;

/// Most rows the report lists at once; the filters narrow it.
const MAX_ROWS: usize = 2000;

/// The report's filters, the row picked, and whether it shows.
#[derive(Default)]
pub(super) struct EntityReport {
    open: bool,
    class: String,
    name: String,
    key: String,
    value: String,
    selected: Option<u32>,
    /// Set when Find Entities opens it, so the name filter takes the keyboard once.
    focus_name: bool,
}

/// One listed entity: its id, class and name.
struct Row {
    id: u32,
    class: String,
    name: String,
}

impl MalletApp {
    /// Map > Entity Report.
    pub(super) fn open_entity_report(&mut self) {
        self.entity_report.open = true;
    }

    /// Edit > Find Entities: the report with its name filter ready for typing.
    pub(super) fn open_find_entities(&mut self) {
        self.entity_report.open = true;
        self.entity_report.focus_name = true;
    }

    /// Draws the Entity Report while it is open.
    pub(super) fn entity_report_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if !self.entity_report.open {
            return;
        }
        let report = &mut self.entity_report;
        let rows = matching_rows(editor, report);
        let mut open = true;
        let mut go_to = None;
        let mut select_all = false;
        let mut properties = false;
        let mut delete = false;
        egui::Window::new("Entity Report")
            .open(&mut open)
            .default_size([520.0, 440.0])
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                egui::Grid::new("entity_report_filters").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                    ui.label("Class");
                    ui.add(egui::TextEdit::singleline(&mut report.class).hint_text("such as light or func_*").desired_width(220.0));
                    ui.end_row();
                    ui.label("Name");
                    let name = ui.add(egui::TextEdit::singleline(&mut report.name).hint_text("targetname").desired_width(220.0));
                    if std::mem::take(&mut report.focus_name) {
                        name.request_focus();
                    }
                    ui.end_row();
                    ui.label("Keyvalue");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut report.key).hint_text("key").desired_width(104.0));
                        ui.add(egui::TextEdit::singleline(&mut report.value).hint_text("value").desired_width(104.0));
                    });
                    ui.end_row();
                });
                ui.label(RichText::new(format!("{} entities", rows.len())).small().weak());
                ui.separator();
                egui::ScrollArea::vertical().max_height(ui.available_height() - 36.0).auto_shrink([false, false]).show(ui, |ui| {
                    egui::Grid::new("entity_report_rows").num_columns(2).striped(true).spacing([16.0, 2.0]).show(ui, |ui| {
                        for row in rows.iter().take(MAX_ROWS) {
                            let picked = report.selected == Some(row.id);
                            let response = ui.selectable_label(picked, &row.class);
                            ui.label(&row.name);
                            ui.end_row();
                            if response.clicked() {
                                report.selected = Some(row.id);
                            }
                            if response.double_clicked() {
                                go_to = Some(row.id);
                            }
                        }
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    let picked = report.selected.filter(|id| rows.iter().any(|row| row.id == *id));
                    if ui.add_enabled(picked.is_some(), egui::Button::new("Go to")).clicked() {
                        go_to = picked;
                    }
                    properties = ui.add_enabled(picked.is_some(), egui::Button::new("Properties")).clicked();
                    delete = ui.add_enabled(picked.is_some(), egui::Button::new("Delete")).clicked();
                    select_all = ui
                        .add_enabled(!rows.is_empty(), egui::Button::new("Select All"))
                        .on_hover_text("Select every entity listed")
                        .clicked();
                });
            });
        let picked = report.selected;
        if !open {
            report.open = false;
        }
        if let Some(id) = go_to {
            editor.select([Owner::Entity(id)], SelectMode::Replace);
            editor.center_2d_on_selection();
            editor.center_3d_on_selection();
        }
        if select_all {
            editor.select(rows.iter().map(|row| Owner::Entity(row.id)), SelectMode::Replace);
            editor.message = format!("Selected {} entities", rows.len());
        }
        if let Some(id) = picked {
            if properties {
                editor.select([Owner::Entity(id)], SelectMode::Replace);
                editor.properties_world = false;
                editor.properties_open = true;
            }
            if delete {
                editor.select([Owner::Entity(id)], SelectMode::Replace);
                if let Err(error) = editor.delete_selection() {
                    editor.message = error;
                }
                report.selected = None;
            }
        }
    }
}

/// The map's entities that pass the report's filters, by class and then name. A class or
/// name ending in `*` matches everything starting with what comes before it; otherwise
/// the filter matches anywhere in it, ignoring case.
fn matching_rows(editor: &Editor, report: &EntityReport) -> Vec<Row> {
    let Some(document) = &editor.document else {
        return Vec::new();
    };
    let matches = |filter: &str, text: &str| {
        let filter = filter.trim().to_ascii_lowercase();
        let text = text.to_ascii_lowercase();
        match filter.strip_suffix('*') {
            _ if filter.is_empty() => true,
            Some(prefix) => text.starts_with(prefix),
            None => text.contains(&filter),
        }
    };
    let mut rows: Vec<Row> = document
        .vmf
        .entities()
        .filter_map(|entity| {
            let id = entity.id()?;
            let class = entity.classname();
            let name = entity.get("targetname").unwrap_or("");
            let keyvalue = report.key.trim().is_empty()
                || entity
                    .get(report.key.trim())
                    .is_some_and(|value| matches(&report.value, value));
            (matches(&report.class, class) && matches(&report.name, name) && keyvalue).then(|| Row {
                id,
                class: class.to_owned(),
                name: name.to_owned(),
            })
        })
        .collect();
    rows.sort_by(|a, b| (&a.class, &a.name, a.id).cmp(&(&b.class, &b.name, b.id)));
    rows
}
