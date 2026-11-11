//! The Object Properties window (Alt+Enter or double-click): Hammer's entity dialog laid
//! out as one resizable window. Keyvalues get editors that match their FGD type, help for
//! the hovered key shows at the bottom instead of in a side box, and every change is an
//! undoable edit applied at once, so there is no Apply button.

use std::collections::HashMap;

use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icon;
use mallet_content::{ClassKind, EntityClass, Property};
use mallet_vmf::Connection;

use super::MalletApp;
use crate::editor::{Editor, SelectMode};
use crate::properties::InputLink;
use crate::scene::Owner;

const LABEL_WIDTH: f32 = 170.0;
const MAX_CLASS_ITEMS: usize = 300;
const WARNING: Color32 = Color32::from_rgb(235, 110, 90);
const MUTED: Color32 = Color32::from_gray(140);

/// The window's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Tab {
    #[default]
    Keyvalues,
    Outputs,
    Inputs,
    Flags,
}

/// What the window remembers between frames.
#[derive(Default)]
pub(super) struct PropertiesState {
    tab: Tab,
    /// Shows every key as plain text, like Hammer with SmartEdit off.
    raw: bool,
    filter: String,
    /// Text being typed into a field, by field, until it loses focus.
    drafts: HashMap<String, String>,
    /// The entities the drafts belong to; drafts are dropped when this changes.
    drafts_for: Vec<u32>,
    selected_output: Option<usize>,
    new_key: (String, String),
    /// Width of the value column, from the window's width at the start of the frame.
    /// Fields never ask for the space that is left, which would widen the window a
    /// little every frame.
    field_width: f32,
}

/// One entity's keys merged over the whole selection.
enum Value {
    Missing,
    Same(String),
    Different,
}

/// Everything the window shows, copied out of the editor so the UI code can collect
/// changes and apply them afterwards.
struct Snapshot {
    ids: Vec<u32>,
    classname: Option<String>,
    /// Keys in the order of the first entity, then any others.
    keys: Vec<(String, Value)>,
    class: Option<EntityClass>,
    class_names: Vec<String>,
    /// For a single entity: its outputs and the outputs that target it.
    outputs: Option<Vec<Connection>>,
    inputs: Vec<InputLink>,
    targetnames: Vec<String>,
    /// For each output, whether its target names something in the map.
    target_valid: Vec<bool>,
    /// Inputs of each output's target class, by output row.
    target_inputs: Vec<Vec<(String, String)>>,
    /// The side list key that clicks in the 3D view pick faces for, when it is picked
    /// for these entities.
    picking: Option<String>,
}

/// A change made in the window, applied after drawing it.
enum Change {
    SetKey(String, String),
    RemoveKey(String),
    Flag(u32, bool),
    Outputs(Vec<Connection>),
    Select(u32),
    /// Open the model browser to pick a model for this key.
    BrowseModel(String),
    /// Start picking faces for this side list key in the 3D view, or stop when it is
    /// being picked already.
    PickFaces(String),
}

impl MalletApp {
    /// Draws the Object Properties window when it is open.
    pub(super) fn properties_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if !editor.properties_open {
            editor.stop_face_pick();
            return;
        }
        let snapshot = snapshot(editor);
        let state = &mut self.properties;
        if let Some(tab) = editor.properties_tab.take() {
            state.tab = match tab.as_str() {
                "outputs" => Tab::Outputs,
                "inputs" => Tab::Inputs,
                "flags" => Tab::Flags,
                _ => Tab::Keyvalues,
            };
        }
        if state.drafts_for != snapshot.ids {
            state.drafts.clear();
            state.selected_output = None;
            state.drafts_for = snapshot.ids.clone();
        }
        let title = match (&snapshot.classname, snapshot.ids.len()) {
            (_, 0) => "Object Properties".to_owned(),
            (Some(classname), 1) => format!("Object Properties: {classname}"),
            (_, count) => format!("Object Properties: {count} entities"),
        };
        let mut open = true;
        let mut changes = Vec::new();
        let mut browse_model = None;
        egui::Window::new(title)
            .id(egui::Id::new("object_properties"))
            .open(&mut open)
            .default_size([600.0, 560.0])
            .min_width(460.0)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                if snapshot.ids.is_empty() {
                    ui.add_space(12.0);
                    ui.label(RichText::new("Select an entity or brush to see its properties.").color(MUTED));
                    return;
                }
                draw(ui, state, &snapshot, &mut changes);
            });
        if !open {
            editor.properties_open = false;
        }
        let stale_pick = editor.face_pick.as_ref().is_some_and(|pick| {
            !editor.properties_open || pick.ids != snapshot.ids || pick.tab != editor.active_tab_id()
        });
        if stale_pick {
            editor.stop_face_pick();
        }
        for change in changes {
            let ids = &snapshot.ids;
            let result = match change {
                Change::SetKey(key, value) => editor.set_key_on(ids, &key, &value),
                Change::RemoveKey(key) => editor.remove_key_on(ids, &key),
                Change::Flag(bit, on) => editor.set_flag_on(ids, bit, on),
                Change::Outputs(outputs) => editor.set_outputs(ids[0], outputs),
                Change::Select(id) => {
                    editor.select([Owner::Entity(id)], SelectMode::Replace);
                    Ok(())
                }
                Change::BrowseModel(key) => {
                    browse_model = Some(key);
                    Ok(())
                }
                Change::PickFaces(key) => {
                    if snapshot.picking.as_ref() == Some(&key) {
                        editor.stop_face_pick();
                    } else {
                        editor.start_face_pick(ids.clone(), &key);
                    }
                    Ok(())
                }
            };
            if let Err(error) = result {
                editor.message = error;
            }
        }
        if let Some(key) = browse_model {
            let pick = super::model_browser::Pick::Keys {
                ids: snapshot.ids.clone(),
                key,
            };
            self.open_model_browser(editor, pick);
        }
    }
}

