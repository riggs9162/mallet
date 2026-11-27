//! The texture browser: every material the game mounts, as thumbnails that load in the
//! background while you scroll. Like Hammer's texture list, with a sidebar to narrow it
//! to one game, addon or folder, filters for what a material does along the top, and a
//! pane describing the material clicked last. The parts it shares with the model
//! browser live in [`super::asset_browser`].

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use eframe::egui::{self, RichText};
use egui_phosphor::regular as icon;
use mallet_content::vtf::Image;
use mallet_content::{parallel_map, MaterialInfo, MaterialLoader};

use super::asset_browser::{
    self, card, checkerboard, chip, empty_state, grid_columns, grid_lines, group_by_folder, hidden_paths, in_folder,
    muted, paint_thumbnail, path_bar, per_row_slider, remember, restore_hidden, source_lines, status_line, thousands,
    toggle_chip, toggle_favourite, upload, warning, Catalog, FolderPick, Grid, KeyMove, Listing, Loaders, Marks,
    Narrowing, Search, SearchBox, Shelf, ShelfAction, Shown, Thumbnail, Thumbnails,
};
use super::thumbnail_cache::ThumbnailCache;
use super::MalletApp;
use crate::config::{TextureBrowserOptions, TextureBrowserView};
use crate::editor::Editor;

/// Requests waiting to load; older ones are dropped, as they have scrolled out of view.
const MAX_REQUESTS: usize = 256;
/// Materials described per batch while indexing. Between batches the index waits for
/// the thumbnails on screen, which share the same drives.
const INDEX_BATCH: usize = 256;
const SIDEBAR_WIDTH: f32 = 210.0;
const DETAILS_WIDTH: f32 = 300.0;
/// Folders whose materials are not meant for brushes, hidden while "Brush materials" is on.
const NON_BRUSH_FOLDERS: [&str; 9] = [
    "models",
    "particle",
    "particles",
    "vgui",
    "hud",
    "console",
    "sprites",
    "editor",
    "effects",
];

/// What a texture picked in the browser is for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Pick {
    /// The current texture, which the texture tool applies.
    #[default]
    Current,
    /// The material Replace Textures looks for.
    Find,
    /// The material Replace Textures puts in its place.
    Replace,
}

/// How many of the described materials have each trait, for the counts on the filters.
#[derive(Clone, Copy, Default)]
struct TraitCounts {
    opaque: usize,
    translucent: usize,
    self_illum: usize,
    envmap: usize,
    errors: usize,
}

/// What the texture browser remembers between frames.
pub(super) struct TextureBrowserState {
    pub(super) pick: Pick,
    /// The filters and layout, saved to the settings when they change.
    view: TextureBrowserView,
    /// Whether `view` has been read from the settings yet.
    view_loaded: bool,
    catalog: Catalog<()>,
    hidden_sources: HashSet<usize>,
    loaders: Option<Loaders<Option<Image>>>,
    thumbnails: Thumbnails,
    info: HashMap<String, MaterialInfo>,
    index_receiver: Option<Receiver<Vec<(String, MaterialInfo)>>>,
    /// The game's loader, for describing materials once they are listed.
    index_loader: Option<MaterialLoader>,
    /// The material clicked last, shown in the details pane.
    selected: Option<String>,
    /// The selected material's VMT, read when it is picked.
    vmt: Option<(String, Option<String>)>,
    /// Whether the sidebar should open the way down to the chosen folder this frame.
    reveal_folder: bool,
    /// The text the sidebar's folder tree is searched for.
    folder_search: String,
    /// Whether the shelf shows the starred materials rather than the recent ones.
    shelf_starred: bool,
    /// The window's place on screen last frame, for the keys that move the selection.
    window: Option<egui::Rect>,
    /// The keys read this frame, before the editor's shortcuts could act on them.
    keys: KeyMove,
    /// The trait counts, with how many materials were described when they were made.
    trait_counts: (usize, TraitCounts),
    /// Materials the map uses, cached per geometry generation.
    used: Option<(u64, Arc<BTreeSet<String>>)>,
    pub(super) focus_search: bool,
}

impl Default for TextureBrowserState {
    fn default() -> Self {
        Self {
            pick: Pick::Current,
            view: TextureBrowserView::default(),
            view_loaded: false,
            catalog: Catalog::default(),
            hidden_sources: HashSet::new(),
            loaders: None,
            thumbnails: Thumbnails::new(MAX_REQUESTS),
            info: HashMap::new(),
            index_receiver: None,
            index_loader: None,
            selected: None,
            vmt: None,
            reveal_folder: false,
            folder_search: String::new(),
            shelf_starred: false,
            window: None,
            keys: KeyMove::default(),
            trait_counts: (usize::MAX, TraitCounts::default()),
            used: None,
            focus_search: true,
        }
    }
}

