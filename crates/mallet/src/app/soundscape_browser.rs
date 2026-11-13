//! The soundscape browser: every soundscape the game's scripts define, laid out like the
//! texture and model browsers. A sidebar narrows the list to a script file, the pane on
//! the right says what the clicked soundscape plays and lets you listen to it, and
//! double-clicking one puts it on the selected soundscape entities or makes it the
//! soundscape of new ones.

use std::collections::BTreeMap;
use std::sync::Arc;

use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icon;
use mallet_vmf::sound::{Outline, OutlineKind};

use super::model_browser::{Pick, Search};
use super::texture_browser::thousands;
use super::MalletApp;
use crate::config::SoundscapeBrowserView;
use crate::editor::{class_plays_soundscape, Editor, Tool};

const MUTED: Color32 = Color32::from_gray(140);
const ACCENT: Color32 = Color32::from_rgb(70, 140, 230);
const WARNING: Color32 = Color32::from_rgb(235, 160, 80);
const SIDEBAR_WIDTH: f32 = 230.0;
const DETAILS_WIDTH: f32 = 400.0;
const MAX_FILTER_HISTORY: usize = 12;
const ROW_HEIGHT: f32 = 20.0;
/// The class the entity tool switches to for a soundscape picked with nothing selected.
const SOUNDSCAPE_CLASS: &str = "env_soundscape";

/// One soundscape of the list.
struct Entry {
    name: String,
    /// The name in lower case, which searches and the map's entities are matched on.
    key: String,
    file: String,
}

/// The game's soundscapes sorted by name, rebuilt when the scripts are read again.
struct Listing {
    /// Which reading of the scripts the list was built from.
    built_from: u64,
    entries: Vec<Entry>,
    /// Script files in the order the game reads them, with how many soundscapes each
    /// defines.
    files: Vec<(String, usize)>,
}

/// What the picked soundscape will be used for, worked out each frame.
struct Target {
    /// The soundscape in use now, lower case, to highlight it.
    current: Option<String>,
    /// What the Use button does, such as "Use on 3 entities".
    action: String,
    /// The sound positions each entity it goes on has, for warning about the ones a
    /// soundscape needs and an entity lacks.
    positions: Vec<(u32, [bool; 8])>,
}

/// What the soundscape browser remembers between frames.
#[derive(Default)]
pub(super) struct SoundscapeBrowserState {
    pub(super) pick: Pick,
    /// Whether the window was open last frame, to tell when it opens.
    was_open: bool,
    /// The filters, saved to the settings when they change.
    view: SoundscapeBrowserView,
    /// Whether `view` has been read from the settings yet.
    view_loaded: bool,
    listing: Option<Listing>,
    /// The soundscape clicked last, shown in the details pane.
    selected: Option<String>,
    /// What the selected soundscape plays, worked out when the selection changes.
    outline: Option<(String, Option<Outline>)>,
    /// How often the map's entities use each soundscape, by lower-case name, cached per
    /// geometry generation.
    used: Option<(u64, Arc<BTreeMap<String, usize>>)>,
    focus_search: bool,
    /// Scroll the list to the selected soundscape this frame.
    reveal: bool,
    /// Bring the selected row into view this frame, scrolling no further than needed.
    follow: bool,
    /// The selection was just picked by hand, which starts it playing when listening is
    /// automatic.
    picked: bool,
    /// Where the window was last frame, so the arrow keys move through the list only
    /// while the mouse is over it.
    window: Option<egui::Rect>,
}

/// Something the browser asks for once it is drawn.
enum Command {
    Use(String),
    Copy(String),
    /// Listen to a soundscape, or stop listening.
    Listen(Option<String>),
    Reload,
}

impl MalletApp {
    /// Opens the soundscape browser to pick a soundscape for `pick`.
    pub(super) fn open_soundscape_browser(&mut self, editor: &mut Editor, pick: Pick) {
        editor.soundscape_browser_open = true;
        self.soundscape_browser.pick = pick;
        self.soundscape_browser.was_open = true;
        self.soundscape_browser.focus_search = true;
        self.soundscape_browser.select_current(editor);
    }