fn snapshot(editor: &Editor) -> Snapshot {
    let ids = editor.property_targets();
    let entities: Vec<_> = ids.iter().filter_map(|id| editor.entity(*id)).collect();
    let mut keys: Vec<(String, Value)> = Vec::new();
    for (index, entity) in entities.iter().enumerate() {
        for item in &entity.items {
            let mallet_vmf::EntityItem::Key(pair) = item else {
                continue;
            };
            if keys.iter().any(|(key, _)| key.eq_ignore_ascii_case(&pair.key)) {
                continue;
            }
            // A key the earlier entities lack differs between them.
            let value = if index == 0 {
                Value::Same(pair.value.clone())
            } else {
                Value::Different
            };
            keys.push((pair.key.clone(), value));
        }
    }
    for (key, value) in &mut keys {
        if let Value::Same(first) = value {
            if entities.iter().any(|entity| entity.get(key) != Some(first.as_str())) {
                *value = Value::Different;
            }
        }
    }
    let classname = match entities.first() {
        Some(first) if entities.iter().all(|entity| entity.classname() == first.classname()) => {
            Some(first.classname().to_owned())
        }
        _ => None,
    };
    let class = classname.as_deref().and_then(|name| editor.entity_class(name)).cloned();
    let kind = class.as_ref().map_or(ClassKind::Point, |class| class.kind);
    let class_names = editor
        .game_data
        .as_deref()
        .map(|data| data.class_names(kind).into_iter().map(str::to_owned).collect())
        .unwrap_or_default();
    let single = (entities.len() == 1).then(|| entities[0]);
    let outputs = single.map(|entity| entity.connections());
    let inputs = single
        .and_then(|entity| entity.id())
        .map(|id| editor.inputs_to(id))
        .unwrap_or_default();
    let outputs_list = outputs.clone().unwrap_or_default();
    let picking = editor
        .active_face_pick()
        .filter(|pick| pick.ids == ids)
        .map(|pick| pick.key.clone());
    Snapshot {
        target_valid: outputs_list
            .iter()
            .map(|output| editor.target_exists(&output.target))
            .collect(),
        target_inputs: outputs_list
            .iter()
            .map(|output| {
                editor
                    .target_class(&output.target)
                    .map(|class| {
                        class
                            .inputs
                            .iter()
                            .map(|port| (port.name.clone(), port.description.clone()))
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect(),
        ids,
        classname,
        keys,
        class,
        class_names,
        outputs,
        inputs,
        targetnames: editor.targetnames(),
        picking,
    }
}

fn draw(ui: &mut egui::Ui, state: &mut PropertiesState, snapshot: &Snapshot, changes: &mut Vec<Change>) {
    state.field_width = (ui.available_width() - LABEL_WIDTH - 48.0).max(140.0);
    header(ui, state, snapshot, changes);
    ui.add_space(4.0);
    let flags = snapshot
        .class
        .as_ref()
        .and_then(|class| class.property("spawnflags"))
        .filter(|property| !property.choices.is_empty());
    ui.horizontal(|ui| {
        ui.selectable_value(
            &mut state.tab,
            Tab::Keyvalues,
            format!("{} Keyvalues", icon::LIST_BULLETS),
        );
        let outputs = snapshot.outputs.as_ref().map_or(0, Vec::len);
        ui.selectable_value(
            &mut state.tab,
            Tab::Outputs,
            format!("{} Outputs ({outputs})", icon::ARROW_SQUARE_OUT),
        );
        ui.selectable_value(
            &mut state.tab,
            Tab::Inputs,
            format!("{} Inputs ({})", icon::ARROW_SQUARE_IN, snapshot.inputs.len()),
        );
        if flags.is_some() {
            ui.selectable_value(&mut state.tab, Tab::Flags, format!("{} Flags", icon::FLAG));
        }
    });
    if flags.is_none() && state.tab == Tab::Flags {
        state.tab = Tab::Keyvalues;
    }
    ui.separator();

    // The help box sits at the bottom; the tab gets the space above it.
    let mut help: Option<(String, String)> = None;
    let help_height = 74.0;
    let body_height = (ui.available_height() - help_height - 12.0).max(120.0);
    egui::ScrollArea::vertical()
        .id_salt(("properties_tab", state.tab as u8))
        .max_height(body_height)
        .auto_shrink([false, false])
        .show(ui, |ui| match state.tab {
            Tab::Keyvalues => keyvalues_tab(ui, state, snapshot, changes, &mut help),
            Tab::Outputs => outputs_tab(ui, state, snapshot, changes, &mut help),
            Tab::Inputs => inputs_tab(ui, snapshot, changes),
            Tab::Flags => {
                if let Some(flags) = flags {
                    flags_tab(ui, snapshot, flags, changes);
                }
            }
        });
    ui.separator();
    let (title, text) = help.unwrap_or_else(|| match &snapshot.class {
        Some(class) => (class.name.clone(), class.description.clone()),
        None => (
            "No FGD class".to_owned(),
            "Mallet has no game data for this class. Keys can still be edited as text.".to_owned(),
        ),
    });
    egui::ScrollArea::vertical()
        .id_salt("properties_help")
        .max_height(help_height)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.label(RichText::new(title).strong());
            ui.label(RichText::new(text).color(MUTED));
        });
}

/// Class name with a picker, and the entity id.
fn header(ui: &mut egui::Ui, state: &mut PropertiesState, snapshot: &Snapshot, changes: &mut Vec<Change>) {
    ui.horizontal(|ui| {
        let glyph = match snapshot.class.as_ref().map(|class| class.kind) {
            Some(ClassKind::Solid) => icon::CUBE,
            _ => icon::LIGHTBULB,
        };
        ui.label(RichText::new(glyph).size(20.0));
        let current = snapshot.classname.clone().unwrap_or_default();
        let hint = if snapshot.classname.is_none() {
            "(different classes)"
        } else {
            "classname"
        };
        if let Some(value) = text_field(ui, &mut state.drafts, "classname".to_owned(), &current, hint, 220.0) {
            changes.push(Change::SetKey("classname".to_owned(), value));
        }
        ui.menu_button(icon::CARET_DOWN, |ui| {
            ui.set_max_height(360.0);
            let typed = state
                .drafts
                .get("classname")
                .cloned()
                .unwrap_or_default()
                .to_ascii_lowercase();
            egui::ScrollArea::vertical().show(ui, |ui| {
                let matching = snapshot
                    .class_names
                    .iter()
                    .filter(|name| typed.is_empty() || name.to_ascii_lowercase().contains(&typed));
                for name in matching.take(MAX_CLASS_ITEMS) {
                    if ui
                        .selectable_label(Some(name) == snapshot.classname.as_ref(), name)
                        .clicked()
                    {
                        state.drafts.remove("classname");
                        changes.push(Change::SetKey("classname".to_owned(), name.clone()));
                        ui.close_menu();
                    }
                }
            });
        })
        .response
        .on_hover_text("Change the class");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let ids = if snapshot.ids.len() == 1 {
                format!("id {}", snapshot.ids[0])
            } else {
                format!("{} entities", snapshot.ids.len())
            };
            ui.label(RichText::new(ids).color(MUTED));
        });
    });
}

