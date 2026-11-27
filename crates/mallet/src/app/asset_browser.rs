//! What the texture and model browsers share: listing the game's files with where each
//! comes from, loading thumbnails in the background while you scroll, the sidebar of
//! sources and folders, the search box, and the grid of cards. A change made here shows
//! in both browsers.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icon;
use mallet_content::{Source, SourceKind};

pub(super) const MUTED: Color32 = Color32::from_gray(140);
pub(super) const ACCENT: Color32 = Color32::from_rgb(70, 140, 230);
/// Earlier searches each browser remembers.
pub(super) const MAX_FILTER_HISTORY: usize = 12;
/// The name the sidebar gives files that lie in no folder.
pub(super) const ROOT_FOLDER: &str = "(no folder)";
/// Space between cards, across and down.
const CARD_GAP: f32 = 6.0;
/// Height of the name bar under each card's picture.
const CARD_LABEL: f32 = 22.0;
/// Width the grid leaves free on the right for its scroll bar.
const SCROLL_BAR_ROOM: f32 = 16.0;

/// A card's picture: waiting to load, loaded at its real size, or failed with a reason
/// when one is known.
pub(super) enum Thumbnail {
    Loading,
    Ready(egui::TextureHandle, [u32; 2]),
    Failed(Option<String>),
}

/// The thumbnails waiting to load, newest last, shared by the loaders and any other
/// background work that should wait for them.
#[derive(Default)]
pub(super) struct Queue {
    names: Mutex<Vec<String>>,
    /// Signalled when a name is added, so an idle loader starts on it straight away.
    added: Condvar,
    /// Thumbnails a loader is working on right now.
    loading: AtomicUsize,
}

impl Queue {
    fn names(&self) -> MutexGuard<'_, Vec<String>> {
        self.names.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether no thumbnail is waiting or loading, so other work may use the drives.
    pub(super) fn idle(&self) -> bool {
        self.loading.load(Ordering::Relaxed) == 0 && self.names().is_empty()
    }

    /// The newest name waiting, after waiting for one to be added, counted as loading
    /// until [`Queue::done`]. `None` once `stop` is set.
    fn next(&self, stop: &AtomicBool) -> Option<String> {
        let mut names = self.names();
        loop {
            if stop.load(Ordering::Relaxed) {
                return None;
            }
            if let Some(name) = names.pop() {
                self.loading.fetch_add(1, Ordering::Relaxed);
                return Some(name);
            }
            names = self
                .added
                .wait_timeout(names, Duration::from_millis(250))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Marks a thumbnail taken with [`Queue::next`] as finished.
    fn done(&self) {
        self.loading.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The background thumbnail loaders for one game, started with these thread and size
/// settings. Each turns a name into an `R`, sent back to the browser.
pub(super) struct Loaders<R> {
    game: PathBuf,
    threads: usize,
    side: u32,
    results: Receiver<(String, R)>,
    stop: Arc<AtomicBool>,
    queue: Arc<Queue>,
}

impl<R> Drop for Loaders<R> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.queue.added.notify_all();
    }
}

impl<R: Send + 'static> Loaders<R> {
    /// Starts `threads` loaders that take names from the thumbnails' queue, newest first,
    /// and run `work` on each.
    pub(super) fn start<W>(game: &Path, threads: usize, side: u32, thumbnails: &Thumbnails, work: W) -> Self
    where
        W: Fn(&str) -> R + Send + Sync + 'static,
    {
        let queue = Arc::clone(&thumbnails.queue);
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, results) = mpsc::channel();
        let work = Arc::new(work);
        for _ in 0..threads {
            let (queue, stop, sender, work) =
                (Arc::clone(&queue), Arc::clone(&stop), sender.clone(), Arc::clone(&work));
            thread::spawn(move || {
                while let Some(name) = queue.next(&stop) {
                    let result = work(&name);
                    queue.done();
                    if sender.send((name, result)).is_err() {
                        return;
                    }
                }
            });
        }
        Self {
            game: game.to_path_buf(),
            threads,
            side,
            results,
            stop,
            queue,
        }
    }