    pub(super) fn soundscape_browser_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if !self.soundscape_browser.view_loaded {
            self.soundscape_browser.view = editor.config.soundscape_browser_view.clone();
            self.soundscape_browser.view_loaded = true;
        }
        if let Some(filter) = editor.soundscape_browser_filter.take() {
            self.soundscape_browser.view.filter = filter;
        }
        if !editor.soundscape_browser_open {
            if self.soundscape_browser.was_open {
                self.soundscape_browser.was_open = false;
                editor.sound.listen(None);
                if self.soundscape_browser.view != editor.config.soundscape_browser_view {
                    editor.config.soundscape_browser_view = self.soundscape_browser.view.clone();
                    editor.save_config();
                }
            }
            return;
        }
        if editor.materials.is_none() {
            editor.soundscape_browser_open = false;
            editor.message = "Choose a game folder first, so there are soundscapes to browse".to_owned();
            return;
        }
        if !self.soundscape_browser.was_open {
            let ids = editor.soundscape_targets();
            let pick = if ids.is_empty() {
                Pick::NewEntity
            } else {
                Pick::Keys {
                    ids,
                    key: "soundscape".to_owned(),
                }
            };
            self.open_soundscape_browser(editor, pick);
        }
        let state = &mut self.soundscape_browser;
        state.list(editor);
        let used = state.used_soundscapes(editor);
        let target = state.target(editor);
        state.describe_selected(editor);

        let title = match &state.pick {
            Pick::NewEntity => format!("{} Soundscapes: pick the soundscape for new entities", icon::WAVEFORM),
            Pick::Keys { ids, .. } if ids.len() == 1 => {
                format!("{} Soundscapes: pick the entity's soundscape", icon::WAVEFORM)
            }
            Pick::Keys { ids, .. } => format!(
                "{} Soundscapes: pick the soundscape of {} entities",
                icon::WAVEFORM,
                ids.len()
            ),
        };
        let mut open = true;
        let mut commands = Vec::new();
        let over_window = ctx
            .pointer_hover_pos()
            .zip(state.window)
            .is_some_and(|(pointer, window)| window.contains(pointer));
        let steps = if over_window && !ctx.wants_keyboard_input() {
            list_steps(ctx)
        } else {
            0
        };
        let shown = egui::Window::new(title)
            .id(egui::Id::new("soundscape_browser"))
            .open(&mut open)
            .default_size([1040.0, 660.0])
            .max_height(ctx.screen_rect().height() - 60.0)
            .min_width(680.0)
            .min_height(340.0)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                egui::TopBottomPanel::bottom("soundscape_browser_bar")
                    .show_inside(ui, |ui| state.bottom_bar(ui, editor, &target, &mut commands));
                egui::SidePanel::left("soundscape_browser_files")
                    .resizable(true)
                    .default_width(SIDEBAR_WIDTH)
                    .width_range(160.0..=380.0)
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("soundscape_sidebar")
                            .auto_shrink([false, false])
                            .show(ui, |ui| state.sidebar(ui));
                    });
                egui::SidePanel::right("soundscape_browser_details")
                    .resizable(true)
                    .default_width(DETAILS_WIDTH)
                    .width_range(280.0..=640.0)
                    .show_inside(ui, |ui| state.details(ui, editor, &target, &mut commands));
                egui::CentralPanel::default()
                    .show_inside(ui, |ui| state.list_pane(ui, &used, &target, steps, &mut commands));
            });
        state.window = shown.map(|shown| shown.response.rect);
        if std::mem::take(&mut state.picked) && state.view.listen_on_select {
            if let Some(name) = &state.selected {
                commands.push(Command::Listen(Some(name.clone())));
            }
        }
        state.save_view(ctx, editor);
        for command in commands {
            match command {
                Command::Use(name) => {
                    let pick = self.soundscape_browser.pick.clone();
                    if let Err(error) = use_soundscape(editor, &pick, &name) {
                        editor.message = error;
                    }
                    editor.soundscape_browser_open = false;
                }
                Command::Copy(name) => {
                    editor.message = format!("Copied {name}");
                    editor.system_clipboard = Some(name);
                }
                Command::Listen(name) => editor.sound.listen(name.as_deref()),
                Command::Reload => editor.sound.reload_scripts(),
            }
        }
        if !open {
            editor.soundscape_browser_open = false;
        }
    }
}