fn keyvalues_tab(
    ui: &mut egui::Ui,
    state: &mut PropertiesState,
    snapshot: &Snapshot,
    changes: &mut Vec<Change>,
    help: &mut Option<(String, String)>,
) {
    ui.horizontal(|ui| {
        ui.label(icon::MAGNIFYING_GLASS);
        ui.add(
            egui::TextEdit::singleline(&mut state.filter)
                .hint_text("Filter keys")
                .desired_width(180.0),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.toggle_value(&mut state.raw, "Raw keys")
                .on_hover_text("Show every key as plain text, like Hammer with SmartEdit off");
        });
    });
    ui.add_space(4.0);
    let filter = state.filter.to_ascii_lowercase();
    let shown = |name: &str, display: &str| {
        filter.is_empty()
            || name.to_ascii_lowercase().contains(&filter)
            || display.to_ascii_lowercase().contains(&filter)
    };
    let value_of = |key: &str| {
        snapshot
            .keys
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map_or(&Value::Missing, |(_, value)| value)
    };

    if state.raw || snapshot.class.is_none() {
        egui::Grid::new("raw_keys")
            .num_columns(3)
            .striped(true)
            .spacing([10.0, 4.0])
            .show(ui, |ui| {
                for (key, value) in &snapshot.keys {
                    if key.eq_ignore_ascii_case("id") || !shown(key, key) {
                        continue;
                    }
                    key_label(ui, key);
                    let (current, hint) = current_and_hint(value, "");
                    let width = state.field_width - 30.0;
                    if let Some(text) = text_field(ui, &mut state.drafts, format!("raw:{key}"), &current, &hint, width)
                    {
                        changes.push(Change::SetKey(key.clone(), text));
                    }
                    let removable = !key.eq_ignore_ascii_case("classname");
                    if ui
                        .add_enabled(removable, egui::Button::new(icon::TRASH).small())
                        .on_hover_text("Remove this key")
                        .clicked()
                    {
                        changes.push(Change::RemoveKey(key.clone()));
                    }
                    ui.end_row();
                }
                let (new_key, new_value) = &mut state.new_key;
                ui.add(
                    egui::TextEdit::singleline(new_key)
                        .hint_text("new key")
                        .desired_width(LABEL_WIDTH),
                );
                let width = state.field_width - 30.0;
                ui.add(
                    egui::TextEdit::singleline(new_value)
                        .hint_text("value")
                        .desired_width(width),
                );
                let valid = !new_key.trim().is_empty();
                if ui
                    .add_enabled(valid, egui::Button::new(icon::PLUS).small())
                    .on_hover_text("Add the key")
                    .clicked()
                {
                    changes.push(Change::SetKey(new_key.trim().to_owned(), new_value.clone()));
                    new_key.clear();
                    new_value.clear();
                }
                ui.end_row();
            });
        return;
    }

    let class = snapshot.class.as_ref().expect("checked above");
    egui::Grid::new("smart_keys")
        .num_columns(2)
        .striped(true)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            for property in &class.properties {
                if property.kind == "flags" || !shown(&property.name, &property.display) {
                    continue;
                }
                let label =
                    key_label(ui, &property.display).on_hover_text(format!("{} ({})", property.name, property.kind));
                let editor = property_editor(ui, state, snapshot, property, value_of(&property.name), changes);
                if label.hovered() || editor.hovered() || editor.has_focus() {
                    *help = Some((
                        format!("{} ({})", property.display, property.name),
                        property.description.clone(),
                    ));
                }
                ui.end_row();
            }
        });

    // Keys the entity has that its class does not describe.
    let extra: Vec<&(String, Value)> = snapshot
        .keys
        .iter()
        .filter(|(key, _)| {
            !["id", "classname", "spawnflags"]
                .iter()
                .any(|skip| key.eq_ignore_ascii_case(skip))
                && class.property(key).is_none()
                && shown(key, key)
        })
        .collect();
    if !extra.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new("Other keys").strong())
            .on_hover_text("Keys this entity has that its FGD class does not list");
        egui::Grid::new("extra_keys")
            .num_columns(2)
            .striped(true)
            .spacing([10.0, 4.0])
            .show(ui, |ui| {
                for (key, value) in extra {
                    key_label(ui, key);
                    let (current, hint) = current_and_hint(value, "");
                    let width = state.field_width;
                    if let Some(text) = text_field(ui, &mut state.drafts, format!("key:{key}"), &current, &hint, width)
                    {
                        changes.push(Change::SetKey(key.clone(), text));
                    }
                    ui.end_row();
                }
            });
    }
}