/// Something the browser asks for once it is drawn.
enum Command {
    Choose(String, bool),
    Mark(String),
    Replace(String),
    Copy(String),
}

impl MalletApp {
    /// Takes the keys that move the texture browser's selection, before the editor's
    /// shortcuts see them, while the browser is open and the pointer is over it.
    pub(super) fn read_texture_browser_keys(&mut self, ctx: &egui::Context, editor: &Editor) {
        let state = &mut self.texture_browser;
        state.keys = if editor.texture_browser_open {
            KeyMove::read(ctx, state.window)
        } else {
            KeyMove::default()
        };
    }

    /// Opens the texture browser to pick a texture for `pick`.
    pub(super) fn open_texture_browser(&mut self, editor: &mut Editor, pick: Pick) {
        editor.texture_browser_open = true;
        self.texture_browser.pick = pick;
        self.texture_browser.focus_search = true;
    }

    pub(super) fn texture_browser_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if !self.texture_browser.view_loaded {
            self.texture_browser.view = editor.config.texture_browser_view.clone();
            self.texture_browser.view_loaded = true;
        }
        if let Some(filter) = editor.texture_browser_filter.take() {
            self.texture_browser.view.filter = filter;
        }
        if !editor.texture_browser_open {
            if self.texture_browser.view_loaded && self.texture_browser.view != editor.config.texture_browser_view {
                editor.config.texture_browser_view = self.texture_browser.view.clone();
                editor.save_config();
            }
            return;
        }
        let Some(loader) = editor.materials.as_ref().map(|materials| materials.loader()) else {
            editor.texture_browser_open = false;
            editor.message = "Choose a game folder first, so there are materials to browse".to_owned();
            return;
        };
        let game = editor.game_dir().map(PathBuf::from).unwrap_or_default();
        let state = &mut self.texture_browser;
        state.thumbnails.next_frame();
        let options = editor.config.texture_browser.clone();
        state.start(&game, &loader, &options);
        state.receive(ctx);
        let used = state.used_materials(editor);
        let scroll_step = editor.config.view.texture_scroll.clamp(0.25, 2.0) as f32;
        let current = match state.pick {
            Pick::Current => editor.material.to_ascii_lowercase(),
            Pick::Find => self.face_edit.find.to_ascii_lowercase(),
            Pick::Replace => self.face_edit.replace.to_ascii_lowercase(),
        };
        let keys = std::mem::take(&mut state.keys);
        let mut names = state.filtered(&used);
        let groups = state
            .catalog
            .listing
            .as_ref()
            .filter(|_| state.view.group_by_folder)
            .map(|listing| group_by_folder(listing, &mut names, whole));

        let title = match state.pick {
            Pick::Current => format!("{} Textures", icon::IMAGES),
            Pick::Find => format!("{} Textures: pick the texture to find", icon::IMAGES),
            Pick::Replace => format!("{} Textures: pick the replacement", icon::IMAGES),
        };
        let mut open = true;
        let mut commands = Vec::new();
        let shown = egui::Window::new(title)
            .id(egui::Id::new("texture_browser"))
            .open(&mut open)
            .default_size([1180.0, 740.0])
            .max_height(ctx.screen_rect().height() - 60.0)
            .min_width(720.0)
            .min_height(380.0)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                // Panels fill exactly the window, so it never grows to fit its content.
                egui::TopBottomPanel::top("texture_browser_tools").show_inside(ui, |ui| state.toolbar(ui, &used));
                egui::TopBottomPanel::bottom("texture_browser_status").show_inside(ui, |ui| state.status(ui, &names));
                egui::SidePanel::left("texture_browser_sources")
                    .resizable(true)
                    .default_width(SIDEBAR_WIDTH)
                    .width_range(160.0..=360.0)
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("texture_sidebar")
                            .auto_shrink([false, false])
                            .show(ui, |ui| state.sidebar(ui));
                    });
                if state.view.show_details {
                    egui::SidePanel::right("texture_browser_details")
                        .resizable(true)
                        .default_width(DETAILS_WIDTH)
                        .width_range(220.0..=560.0)
                        .show_inside(ui, |ui| state.details(ui, &loader, &used, &mut commands));
                }
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    state.grid(
                        ui,
                        Shown {
                            entries: &names,
                            groups: groups.as_deref(),
                        },
                        &current,
                        scroll_step,
                        keys,
                        &mut commands,
                    );
                });
            });
        state.window = shown.map(|shown| shown.response.rect);
        state.thumbnails.trim(options.cache_size());
        state.save_view(ctx, editor);
        for command in commands {
            match command {
                Command::Choose(material, confirm) => {
                    remember(&mut self.texture_browser.view.recent, &material);
                    match self.texture_browser.pick {
                        Pick::Current => {
                            editor.material = material.clone();
                            let faces = editor.texture_targets();
                            if confirm && !faces.is_empty() {
                                if let Err(error) = editor.apply_material(&faces, &material) {
                                    editor.message = error;
                                }
                            }
                        }
                        Pick::Find => self.face_edit.find = material,
                        Pick::Replace => self.face_edit.replace = material,
                    }
                    if confirm {
                        editor.texture_browser_open = false;
                        self.texture_browser.pick = Pick::Current;
                    }
                }
                Command::Mark(material) => {
                    editor.mark_material(&material);
                }
                Command::Replace(material) => self.open_replace(&material),
                Command::Copy(material) => {
                    editor.message = format!("Copied {material}");
                    editor.system_clipboard = Some(material);
                }
            }
        }
        if !open {
            editor.texture_browser_open = false;
            self.texture_browser.pick = Pick::Current;
        }
    }
}