/// Puts a picked soundscape where `pick` says: on the entities' key, or on the entity
/// tool, which switches to `env_soundscape` when its class plays no soundscape.
fn use_soundscape(editor: &mut Editor, pick: &Pick, name: &str) -> Result<(), String> {
    match pick {
        Pick::Keys { ids, key } => {
            editor.set_key_on(ids, key, name)?;
            editor.message = match ids.len() {
                1 => format!("Set the soundscape to {name}"),
                count => format!("Set the soundscape of {count} entities to {name}"),
            };
        }
        Pick::NewEntity => {
            editor.new_soundscape = name.to_owned();
            if !class_plays_soundscape(&editor.entity_class) {
                editor.entity_class = SOUNDSCAPE_CLASS.to_owned();
            }
            editor.set_tool(Tool::Entity);
            editor.message = format!("The entity tool now places {} with {name}", editor.entity_class);
        }
    }
    Ok(())
}

impl SoundscapeBrowserState {
    /// Sorts the game's soundscapes by name once the scripts are read, and again when
    /// they were read anew. A name defined twice shows once, as the game plays the first.
    fn list(&mut self, editor: &Editor) {
        let library = editor.sound.library();
        let generation = editor.sound.library_generation();
        if self
            .listing
            .as_ref()
            .is_some_and(|listing| listing.built_from == generation)
        {
            return;
        }
        self.outline = None;
        let mut files: Vec<(String, usize)> = Vec::new();
        let mut entries: Vec<Entry> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for soundscape in &library.soundscapes {
            let key = soundscape.name.to_ascii_lowercase();
            if !seen.insert(key.clone()) {
                continue;
            }
            match files.iter_mut().find(|(file, _)| *file == soundscape.file) {
                Some((_, defined)) => *defined += 1,
                None => files.push((soundscape.file.clone(), 1)),
            }
            entries.push(Entry {
                name: soundscape.name.clone(),
                key,
                file: soundscape.file.clone(),
            });
        }
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        let file_gone = |file: &String| !files.iter().any(|(name, _)| name == file);
        if !entries.is_empty() && self.view.file.as_ref().is_some_and(file_gone) {
            self.view.file = None;
        }
        self.listing = Some(Listing {
            built_from: generation,
            entries,
            files,
        });
    }

    /// Selects the soundscape in use where the pick goes, so the browser opens on it.
    fn select_current(&mut self, editor: &Editor) {
        let current = match &self.pick {
            Pick::Keys { ids, key } => ids
                .first()
                .and_then(|id| editor.entity(*id))
                .and_then(|entity| entity.get(key))
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
            Pick::NewEntity => Some(editor.new_soundscape.trim().to_owned()).filter(|name| !name.is_empty()),
        };
        if current.is_some() {
            self.selected = current;
            self.reveal = true;
        }
    }