    /// Whether these loaders were started for this game with these settings.
    pub(super) fn matches(&self, game: &Path, threads: usize, side: u32) -> bool {
        self.game == game && self.threads == threads && self.side == side
    }

    /// Every result finished since the last call.
    pub(super) fn finished(&self) -> Vec<(String, R)> {
        self.results.try_iter().collect()
    }
}

/// The thumbnails loaded or loading, with when each was last drawn so the ones off
/// screen longest are dropped first.
pub(super) struct Thumbnails {
    pictures: HashMap<String, Thumbnail>,
    last_shown: HashMap<String, u64>,
    frame: u64,
    /// Requests waiting to load; older ones are dropped, as they have scrolled out of view.
    max_requests: usize,
    queue: Arc<Queue>,
}

impl Thumbnails {
    pub(super) fn new(max_requests: usize) -> Self {
        Self {
            pictures: HashMap::new(),
            last_shown: HashMap::new(),
            frame: 0,
            max_requests,
            queue: Arc::default(),
        }
    }

    /// The queue the loaders read, for background work that waits until it is idle.
    pub(super) fn queue(&self) -> Arc<Queue> {
        Arc::clone(&self.queue)
    }

    /// Counts a new frame, for telling which thumbnails were drawn last.
    pub(super) fn next_frame(&mut self) {
        self.frame += 1;
    }

    pub(super) fn get(&self, name: &str) -> Option<&Thumbnail> {
        self.pictures.get(name)
    }

    /// The real size of a loaded thumbnail's image.
    pub(super) fn size(&self, name: &str) -> Option<[u32; 2]> {
        match self.pictures.get(name) {
            Some(Thumbnail::Ready(_, size)) => Some(*size),
            _ => None,
        }
    }

    /// Notes that a thumbnail is drawn this frame, and asks for it when it is not loaded
    /// or loading and `loading` says loaders are running.
    pub(super) fn show(&mut self, name: &str, loading: bool) {
        match self.last_shown.get_mut(name) {
            Some(frame) => *frame = self.frame,
            None => {
                self.last_shown.insert(name.to_owned(), self.frame);
            }
        }
        if !loading || self.pictures.contains_key(name) {
            return;
        }
        let dropped: Vec<String> = {
            let mut queue = self.queue.names();
            queue.push(name.to_owned());
            let excess = queue.len().saturating_sub(self.max_requests);
            queue.drain(..excess).collect()
        };
        for dropped in dropped {
            self.pictures.remove(&dropped);
        }
        self.queue.added.notify_one();
        self.pictures.insert(name.to_owned(), Thumbnail::Loading);
    }

    /// Keeps a finished thumbnail, unless nobody waits for it any more.
    pub(super) fn finish(&mut self, name: String, thumbnail: Thumbnail) {
        if let Some(slot) = self.pictures.get_mut(&name) {
            *slot = thumbnail;
        }
    }

    /// Forgets one thumbnail, so it loads again the next time it is drawn.
    pub(super) fn remove(&mut self, name: &str) {
        self.pictures.remove(name);
    }

    /// Forgets every thumbnail and every request still waiting.
    pub(super) fn clear(&mut self) {
        self.pictures.clear();
        self.last_shown.clear();
        self.queue.names().clear();
    }

    /// Whether a thumbnail is still loading.
    pub(super) fn waiting(&self) -> bool {
        self.pictures
            .values()
            .any(|thumbnail| matches!(thumbnail, Thumbnail::Loading))
    }