/// A key's name in the left column, left-aligned and cut short when too long.
fn key_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.allocate_ui_with_layout(
        egui::vec2(LABEL_WIDTH, 18.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(LABEL_WIDTH);
            ui.set_max_width(LABEL_WIDTH);
            ui.add(egui::Label::new(text).truncate().sense(egui::Sense::hover()))
        },
    )
    .inner
}

/// The text a field shows and its grey hint: the value, nothing with "(different)" when
/// the selection disagrees, or nothing with the default when the key is missing.
fn current_and_hint(value: &Value, default: &str) -> (String, String) {
    match value {
        Value::Same(text) => (text.clone(), String::new()),
        Value::Different => (String::new(), "(different)".to_owned()),
        Value::Missing => (String::new(), default.to_owned()),
    }
}

/// An editor matching the keyvalue's FGD type. Returns its response for help on hover.
fn property_editor(
    ui: &mut egui::Ui,
    state: &mut PropertiesState,
    snapshot: &Snapshot,
    property: &Property,
    value: &Value,
    changes: &mut Vec<Change>,
) -> egui::Response {
    let width = state.field_width;
    let (current, hint) = current_and_hint(value, &property.default);
    let draft_key = format!("key:{}", property.name);
    if property.read_only {
        return ui.add_enabled(
            false,
            egui::TextEdit::singleline(&mut current.clone()).desired_width(width),
        );
    }
    match property.kind.as_str() {
        "choices" if !property.choices.is_empty() => {
            let effective = if matches!(value, Value::Missing) {
                property.default.as_str()
            } else {
                current.as_str()
            };
            let selected = match value {
                Value::Different => "(different)".to_owned(),
                _ => property
                    .choices
                    .iter()
                    .find(|choice| choice.value.trim() == effective.trim())
                    .map_or_else(|| effective.to_owned(), |choice| choice.name.clone()),
            };
            egui::ComboBox::from_id_salt(&draft_key)
                .selected_text(selected)
                .width(width - 8.0)
                .show_ui(ui, |ui| {
                    for choice in &property.choices {
                        let chosen = effective.trim() == choice.value.trim();
                        if ui
                            .selectable_label(chosen, &choice.name)
                            .on_hover_text(&choice.value)
                            .clicked()
                            && !chosen
                        {
                            changes.push(Change::SetKey(property.name.clone(), choice.value.clone()));
                        }
                    }
                })
                .response
        }
        "color255" | "color1" => {
            let scale = if property.kind == "color1" { 255.0 } else { 1.0 };
            let source = if current.is_empty() {
                property.default.clone()
            } else {
                current.clone()
            };
            let numbers: Vec<f32> = source.split_whitespace().filter_map(|n| n.parse().ok()).collect();
            let mut rgb = [0u8; 3];
            for (channel, number) in rgb.iter_mut().zip(&numbers) {
                *channel = (number * scale).round().clamp(0.0, 255.0) as u8;
            }
            ui.horizontal(|ui| {
                let picker = ui.color_edit_button_srgb(&mut rgb);
                if picker.changed() {
                    let mut parts: Vec<String> = rgb
                        .iter()
                        .map(|channel| {
                            if scale == 1.0 {
                                channel.to_string()
                            } else {
                                format!("{:.3}", f32::from(*channel) / scale)
                            }
                        })
                        .collect();
                    parts.extend(numbers.iter().skip(3).map(|n| n.to_string()));
                    changes.push(Change::SetKey(property.name.clone(), parts.join(" ")));
                }
                let width = state.field_width - 44.0;
                if let Some(text) = text_field(ui, &mut state.drafts, draft_key.clone(), &current, &hint, width) {
                    changes.push(Change::SetKey(property.name.clone(), text));
                }
                picker
            })
            .inner
        }
        "studio" => {
            ui.horizontal(|ui| {
                let width = (width - 34.0).max(80.0);
                let (response, committed) =
                    text_field_response(ui, &mut state.drafts, draft_key.clone(), &current, &hint, width);
                if let Some(text) = committed {
                    changes.push(Change::SetKey(property.name.clone(), text));
                }
                if ui
                    .button(icon::CUBE_FOCUS)
                    .on_hover_text("Model browser: pick a model")
                    .clicked()
                {
                    state.drafts.remove(&draft_key);
                    changes.push(Change::BrowseModel(property.name.clone()));
                }
                response
            })
            .inner
        }
        "sidelist" => {
            ui.horizontal(|ui| {
                let width = (width - 34.0).max(80.0);
                let (response, committed) =
                    text_field_response(ui, &mut state.drafts, draft_key.clone(), &current, &hint, width);
                if let Some(text) = committed {
                    changes.push(Change::SetKey(property.name.clone(), text));
                }
                let picking = snapshot.picking.as_deref() == Some(property.name.as_str());
                let tip = if picking {
                    "Stop picking faces (Escape)"
                } else {
                    "Pick faces in the 3D view: click a face to list only it, Ctrl+click to add or take off a face"
                };
                if ui
                    .selectable_label(picking, icon::EYEDROPPER)
                    .on_hover_text(tip)
                    .clicked()
                {
                    state.drafts.remove(&draft_key);
                    changes.push(Change::PickFaces(property.name.clone()));
                }
                response
            })
            .inner
        }
        "target_destination" | "target_name_or_class" => {
            let (response, committed) =
                target_field(ui, state, draft_key, &current, &hint, &snapshot.targetnames, width);
            if let Some(text) = committed {
                changes.push(Change::SetKey(property.name.clone(), text));
            }
            response
        }
        _ => {
            let (response, committed) = text_field_response(ui, &mut state.drafts, draft_key, &current, &hint, width);
            if let Some(text) = committed {
                changes.push(Change::SetKey(property.name.clone(), text));
            }
            response
        }
    }
}