/// A material's name as the browser shows it, which is its whole name.
fn whole(name: &str) -> &str {
    name
}

/// Whether a material lies in a folder whose materials are not meant for brushes.
fn non_brush(name: &str) -> bool {
    NON_BRUSH_FOLDERS
        .iter()
        .any(|folder| name.strip_prefix(folder).is_some_and(|rest| rest.starts_with('/')))
}

/// The shader a VMT names, its first word, without quotes.
fn shader_of(vmt: &str) -> Option<String> {
    let line = vmt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("//"))?;
    let word = line.split(['{', ' ', '\t']).next()?.trim_matches('"');
    (!word.is_empty()).then(|| word.to_owned())
}

impl TextureBrowserState {
    /// Lists the game's materials, starts describing them for the filters, and starts the
    /// thumbnail loaders, once per game and again when their settings change.
    fn start(&mut self, game: &PathBuf, loader: &MaterialLoader, options: &TextureBrowserOptions) {
        self.index_loader = Some(loader.clone());
        let (listing_loader, listing_game) = (loader.clone(), game.clone());
        let started = self.catalog.list(game, move || {
            let materials = listing_loader.list_sourced();
            Listing::new(listing_game, materials, listing_loader.sources(), whole, ())
        });
        if started {
            self.index_receiver = None;
            self.thumbnails.clear();
            self.info.clear();
        }
        let (threads, side) = (options.thread_count(), options.thumbnail_side());
        if self
            .loaders
            .as_ref()
            .is_none_or(|loaders| !loaders.matches(game, threads, side))
        {
            self.loaders = None;
            self.thumbnails.clear();
            let cache = ThumbnailCache::new(game, side);
            let loader = loader.clone();
            self.loaders = Some(Loaders::start(game, threads, side, &self.thumbnails, move |name| {
                load_thumbnail(&loader, cache.as_ref(), name, side)
            }));
        }
    }

    /// Loads the thumbnails and descriptions of `materials` again, after their files
    /// were saved.
    pub(super) fn forget(&mut self, materials: &[String]) {
        for name in materials {
            self.thumbnails.remove(name);
            if self.vmt.as_ref().is_some_and(|(shown, _)| shown == name) {
                self.vmt = None;
            }
            if let Some(loader) = &self.index_loader {
                if self.info.contains_key(name) {
                    self.info.insert(name.clone(), loader.describe(name));
                }
            }
        }
    }

