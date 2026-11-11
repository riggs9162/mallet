//! The texture browser: every material the game mounts, as thumbnails that load in the
//! background while you scroll. Like Hammer's texture list, with a sidebar to narrow it
//! to one game, addon or folder, and filters for what a material does.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icon;
use mallet_content::vtf::Image;
use mallet_content::{parallel_map, MaterialInfo, MaterialLoader, Source, SourceKind};

use super::MalletApp;
use crate::config::{TextureBrowserOptions, TextureBrowserView};
use crate::editor::Editor;

const MUTED: Color32 = Color32::from_gray(140);
const ACCENT: Color32 = Color32::from_rgb(70, 140, 230);
/// Requests waiting to load; older ones are dropped, as they have scrolled out of view.
const MAX_REQUESTS: usize = 256;
/// Materials described per batch while indexing.
const INDEX_BATCH: usize = 1500;
const SIDEBAR_WIDTH: f32 = 210.0;
const MAX_FILTER_HISTORY: usize = 12;
/// Folders whose materials are not meant for brushes, hidden while "Brush materials" is on.
const NON_BRUSH_FOLDERS: [&str; 9] = [
    "models/",
    "particle/",
    "particles/",
    "vgui/",
    "hud/",
    "console/",
    "sprites/",
    "editor/",
    "effects/",
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

enum Thumbnail {
    Loading,
    Ready(egui::TextureHandle, [u32; 2]),
    Missing,
}

/// The background thumbnail loaders for one game, started with these thread and size
/// settings.
struct Loaders {
    game: PathBuf,
    threads: usize,
    side: u32,
    requests: Arc<Mutex<Vec<String>>>,
    results: Receiver<(String, Option<Image>)>,
    stop: Arc<AtomicBool>,
}

impl Drop for Loaders {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The game's materials with where each comes from, and counts for the sidebar.
struct Listing {
    game: PathBuf,
    /// Each material with every source that has it; the first is where it loads from.
    materials: Vec<(String, Vec<usize>)>,
    sources: Vec<Source>,
    per_source: HashMap<usize, usize>,
    per_folder: BTreeMap<String, usize>,
}

/// What the texture browser remembers between frames.
pub(super) struct TextureBrowserState {
    pub(super) pick: Pick,
    /// The filters and layout, saved to the settings when they change.
    view: TextureBrowserView,
    /// Whether `view` has been read from the settings yet.
    view_loaded: bool,
    listing: Option<Listing>,
    list_receiver: Option<(PathBuf, Receiver<Listing>)>,
    hidden_sources: HashSet<usize>,
    loaders: Option<Loaders>,
    thumbnails: HashMap<String, Thumbnail>,
    info: HashMap<String, MaterialInfo>,
    index_receiver: Option<Receiver<Vec<(String, MaterialInfo)>>>,
    /// The game's loader, for describing materials once they are listed.
    index_loader: Option<MaterialLoader>,
    /// The material clicked last, shown in the bar at the bottom.
    selected: Option<String>,
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
            listing: None,
            list_receiver: None,
            hidden_sources: HashSet::new(),
            loaders: None,
            thumbnails: HashMap::new(),
            info: HashMap::new(),
            index_receiver: None,
            index_loader: None,
            selected: None,
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
        state.trim(options.cache_size());
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

impl TextureBrowserState {
    /// Lists the game's materials, starts describing them for the filters, and starts the
    /// thumbnail loaders, once per game and again when their settings change.
    fn start(&mut self, game: &PathBuf, loader: &MaterialLoader, options: &TextureBrowserOptions) {
        self.index_loader = Some(loader.clone());
        let listed = self.listing.as_ref().is_some_and(|listing| &listing.game == game);
        let listing = self.list_receiver.as_ref().is_some_and(|(listing, _)| listing == game);
        if !listed && !listing {
            let (sender, receiver) = mpsc::channel();
            let (loader, listing_game) = (loader.clone(), game.clone());
            thread::spawn(move || {
                let materials = loader.list_sourced();
                let mut per_source: HashMap<usize, usize> = HashMap::new();
                let mut per_folder: BTreeMap<String, usize> = BTreeMap::new();
                for (name, sources) in &materials {
                    for source in sources {
                        *per_source.entry(*source).or_default() += 1;
                    }
                    let folder = name.split('/').next().unwrap_or_default().to_owned();
                    *per_folder.entry(folder).or_default() += 1;
                }
                let _ = sender.send(Listing {
                    game: listing_game,
                    sources: loader.sources(),
                    materials,
                    per_source,
                    per_folder,
                });
            });
            self.list_receiver = Some((game.clone(), receiver));
            self.thumbnails.clear();
            self.info.clear();
        }
        let (threads, side) = (options.thread_count(), options.thumbnail_side());
        let stale = |loaders: &Loaders| &loaders.game != game || loaders.threads != threads || loaders.side != side;
        if self.loaders.as_ref().is_none_or(stale) {
            let requests: Arc<Mutex<Vec<String>>> = Arc::default();
            let stop = Arc::new(AtomicBool::new(false));
            let (sender, results) = mpsc::channel();
            for _ in 0..threads {
                let (requests, stop, sender, loader) =
                    (Arc::clone(&requests), Arc::clone(&stop), sender.clone(), loader.clone());
                thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        // Newest first: what was asked for last is what is on screen now.
                        let next = requests.lock().ok().and_then(|mut queue| queue.pop());
                        match next {
                            Some(name) => {
                                let image = loader.thumbnail(&name, side);
                                if sender.send((name, image)).is_err() {
                                    return;
                                }
                            }
                            None => thread::sleep(Duration::from_millis(15)),
                        }
                    }
                });
            }
            self.loaders = Some(Loaders {
                game: game.clone(),
                threads,
                side,
                requests,
                results,
                stop,
            });
            self.thumbnails.clear();
        }
    }

    /// Takes in the listing, material descriptions and finished thumbnails.
    fn receive(&mut self, ctx: &egui::Context) {
        if let Some((_, receiver)) = &self.list_receiver {
            match receiver.try_recv() {
                Ok(listing) => {
                    self.start_index(&listing);
                    self.restore_view(&listing);
                    self.listing = Some(listing);
                    self.list_receiver = None;
                }
                Err(_) => ctx.request_repaint_after(Duration::from_millis(50)),
            }
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
        let mut received = false;
        while let Ok((name, image)) = loaders.results.try_recv() {
            received = true;
            let thumbnail = match image {
                Some(image) => {
                    let pixels = egui::ColorImage::from_rgba_unmultiplied(
                        [image.pixel_width as usize, image.pixel_height as usize],
                        &image.rgba,
                    );
                    let options = egui::TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear));
                    let texture = ctx.load_texture(format!("thumbnail:{name}"), pixels, options);
                    Thumbnail::Ready(texture, [image.width, image.height])
                }
                None => Thumbnail::Missing,
            };
            self.thumbnails.insert(name, thumbnail);
        }
        let waiting = self
            .thumbnails
            .values()
            .any(|thumbnail| matches!(thumbnail, Thumbnail::Loading));
        if received || waiting {
            ctx.request_repaint_after(Duration::from_millis(30));
        }
    }

    /// Describes every material in the background, in batches, for the filters.
    fn start_index(&mut self, listing: &Listing) {
        let names: Vec<String> = listing.materials.iter().map(|(name, _)| name.clone()).collect();
        let (sender, receiver) = mpsc::channel();
        if let Some(loader) = self.index_loader.clone() {
            thread::spawn(move || {
                for chunk in names.chunks(INDEX_BATCH) {
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

    /// The sidebar: sources grouped by kind, then top-level folders, each with a count.
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let Some(listing) = &self.listing else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Listing materials...");
            });
            return;
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new("Sources").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("None").clicked() {
                    self.hidden_sources = listing.per_source.keys().copied().collect();
                }
                if ui.small_button("All").clicked() {
                    self.hidden_sources.clear();
                }
            });
        });
        let groups = [
            (SourceKind::Game, "Game"),
            (SourceKind::MountedGame, "Mounted games"),
            (SourceKind::Addon, "Addons"),
            (SourceKind::Download, "Downloads"),
        ];
        for (kind, label) in groups {
            let members: Vec<(usize, &Source, usize)> = listing
                .sources
                .iter()
                .enumerate()
                .filter(|(_, source)| source.kind == kind)
                .filter_map(|(index, source)| Some((index, source, *listing.per_source.get(&index)?)))
                .collect();
            if members.is_empty() {
                continue;
            }
            let total: usize = members.iter().map(|(_, _, count)| count).sum();
            let shown = members
                .iter()
                .filter(|(index, _, _)| !self.hidden_sources.contains(index))
                .count();
            egui::CollapsingHeader::new(format!("{label}  {}", thousands(total)))
                .id_salt(("source_group", label))
                .default_open(kind != SourceKind::Addon)
                .show(ui, |ui| {
                    let mut all = shown == members.len();
                    if ui.checkbox(&mut all, RichText::new("All of these").small()).changed() {
                        for (index, _, _) in &members {
                            if all {
                                self.hidden_sources.remove(index);
                            } else {
                                self.hidden_sources.insert(*index);
                            }
                        }
                    }
                    for (index, source, count) in &members {
                        let mut on = !self.hidden_sources.contains(index);
                        ui.horizontal(|ui| {
                            if ui
                                .checkbox(&mut on, &source.name)
                                .on_hover_text(source.path.display().to_string())
                                .changed()
                            {
                                if on {
                                    self.hidden_sources.remove(index);
                                } else {
                                    self.hidden_sources.insert(*index);
                                }
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new(thousands(*count)).small().color(MUTED));
                            });
                        });
                    }
                });
        }
        ui.add_space(8.0);
        ui.label(RichText::new("Folders").strong());
        if ui.selectable_label(self.view.folder.is_none(), "All folders").clicked() {
            self.view.folder = None;
        }
        for (folder, count) in &listing.per_folder {
            let hidden = self.view.brush_only
                && NON_BRUSH_FOLDERS
                    .iter()
                    .any(|skip| skip.trim_end_matches('/') == folder);
            if hidden {
                continue;
            }
            ui.horizontal(|ui| {
                let selected = self.view.folder.as_deref() == Some(folder.as_str());
                if ui.selectable_label(selected, folder).clicked() {
                    self.view.folder = if selected { None } else { Some(folder.clone()) };
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(thousands(*count)).small().color(MUTED));
                });
            });
        }
    }

    /// The materials passing every filter, in name order.
    /// Indices into the listing of the materials passing every filter. A material shows
    /// while any source that has it is ticked.
    fn filtered(&self, used: &BTreeSet<String>) -> Vec<usize> {
        let Some(listing) = &self.listing else {
            return Vec::new();
        };
        let terms: Vec<String> = self
            .view
            .filter
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect();
        let keywords: Vec<String> = self
            .view
            .keywords
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect();
        let folder = self.view.folder.as_ref().map(|folder| format!("{folder}/"));
        listing
            .materials
            .iter()
            .enumerate()
            .map(|(index, (name, sources))| (index, name, sources))
            .filter(|(_, _, sources)| sources.iter().any(|source| !self.hidden_sources.contains(source)))
            .map(|(index, name, _)| (index, name))
            .filter(|(_, name)| folder.as_ref().is_none_or(|folder| name.starts_with(folder.as_str())))
            .filter(|(_, name)| !self.view.brush_only || !NON_BRUSH_FOLDERS.iter().any(|skip| name.starts_with(skip)))
            .filter(|(_, name)| !self.view.used_only || used.contains(name.as_str()))
            .filter(|(_, name)| terms.iter().all(|term| name.contains(term.as_str())))
            .filter(|(_, name)| match self.info.get(name.as_str()) {
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

    /// The thumbnail grid. Cards share the width evenly, `per_row` to a row.
    fn grid(
        &mut self,
        ui: &mut egui::Ui,
        used: &BTreeSet<String>,
        current: &str,
        [width, height]: [f32; 2],
        scroll_step: f32,
        commands: &mut Vec<Command>,
    ) {
        if self.listing.is_none() {
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
            ui.label(RichText::new("No materials match. Clear the search or turn on more sources.").color(MUTED));
            return;
        }
        let gap = 6.0;
        let columns = self.view.per_row.max(1);
        let card = ((width - 16.0 - gap * (columns as f32 - 1.0)) / columns as f32).max(32.0);
        let row_height = card + 22.0 + gap;
        let rows = names.len().div_ceil(columns);
        // The wheel scrolls by whole cards, as set in Mallet Options, instead of egui's
        // fixed line height. Its input is taken here so the scroll area does not also
        // scroll.
        let area = ui.available_rect_before_wrap();
        let mut wheel = 0.0;
        if ui.rect_contains_pointer(area) {
            wheel = ui.input(|input| {
                input
                    .events
                    .iter()
                    .map(|event| match event {
                        egui::Event::MouseWheel { unit, delta, modifiers } if !modifiers.ctrl => match unit {
                            egui::MouseWheelUnit::Line => delta.y,
                            egui::MouseWheelUnit::Point => delta.y / 50.0,
                            egui::MouseWheelUnit::Page => delta.y * (height / row_height).max(1.0),
                        },
                        _ => 0.0,
                    })
                    .sum::<f32>()
            });
            if wheel != 0.0 {
                ui.ctx().input_mut(|input| {
                    input.smooth_scroll_delta = egui::Vec2::ZERO;
                    input.raw_scroll_delta = egui::Vec2::ZERO;
                });
            }
        }
        // A solid bar that is always shown, with a handle long enough to grab. egui's
        // default floating bar is hidden until hovered, and with tens of thousands of
        // materials its handle shrinks to a dot that is hard to find and catch. It fits in
        // the 16 points the cards leave free on the right.
        ui.spacing_mut().scroll = egui::style::ScrollStyle {
            bar_width: 10.0,
            handle_min_length: 48.0,
            bar_inner_margin: 4.0,
            dormant_background_opacity: 0.5,
            active_background_opacity: 0.7,
            interact_background_opacity: 0.9,
            dormant_handle_opacity: 0.8,
            active_handle_opacity: 1.0,
            interact_handle_opacity: 1.0,
            ..egui::style::ScrollStyle::solid()
        };
        egui::ScrollArea::vertical()
            .id_salt("texture_grid")
            .max_height(height)
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show_rows(ui, row_height, rows, |ui, range| {
                if wheel != 0.0 {
                    ui.scroll_with_delta(egui::vec2(0.0, wheel * row_height * scroll_step));
                }
                for row in range {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for index in names.iter().skip(row * columns).take(columns) {
                            let Some((name, sources)) = self.listing.as_ref().and_then(|listing| {
                                let (name, sources) = listing.materials.get(*index)?;
                                let names: Vec<String> = sources
                                    .iter()
                                    .filter_map(|source| listing.sources.get(*source))
                                    .map(|source| source.name.clone())
                                    .collect();
                                Some((name.clone(), names))
                            }) else {
                                continue;
                            };
                            let response = self.card(ui, &name, &sources, card, name == current);
                            let name = &name;
                            if response.clicked() {
                                self.selected = Some(name.clone());
                                commands.push(Command::Choose(name.clone(), false));
                            }
                            if response.double_clicked() {
                                commands.push(Command::Choose(name.clone(), true));
                            }
                        }
                    });
                    ui.add_space(gap);
                }
            });
    }

    /// One card: the thumbnail at its real size up to the card's, and the name under it.
    fn card(&mut self, ui: &mut egui::Ui, name: &str, sources: &[String], size: f32, current: bool) -> egui::Response {
        self.request(name);
        let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size + 22.0), egui::Sense::click());
        let image_rect = egui::Rect::from_min_size(rect.min, egui::Vec2::splat(size));
        let painter = ui.painter();
        painter.rect_filled(rect, 4, Color32::from_gray(if response.hovered() { 44 } else { 30 }));
        if self.view.show_alpha {
            checkerboard(painter, image_rect.shrink(3.0));
        }
        match self.thumbnails.get(name) {
            Some(Thumbnail::Ready(texture, [width, height])) => {
                // Small textures stay small, as in Hammer; large ones shrink to fit.
                let fit = (size - 6.0) / (*width).max(*height).max(1) as f32;
                let scale = fit.min(1.0);
                let target = egui::Rect::from_center_size(
                    image_rect.center(),
                    egui::vec2(*width as f32 * scale, *height as f32 * scale),
                );
                let full = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
                painter.image(texture.id(), target, full, Color32::WHITE);
            }
            Some(Thumbnail::Missing) => {
                painter.text(
                    image_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    icon::IMAGE_BROKEN,
                    egui::FontId::proportional(22.0),
                    MUTED,
                );
            }
            _ => {}
        }
        let label = egui::Rect::from_min_max(egui::pos2(rect.min.x, image_rect.max.y), rect.max);
        painter.rect_filled(
            label,
            egui::CornerRadius {
                nw: 0,
                ne: 0,
                sw: 4,
                se: 4,
            },
            if current { ACCENT } else { Color32::from_gray(22) },
        );
        let short = name.rsplit('/').next().unwrap_or(name);
        painter.text(
            label.center(),
            egui::Align2::CENTER_CENTER,
            elide(short, ((size - 8.0) / 6.0) as usize),
            egui::FontId::proportional(11.0),
            Color32::from_gray(if current { 255 } else { 210 }),
        );
        if current {
            painter.rect_stroke(rect, 4, egui::Stroke::new(2.0_f32, ACCENT), egui::StrokeKind::Inside);
        }
        let mut details = vec![name.to_owned()];
        if let Some(Thumbnail::Ready(_, [width, height])) = self.thumbnails.get(name) {
            details.push(format!("{width} x {height}"));
        }
        if let Some((first, others)) = sources.split_first() {
            details.push(format!("loads from {first}"));
            if !others.is_empty() {
                details.push(format!("also in {}", others.join(", ")));
            }
        }
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
                    ui.label("Filter");
                    let search = ui.add(
                        egui::TextEdit::singleline(&mut self.view.filter)
                            .hint_text("such as brick wall")
                            .desired_width(170.0),
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
                });
                ui.horizontal(|ui| {
                    let name = selected.as_deref().unwrap_or("Click a texture");
                    ui.label(RichText::new(name).monospace());
                    if let Some(Thumbnail::Ready(_, [width, height])) =
                        selected.as_ref().and_then(|name| self.thumbnails.get(name))
                    {
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
                        self.listing = None;
                        self.list_receiver = None;
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
        let status = match (&self.listing, &self.index_receiver) {
            (Some(listing), Some(_)) => format!(
                "{} materials; reading {} of them for the filters",
                thousands(listing.materials.len()),
                thousands(listing.materials.len() - self.info.len().min(listing.materials.len()))
            ),
            (Some(listing), None) => format!("{} materials", thousands(listing.materials.len())),
            (None, _) => String::new(),
        };
        ui.label(RichText::new(status).small().color(MUTED));
    }

    /// Hides the sources the settings hide and drops a saved folder the game lacks, once
    /// a listing arrives.
    fn restore_view(&mut self, listing: &Listing) {
        self.hidden_sources = listing
            .sources
            .iter()
            .enumerate()
            .filter(|(_, source)| self.view.hidden_sources.contains(&source.path))
            .map(|(index, _)| index)
            .collect();
        if self
            .view
            .folder
            .as_ref()
            .is_some_and(|folder| !listing.per_folder.contains_key(folder))
        {
            self.view.folder = None;
        }
    }

    /// Writes the filters and layout to the settings when they have changed, waiting
    /// while a text box is being typed in so every key press does not save.
    fn save_view(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if let Some(listing) = &self.listing {
            let mut hidden: Vec<PathBuf> = self
                .hidden_sources
                .iter()
                .filter_map(|index| listing.sources.get(*index))
                .map(|source| source.path.clone())
                .collect();
            hidden.sort();
            self.view.hidden_sources = hidden;
        }
        if self.view == editor.config.texture_browser_view || ctx.wants_keyboard_input() {
            return;
        }
        editor.config.texture_browser_view = self.view.clone();
        editor.save_config();
    }

    /// Asks for a thumbnail if it is not loaded or loading.
    fn request(&mut self, name: &str) {
        if self.thumbnails.contains_key(name) {
            return;
        }
        let Some(loaders) = &self.loaders else { return };
        if let Ok(mut queue) = loaders.requests.lock() {
            queue.push(name.to_owned());
            let excess = queue.len().saturating_sub(MAX_REQUESTS);
            if excess > 0 {
                for dropped in queue.drain(..excess) {
                    self.thumbnails.remove(&dropped);
                }
            }
        }
        self.thumbnails.insert(name.to_owned(), Thumbnail::Loading);
    }

    /// Drops every thumbnail when there are more than `limit`, so they reload as needed and
    /// browsing never fills GPU memory.
    fn trim(&mut self, limit: usize) {
        if self.thumbnails.len() > limit {
            self.thumbnails
                .retain(|_, thumbnail| matches!(thumbnail, Thumbnail::Loading));
        }
    }
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

/// A grey checkerboard, so transparent parts of a texture show.
pub(super) fn checkerboard(painter: &egui::Painter, rect: egui::Rect) {
    let cell = 8.0;
    let columns = (rect.width() / cell).ceil() as usize;
    let rows = (rect.height() / cell).ceil() as usize;
    for row in 0..rows {
        for column in 0..columns {
            if (row + column) % 2 == 0 {
                let min = rect.min + egui::vec2(column as f32 * cell, row as f32 * cell);
                let square = egui::Rect::from_min_size(min, egui::Vec2::splat(cell)).intersect(rect);
                painter.rect_filled(square, 0, Color32::from_gray(60));
            }
        }
    }
}

/// `30782` as `30,782`.
pub(super) fn thousands(number: usize) -> String {
    let digits = number.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// Cuts text to `limit` characters with an ellipsis.
pub(super) fn elide(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit.max(4) {
        text.to_owned()
    } else {
        format!("{}…", text.chars().take(limit.max(4) - 1).collect::<String>())
    }
}