    fn target(&self, editor: &Editor) -> Target {
        match &self.pick {
            Pick::Keys { ids, key } => {
                let mut values = ids
                    .iter()
                    .filter_map(|id| editor.entity(*id))
                    .map(|entity| entity.get(key).unwrap_or_default().trim().to_ascii_lowercase());
                let first = values.next();
                let same = first.is_some() && values.all(|value| Some(&value) == first.as_ref());
                let positions = ids
                    .iter()
                    .filter_map(|id| editor.scene.sounds.soundscape(*id))
                    .map(|soundscape| {
                        (
                            soundscape.entity,
                            soundscape.positions.map(|position| position.is_some()),
                        )
                    })
                    .collect();
                Target {
                    current: first.filter(|name| same && !name.is_empty()),
                    action: match ids.len() {
                        1 => "Use on the entity".to_owned(),
                        count => format!("Use on {count} entities"),
                    },
                    positions,
                }
            }
            Pick::NewEntity => Target {
                current: Some(editor.new_soundscape.trim().to_ascii_lowercase()).filter(|name| !name.is_empty()),
                action: "Use for new entities".to_owned(),
                positions: Vec::new(),
            },
        }
    }

    /// How often the map's soundscape entities name each soundscape.
    fn used_soundscapes(&mut self, editor: &Editor) -> Arc<BTreeMap<String, usize>> {
        let generation = editor.scene.geometry_generation;
        if let Some((cached, used)) = &self.used {
            if *cached == generation {
                return Arc::clone(used);
            }
        }
        let mut used = BTreeMap::new();
        for soundscape in &editor.scene.sounds.soundscapes {
            if !soundscape.soundscape.is_empty() {
                *used.entry(soundscape.soundscape.to_ascii_lowercase()).or_insert(0) += 1;
            }
        }
        let used = Arc::new(used);
        self.used = Some((generation, Arc::clone(&used)));
        used
    }

