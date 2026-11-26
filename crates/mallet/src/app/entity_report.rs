//! Hammer's Entity Report (Map menu), which Edit > Find Entities (Ctrl+Shift+F) opens with
//! the name filter ready: the map's entities, narrowed by class, name and keyvalue, with
//! Go to, Select All, Properties and Delete. Its rows follow the editor's selection, so
//! Ctrl and Shift pick several and every button acts on all of them.

use eframe::egui::{self, RichText};

use super::MalletApp;
use crate::editor::{Editor, SelectMode};
use crate::scene::Owner;

/// Most rows the report lists at once; the filters narrow it.
const MAX_ROWS: usize = 2000;

/// The report's filters, the rows it lists, and whether it shows.
#[derive(Default)]
pub(super) struct EntityReport {
    open: bool,
    class: String,
    name: String,
    key: String,
    value: String,
    /// Set when Find Entities opens it, so the name filter takes the keyboard once.
    focus_name: bool,
    /// The row last clicked, where a Shift-click range starts.
    anchor: Option<u32>,
    /// The editor's selection generation the report last saw, so a selection made in the
    /// viewports scrolls its first row into view.
    seen_selection: u64,
    /// The rows of the last filtering, kept until the map or a filter changes.
    cache: Option<Cache>,
}

/// The rows one filtering produced and what they were produced from.
struct Cache {
    key: CacheKey,
    rows: Vec<Row>,
}

/// What the listed rows depend on: the map's edits, the map itself and the filters.
#[derive(PartialEq)]
struct CacheKey {
    changes: u64,
    geometry: u64,
    filters: [String; 4],
}

/// One listed entity: its id, class and name.
struct Row {
    id: u32,
    class: String,
    name: String,
}

/// What a click on a row asks for.
enum RowClick {
    /// Select only this row.
    Only(u32),
    /// Add or remove this row (Ctrl).
    Toggle(u32),
    /// Select the rows from the anchor to this one (Shift), adding them with Ctrl.
    Range(u32, bool),
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
        report.refresh(editor);
        let cache = report.cache.take();
        let rows = cache.as_ref().map(|cache| cache.rows.as_slice()).unwrap_or_default();
        let picked: Vec<u32> = rows
            .iter()
            .map(|row| row.id)
            .filter(|id| editor.selection.contains(&Owner::Entity(*id)))
            .collect();
        let generation = editor.scene.selection_generation;
        let scroll_to = (generation != report.seen_selection).then(|| picked.first().copied()).flatten();
        let mut open = true;
        let mut click = None;
        let mut go_to = false;
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
                ui.label(RichText::new(count_text(rows.len(), picked.len())).small().weak());
                ui.separator();
                egui::ScrollArea::vertical().max_height(ui.available_height() - 36.0).auto_shrink([false, false]).show(ui, |ui| {
                    egui::Grid::new("entity_report_rows").num_columns(2).striped(true).spacing([16.0, 2.0]).show(ui, |ui| {
                        for row in rows.iter().take(MAX_ROWS) {
                            let response = ui.selectable_label(picked.contains(&row.id), &row.class);
                            ui.label(&row.name);
                            ui.end_row();
                            if scroll_to == Some(row.id) {
                                response.scroll_to_me(Some(egui::Align::Center));
                            }
                            if response.clicked() {
                                let modifiers = ui.input(|input| input.modifiers);
                                click = Some(match (modifiers.shift, modifiers.command) {
                                    (true, add) => RowClick::Range(row.id, add),
                                    (false, true) => RowClick::Toggle(row.id),
                                    (false, false) => RowClick::Only(row.id),
                                });
                            }
                            if response.double_clicked() {
                                click = Some(RowClick::Only(row.id));
                                go_to = true;
                            }
                        }
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    let any = !picked.is_empty();
                    go_to |= ui.add_enabled(any, egui::Button::new("Go to")).clicked();
                    properties = ui.add_enabled(any, egui::Button::new("Properties")).clicked();
                    delete = ui
                        .add_enabled(any, egui::Button::new("Delete"))
                        .on_hover_text("Delete every picked entity")
                        .clicked();
                    select_all = ui
                        .add_enabled(!rows.is_empty(), egui::Button::new("Select All"))
                        .on_hover_text("Select every entity listed")
                        .clicked();
                });
            });
        if !open {
            report.open = false;
        }
        let mut targets = picked;
        if let Some(click) = click {
            targets = report.apply_click(editor, rows, &targets, click);
        }
        if select_all {
            editor.select(rows.iter().map(|row| Owner::Entity(row.id)), SelectMode::Replace);
            editor.message = format!("Selected {} entities", rows.len());
        }
        if go_to || properties || delete {
            editor.select(targets.iter().map(|id| Owner::Entity(*id)), SelectMode::Replace);
        }
        if go_to {
            editor.center_2d_on_selection();
            editor.center_3d_on_selection();
        }
        if properties {
            editor.properties_world = false;
            editor.properties_open = true;
        }
        if delete {
            if let Err(error) = editor.delete_selection() {
                editor.message = error;
            }
            report.anchor = None;
        }
        report.seen_selection = editor.scene.selection_generation;
        report.cache = cache;
    }
}