fn draft_text(drafts: &HashMap<String, String>, key: &str, current: &str) -> String {
    drafts.get(key).cloned().unwrap_or_else(|| current.to_owned())
}

/// A text field that keeps what is typed as a draft and returns the new text once, when
/// the field loses focus (Enter, Tab or clicking elsewhere) with a changed value.
fn text_field(
    ui: &mut egui::Ui,
    drafts: &mut HashMap<String, String>,
    key: String,
    current: &str,
    hint: &str,
    width: f32,
) -> Option<String> {
    text_field_response(ui, drafts, key, current, hint, width).1
}

fn text_field_response(
    ui: &mut egui::Ui,
    drafts: &mut HashMap<String, String>,
    key: String,
    current: &str,
    hint: &str,
    width: f32,
) -> (egui::Response, Option<String>) {
    let mut text = draft_text(drafts, &key, current);
    let response = ui.add(
        egui::TextEdit::singleline(&mut text)
            .hint_text(hint)
            .desired_width(width),
    );
    if response.changed() {
        drafts.insert(key.clone(), text);
    }
    let mut committed = None;
    if response.lost_focus() {
        if let Some(draft) = drafts.remove(&key) {
            if draft != current {
                committed = Some(draft);
            }
        }
    }
    (response, committed)
}

