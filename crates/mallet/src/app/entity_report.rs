//! Hammer's Entity Report (Map menu), which Edit > Find Entities (Ctrl+Shift+F) opens with
//! the name filter ready: the map's entities, narrowed by class, name and keyvalue, with
//! Go to, Select All, Properties and Delete. Its rows follow the editor's selection, so
//! Ctrl and Shift pick several and every button acts on all of them. With one picked, it
//! lists what that entity's outputs fire and what fires its inputs, each a link to the
//! entities at the other end. Rows with something Check for Problems would report carry a
//! warning, and the report can list only those.

use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui::{self, RichText};
use egui_phosphor::regular as icon;

use super::MalletApp;
use crate::editor::{Editor, SelectMode};
use crate::scene::Owner;
use mallet_vmf::{Connection, Entity};

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
    /// Whether only entities with problems are listed.
    problems_only: bool,
    /// The rows of the last filtering, kept until the map or a filter changes.
    cache: Option<Cache>,
    /// The problems of each entity, kept until the map or the game changes.
    problems: Option<ProblemCache>,
}

/// What Check for Problems found about each entity, by id, and what that depends on.
struct ProblemCache {
    key: ProblemKey,
    by_entity: HashMap<u32, Vec<String>>,
}

/// What the problems found depend on: the map's edits, the map itself, the game data and
/// whether a game's files are mounted.
#[derive(PartialEq)]
struct ProblemKey {
    changes: u64,
    geometry: u64,
    game_data: Option<usize>,
    materials: bool,
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
    problems_only: bool,
}

/// One listed entity: its id, class, name and problems.
struct Row {
    id: u32,
    class: String,
    name: String,
    /// What Check for Problems says about it, each as its kind and detail.
    problems: Vec<String>,
}

/// Height of the outputs and inputs list under the rows.
const LINKS_HEIGHT: f32 = 150.0;

/// One output that fires from or into the picked entity, and the entities at its other
/// end: its targets for an output, its source for an input.
struct Link {
    connection: Connection,
    /// The entities at the other end, empty when nothing matches.
    others: Vec<u32>,
    /// How the other end is shown: the target as written, or the source's name and class.
    label: String,
}