    /// Works out what the selected soundscape plays, when the selection changed.
    fn describe_selected(&mut self, editor: &Editor) {
        let Some(selected) = &self.selected else {
            self.outline = Some((String::new(), None));
            return;
        };
        if self.outline.as_ref().is_some_and(|(name, _)| name == selected) {
            return;
        }
        self.outline = Some((selected.clone(), editor.sound.library().outline(selected)));
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        let Some(listing) = &self.listing else {
            return;
        };
        ui.label(RichText::new("Script files").strong());
        ui.horizontal(|ui| {
            if ui.selectable_label(self.view.file.is_none(), "All files").clicked() {
                self.view.file = None;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(thousands(listing.entries.len())).small().color(MUTED));
            });
        });
        for (file, defined) in &listing.files {
            ui.horizontal(|ui| {
                let chosen = self.view.file.as_deref() == Some(file.as_str());
                if ui
                    .selectable_label(chosen, short_file(file))
                    .on_hover_text(file)
                    .clicked()
                {
                    self.view.file = (!chosen).then(|| file.clone());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(thousands(*defined)).small().color(MUTED));
                });
            });
        }
    }

    /// Indices into the listing of the soundscapes passing every filter, in name order.
    fn filtered(&self, used: &BTreeMap<String, usize>) -> Vec<usize> {
        let Some(listing) = &self.listing else {
            return Vec::new();
        };
        let search = Search::parse(&self.view.filter);
        listing
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| self.view.file.as_ref().is_none_or(|file| *file == entry.file))
            .filter(|(_, entry)| !self.view.used_only || used.contains_key(&entry.key))
            .filter(|(_, entry)| search.matches(&entry.key))
            .map(|(index, _)| index)
            .collect()
    }

    /// The list of names: a click selects, a double-click uses, and `steps` rows down
    /// (or up, when negative) is where the arrow keys move the selection.
    fn list_pane(
        &mut self,
        ui: &mut egui::Ui,
        used: &BTreeMap<String, usize>,
        target: &Target,
        steps: i32,
        commands: &mut Vec<Command>,
    ) {
        let shown = self.filtered(used);
        let Some(listing) = &self.listing else {
            return;
        };
        if listing.entries.is_empty() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading the game's soundscape scripts...");
            });
            return;
        }
        if shown.is_empty() {
            ui.label(RichText::new("No soundscape matches the filters.").color(MUTED));
            return;
        }
        let row_of = |selected: &Option<String>| {
            let key = selected.as_ref()?.to_ascii_lowercase();
            shown.iter().position(|index| listing.entries[*index].key == key)
        };
        if steps != 0 {
            let last = shown.len() as i64 - 1;
            let row = match row_of(&self.selected) {
                Some(row) => (row as i64 + i64::from(steps)).clamp(0, last),
                None if steps > 0 => 0,
                None => last,
            };
            let name = listing.entries[shown[row as usize]].name.clone();
            if self.selected.as_ref() != Some(&name) {
                self.selected = Some(name);
                self.picked = true;
                self.follow = true;
            }
        }
        let selected_key = self.selected.as_ref().map(|name| name.to_ascii_lowercase());
        let selected_row = row_of(&self.selected);
        let mut area = egui::ScrollArea::vertical()
            .id_salt("soundscape_list")
            .auto_shrink([false, false]);
        if std::mem::take(&mut self.reveal) {
            if let Some(row) = selected_row {
                let spacing = ui.spacing().item_spacing.y;
                area = area.vertical_scroll_offset((row as f32 * (ROW_HEIGHT + spacing) - 60.0).max(0.0));
            }
        }
        let follow = std::mem::take(&mut self.follow);
        let mut followed = false;
        let mut clicked = None;
        area.show_rows(ui, ROW_HEIGHT, shown.len(), |ui, rows| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            for row in rows {
                let entry = &listing.entries[shown[row]];
                let chosen = Some(&entry.key) == selected_key.as_ref();
                let current = target.current.as_ref() == Some(&entry.key);
                ui.horizontal(|ui| {
                    ui.set_height(ROW_HEIGHT);
                    let mut text = RichText::new(&entry.name);
                    if current && !chosen {
                        text = text.color(ACCENT).strong();
                    } else if current {
                        text = text.strong();
                    }
                    let label = ui.selectable_label(chosen, text);
                    if follow && chosen {
                        label.scroll_to_me(None);
                        followed = true;
                    }
                    if label.double_clicked() {
                        commands.push(Command::Use(entry.name.clone()));
                    } else if label.clicked() {
                        clicked = Some(entry.name.clone());
                    }
                    if let Some(count) = used.get(&entry.key) {
                        let times = if *count == 1 {
                            "once".to_owned()
                        } else {
                            format!("{count} times")
                        };
                        ui.label(RichText::new(format!("used {times}")).small().color(MUTED));
                    }
                });
            }
        });
        if let Some(name) = clicked {
            self.selected = Some(name);
            self.picked = true;
        }
        if follow && !followed && selected_row.is_some() {
            self.reveal = true;
            ui.ctx().request_repaint();
        }
    }

    /// What the selected soundscape plays, with the buttons to listen to it and use it.
    fn details(&mut self, ui: &mut egui::Ui, editor: &mut Editor, target: &Target, commands: &mut Vec<Command>) {
        let Some(name) = self.selected.clone() else {
            ui.add_space(8.0);
            ui.label(RichText::new("Click a soundscape to see what it plays and listen to it.").color(MUTED));
            return;
        };
        let listening_to = editor.sound.listening().map(str::to_owned);
        let listening = listening_to
            .as_ref()
            .is_some_and(|playing| playing.eq_ignore_ascii_case(&name));
        ui.add_space(4.0);
        ui.label(RichText::new(&name).strong().size(15.0));
        let file = self
            .listing
            .as_ref()
            .and_then(|listing| {
                listing
                    .entries
                    .iter()
                    .find(|entry| entry.name.eq_ignore_ascii_case(&name))
            })
            .map(|entry| entry.file.clone());
        if let Some(file) = &file {
            ui.label(RichText::new(file).small().color(MUTED));
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let listen = if listening {
                ui.button(format!("{} Stop", icon::STOP))
            } else {
                ui.button(format!("{} Listen", icon::PLAY))
                    .on_hover_text("Play the soundscape around the 3D camera, in place of what the map plays there")
            };
            if listen.clicked() {
                commands.push(Command::Listen((!listening).then(|| name.clone())));
            }
            if ui.button(&target.action).clicked() {
                commands.push(Command::Use(name.clone()));
            }
            if ui.button(icon::COPY).on_hover_text("Copy the name").clicked() {
                commands.push(Command::Copy(name.clone()));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Volume");
            let mut percent = editor.config.sound.volume * 100.0;
            let slider = egui::Slider::new(&mut percent, 0.0..=100.0)
                .suffix(" %")
                .fixed_decimals(0);
            let volume = ui.add(slider);
            if volume.changed() {
                editor.config.sound.volume = percent / 100.0;
            }
            if volume.drag_stopped() || volume.lost_focus() {
                editor.save_config();
            }
        });
        ui.separator();
        let Some((_, Some(outline))) = &self.outline else {
            ui.colored_label(
                WARNING,
                "No script defines this soundscape, so the game plays nothing for it.",
            );
            return;
        };
        if !outline.positions.is_empty() {
            let numbers: Vec<String> = outline.positions.iter().map(i32::to_string).collect();
            ui.label(format!(
                "Plays at sound position{} {}; its entity names them with position0 to position7. The crosshair \
                 button next to those in Object Properties places one with a click in the 3D view.",
                if numbers.len() == 1 { "" } else { "s" },
                numbers.join(", ")
            ));
            for (entity, has) in &target.positions {
                let lacking: Vec<String> = outline
                    .positions
                    .iter()
                    .filter(|position| usize::try_from(**position).ok().and_then(|index| has.get(index)) != Some(&true))
                    .map(|position| format!("position{position}"))
                    .collect();
                if !lacking.is_empty() {
                    ui.colored_label(
                        WARNING,
                        format!(
                            "Entity {entity} has no {}, so those sounds stay silent.",
                            lacking.join(", ")
                        ),
                    );
                }
            }
        }
        egui::ScrollArea::vertical()
            .id_salt("soundscape_outline")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for row in &outline.rows {
                    let glyph = match row.kind {
                        OutlineKind::Dsp => icon::WAVE_SINE,
                        OutlineKind::Loop => icon::REPEAT,
                        OutlineKind::Random => icon::SHUFFLE,
                        OutlineKind::Wave => icon::MUSIC_NOTE,
                        OutlineKind::Soundscape => icon::WAVEFORM,
                        OutlineKind::Problem => icon::WARNING,
                    };
                    let missing = row
                        .wave
                        .as_deref()
                        .is_some_and(|wave| editor.sound.has_sound(wave) == Some(false));
                    ui.horizontal_wrapped(|ui| {
                        ui.add_space(14.0 * row.depth as f32);
                        let mut text = RichText::new(format!("{glyph}  {}", row.text));
                        if row.kind == OutlineKind::Problem || missing {
                            text = text.color(WARNING);
                        } else if row.kind == OutlineKind::Soundscape {
                            text = text.strong();
                        } else if row.kind == OutlineKind::Wave {
                            text = text.small();
                        }
                        let label = ui.label(text);
                        if missing {
                            label.on_hover_text("This sound file is not among the game's files");
                        }
                        if !row.detail.is_empty() {
                            ui.label(RichText::new(&row.detail).small().color(MUTED));
                        }
                    });
                }
            });
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui, editor: &Editor, target: &Target, commands: &mut Vec<Command>) {
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.label("Filter");
            let search = ui
                .add(
                    egui::TextEdit::singleline(&mut self.view.filter)
                        .hint_text("such as canals !util")
                        .desired_width(210.0),
                )
                .on_hover_text(
                    "Every word must be in the soundscape's name. Start a word with ! to leave out soundscapes \
                     that have it. * stands for any text and ? for one character.",
                );
            if self.focus_search {
                search.request_focus();
                self.focus_search = false;
            }
            if search.lost_focus() && !self.view.filter.trim().is_empty() {
                let filter = self.view.filter.trim().to_owned();
                self.view.filter_history.retain(|old| old != &filter);
                self.view.filter_history.insert(0, filter);
                self.view.filter_history.truncate(MAX_FILTER_HISTORY);
            }
            ui.add_enabled_ui(!self.view.filter_history.is_empty(), |ui| {
                ui.menu_button(icon::CARET_DOWN, |ui| {
                    for old in self.view.filter_history.clone() {
                        if ui.button(&old).clicked() {
                            self.view.filter = old;
                            ui.close_menu();
                        }
                    }
                })
                .response
                .on_hover_text("Earlier searches");
            });
            if ui
                .add_enabled(!self.view.filter.is_empty(), egui::Button::new(icon::X))
                .on_hover_text("Clear the search")
                .clicked()
            {
                self.view.filter.clear();
            }
            ui.separator();
            ui.checkbox(&mut self.view.used_only, "Only used soundscapes")
                .on_hover_text("Show only the soundscapes the map's entities name");
            let automatic = ui
                .checkbox(&mut self.view.listen_on_select, "Listen automatically")
                .on_hover_text(
                    "Play each soundscape as soon as you pick it in the list, with a click or with the arrow keys \
                     while the mouse is over this window, without pressing Listen",
                );
            if automatic.changed() && self.view.listen_on_select {
                self.picked = true;
            }
            ui.separator();
            if ui
                .button(format!("{} Reload", icon::ARROWS_CLOCKWISE))
                .on_hover_text("Read the game's soundscape scripts again after editing them")
                .clicked()
            {
                commands.push(Command::Reload);
            }
        });
        ui.horizontal(|ui| {
            let status = editor.sound.status();
            let count = self.listing.as_ref().map_or(0, |listing| listing.entries.len());
            let text = if status.scripts.loading && count == 0 {
                "Reading the scripts...".to_owned()
            } else {
                format!(
                    "{} soundscapes from {} script files",
                    thousands(count),
                    status.scripts.soundscape_files
                )
            };
            ui.label(RichText::new(text).small().color(MUTED));
            for error in &status.scripts.errors {
                ui.separator();
                ui.label(RichText::new(error).small().color(WARNING));
            }
            if let Some(name) = &self.selected {
                ui.separator();
                ui.label(RichText::new(name).monospace().small());
                if ui.small_button(&target.action).clicked() {
                    commands.push(Command::Use(name.clone()));
                }
            }
        });
    }

    /// Saves the filters to the settings once they changed and nothing is being typed.
    fn save_view(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if self.view == editor.config.soundscape_browser_view || ctx.wants_keyboard_input() {
            return;
        }
        editor.config.soundscape_browser_view = self.view.clone();
        editor.save_config();
    }
}

/// How many rows the arrow, page, Home and End keys pressed this frame move the list's
/// selection down, or up when negative. The keys are used up, so nothing behind the
/// window acts on them as well.
fn list_steps(ctx: &egui::Context) -> i32 {
    const PAGE: i32 = 12;
    const END: i32 = 1_000_000;
    let keys = [
        (egui::Key::ArrowDown, 1),
        (egui::Key::ArrowUp, -1),
        (egui::Key::PageDown, PAGE),
        (egui::Key::PageUp, -PAGE),
        (egui::Key::End, END),
        (egui::Key::Home, -END),
    ];
    ctx.input_mut(|input| {
        keys.iter()
            .map(|(key, rows)| input.count_and_consume_key(egui::Modifiers::NONE, *key) as i32 * rows)
            .sum()
    })
}

/// A script file's name without its folder, extension and `soundscapes_` prefix.
fn short_file(file: &str) -> &str {
    let name = file.rsplit('/').next().unwrap_or(file);
    let name = name.strip_suffix(".txt").unwrap_or(name);
    name.strip_prefix("soundscapes_")
        .filter(|rest| !rest.is_empty())
        .unwrap_or(name)
}