    /// Drops the thumbnails off screen longest when there are more than `limit`, down to
    /// three quarters of it, so they reload as needed and browsing never fills GPU memory.
    pub(super) fn trim(&mut self, limit: usize) {
        if self.pictures.len() <= limit {
            return;
        }
        let mut loaded: Vec<(u64, String)> = self
            .pictures
            .iter()
            .filter(|(_, thumbnail)| !matches!(thumbnail, Thumbnail::Loading))
            .map(|(name, _)| (self.last_shown.get(name).copied().unwrap_or(0), name.clone()))
            .collect();
        loaded.sort_unstable();
        let excess = self.pictures.len().saturating_sub(limit * 3 / 4);
        for (_, name) in loaded.into_iter().take(excess) {
            self.pictures.remove(&name);
            self.last_shown.remove(&name);
        }
    }
}

/// Turns RGBA pixels into a texture for a card, smoothed when drawn smaller.
pub(super) fn upload(ctx: &egui::Context, key: String, size: [usize; 2], rgba: &[u8]) -> egui::TextureHandle {
    let pixels = egui::ColorImage::from_rgba_unmultiplied(size, rgba);
    let options = egui::TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear));
    ctx.load_texture(key, pixels, options)
}

/// A folder in the sidebar's tree.
#[derive(Default)]
pub(super) struct Folder {
    /// Files in this folder and every folder below it.
    pub(super) count: usize,
    pub(super) children: BTreeMap<String, Folder>,
}

impl Folder {
    /// Counts a file, by its name in the browser, in every folder on its way down.
    fn add(&mut self, name: &str) {
        self.count += 1;
        let mut folder = self;
        for part in folder_of(name).split('/') {
            folder = folder.children.entry(part.to_owned()).or_default();
            folder.count += 1;
        }
    }

    /// Whether a folder path such as `props/de_dust` is in the tree.
    pub(super) fn contains(&self, path: &str) -> bool {
        let mut node = self;
        path.split('/').all(|part| match node.children.get(part) {
            Some(child) => {
                node = child;
                true
            }
            None => false,
        })
    }
}

/// The folder a file lies in, by its name in the browser: [`ROOT_FOLDER`] for one in
/// no folder.
fn folder_of(name: &str) -> &str {
    name.rsplit_once('/').map_or(ROOT_FOLDER, |(directory, _)| directory)
}

/// Whether a file, by its name in the browser, lies in a sidebar folder or below it.
pub(super) fn in_folder(name: &str, folder: &str) -> bool {
    match name.rsplit_once('/') {
        None => folder == ROOT_FOLDER,
        Some((directory, _)) => {
            directory == folder || directory.strip_prefix(folder).is_some_and(|rest| rest.starts_with('/'))
        }
    }
}

/// The game's files of one kind with where each comes from, counts for the sidebar, and
/// whatever else `X` the browser learns while listing.
pub(super) struct Listing<X> {
    pub(super) game: PathBuf,
    /// Each file with every source that has it; the first is where it loads from.
    pub(super) entries: Vec<(String, Vec<usize>)>,
    pub(super) sources: Vec<Source>,
    pub(super) per_source: HashMap<usize, usize>,
    pub(super) folders: Folder,
    pub(super) extra: X,
}

impl<X> Listing<X> {
    /// Counts the entries per source and folder. `short` gives an entry's name as the
    /// browser shows it, such as a model's path without `models/`, which the folders
    /// are taken from.
    pub(super) fn new(
        game: PathBuf,
        entries: Vec<(String, Vec<usize>)>,
        sources: Vec<Source>,
        short: fn(&str) -> &str,
        extra: X,
    ) -> Self {
        let mut per_source: HashMap<usize, usize> = HashMap::new();
        let mut folders = Folder::default();
        for (name, sources) in &entries {
            for source in sources {
                *per_source.entry(*source).or_default() += 1;
            }
            folders.add(short(name));
        }
        Self {
            game,
            entries,
            sources,
            per_source,
            folders,
            extra,
        }
    }

    /// The names of the sources that have an entry, where it loads from first.
    pub(super) fn source_names(&self, index: usize) -> Option<(String, Vec<String>)> {
        let (name, sources) = self.entries.get(index)?;
        let names = sources
            .iter()
            .filter_map(|source| self.sources.get(*source))
            .map(|source| source.name.clone())
            .collect();
        Some((name.clone(), names))
    }
}