/// The picked entity's outputs and the outputs that target it.
struct Links {
    outputs: Vec<Link>,
    inputs: Vec<Link>,
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
        let mut jump: Option<Vec<u32>> = None;
        let links = match picked.as_slice() {
            [id] => links(editor, *id),
            _ => None,
        };
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
                ui.horizontal(|ui| {
                    ui.label(RichText::new(count_text(rows.len(), picked.len())).small().weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.checkbox(&mut report.problems_only, "Only with problems")
                            .on_hover_text("List only the entities Check for Problems has something to say about");
                    });
                });
                ui.separator();
                let links_height = if links.is_some() { LINKS_HEIGHT } else { 0.0 };
                egui::ScrollArea::vertical()
                    .id_salt("entity_report_list")
                    .max_height(ui.available_height() - 36.0 - links_height).auto_shrink([false, false]).show(ui, |ui| {
                    egui::Grid::new("entity_report_rows").num_columns(3).striped(true).spacing([16.0, 2.0]).show(ui, |ui| {
                        for row in rows.iter().take(MAX_ROWS) {
                            let response = ui.selectable_label(picked.contains(&row.id), &row.class);
                            ui.label(&row.name);
                            problem_badge(ui, &row.problems);
                            ui.end_row();
                            if scroll_to == Some(row.id) {
                                response.scroll_to_me(Some(egui::Align::Center));
                            }
                            if response.clicked() {
                                let modifiers = click_modifiers(ui);
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
                if let Some(links) = &links {
                    ui.separator();
                    jump = links_view(ui, links);
                }
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
        if let Some(ids) = jump {
            editor.select(ids.iter().map(|id| Owner::Entity(*id)), SelectMode::Replace);
            editor.center_2d_on_selection();
            editor.center_3d_on_selection();
            report.anchor = ids.first().copied();
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
    /// Checks the map for problems again when it or the game changed since the last check,
    /// and filters it again when it, the problems or a filter changed since the last
    /// filtering.
    fn refresh(&mut self, editor: &Editor) {
        let changes = editor.document.as_ref().map_or(0, |document| document.history.changes());
        let geometry = editor.scene.geometry_generation;
        let problem_key = ProblemKey {
            changes,
            geometry,
            game_data: editor.game_data.as_ref().map(|data| Arc::as_ptr(data) as usize),
            materials: editor.materials.is_some(),
        };
        if self.problems.as_ref().is_none_or(|problems| problems.key != problem_key) {
            self.problems = Some(ProblemCache {
                key: problem_key,
                by_entity: entity_problems(editor),
            });
            self.cache = None;
        }
        let key = CacheKey {
            changes,
            geometry,
            filters: [self.class.clone(), self.name.clone(), self.key.clone(), self.value.clone()],
            problems_only: self.problems_only,
        };
        if self.cache.as_ref().is_some_and(|cache| cache.key == key) {
            return;
        }
        let empty = HashMap::new();
        let problems = self.problems.as_ref().map_or(&empty, |problems| &problems.by_entity);
        let rows = matching_rows(editor, &key.filters, key.problems_only, problems);
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

/// The modifier keys held for this frame's click: those the button's release carries,
/// along with those still held. A key pressed and let go within the same frame as the
/// click is gone from the held keys by the end of the frame, but the release still
/// carries it.
fn click_modifiers(ui: &egui::Ui) -> egui::Modifiers {
    ui.input(|input| {
        let released = input.events.iter().rev().find_map(|event| match event {
            egui::Event::PointerButton {
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers,
                ..
            } => Some(*modifiers),
            _ => None,
        });
        released.map_or(input.modifiers, |modifiers| modifiers | input.modifiers)
    })
}

/// What Check for Problems says about each entity, by id.
fn entity_problems(editor: &Editor) -> HashMap<u32, Vec<String>> {
    let mut by_entity: HashMap<u32, Vec<String>> = HashMap::new();
    for problem in editor.check_for_problems() {
        if let Some(Owner::Entity(id)) = problem.object {
            by_entity
                .entry(id)
                .or_default()
                .push(format!("{}: {}", problem.kind.label(), problem.detail));
        }
    }
    by_entity
}

/// A warning with the number of problems a row has, listing them when hovered, or an empty
/// cell when it has none.
fn problem_badge(ui: &mut egui::Ui, problems: &[String]) {
    if problems.is_empty() {
        ui.label("");
        return;
    }
    let color = ui.visuals().warn_fg_color;
    ui.label(RichText::new(format!("{} {}", icon::WARNING, problems.len())).color(color))
        .on_hover_text(problems.join("\n"));
}

/// Whether an output aimed at `target` reaches `entity`: by its name or its class, ignoring
/// case, or by the start of its name when `target` ends in `*`. Names starting with `!`,
/// such as `!activator`, are only known while the game runs, so they match nothing here.
fn targets(target: &str, entity: &Entity) -> bool {
    let target = target.trim().to_ascii_lowercase();
    if target.is_empty() || target.starts_with('!') {
        return false;
    }
    let name = entity.get("targetname").unwrap_or("").to_ascii_lowercase();
    match target.strip_suffix('*') {
        Some(prefix) => !name.is_empty() && name.starts_with(prefix),
        None => name == target || entity.classname().eq_ignore_ascii_case(&target),
    }
}

/// The outputs of entity `id` with the entities each one reaches, and every output in the
/// map that reaches it with the entity it comes from.
fn links(editor: &Editor, id: u32) -> Option<Links> {
    let vmf = &editor.document.as_ref()?.vmf;
    let entity = vmf.entities().find(|entity| entity.id() == Some(id))?;
    let outputs = entity
        .connections()
        .into_iter()
        .map(|connection| Link {
            others: vmf
                .entities()
                .filter(|other| targets(&connection.target, other))
                .filter_map(Entity::id)
                .collect(),
            label: connection.target.clone(),
            connection,
        })
        .collect();
    let inputs = vmf
        .entities()
        .filter_map(|source| Some((source, source.id()?)))
        .flat_map(|(source, source_id)| {
            let label = match source.get("targetname").filter(|name| !name.is_empty()) {
                Some(name) => format!("{name} ({})", source.classname()),
                None => format!("{} {source_id}", source.classname()),
            };
            source
                .connections()
                .into_iter()
                .filter(|connection| targets(&connection.target, entity))
                .map(move |connection| Link {
                    connection,
                    others: vec![source_id],
                    label: label.clone(),
                })
        })
        .collect();
    Some(Links { outputs, inputs })
}

/// Draws the picked entity's outputs and inputs, and returns the entities to go to when
/// the other end of one is clicked.
fn links_view(ui: &mut egui::Ui, links: &Links) -> Option<Vec<u32>> {
    let mut jump = None;
    egui::ScrollArea::vertical()
        .id_salt("entity_report_links")
        .max_height(LINKS_HEIGHT - 8.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (heading, list, outgoing) in [("Outputs", &links.outputs, true), ("Inputs", &links.inputs, false)] {
                ui.label(RichText::new(format!("{heading} ({})", list.len())).strong());
                if list.is_empty() {
                    ui.label(RichText::new("None").weak());
                }
                for link in list {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let connection = &link.connection;
                        if !outgoing {
                            jump = jump.take().or(link_button(ui, link));
                        }
                        ui.label(&connection.output);
                        ui.label(RichText::new("to").weak());
                        if outgoing {
                            jump = jump.take().or(link_button(ui, link));
                        }
                        ui.label(&connection.input);
                        let extra = connection_details(connection);
                        if !extra.is_empty() {
                            ui.label(RichText::new(extra).weak());
                        }
                    });
                }
            }
        });
    jump
}

/// A connection's parameter, delay and refire limit, as words, leaving out the defaults.
fn connection_details(connection: &Connection) -> String {
    let mut details = Vec::new();
    if !connection.parameter.is_empty() {
        details.push(format!("\"{}\"", connection.parameter));
    }
    if connection.delay > 0.0 {
        details.push(format!("after {}s", connection.delay));
    }
    match connection.refires {
        1 => details.push("once".to_owned()),
        count if count > 1 => details.push(format!("{count} times")),
        _ => {}
    }
    details.join(", ")
}

/// The other end of a link as a button that goes to it, shown as an error when nothing
/// in the map matches. A name only known while the game runs, such as `!activator`, is
/// plain text.
fn link_button(ui: &mut egui::Ui, link: &Link) -> Option<Vec<u32>> {
    if link.label.trim().starts_with('!') {
        ui.label(RichText::new(&link.label).italics());
        return None;
    }
    if link.others.is_empty() {
        let color = ui.visuals().error_fg_color;
        ui.label(RichText::new(&link.label).color(color))
            .on_hover_text("Nothing in the map has this name or class");
        return None;
    }
    let hover = match link.others.len() {
        1 => "Go to this entity".to_owned(),
        count => format!("Go to these {count} entities"),
    };
    ui.link(&link.label).on_hover_text(hover).clicked().then(|| link.others.clone())
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
/// class and then name, with their problems. A class or name ending in `*` matches
/// everything starting with what comes before it; otherwise the filter matches anywhere in
/// it, ignoring case. With `problems_only`, entities with no problems are left out.
fn matching_rows(
    editor: &Editor,
    filters: &[String; 4],
    problems_only: bool,
    problems: &HashMap<u32, Vec<String>>,
) -> Vec<Row> {
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
            let found = problems.get(&id);
            let listed = matches(class_filter, class)
                && matches(name_filter, name)
                && keyvalue
                && (!problems_only || found.is_some());
            listed.then(|| Row {
                id,
                class: class.to_owned(),
                name: name.to_owned(),
                problems: found.cloned().unwrap_or_default(),
            })
        })
        .collect();
    rows.sort_by(|a, b| (&a.class, &a.name, a.id).cmp(&(&b.class, &b.name, b.id)));
    rows
}