/// A text field with a menu of the map's targetnames, filtered by what is typed.
fn target_field(
    ui: &mut egui::Ui,
    state: &mut PropertiesState,
    key: String,
    current: &str,
    hint: &str,
    targetnames: &[String],
    width: f32,
) -> (egui::Response, Option<String>) {
    let inner = ui.horizontal(|ui| {
        let (response, mut committed) = text_field_response(
            ui,
            &mut state.drafts,
            key.clone(),
            current,
            hint,
            (width - 34.0).max(80.0),
        );
        let typed = draft_text(&state.drafts, &key, current).to_ascii_lowercase();
        ui.menu_button(icon::CARET_DOWN, |ui| {
            ui.set_max_height(300.0);
            egui::ScrollArea::vertical().show(ui, |ui| {
                let matching: Vec<&String> = targetnames
                    .iter()
                    .filter(|name| typed.is_empty() || name.to_ascii_lowercase().contains(&typed))
                    .take(MAX_CLASS_ITEMS)
                    .collect();
                if matching.is_empty() {
                    ui.label(RichText::new("No named entities").color(MUTED));
                }
                for name in matching {
                    if ui.selectable_label(name == current, name).clicked() {
                        state.drafts.remove(&key);
                        committed = Some(name.clone());
                        ui.close_menu();
                    }
                }
            });
        })
        .response
        .on_hover_text("Pick a named entity");
        (response, committed)
    });
    inner.inner
}