/// A listing and the background work that makes it.
pub(super) struct Catalog<X> {
    pub(super) listing: Option<Listing<X>>,
    pending: Option<(PathBuf, Receiver<Listing<X>>)>,
}

impl<X> Default for Catalog<X> {
    fn default() -> Self {
        Self {
            listing: None,
            pending: None,
        }
    }
}

impl<X: Send + 'static> Catalog<X> {
    /// Starts listing `game` in the background with `work`, unless it is listed or being
    /// listed already. Returns whether it started.
    pub(super) fn list(&mut self, game: &Path, work: impl FnOnce() -> Listing<X> + Send + 'static) -> bool {
        let listed = self.listing.as_ref().is_some_and(|listing| listing.game == game);
        let listing = self.pending.as_ref().is_some_and(|(pending, _)| pending == game);
        if listed || listing {
            return false;
        }
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(work());
        });
        self.pending = Some((game.to_path_buf(), receiver));
        self.listing = None;
        true
    }

    /// Takes in a finished listing. Returns whether one arrived this frame.
    pub(super) fn receive(&mut self, ctx: &egui::Context) -> bool {
        let Some((_, receiver)) = &self.pending else {
            return false;
        };
        match receiver.try_recv() {
            Ok(listing) => {
                self.listing = Some(listing);
                self.pending = None;
                true
            }
            Err(_) => {
                ctx.request_repaint_after(Duration::from_millis(50));
                false
            }
        }
    }

    /// Forgets the listing, so the game is listed again.
    pub(super) fn reset(&mut self) {
        self.listing = None;
        self.pending = None;
    }
}

/// The sources the settings hide, as indices into the listing's sources.
pub(super) fn restore_hidden(sources: &[Source], saved: &[PathBuf]) -> HashSet<usize> {
    sources
        .iter()
        .enumerate()
        .filter(|(_, source)| saved.contains(&source.path))
        .map(|(index, _)| index)
        .collect()
}

/// The hidden sources by path, sorted, as the settings keep them.
pub(super) fn hidden_paths(sources: &[Source], hidden: &HashSet<usize>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = hidden
        .iter()
        .filter_map(|index| sources.get(*index))
        .map(|source| source.path.clone())
        .collect();
    paths.sort();
    paths
}

/// The sidebar: sources grouped by kind, then the folder tree, each with a count. The
/// top-level folders `skip` names are left out. With `reveal`, the folders on the way
/// down to the chosen one open.
pub(super) fn sidebar<X>(
    ui: &mut egui::Ui,
    catalog: &Catalog<X>,
    noun: &str,
    hidden: &mut HashSet<usize>,
    folder: &mut Option<String>,
    reveal: bool,
    skip: impl Fn(&str) -> bool,
) {
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
    let Some(listing) = &catalog.listing else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(format!("Listing {noun}..."));
        });
        return;
    };
    sources_list(ui, listing, hidden);
    ui.add_space(8.0);
    ui.label(RichText::new("Folders").strong());
    ui.horizontal(|ui| {
        if ui.selectable_label(folder.is_none(), "All folders").clicked() {
            *folder = None;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(thousands(listing.folders.count)).small().color(MUTED));
        });
    });
    folder_tree(ui, &listing.folders, "", folder, reveal, &skip);
}