impl EntityReport {
    /// Filters the map again when it or a filter changed since the last filtering.
    fn refresh(&mut self, editor: &Editor) {
        let key = CacheKey {
            changes: editor.document.as_ref().map_or(0, |document| document.history.changes()),
            geometry: editor.scene.geometry_generation,
            filters: [self.class.clone(), self.name.clone(), self.key.clone(), self.value.clone()],
        };
        if self.cache.as_ref().is_some_and(|cache| cache.key == key) {
            return;
        }
        let rows = matching_rows(editor, &key.filters);
        self.cache = Some(Cache { key, rows });
    }

    /// Changes the editor's selection the way a click on a row asks, and returns the ids
    /// picked afterwards. A Shift-click without an anchor picks only the row clicked.
    fn apply_click(&mut self, editor: &mut Editor, rows: &[Row], picked: &[u32], click: RowClick) -> Vec<u32> {
        let ids: Vec<u32> = match click {
            RowClick::Only(id) => {
                self.anchor = Some(id);
                vec![id]
            }
            RowClick::Toggle(id) => {
                self.anchor = Some(id);
                let mut ids: Vec<u32> = picked.iter().copied().filter(|picked| *picked != id).collect();
                if !picked.contains(&id) {
                    ids.push(id);
                }
                ids
            }
            RowClick::Range(id, add) => {
                let position = |id: u32| rows.iter().position(|row| row.id == id);
                let end = position(id);
                let start = self.anchor.and_then(position).or(end);
                let range: Vec<u32> = match (start, end) {
                    (Some(start), Some(end)) => rows[start.min(end)..=start.max(end)].iter().map(|row| row.id).collect(),
                    _ => vec![id],
                };
                if add {
                    picked.iter().copied().chain(range.into_iter().filter(|id| !picked.contains(id))).collect()
                } else {
                    range
                }
            }
        };
        editor.select(ids.iter().map(|id| Owner::Entity(*id)), SelectMode::Replace);
        ids
    }
}

/// The line under the filters: how many entities match, how many are listed when the
/// list is cut short, and how many are picked.
fn count_text(matching: usize, picked: usize) -> String {
    let mut text = match matching {
        1 => "1 entity".to_owned(),
        count if count > MAX_ROWS => format!("{count} entities, showing the first {MAX_ROWS}"),
        count => format!("{count} entities"),
    };
    if picked > 0 {
        text.push_str(&format!(", {picked} picked"));
    }
    text
}

/// The map's entities that pass the report's filters (class, name, key and value), by
/// class and then name. A class or name ending in `*` matches everything starting with
/// what comes before it; otherwise the filter matches anywhere in it, ignoring case.
fn matching_rows(editor: &Editor, filters: &[String; 4]) -> Vec<Row> {
    let Some(document) = &editor.document else {
        return Vec::new();
    };
    let [class_filter, name_filter, key_filter, value_filter] = filters;
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
            let keyvalue = key_filter.trim().is_empty()
                || entity
                    .get(key_filter.trim())
                    .is_some_and(|value| matches(value_filter, value));
            (matches(class_filter, class) && matches(name_filter, name) && keyvalue).then(|| Row {
                id,
                class: class.to_owned(),
                name: name.to_owned(),
            })
        })
        .collect();
    rows.sort_by(|a, b| (&a.class, &a.name, a.id).cmp(&(&b.class, &b.name, b.id)));
    rows
}