fn flags_tab(ui: &mut egui::Ui, snapshot: &Snapshot, flags: &Property, changes: &mut Vec<Change>) {
    let value = snapshot
        .keys
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("spawnflags"))
        .map(|(_, value)| value);
    let (bits, mixed) = match value {
        Some(Value::Same(text)) => (text.trim().parse::<u32>().unwrap_or(0), false),
        Some(Value::Different) => (0, true),
        _ => (0, false),
    };
    for choice in &flags.choices {
        let Ok(bit) = choice.value.trim().parse::<u32>() else {
            continue;
        };
        let mut on = bits & bit != 0;
        let checkbox = egui::Checkbox::new(&mut on, &choice.name).indeterminate(mixed);
        if ui.add(checkbox).on_hover_text(format!("Bit {bit}")).changed() {
            changes.push(Change::Flag(bit, on));
        }
    }
}

fn outputs_tab(
    ui: &mut egui::Ui,
    state: &mut PropertiesState,
    snapshot: &Snapshot,
    changes: &mut Vec<Change>,
    help: &mut Option<(String, String)>,
) {
    let Some(outputs) = &snapshot.outputs else {
        ui.label(RichText::new("Select a single entity to edit its outputs.").color(MUTED));
        return;
    };
    let class_outputs: Vec<(String, String)> = snapshot
        .class
        .as_ref()
        .map(|class| {
            class
                .outputs
                .iter()
                .map(|port| (port.name.clone(), port.description.clone()))
                .collect()
        })
        .unwrap_or_default();

    ui.horizontal(|ui| {
        if ui.button(format!("{} Add", icon::PLUS)).clicked() {
            let mut list = outputs.clone();
            list.push(Connection {
                output: class_outputs.first().map(|(name, _)| name.clone()).unwrap_or_default(),
                target: String::new(),
                input: String::new(),
                parameter: String::new(),
                delay: 0.0,
                refires: -1,
            });
            state.selected_output = Some(list.len() - 1);
            changes.push(Change::Outputs(list));
        }
        let selected = state.selected_output.filter(|row| *row < outputs.len());
        if ui
            .add_enabled(
                selected.is_some(),
                egui::Button::new(format!("{} Duplicate", icon::COPY)),
            )
            .clicked()
        {
            if let Some(row) = selected {
                let mut list = outputs.clone();
                list.insert(row + 1, outputs[row].clone());
                state.selected_output = Some(row + 1);
                changes.push(Change::Outputs(list));
            }
        }
        if ui
            .add_enabled(selected.is_some(), egui::Button::new(format!("{} Delete", icon::TRASH)))
            .clicked()
        {
            if let Some(row) = selected {
                let mut list = outputs.clone();
                list.remove(row);
                state.selected_output = None;
                changes.push(Change::Outputs(list));
            }
        }
    });
    ui.add_space(4.0);

    if outputs.is_empty() {
        ui.label(RichText::new("No outputs. Add one to make this entity fire inputs on others.").color(MUTED));
        return;
    }
    egui::Grid::new("outputs")
        .num_columns(6)
        .striped(true)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for heading in ["My output", "Target", "Input", "Parameter", "Delay", "Refires"] {
                ui.label(RichText::new(heading).strong());
            }
            ui.end_row();
            for (row, output) in outputs.iter().enumerate() {
                let selected = state.selected_output == Some(row);
                if ui.selectable_label(selected, &output.output).clicked() {
                    state.selected_output = Some(row);
                }
                let target = if snapshot.target_valid.get(row).copied().unwrap_or(true) {
                    RichText::new(&output.target)
                } else {
                    RichText::new(&output.target).color(WARNING)
                };
                let response = ui.selectable_label(selected, target);
                if !snapshot.target_valid.get(row).copied().unwrap_or(true) {
                    response.clone().on_hover_text("No entity in the map has this name");
                }
                if response.clicked() {
                    state.selected_output = Some(row);
                }
                for text in [
                    output.input.clone(),
                    output.parameter.clone(),
                    output.delay.to_string(),
                    if output.refires < 0 {
                        "∞".to_owned()
                    } else {
                        output.refires.to_string()
                    },
                ] {
                    if ui.selectable_label(selected, text).clicked() {
                        state.selected_output = Some(row);
                    }
                }
                ui.end_row();
            }
        });

    let Some(row) = state.selected_output.filter(|row| *row < outputs.len()) else {
        ui.add_space(6.0);
        ui.label(RichText::new("Click an output to edit it.").color(MUTED));
        return;
    };
    ui.add_space(8.0);
    let output = &outputs[row];
    let mut edited = output.clone();
    let mut changed = false;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        egui::Grid::new("output_editor")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                let width = (state.field_width + LABEL_WIDTH - 150.0 - 34.0).max(140.0);
                ui.label("My output");
                if let Some(text) = port_field(
                    ui,
                    state,
                    format!("out:{row}:output"),
                    &output.output,
                    &class_outputs,
                    width,
                    help,
                ) {
                    edited.output = text;
                    changed = true;
                }
                ui.end_row();

                ui.label("Target entity");
                if let (_, Some(text)) = target_field(
                    ui,
                    state,
                    format!("out:{row}:target"),
                    &output.target,
                    "name",
                    &snapshot.targetnames,
                    width + 34.0,
                ) {
                    edited.target = text;
                    changed = true;
                }
                ui.end_row();

                ui.label("Via this input");
                let inputs = snapshot.target_inputs.get(row).cloned().unwrap_or_default();
                if let Some(text) = port_field(
                    ui,
                    state,
                    format!("out:{row}:input"),
                    &output.input,
                    &inputs,
                    width,
                    help,
                ) {
                    edited.input = text;
                    changed = true;
                }
                ui.end_row();

                ui.label("Parameter");
                if let Some(text) = text_field(
                    ui,
                    &mut state.drafts,
                    format!("out:{row}:parameter"),
                    &output.parameter,
                    "none",
                    width + 34.0,
                ) {
                    edited.parameter = text;
                    changed = true;
                }
                ui.end_row();

                ui.label("Delay (seconds)");
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::DragValue::new(&mut edited.delay)
                                .speed(0.05)
                                .range(0.0..=f64::MAX),
                        )
                        .changed()
                    {
                        changed = true;
                    }
                    ui.add_space(16.0);
                    let mut once = edited.refires == 1;
                    if ui.checkbox(&mut once, "Fire once only").changed() {
                        edited.refires = if once { 1 } else { -1 };
                        changed = true;
                    }
                    ui.add_space(8.0);
                    ui.label("Refires");
                    if ui
                        .add(egui::DragValue::new(&mut edited.refires).range(-1..=i32::MAX))
                        .on_hover_text("-1 fires every time")
                        .changed()
                    {
                        changed = true;
                    }
                });
                ui.end_row();
            });
    });
    if changed && edited != *output {
        let mut list = outputs.clone();
        list[row] = edited;
        changes.push(Change::Outputs(list));
    }
}