/// The sources part of the sidebar, a tick box for each, grouped by kind.
fn sources_list<X>(ui: &mut egui::Ui, listing: &Listing<X>, hidden: &mut HashSet<usize>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Sources").strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("None").clicked() {
                *hidden = listing.per_source.keys().copied().collect();
            }
            if ui.small_button("All").clicked() {
                hidden.clear();
            }
        });
    });
    let groups = [
        (SourceKind::Game, "Game"),
        (SourceKind::MountedGame, "Mounted games"),
        (SourceKind::Addon, "Addons"),
        (SourceKind::Download, "Downloads"),
    ];
    let set = |hidden: &mut HashSet<usize>, index: usize, on: bool| {
        if on {
            hidden.remove(&index);
        } else {
            hidden.insert(index);
        }
    };
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
        let shown = members.iter().filter(|(index, _, _)| !hidden.contains(index)).count();
        egui::CollapsingHeader::new(format!("{label}  {}", thousands(total)))
            .id_salt(("source_group", label))
            .default_open(kind != SourceKind::Addon)
            .show(ui, |ui| {
                let mut all = shown == members.len();
                if ui.checkbox(&mut all, RichText::new("All of these").small()).changed() {
                    for (index, _, _) in &members {
                        set(hidden, *index, all);
                    }
                }
                for (index, source, count) in &members {
                    let mut on = !hidden.contains(index);
                    ui.horizontal(|ui| {
                        if ui
                            .checkbox(&mut on, &source.name)
                            .on_hover_text(source.path.display().to_string())
                            .changed()
                        {
                            set(hidden, *index, on);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(thousands(*count)).small().color(MUTED));
                        });
                    });
                }
            });
    }
}

/// The sidebar's folder tree under `folder`, whose own path is `prefix`. Clicking a
/// folder's name narrows the list to it and everything below; its arrow opens it.
fn folder_tree(
    ui: &mut egui::Ui,
    folder: &Folder,
    prefix: &str,
    selected: &mut Option<String>,
    reveal: bool,
    skip: &dyn Fn(&str) -> bool,
) {
    for (name, child) in &folder.children {
        if prefix.is_empty() && skip(name) {
            continue;
        }
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let label = |ui: &mut egui::Ui, selected: &mut Option<String>| {
            let chosen = selected.as_deref() == Some(path.as_str());
            if ui.selectable_label(chosen, name).on_hover_text(&path).clicked() {
                *selected = if chosen { None } else { Some(path.clone()) };
            }
            ui.label(RichText::new(thousands(child.count)).small().color(MUTED));
        };
        if child.children.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(ui.spacing().indent);
                label(ui, selected);
            });
            continue;
        }
        let id = ui.make_persistent_id(("browser_folder", &path));
        let holds_selection = selected
            .as_deref()
            .is_some_and(|chosen| chosen.starts_with(&format!("{path}/")));
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
        if reveal && holds_selection {
            state.set_open(true);
        }
        state
            .show_header(ui, |ui| label(ui, selected))
            .body(|ui| folder_tree(ui, child, &path, selected, reveal, skip));
    }
}

/// What a search box looks for and how it explains itself.
pub(super) struct SearchBox<'a> {
    pub(super) hint: &'a str,
    pub(super) help: &'a str,
    pub(super) width: f32,
}

impl SearchBox<'_> {
    /// The search box with its earlier searches and a button to clear it. Takes the
    /// keyboard when `focus` is set, and keeps a search in the history once it is left.
    pub(super) fn show(&self, ui: &mut egui::Ui, filter: &mut String, history: &mut Vec<String>, focus: &mut bool) {
        ui.label("Filter");
        let search = ui
            .add(
                egui::TextEdit::singleline(filter)
                    .hint_text(self.hint)
                    .desired_width(self.width),
            )
            .on_hover_text(self.help);
        if *focus {
            search.request_focus();
            *focus = false;
        }
        if search.lost_focus() && !filter.trim().is_empty() {
            let kept = filter.trim().to_owned();
            history.retain(|old| old != &kept);
            history.insert(0, kept);
            history.truncate(MAX_FILTER_HISTORY);
        }
        ui.add_enabled_ui(!history.is_empty(), |ui| {
            ui.menu_button(icon::CARET_DOWN, |ui| {
                for old in history.clone() {
                    if ui.button(&old).clicked() {
                        *filter = old;
                        ui.close_menu();
                    }
                }
            })
            .response
            .on_hover_text("Earlier searches");
        });
        if ui
            .add_enabled(!filter.is_empty(), egui::Button::new(icon::X))
            .on_hover_text("Clear the search")
            .clicked()
        {
            filter.clear();
        }
    }
}

