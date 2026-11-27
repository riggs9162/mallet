//! The texture browser: every material the game mounts, as thumbnails that load in the
//! background while you scroll. Like Hammer's texture list, with a sidebar to narrow it
//! to one game, addon or folder, and filters for what a material does. The parts it
//! shares with the model browser live in [`super::asset_browser`].

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
    self, card, card_grid, checkerboard, hidden_paths, in_folder, paint_thumbnail, restore_hidden, source_lines,
    thousands, upload, Catalog, Listing, Loaders, Search, SearchBox, Thumbnail, Thumbnails, MUTED,
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
    /// The material clicked last, shown in the bar at the bottom.
    selected: Option<String>,
    /// Whether the sidebar should open the way down to the chosen folder this frame.
    reveal_folder: bool,
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
            reveal_folder: false,
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
}

impl MalletApp {
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

        let title = match state.pick {
            Pick::Current => format!("{} Textures", icon::IMAGES),
            Pick::Find => format!("{} Textures: pick the texture to find", icon::IMAGES),
            Pick::Replace => format!("{} Textures: pick the replacement", icon::IMAGES),
        };
        let mut open = true;
        let mut commands = Vec::new();
        egui::Window::new(title)
            .id(egui::Id::new("texture_browser"))
            .open(&mut open)
            .default_size([1080.0, 720.0])
            .max_height(ctx.screen_rect().height() - 60.0)
            .min_width(640.0)
            .min_height(360.0)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                // Panels fill exactly the window, so it never grows to fit its content.
                egui::TopBottomPanel::bottom("texture_browser_bar")
                    .show_inside(ui, |ui| state.bottom_bar(ui, &loader, &mut commands));
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
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    let size = [ui.available_width(), ui.available_height()];
                    state.grid(ui, &used, &current, size, scroll_step, &mut commands);
                });
            });
        state.thumbnails.trim(options.cache_size());
        state.save_view(ctx, editor);
        for command in commands {
            match command {
                Command::Choose(material, confirm) => {
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
        let finished = loaders.finished();
        let received = !finished.is_empty();
        for (name, image) in finished {
            let thumbnail = match image {
                Some(image) => {
                    let size = [image.pixel_width as usize, image.pixel_height as usize];
                    let texture = upload(ctx, format!("thumbnail:{name}"), size, &image.rgba);
                    Thumbnail::Ready(texture, [image.width, image.height])
                }
                None => Thumbnail::Failed(None),
            };
            self.thumbnails.finish(name, thumbnail);
        }
        if received || self.thumbnails.waiting() {
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

    /// The sidebar: sources grouped by kind, then the folder tree, each with a count.
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let brush_only = self.view.brush_only;
        asset_browser::sidebar(
            ui,
            &self.catalog,
            "materials",
            &mut self.hidden_sources,
            &mut self.view.folder,
            self.reveal_folder,
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

    /// The thumbnail grid.
    fn grid(
        &mut self,
        ui: &mut egui::Ui,
        used: &BTreeSet<String>,
        current: &str,
        size: [f32; 2],
        scroll_step: f32,
        commands: &mut Vec<Command>,
    ) {
        if self.catalog.listing.is_none() {
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Listing the game's materials...");
            });
            return;
        }
        let names = self.filtered(used);
        if names.is_empty() {
            ui.add_space(20.0);
            ui.label(
                RichText::new("No materials match. Clear the search, choose All folders or turn on more sources.")
                    .color(MUTED),
            );
            return;
        }
        card_grid(
            ui,
            "texture_grid",
            names.len(),
            self.view.per_row,
            size,
            scroll_step,
            |ui, shown, side| {
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
                    self.selected = Some(name.clone());
                    commands.push(Command::Choose(name.clone(), false));
                }
                if response.double_clicked() {
                    commands.push(Command::Choose(name.clone(), true));
                }
                response.context_menu(|ui| {
                    if ui
                        .button("Mark")
                        .on_hover_text("Select every face using this texture")
                        .clicked()
                    {
                        commands.push(Command::Mark(name.clone()));
                        ui.close_menu();
                    }
                    if ui.button("Replace...").clicked() {
                        commands.push(Command::Replace(name.clone()));
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
                });
            },
        );
    }

    /// One card: the thumbnail at its real size up to the card's, and the name under it.
    fn card(&mut self, ui: &mut egui::Ui, name: &str, sources: &[String], side: f32, current: bool) -> egui::Response {
        self.thumbnails.show(name, self.loaders.is_some());
        let selected = self.selected.as_deref() == Some(name);
        let (show_alpha, thumbnail) = (self.view.show_alpha, self.thumbnails.get(name));
        let response = card(ui, side, name, current, selected, |painter, rect| {
            if show_alpha {
                checkerboard(painter, rect.shrink(3.0));
            }
            paint_thumbnail(painter, rect, thumbnail, false);
        });
        let mut details = vec![name.to_owned()];
        if let Some([width, height]) = self.thumbnails.size(name) {
            details.push(format!("{width} x {height}"));
        }
        details.extend(source_lines(sources));
        if let Some(info) = self.info.get(name) {
            let mut traits = Vec::new();
            if info.translucent {
                traits.push("translucent");
            }
            if info.self_illum {
                traits.push("self-illuminated");
            }
            if info.envmap {
                traits.push("reflective");
            }
            if info.water {
                traits.push("water");
            }
            if info.error {
                traits.push("texture missing");
            }
            if !traits.is_empty() {
                details.push(traits.join(", "));
            }
            if !info.keywords.is_empty() {
                details.push(format!("keywords: {}", info.keywords));
            }
        }
        response.on_hover_text(details.join("\n"))
    }

    /// Hammer's controls under the list: size, filter and keywords, used only, the
    /// selected texture with Mark, Replace, Open Source and Reload, and the kind filters.
    fn bottom_bar(&mut self, ui: &mut egui::Ui, loader: &MaterialLoader, commands: &mut Vec<Command>) {
        let selected = self.selected.clone();
        egui::Grid::new("browser_controls")
            .num_columns(4)
            .spacing([14.0, 6.0])
            .show(ui, |ui| {
                ui.add(egui::Slider::new(&mut self.view.per_row, 2..=14).text("per row"))
                    .on_hover_text("How many textures fit across; they grow and shrink with the window");
                ui.horizontal(|ui| {
                    SearchBox {
                        hint: "such as brick wall !dirt",
                        help: "Every word must be in the material's name. Start a word with ! to leave out \
                               materials that have it. * stands for any text and ? for one character.",
                        width: 170.0,
                    }
                    .show(
                        ui,
                        &mut self.view.filter,
                        &mut self.view.filter_history,
                        &mut self.focus_search,
                    );
                });
                ui.horizontal(|ui| {
                    let name = selected.as_deref().unwrap_or("Click a texture");
                    ui.label(RichText::new(name).monospace());
                    if let Some([width, height]) = selected.as_ref().and_then(|name| self.thumbnails.size(name)) {
                        ui.label(RichText::new(format!("{width}x{height}")).color(MUTED));
                    }
                });
                ui.horizontal(|ui| {
                    ui.toggle_value(&mut self.view.show_opaque, "Opaque");
                    ui.toggle_value(&mut self.view.show_self_illum, "Selfillum");
                    ui.toggle_value(&mut self.view.show_errors, "Show errors")
                        .on_hover_text("Materials whose texture cannot be found");
                });
                ui.end_row();

                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.view.used_only, "Only used textures");
                    ui.checkbox(&mut self.view.brush_only, "Brush materials")
                        .on_hover_text("Hide model, particle, sprite and interface materials");
                });
                ui.horizontal(|ui| {
                    ui.label("Keywords");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.view.keywords)
                            .hint_text("from %keywords")
                            .desired_width(150.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(selected.is_some(), |ui| {
                        if ui
                            .button("Mark")
                            .on_hover_text("Select every face using this texture")
                            .clicked()
                        {
                            commands.extend(selected.clone().map(Command::Mark));
                        }
                        if ui.button("Replace...").clicked() {
                            commands.extend(selected.clone().map(Command::Replace));
                        }
                        if ui
                            .button("Open Source")
                            .on_hover_text("Open the material's VMT")
                            .clicked()
                        {
                            if let Some(name) = &selected {
                                open_source(loader, name);
                            }
                        }
                    });
                    if ui.button(format!("{} Reload", icon::ARROWS_CLOCKWISE)).clicked() {
                        self.catalog.reset();
                        self.loaders = None;
                        self.index_receiver = None;
                    }
                });
                ui.horizontal(|ui| {
                    ui.toggle_value(&mut self.view.show_translucent, "Translucent");
                    ui.toggle_value(&mut self.view.show_envmap, "EnvMask")
                        .on_hover_text("Reflective materials, with an $envmap");
                    ui.toggle_value(&mut self.view.show_alpha, "Show alpha")
                        .on_hover_text("Draw a checkerboard behind textures so transparency shows");
                });
                ui.end_row();
            });
        let status = match (&self.catalog.listing, &self.index_receiver) {
            (Some(listing), Some(_)) => format!(
                "{} materials; reading {} of them for the filters",
                thousands(listing.entries.len()),
                thousands(listing.entries.len() - self.info.len().min(listing.entries.len()))
            ),
            (Some(listing), None) => format!("{} materials", thousands(listing.entries.len())),
            (None, _) => String::new(),
        };
        ui.label(RichText::new(status).small().color(MUTED));
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
}