    /// Takes in the listing, material descriptions and finished thumbnails.
    fn receive(&mut self, ctx: &egui::Context) {
        if self.catalog.receive(ctx) {
            self.start_index();
            self.restore_view();
        }
        if let Some(receiver) = &self.index_receiver {
            let mut finished = false;
            loop {
                match receiver.try_recv() {
                    Ok(batch) => self.info.extend(batch),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        finished = true;
                        break;
                    }
                }
            }
            if finished {
                self.index_receiver = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
        let Some(loaders) = &self.loaders else { return };
        let now = ctx.input(|input| input.time);
        for (name, image) in loaders.finished() {
            let thumbnail = match image {
                Some(image) => {
                    let size = [image.pixel_width as usize, image.pixel_height as usize];
                    let texture = upload(ctx, format!("thumbnail:{name}"), size, &image.rgba);
                    Thumbnail::Ready(texture, [image.width, image.height])
                }
                None => Thumbnail::Failed(None),
            };
            self.thumbnails.finish(name, thumbnail, now);
        }
        if self.thumbnails.busy(now) {
            ctx.request_repaint_after(Duration::from_millis(30));
        }
    }

    /// Describes every material in the background, in batches, for the filters.
    fn start_index(&mut self) {
        let Some(listing) = &self.catalog.listing else { return };
        let names: Vec<String> = listing.entries.iter().map(|(name, _)| name.clone()).collect();
        let (sender, receiver) = mpsc::channel();
        if let Some(loader) = self.index_loader.clone() {
            let queue = self.thumbnails.queue();
            thread::spawn(move || {
                for chunk in names.chunks(INDEX_BATCH) {
                    while !queue.idle() {
                        thread::sleep(Duration::from_millis(20));
                    }
                    let described = parallel_map(chunk, |name| loader.describe(name));
                    let batch: Vec<(String, MaterialInfo)> = chunk.iter().cloned().zip(described).collect();
                    if sender.send(batch).is_err() {
                        return;
                    }
                }
            });
            self.index_receiver = Some(receiver);
        }
    }

    /// The materials the map uses, recounted when the map changes.
    fn used_materials(&mut self, editor: &Editor) -> Arc<BTreeSet<String>> {
        let generation = editor.scene.geometry_generation;
        if let Some((cached, used)) = &self.used {
            if *cached == generation {
                return Arc::clone(used);
            }
        }
        let used: BTreeSet<String> = editor
            .scene
            .meshes
            .iter()
            .flat_map(|mesh| &mesh.faces)
            .filter_map(|face| face.material.as_deref())
            .map(|material| material.replace('\\', "/").to_ascii_lowercase())
            .collect();
        let used = Arc::new(used);
        self.used = Some((generation, Arc::clone(&used)));
        used
    }

    /// How many described materials have each trait, counted again as more are described.
    fn trait_counts(&mut self) -> TraitCounts {
        if self.trait_counts.0 != self.info.len() {
            let mut counts = TraitCounts::default();
            for info in self.info.values() {
                if info.translucent {
                    counts.translucent += 1;
                } else {
                    counts.opaque += 1;
                }
                counts.self_illum += usize::from(info.self_illum);
                counts.envmap += usize::from(info.envmap);
                counts.errors += usize::from(info.error);
            }
            self.trait_counts = (self.info.len(), counts);
        }
        self.trait_counts.1
    }

    /// The bar along the top: the search, the keywords and the card size, then the
    /// filters for what a material does, each with how many materials it covers.
    fn toolbar(&mut self, ui: &mut egui::Ui, used: &BTreeSet<String>) {
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            SearchBox {
                hint: "such as brick wall !dirt",
                help: "Every word must be in the material's name. Start a word with ! to leave out \
                       materials that have it. * stands for any text and ? for one character.",
                width: 200.0,
            }
            .show(
                ui,
                &mut self.view.filter,
                &mut self.view.filter_history,
                &mut self.focus_search,
            );
            ui.separator();
            ui.label("Keywords");
            ui.add(
                egui::TextEdit::singleline(&mut self.view.keywords)
                    .hint_text("from %keywords")
                    .desired_width(130.0),
            );
            ui.separator();
            per_row_slider(ui, &mut self.view.per_row);
            ui.separator();
            toggle_chip(
                ui,
                &mut self.view.group_by_folder,
                &format!("{} Folders", icon::FOLDERS),
                None,
            )
            .on_hover_text("Put the textures under a heading for each folder");
            toggle_chip(ui, &mut self.view.show_alpha, "Alpha", None)
                .on_hover_text("Draw a checkerboard behind textures so transparency shows");
            toggle_chip(
                ui,
                &mut self.view.show_details,
                &format!("{} Details", icon::SIDEBAR_SIMPLE),
                None,
            )
            .on_hover_text("Show the clicked material in a pane on the right");
            if ui
                .button(icon::ARROWS_CLOCKWISE)
                .on_hover_text("List the game's materials again")
                .clicked()
            {
                self.catalog.reset();
                self.loaders = None;
                self.index_receiver = None;
            }
        });
        let counts = self.trait_counts();
        ui.horizontal_wrapped(|ui| {
            toggle_chip(ui, &mut self.view.show_opaque, "Opaque", Some(counts.opaque));
            toggle_chip(
                ui,
                &mut self.view.show_translucent,
                "Translucent",
                Some(counts.translucent),
            );
            toggle_chip(ui, &mut self.view.show_self_illum, "Self-lit", Some(counts.self_illum))
                .on_hover_text("Materials with $selfillum");
            toggle_chip(ui, &mut self.view.show_envmap, "Reflective", Some(counts.envmap))
                .on_hover_text("Materials with an $envmap");
            toggle_chip(ui, &mut self.view.show_errors, "Broken", Some(counts.errors))
                .on_hover_text("Materials whose texture cannot be found");
            ui.separator();
            toggle_chip(ui, &mut self.view.used_only, "Only used", Some(used.len()))
                .on_hover_text("Show only the materials the map uses");
            toggle_chip(ui, &mut self.view.brush_only, "Brush materials", None)
                .on_hover_text("Hide model, particle, sprite and interface materials");
        });
        ui.add_space(2.0);
    }

    /// The sidebar: sources grouped by kind, then the folder tree, each with a count.
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let brush_only = self.view.brush_only;
        asset_browser::sidebar(
            ui,
            &self.catalog,
            "materials",
            &mut self.hidden_sources,
            FolderPick {
                folder: &mut self.view.folder,
                search: &mut self.folder_search,
                reveal: self.reveal_folder,
            },
            |folder| brush_only && NON_BRUSH_FOLDERS.contains(&folder),
        );
        self.reveal_folder = false;
    }

    /// Indices into the listing of the materials passing every filter, in name order. A
    /// material shows while any source that has it is ticked.
    fn filtered(&self, used: &BTreeSet<String>) -> Vec<usize> {
        let Some(listing) = &self.catalog.listing else {
            return Vec::new();
        };
        let search = Search::parse(&self.view.filter);
        let keywords: Vec<String> = self
            .view
            .keywords
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect();
        listing
            .entries
            .iter()
            .enumerate()
            .filter(|(_, (_, sources))| sources.iter().any(|source| !self.hidden_sources.contains(source)))
            .filter(|(_, (name, _))| self.view.folder.as_deref().is_none_or(|folder| in_folder(name, folder)))
            .filter(|(_, (name, _))| !self.view.brush_only || !non_brush(name))
            .filter(|(_, (name, _))| !self.view.used_only || used.contains(name.as_str()))
            .filter(|(_, (name, _))| search.matches(name))
            .filter(|(_, (name, _))| match self.info.get(name.as_str()) {
                None => keywords.is_empty(),
                Some(info) => {
                    let kind = if info.translucent {
                        self.view.show_translucent
                    } else {
                        self.view.show_opaque
                    };
                    let keywords_match = keywords
                        .iter()
                        .all(|word| info.keywords.to_ascii_lowercase().contains(word.as_str()));
                    kind && keywords_match
                        && (self.view.show_self_illum || !info.self_illum)
                        && (self.view.show_envmap || !info.envmap)
                        && (self.view.show_errors || !info.error)
                }
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// Makes `name` the selected material, as clicking its card does.
    fn select(&mut self, name: &str, commands: &mut Vec<Command>) {
        self.selected = Some(name.to_owned());
        commands.push(Command::Choose(name.to_owned(), false));
    }

    /// The grid with the folder trail and the Recent and Starred shelf above it. The
    /// arrow keys move the selection and Enter uses it.
    fn grid(
        &mut self,
        ui: &mut egui::Ui,
        shown: Shown,
        current: &str,
        scroll_step: f32,
        keys: KeyMove,
        commands: &mut Vec<Command>,
    ) {
        let Shown { entries: names, groups } = shown;
        path_bar(ui, "All materials", &mut self.view.folder);
        let shelf = Shelf {
            recent: &self.view.recent,
            favourites: &self.view.favourites,
            selected: self.selected.as_deref(),
            short: whole,
            upscale: false,
        };
        match shelf.show(
            ui,
            &mut self.thumbnails,
            self.loaders.is_some(),
            &mut self.shelf_starred,
        ) {
            Some(ShelfAction::Select(name)) => self.select(&name, commands),
            Some(ShelfAction::Use(name)) => commands.push(Command::Choose(name, true)),
            None => {}
        }
        let Some(listing) = &self.catalog.listing else {
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Listing the game's materials...");
            });
            return;
        };
        if names.is_empty() {
            let view = &mut self.view;
            empty_state(
                ui,
                "materials",
                Narrowing {
                    filter: &mut view.filter,
                    hidden: &mut self.hidden_sources,
                    folder: &mut view.folder,
                    used_only: &mut view.used_only,
                },
                |ui| {
                    if !view.keywords.is_empty() && ui.button("Clear the keywords").clicked() {
                        view.keywords.clear();
                    }
                    let kinds = [
                        view.show_opaque,
                        view.show_translucent,
                        view.show_self_illum,
                        view.show_envmap,
                        view.show_errors,
                    ];
                    if kinds.contains(&false) && ui.button("Every kind of material").clicked() {
                        view.show_opaque = true;
                        view.show_translucent = true;
                        view.show_self_illum = true;
                        view.show_envmap = true;
                        view.show_errors = true;
                    }
                    if view.brush_only && ui.button("Model and effect materials too").clicked() {
                        view.brush_only = false;
                    }
                },
            );
            return;
        }
        let mut reveal = None;
        if keys.moves() {
            let (columns, _) = grid_columns(ui.available_width(), self.view.per_row);
            let lines = grid_lines(names.len(), columns, groups);
            let from = self
                .selected
                .as_ref()
                .and_then(|selected| names.iter().position(|index| listing.entries[*index].0 == *selected));
            if let Some(target) = keys.target(from, &lines) {
                let name = listing.entries[names[target]].0.clone();
                self.select(&name, commands);
                reveal = Some(target);
            }
        }
        if keys.enter {
            commands.extend(self.selected.clone().map(|name| Command::Choose(name, true)));
        }
        let mut per_row = self.view.per_row;
        let grid = Grid {
            salt: "texture_grid",
            count: names.len(),
            groups,
            scroll_step,
            reveal,
        };
        let clicked = grid.show(ui, &mut per_row, |ui, shown, side| {
            let Some((name, sources)) = self
                .catalog
                .listing
                .as_ref()
                .and_then(|listing| listing.source_names(names[shown]))
            else {
                return;
            };
            let response = self.card(ui, &name, &sources, side, name == current);
            if response.clicked() {
                self.select(&name, commands);
            }
            if response.double_clicked() {
                commands.push(Command::Choose(name.clone(), true));
            }
            response.context_menu(|ui| self.card_menu(ui, &name, commands));
        });
        if let Some(group) = clicked.and_then(|group| groups?.get(group)) {
            self.view.folder = Some(group.folder.clone());
            self.reveal_folder = true;
        }
        self.view.per_row = per_row;
    }

    /// The menu a card opens when right-clicked.
    fn card_menu(&mut self, ui: &mut egui::Ui, name: &str, commands: &mut Vec<Command>) {
        let starred = self.view.favourites.iter().any(|starred| starred == name);
        if ui.button(if starred { "Unstar" } else { "Star" }).clicked() {
            toggle_favourite(&mut self.view.favourites, name);
            ui.close_menu();
        }
        if ui
            .button("Mark")
            .on_hover_text("Select every face using this texture")
            .clicked()
        {
            commands.push(Command::Mark(name.to_owned()));
            ui.close_menu();
        }
        if ui.button("Replace...").clicked() {
            commands.push(Command::Replace(name.to_owned()));
            ui.close_menu();
        }
        if ui.button("Copy name").clicked() {
            commands.push(Command::Copy(name.to_owned()));
            ui.close_menu();
        }
        if ui.button("Show its folder").clicked() {
            self.view.folder = Some(
                name.rsplit_once('/')
                    .map_or(asset_browser::ROOT_FOLDER, |(folder, _)| folder)
                    .to_owned(),
            );
            self.reveal_folder = true;
            ui.close_menu();
        }
    }

    /// One card: the thumbnail at its real size up to the card's, and the name under it.
    fn card(&mut self, ui: &mut egui::Ui, name: &str, sources: &[String], side: f32, current: bool) -> egui::Response {
        self.thumbnails.show(name, self.loaders.is_some());
        let marks = Marks {
            current,
            selected: self.selected.as_deref() == Some(name),
            favourite: self.view.favourites.iter().any(|starred| starred == name),
        };
        let (show_alpha, thumbnails) = (self.view.show_alpha, &self.thumbnails);
        let response = card(ui, side, name, marks, |ui, painter, rect| {
            if show_alpha {
                checkerboard(ui, painter, rect.shrink(3.0));
            }
            paint_thumbnail(ui, painter, rect, thumbnails, name, false);
        });
        let mut details = vec![name.to_owned()];
        if let Some([width, height]) = self.thumbnails.size(name) {
            details.push(format!("{width} x {height}"));
        }
        details.extend(source_lines(sources));
        if let Some(info) = self.info.get(name) {
            let traits = traits(info);
            if !traits.is_empty() {
                details.push(traits.join(", "));
            }
            if !info.keywords.is_empty() {
                details.push(format!("keywords: {}", info.keywords));
            }
        }
        response.on_hover_text(details.join("\n"))
    }

    /// The details pane: the material clicked last, large, with what it is, where it
    /// comes from, what can be done with it, and its VMT.
    fn details(
        &mut self,
        ui: &mut egui::Ui,
        loader: &MaterialLoader,
        used: &BTreeSet<String>,
        commands: &mut Vec<Command>,
    ) {
        let Some(name) = self.selected.clone() else {
            ui.add_space(20.0);
            ui.label(RichText::new("Click a texture to look at it here.").color(muted(ui)));
            ui.label(
                RichText::new("The arrow keys move between textures, and Enter uses the one picked.")
                    .small()
                    .color(muted(ui)),
            );
            return;
        };
        if self.vmt.as_ref().is_none_or(|(shown, _)| *shown != name) {
            self.vmt = Some((name.clone(), loader.source_text(&name)));
        }
        self.thumbnails.show(&name, self.loaders.is_some());
        let side = ui.available_width();
        let height = side.min((ui.available_height() * 0.45).max(140.0));
        let (rect, response) = ui.allocate_exact_size(egui::vec2(side, height), egui::Sense::click());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4, ui.visuals().extreme_bg_color);
        if self.view.show_alpha {
            checkerboard(ui, &painter, rect.shrink(2.0));
        }
        paint_thumbnail(ui, &painter, rect, &self.thumbnails, &name, true);
        if response.on_hover_text("Double-click to use it").double_clicked() {
            commands.push(Command::Choose(name.clone(), true));
        }

        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(&name).monospace().strong()).wrap());
        let starred = self.view.favourites.contains(&name);
        ui.horizontal_wrapped(|ui| {
            if ui
                .button(format!("{} Use", icon::CHECK))
                .on_hover_text("Double-clicking a texture does the same")
                .clicked()
            {
                commands.push(Command::Choose(name.clone(), true));
            }
            let star = if starred { icon::STAR_HALF } else { icon::STAR };
            if chip(ui, starred, star, None)
                .on_hover_text(if starred {
                    "Unstar"
                } else {
                    "Star, to keep it on the shelf"
                })
                .clicked()
            {
                toggle_favourite(&mut self.view.favourites, &name);
            }
            if ui
                .button(icon::COPY)
                .on_hover_text("Copy the material's name")
                .clicked()
            {
                commands.push(Command::Copy(name.clone()));
            }
            if ui
                .button("Mark")
                .on_hover_text("Select every face using this texture")
                .clicked()
            {
                commands.push(Command::Mark(name.clone()));
            }
            if ui.button("Replace...").clicked() {
                commands.push(Command::Replace(name.clone()));
            }
            if ui
                .button(icon::FILE_TEXT)
                .on_hover_text("Open the material's VMT")
                .clicked()
            {
                open_source(loader, &name);
            }
        });
        let info = self.info.get(&name);
        if info.is_some_and(|info| info.error) {
            ui.label(RichText::new(format!("{} Its texture cannot be found", icon::WARNING)).color(warning(ui)));
        }
        ui.separator();
        let vmt = self.vmt.as_ref().and_then(|(_, text)| text.clone());
        let sources = self
            .catalog
            .listing
            .as_ref()
            .and_then(|listing| {
                let index = listing.entries.iter().position(|(entry, _)| *entry == name)?;
                listing.source_names(index)
            })
            .map(|(_, sources)| sources)
            .unwrap_or_default();
        egui::ScrollArea::vertical()
            .id_salt("texture_details")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let spacing = egui::vec2(12.0, 3.0);
                let widest_column = (ui.available_width() - spacing.x) / 2.0;
                egui::Grid::new("texture_details_grid")
                    .num_columns(2)
                    .spacing(spacing)
                    .max_col_width(widest_column)
                    .show(ui, |ui| {
                        let mut row = |label: &str, value: String| {
                            ui.label(RichText::new(label).color(muted(ui)));
                            ui.label(value);
                            ui.end_row();
                        };
                        if let Some([width, height]) = self.thumbnails.size(&name) {
                            row("Size", format!("{width} x {height}"));
                        }
                        if let Some(shader) = vmt.as_deref().and_then(shader_of) {
                            row("Shader", shader);
                        }
                        row("In the map", if used.contains(&name) { "yes" } else { "no" }.to_owned());
                        if let Some(info) = info {
                            let traits = traits(info);
                            if !traits.is_empty() {
                                row("Traits", traits.join(", "));
                            }
                            if !info.keywords.is_empty() {
                                row("Keywords", info.keywords.clone());
                            }
                        }
                        if let Some((first, others)) = sources.split_first() {
                            row("Loads from", first.clone());
                            if !others.is_empty() {
                                row("Also in", others.join(", "));
                            }
                        }
                    });
                ui.add_space(6.0);
                if let Some(vmt) = vmt {
                    egui::CollapsingHeader::new("VMT")
                        .id_salt("texture_vmt")
                        .show(ui, |ui| {
                            ui.add(egui::Label::new(RichText::new(vmt).monospace().small()).wrap());
                        });
                }
            });
    }

    /// The line along the bottom: how many materials show, and how far describing them
    /// for the filters has got.
    fn status(&mut self, ui: &mut egui::Ui, names: &[usize]) {
        let total = self.catalog.listing.as_ref().map(|listing| listing.entries.len());
        let progress = total.filter(|_| self.index_receiver.is_some()).map(|total| {
            let done = self.info.len().min(total);
            (
                done as f32 / total.max(1) as f32,
                format!("Reading {} for the filters", thousands(total - done)),
            )
        });
        let selected = self.selected.clone();
        let size = selected.as_ref().and_then(|name| self.thumbnails.size(name));
        let show_details = self.view.show_details;
        status_line(ui, "materials", names.len(), total, progress, |ui| {
            if let Some(name) = selected.filter(|_| !show_details) {
                ui.separator();
                ui.label(RichText::new(name).monospace().small());
                if let Some([width, height]) = size {
                    ui.label(RichText::new(format!("{width}x{height}")).small().color(muted(ui)));
                }
            }
        });
    }

    /// Hides the sources the settings hide and drops a saved folder the game lacks, once
    /// a listing arrives.
    fn restore_view(&mut self) {
        let Some(listing) = &self.catalog.listing else { return };
        self.hidden_sources = restore_hidden(&listing.sources, &self.view.hidden_sources);
        if self
            .view
            .folder
            .as_ref()
            .is_some_and(|folder| !listing.folders.contains(folder))
        {
            self.view.folder = None;
        }
        self.reveal_folder = true;
    }

    /// Writes the filters and layout to the settings when they have changed, waiting
    /// while a text box is being typed in so every key press does not save.
    fn save_view(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if let Some(listing) = &self.catalog.listing {
            self.view.hidden_sources = hidden_paths(&listing.sources, &self.hidden_sources);
        }
        if self.view == editor.config.texture_browser_view || ctx.wants_keyboard_input() {
            return;
        }
        editor.config.texture_browser_view = self.view.clone();
        editor.save_config();
    }
}