/// The grid of cards, `per_row` to a row sharing the width evenly, drawing only the rows
/// on screen. `card` draws the card for an entry, given its index in the shown entries
/// and the side of its picture. The scroll bar is solid and always shown, with a handle
/// long enough to grab: egui's floating bar hides until hovered, and with tens of
/// thousands of entries its handle shrinks to a dot. It fits in the room the cards leave
/// free on the right.
pub(super) fn card_grid(
    ui: &mut egui::Ui,
    salt: &str,
    count: usize,
    per_row: usize,
    [width, height]: [f32; 2],
    scroll_step: f32,
    mut card: impl FnMut(&mut egui::Ui, usize, f32),
) {
    let columns = per_row.clamp(2, 14);
    let side = ((width - SCROLL_BAR_ROOM - CARD_GAP * (columns as f32 - 1.0)) / columns as f32).max(32.0);
    let row_height = side + CARD_LABEL + CARD_GAP;
    let rows = count.div_ceil(columns);
    let wheel = wheel_rows(ui, ui.available_rect_before_wrap());
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
        .id_salt(salt)
        .max_height(height)
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .show_rows(ui, row_height, rows, |ui, range| {
            if wheel != 0.0 {
                ui.scroll_with_delta_animation(
                    egui::vec2(0.0, wheel * row_height * scroll_step),
                    egui::style::ScrollAnimation::none(),
                );
            }
            for row in range {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = CARD_GAP;
                    for index in (row * columns..(row + 1) * columns).take_while(|index| *index < count) {
                        card(ui, index, side);
                    }
                });
                ui.add_space(CARD_GAP);
            }
        });
}

/// One card: a square for the picture, which `picture` paints, and the name under it.
/// The entry in use now is outlined in the accent colour and the one clicked last in
/// grey.
pub(super) fn card(
    ui: &mut egui::Ui,
    side: f32,
    name: &str,
    current: bool,
    selected: bool,
    picture: impl FnOnce(&egui::Painter, egui::Rect),
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(side, side + CARD_LABEL), egui::Sense::click());
    let image_rect = egui::Rect::from_min_size(rect.min, egui::Vec2::splat(side));
    let painter = ui.painter();
    let shade = match (selected, response.hovered()) {
        (true, _) => 54,
        (false, true) => 44,
        (false, false) => 30,
    };
    painter.rect_filled(rect, 4, Color32::from_gray(shade));
    picture(painter, image_rect);
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
        elide(short, ((side - 8.0) / 6.0) as usize),
        egui::FontId::proportional(11.0),
        Color32::from_gray(if current || selected { 255 } else { 210 }),
    );
    if current {
        painter.rect_stroke(rect, 4, egui::Stroke::new(2.0_f32, ACCENT), egui::StrokeKind::Inside);
    } else if selected {
        painter.rect_stroke(
            rect,
            4,
            egui::Stroke::new(1.5_f32, Color32::from_gray(200)),
            egui::StrokeKind::Inside,
        );
    }
    response
}

/// Paints a loaded thumbnail centred in `rect`: small ones at their real size, as in
/// Hammer, and large ones shrunk to fit. A failed one shows a warning icon.
pub(super) fn paint_thumbnail(painter: &egui::Painter, rect: egui::Rect, thumbnail: Option<&Thumbnail>, upscale: bool) {
    match thumbnail {
        Some(Thumbnail::Ready(texture, [width, height])) => {
            let room = rect.width() - 6.0;
            let fit = room / (*width).max(*height).max(1) as f32;
            let scale = if upscale { fit } else { fit.min(1.0) };
            let target =
                egui::Rect::from_center_size(rect.center(), egui::vec2(*width as f32 * scale, *height as f32 * scale));
            let full = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
            painter.image(texture.id(), target, full, Color32::WHITE);
        }
        Some(Thumbnail::Failed(_)) => {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                icon::WARNING,
                egui::FontId::proportional(22.0),
                MUTED,
            );
        }
        _ => {}
    }
}