/// A text field for an output or input name, with a menu of the names the class has and
/// their help.
fn port_field(
    ui: &mut egui::Ui,
    state: &mut PropertiesState,
    key: String,
    current: &str,
    ports: &[(String, String)],
    width: f32,
    help: &mut Option<(String, String)>,
) -> Option<String> {
    ui.horizontal(|ui| {
        let (response, mut committed) = text_field_response(ui, &mut state.drafts, key.clone(), current, "", width);
        if response.hovered() || response.has_focus() {
            if let Some((name, description)) = ports.iter().find(|(name, _)| name.eq_ignore_ascii_case(current)) {
                *help = Some((name.clone(), description.clone()));
            }
        }
        ui.add_enabled_ui(!ports.is_empty(), |ui| {
            ui.menu_button(icon::CARET_DOWN, |ui| {
                ui.set_max_height(300.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (name, description) in ports {
                        let item = ui.selectable_label(name.eq_ignore_ascii_case(current), name);
                        let item = if description.is_empty() {
                            item
                        } else {
                            item.on_hover_text(description)
                        };
                        if item.clicked() {
                            state.drafts.remove(&key);
                            committed = Some(name.clone());
                            ui.close_menu();
                        }
                    }
                });
            });
        });
        committed
    })
    .inner
}

fn inputs_tab(ui: &mut egui::Ui, snapshot: &Snapshot, changes: &mut Vec<Change>) {
    if snapshot.ids.len() != 1 {
        ui.label(RichText::new("Select a single entity to see what fires its inputs.").color(MUTED));
        return;
    }
    if snapshot.inputs.is_empty() {
        ui.label(RichText::new("No outputs in the map target this entity by name.").color(MUTED));
        return;
    }
    egui::Grid::new("inputs")
        .num_columns(6)
        .striped(true)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for heading in ["Source", "Output", "My input", "Parameter", "Delay", ""] {
                ui.label(RichText::new(heading).strong());
            }
            ui.end_row();
            for link in &snapshot.inputs {
                ui.label(&link.source_name);
                ui.label(&link.connection.output);
                ui.label(&link.connection.input);
                ui.label(&link.connection.parameter);
                ui.label(link.connection.delay.to_string());
                if ui.small_button("Select source").clicked() {
                    changes.push(Change::Select(link.source));
                }
                ui.end_row();
            }
        });
}