/// A material's traits in words, as the card's tooltip and the details pane list them.
fn traits(info: &MaterialInfo) -> Vec<&'static str> {
    [
        (info.translucent, "translucent"),
        (info.self_illum, "self-illuminated"),
        (info.envmap, "reflective"),
        (info.water, "water"),
        (info.error, "texture missing"),
    ]
    .into_iter()
    .filter_map(|(has, word)| has.then_some(word))
    .collect()
}

/// Loads a thumbnail from the disk cache when it is still current, otherwise from the
/// game's files, keeping what was loaded in the cache for the next time.
fn load_thumbnail(loader: &MaterialLoader, cache: Option<&ThumbnailCache>, name: &str, side: u32) -> Option<Image> {
    if let Some(image) = cache.and_then(|cache| cache.load(loader, name)) {
        return Some(image);
    }
    let image = loader.thumbnail(name, side)?;
    if let Some(cache) = cache {
        cache.store(loader, name, &image);
    }
    Some(image)
}

/// Opens a material's VMT in the program Windows uses for it. A VMT inside a VPK is
/// copied to a temporary file first.
fn open_source(loader: &MaterialLoader, material: &str) {
    let path = loader.locate(material).or_else(|| {
        let text = loader.source_text(material)?;
        let path = std::env::temp_dir()
            .join("mallet")
            .join("materials")
            .join(format!("{material}.vmt"));
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::write(&path, text).ok()?;
        Some(path)
    });
    if let Some(path) = path {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brush_materials_leave_out_whole_folders_only() {
        assert!(non_brush("models/props_c17/oildrum001"));
        assert!(non_brush("effects/water_warp"));
        assert!(!non_brush("modelsx/brick"));
        assert!(!non_brush("brick/brickwall001a"));
    }

    #[test]
    fn the_shader_is_the_first_word_of_the_vmt() {
        assert_eq!(
            shader_of("// a comment\n\"LightmappedGeneric\"\n{\n}").as_deref(),
            Some("LightmappedGeneric")
        );
        assert_eq!(shader_of("VertexLitGeneric{ }").as_deref(), Some("VertexLitGeneric"));
        assert_eq!(shader_of("  \n"), None);
    }
}