/// Tooltip lines naming where an entry loads from and which other sources have it.
pub(super) fn source_lines(sources: &[String]) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some((first, others)) = sources.split_first() {
        lines.push(format!("loads from {first}"));
        if !others.is_empty() {
            lines.push(format!("also in {}", others.join(", ")));
        }
    }
    lines
}

/// How many wheel notches the pointer scrolled over `area` this frame, taken from egui so
/// its scroll area does not also scroll by its own line height. The browsers scroll that
/// many cards, as set in Mallet Options. egui spreads one notch over several frames, so the
/// share it hands out each frame is taken rather than the wheel events, which would leave
/// the rest of the notch to scroll the grid by egui's fixed amount.
fn wheel_rows(ui: &egui::Ui, area: egui::Rect) -> f32 {
    if !ui.rect_contains_pointer(area) {
        return 0.0;
    }
    let line = ui.ctx().options(|options| options.line_scroll_speed).max(1.0);
    ui.ctx().input_mut(|input| {
        let rows = input.smooth_scroll_delta.y / line;
        input.smooth_scroll_delta.y = 0.0;
        rows
    })
}

/// A grey checkerboard, so transparent parts of a picture show.
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

/// The words of a search: every plain word must be in a name, and no word that started
/// with `!` may be.
pub(super) struct Search {
    wanted: Vec<String>,
    unwanted: Vec<String>,
}

impl Search {
    pub(super) fn parse(text: &str) -> Self {
        let mut search = Self {
            wanted: Vec::new(),
            unwanted: Vec::new(),
        };
        for word in text.split_whitespace() {
            let word = word.to_ascii_lowercase().replace('\\', "/");
            match word.strip_prefix('!') {
                Some("") => {}
                Some(rest) => search.unwanted.push(rest.to_owned()),
                None => search.wanted.push(word),
            }
        }
        search
    }

    pub(super) fn matches(&self, name: &str) -> bool {
        self.wanted.iter().all(|word| contains(name, word)) && !self.unwanted.iter().any(|word| contains(name, word))
    }
}

/// Whether a name has a word in it, where `*` in the word stands for any text and `?`
/// for any one character.
fn contains(name: &str, word: &str) -> bool {
    if !word.contains(['*', '?']) {
        return name.contains(word);
    }
    let (name, pattern) = (name.as_bytes(), format!("*{word}*").into_bytes());
    let (mut at, mut in_pattern) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while at < name.len() {
        match pattern.get(in_pattern) {
            Some(b'*') => {
                star = Some((in_pattern, at));
                in_pattern += 1;
            }
            Some(byte) if *byte == b'?' || *byte == name[at] => {
                at += 1;
                in_pattern += 1;
            }
            _ => match star {
                Some((star_at, matched)) => {
                    in_pattern = star_at + 1;
                    at = matched + 1;
                    star = Some((star_at, matched + 1));
                }
                None => return false,
            },
        }
    }
    pattern[in_pattern..].iter().all(|byte| *byte == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_count_in_every_folder_on_their_way_down() {
        let mut root = Folder::default();
        for name in ["props_c17/oildrum001", "props_c17/trim/a", "lonely"] {
            root.add(name);
        }
        assert_eq!(root.count, 3);
        assert_eq!(root.children["props_c17"].count, 2);
        assert_eq!(root.children[ROOT_FOLDER].count, 1);
        assert!(root.contains("props_c17/trim"));
        assert!(!root.contains("props_c17/missing"));
        assert!(in_folder("props_c17/trim/a", "props_c17"));
        assert!(!in_folder("props_c17x/a", "props_c17"));
        assert!(in_folder("lonely", ROOT_FOLDER));
    }

    #[test]
    fn a_search_takes_wildcards_and_leaves_words_out() {
        let search = Search::parse("props_*/crate?? !gib");
        assert!(search.matches("props_junk/crate01"));
        assert!(!search.matches("props_junk/crate01_gib"));
        assert!(!search.matches("props_junk/barrel01"));
    }
}
