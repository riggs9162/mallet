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
/// The fewest and most cards a row of the grid can hold.
const MIN_PER_ROW: usize = 2;
const MAX_PER_ROW: usize = 14;
/// How far Ctrl and the mouse wheel have to zoom before a row holds one card more or less.
const ZOOM_STEP: f32 = 1.25;
/// Entries the Recent shelf keeps.
const RECENT_LIMIT: usize = 24;
/// Side of a picture on the Recent and Favourites shelf, in points.
const SHELF_SIDE: f32 = 52.0;
/// How long a thumbnail takes to fade in once it has loaded.
const FADE_SECONDS: f64 = 0.18;
/// Rows Page Up and Page Down move the selection by.
const PAGE_ROWS: i64 = 4;

/// Text that matters less, such as counts, in the colours of the current theme.
pub(super) fn muted(ui: &egui::Ui) -> Color32 {
    ui.visuals().weak_text_color()
}

/// The theme's accent colour for text that marks something, such as the entry in use.
pub(super) fn accent(ui: &egui::Ui) -> Color32 {
    ui.visuals().hyperlink_color
}

/// The theme's colour for warnings.
pub(super) fn warning(ui: &egui::Ui) -> Color32 {
    ui.visuals().warn_fg_color
}

/// A card's background: the window's colour moved toward the text's by `amount`, so it
/// stands out a little in a dark theme and a light one alike.
fn tint(visuals: &egui::Visuals, amount: f32) -> Color32 {
    visuals.window_fill.lerp_to_gamma(visuals.text_color(), amount)
}

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
/// screen longest are dropped first, and when each arrived so it can fade in.
pub(super) struct Thumbnails {
    pictures: HashMap<String, Thumbnail>,
    last_shown: HashMap<String, u64>,
    arrived: HashMap<String, f64>,
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
            arrived: HashMap::new(),
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

    /// How far a thumbnail has faded in at `now`, from 0 to 1.
    fn opacity(&self, name: &str, now: f64) -> f32 {
        self.arrived
            .get(name)
            .map_or(1.0, |arrived| ((now - arrived) / FADE_SECONDS).clamp(0.0, 1.0) as f32)
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

    /// Keeps a thumbnail that finished at `now`, unless nobody waits for it any more.
    pub(super) fn finish(&mut self, name: String, thumbnail: Thumbnail, now: f64) {
        if let Some(slot) = self.pictures.get_mut(&name) {
            *slot = thumbnail;
            self.arrived.insert(name, now);
        }
    }

    /// Forgets one thumbnail, so it loads again the next time it is drawn.
    pub(super) fn remove(&mut self, name: &str) {
        self.pictures.remove(name);
        self.arrived.remove(name);
    }

    /// Forgets every thumbnail and every request still waiting.
    pub(super) fn clear(&mut self) {
        self.pictures.clear();
        self.last_shown.clear();
        self.arrived.clear();
        self.queue.names().clear();
    }

    /// Whether a thumbnail is still loading or fading in at `now`, so the window has to
    /// be drawn again soon.
    pub(super) fn busy(&mut self, now: f64) -> bool {
        self.arrived.retain(|_, arrived| now - *arrived < FADE_SECONDS);
        !self.arrived.is_empty()
            || self
                .pictures
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
            self.arrived.remove(&name);
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

    /// Whether this folder, whose path is `path`, or any folder below it has `term` in
    /// its path.
    fn matches(&self, path: &str, term: &str) -> bool {
        path.contains(term)
            || self.children.iter().any(|(name, child)| {
                let below = if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}/{name}")
                };
                child.matches(&below, term)
            })
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

/// The folder chosen in the sidebar, the text the folder tree is searched for, and
/// whether the folders on the way down to the chosen one should open this frame.
pub(super) struct FolderPick<'a> {
    pub(super) folder: &'a mut Option<String>,
    pub(super) search: &'a mut String,
    pub(super) reveal: bool,
}

/// The sidebar: sources grouped by kind, then a search box and the folder tree, each with
/// a count. The top-level folders `skip` names are left out.
pub(super) fn sidebar<X>(
    ui: &mut egui::Ui,
    catalog: &Catalog<X>,
    noun: &str,
    hidden: &mut HashSet<usize>,
    pick: FolderPick,
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
        ui.add(
            egui::TextEdit::singleline(pick.search)
                .hint_text(format!("{} Find a folder", icon::MAGNIFYING_GLASS))
                .desired_width(ui.available_width() - 28.0),
        );
        if ui
            .add_enabled(!pick.search.is_empty(), egui::Button::new(icon::X).small())
            .on_hover_text("Show every folder")
            .clicked()
        {
            pick.search.clear();
        }
    });
    ui.horizontal(|ui| {
        if ui.selectable_label(pick.folder.is_none(), "All folders").clicked() {
            *pick.folder = None;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(thousands(listing.folders.count)).small().color(muted(ui)));
        });
    });
    let term = pick.search.trim().to_ascii_lowercase();
    let tree = Tree {
        term: &term,
        reveal: pick.reveal,
        skip: &skip,
    };
    tree.show(ui, &listing.folders, "", pick.folder);
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
                            ui.label(RichText::new(thousands(*count)).small().color(muted(ui)));
                        });
                    });
                }
            });
    }
}

/// How the sidebar's folder tree is drawn: only the folders whose path has `term` in it
/// and the folders above them, opened, when it is not empty; with `reveal`, the folders
/// on the way down to the chosen one open.
struct Tree<'a> {
    term: &'a str,
    reveal: bool,
    skip: &'a dyn Fn(&str) -> bool,
}

impl Tree<'_> {
    /// The folders under `folder`, whose own path is `prefix`. Clicking a folder's name
    /// narrows the list to it and everything below; its arrow opens it.
    fn show(&self, ui: &mut egui::Ui, folder: &Folder, prefix: &str, selected: &mut Option<String>) {
        for (name, child) in &folder.children {
            if prefix.is_empty() && (self.skip)(name) {
                continue;
            }
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if !self.term.is_empty() && !child.matches(&path.to_ascii_lowercase(), self.term) {
                continue;
            }
            let label = |ui: &mut egui::Ui, selected: &mut Option<String>| {
                let chosen = selected.as_deref() == Some(path.as_str());
                if ui.selectable_label(chosen, name).on_hover_text(&path).clicked() {
                    *selected = if chosen { None } else { Some(path.clone()) };
                }
                ui.label(RichText::new(thousands(child.count)).small().color(muted(ui)));
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
            if (self.reveal && holds_selection) || !self.term.is_empty() {
                state.set_open(true);
            }
            state
                .show_header(ui, |ui| label(ui, selected))
                .body(|ui| self.show(ui, child, &path, selected));
        }
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

/// How many cards a row of a grid `width` points wide holds when `per_row` are asked
/// for, and the side of each card's picture, which share the width evenly.
pub(super) fn grid_columns(width: f32, per_row: usize) -> (usize, f32) {
    let columns = per_row.clamp(MIN_PER_ROW, MAX_PER_ROW);
    let side = ((width - SCROLL_BAR_ROOM - CARD_GAP * (columns as f32 - 1.0)) / columns as f32).max(32.0);
    (columns, side)
}

/// Changes how many cards a row holds as Ctrl and the mouse wheel zoom over `area`:
/// zooming in makes the cards larger, so fewer fit. The zoom is added up across frames,
/// as egui hands out a wheel notch a little at a time.
fn zoom_per_row(ui: &egui::Ui, area: egui::Rect, per_row: &mut usize) {
    if !ui.rect_contains_pointer(area) {
        return;
    }
    let zoom = ui.input(|input| input.zoom_delta());
    if zoom == 1.0 {
        return;
    }
    let id = ui.id().with("card_zoom");
    let total = ui.data_mut(|data| {
        let total = data.get_temp_mut_or(id, 0.0_f32);
        *total += zoom.ln();
        *total
    });
    let steps = (total / ZOOM_STEP.ln()).trunc();
    if steps != 0.0 {
        let fewer = steps as i64;
        *per_row = (*per_row as i64 - fewer).clamp(MIN_PER_ROW as i64, MAX_PER_ROW as i64) as usize;
        ui.data_mut(|data| data.insert_temp(id, total - steps * ZOOM_STEP.ln()));
    }
}

/// The grid of cards, `per_row` to a row sharing the width evenly, drawing only the
/// rows on screen. Holding Ctrl and scrolling over it changes how many fit across.
/// `card` draws the card for an entry, given its index in the shown entries and the side
/// of its picture; with `reveal`, the grid scrolls the entry at that index into view.
///
/// The scroll bar is solid and always shown, with a handle long enough to grab: egui's
/// floating bar hides until hovered, and with tens of thousands of entries its handle
/// shrinks to a dot. It fits in the room the cards leave free on the right.
pub(super) fn card_grid(
    ui: &mut egui::Ui,
    salt: &str,
    count: usize,
    per_row: &mut usize,
    scroll_step: f32,
    reveal: Option<usize>,
    mut card: impl FnMut(&mut egui::Ui, usize, f32),
) {
    let area = ui.available_rect_before_wrap();
    zoom_per_row(ui, area, per_row);
    let (columns, side) = grid_columns(area.width(), *per_row);
    let row_height = side + CARD_LABEL + CARD_GAP;
    let rows = count.div_ceil(columns);
    let wheel = wheel_rows(ui, area);
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
        .max_height(area.height())
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .show_viewport(ui, |ui, viewport| {
            ui.set_height(rows as f32 * row_height);
            if wheel != 0.0 {
                ui.scroll_with_delta_animation(
                    egui::vec2(0.0, wheel * row_height * scroll_step),
                    egui::style::ScrollAnimation::none(),
                );
            }
            let top = ui.max_rect().top();
            let left = ui.max_rect().left();
            if let Some(index) = reveal {
                let y = top + (index / columns) as f32 * row_height;
                let rect = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), y..=y + row_height);
                ui.scroll_to_rect(rect, None);
            }
            let first = (viewport.min.y / row_height).floor().max(0.0) as usize;
            let last = ((viewport.max.y / row_height).ceil() as usize + 1).min(rows);
            for row in first..last {
                let y = top + row as f32 * row_height;
                let rect = egui::Rect::from_min_size(
                    egui::pos2(left, y),
                    egui::vec2(ui.max_rect().width(), side + CARD_LABEL),
                );
                let builder = egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Min));
                ui.allocate_new_ui(builder, |ui| {
                    ui.spacing_mut().item_spacing.x = CARD_GAP;
                    for index in (row * columns..(row + 1) * columns).take_while(|index| *index < count) {
                        ui.push_id(index, |ui| card(ui, index, side));
                    }
                });
            }
        });
}

/// How a card is marked: the entry in use now, the one clicked last, and whether it is
/// starred.
#[derive(Clone, Copy, Default)]
pub(super) struct Marks {
    pub(super) current: bool,
    pub(super) selected: bool,
    pub(super) favourite: bool,
}

/// One card: a square for the picture, which `picture` paints, and the name under it.
/// The entry in use now is outlined in the theme's selection colour and the one clicked
/// last in the text colour; a starred one has a star in its corner.
pub(super) fn card(
    ui: &mut egui::Ui,
    side: f32,
    name: &str,
    marks: Marks,
    picture: impl FnOnce(&egui::Ui, &egui::Painter, egui::Rect),
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(side, side + CARD_LABEL), egui::Sense::click());
    let image_rect = egui::Rect::from_min_size(rect.min, egui::Vec2::splat(side));
    let visuals = ui.visuals().clone();
    let painter = ui.painter();
    let shade = match (marks.selected, response.hovered()) {
        (true, _) => 0.16,
        (false, true) => 0.1,
        (false, false) => 0.05,
    };
    painter.rect_filled(rect, 4, tint(&visuals, shade));
    picture(ui, painter, image_rect);
    let label = egui::Rect::from_min_max(egui::pos2(rect.min.x, image_rect.max.y), rect.max);
    let corners = egui::CornerRadius {
        nw: 0,
        ne: 0,
        sw: 4,
        se: 4,
    };
    let (label_fill, label_text) = if marks.current {
        (visuals.selection.bg_fill, visuals.selection.stroke.color)
    } else if marks.selected {
        (visuals.extreme_bg_color, visuals.strong_text_color())
    } else {
        (visuals.extreme_bg_color, visuals.text_color())
    };
    painter.rect_filled(label, corners, label_fill);
    let short = name.rsplit('/').next().unwrap_or(name);
    painter.text(
        label.center(),
        egui::Align2::CENTER_CENTER,
        elide(short, ((side - 8.0) / 6.0) as usize),
        egui::FontId::proportional(11.0),
        label_text,
    );
    if marks.favourite {
        painter.text(
            image_rect.right_top() + egui::vec2(-5.0, 4.0),
            egui::Align2::RIGHT_TOP,
            icon::STAR,
            egui::FontId::proportional(14.0),
            visuals.warn_fg_color,
        );
    }
    if marks.current {
        painter.rect_stroke(
            rect,
            4,
            egui::Stroke::new(2.0_f32, visuals.selection.bg_fill),
            egui::StrokeKind::Inside,
        );
    } else if marks.selected {
        painter.rect_stroke(
            rect,
            4,
            egui::Stroke::new(1.5_f32, visuals.strong_text_color()),
            egui::StrokeKind::Inside,
        );
    }
    response
}

/// Paints a thumbnail centred in `rect`: small ones at their real size, as in Hammer,
/// and large ones shrunk to fit, or grown to fit as well with `upscale`. A loaded one
/// fades in; one still loading shows a gently pulsing placeholder, and a failed one a
/// warning icon.
pub(super) fn paint_thumbnail(
    ui: &egui::Ui,
    painter: &egui::Painter,
    rect: egui::Rect,
    thumbnails: &Thumbnails,
    name: &str,
    upscale: bool,
) {
    let visuals = ui.visuals();
    let now = ui.input(|input| input.time);
    match thumbnails.get(name) {
        Some(Thumbnail::Ready(texture, [width, height])) => {
            let room = rect.width() - 6.0;
            let fit = room / (*width).max(*height).max(1) as f32;
            let scale = if upscale { fit } else { fit.min(1.0) };
            let target =
                egui::Rect::from_center_size(rect.center(), egui::vec2(*width as f32 * scale, *height as f32 * scale));
            let full = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
            let tint = Color32::WHITE.gamma_multiply(thumbnails.opacity(name, now));
            painter.image(texture.id(), target, full, tint);
        }
        Some(Thumbnail::Failed(_)) => {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                icon::WARNING,
                egui::FontId::proportional(22.0),
                visuals.weak_text_color(),
            );
        }
        _ => {
            let wave = ((now * 2.5 - rect.left() as f64 * 0.004).sin() * 0.5 + 0.5) as f32;
            painter.rect_filled(rect.shrink(6.0), 3, tint(visuals, 0.06 + 0.05 * wave));
        }
    }
}

/// A rounded on-off button for the toolbar, with a count after its label when one is
/// given.
pub(super) fn chip(ui: &mut egui::Ui, on: bool, label: &str, count: Option<usize>) -> egui::Response {
    let text = match count {
        Some(count) => format!("{label}  {}", thousands(count)),
        None => label.to_owned(),
    };
    ui.add(egui::Button::new(text).selected(on).corner_radius(10.0))
}

/// A [`chip`] that turns `on` on and off when clicked.
pub(super) fn toggle_chip(ui: &mut egui::Ui, on: &mut bool, label: &str, count: Option<usize>) -> egui::Response {
    let response = chip(ui, *on, label, count);
    if response.clicked() {
        *on = !*on;
    }
    response
}

/// The slider for how many cards a row holds.
pub(super) fn per_row_slider(ui: &mut egui::Ui, per_row: &mut usize) {
    ui.add(egui::Slider::new(per_row, MIN_PER_ROW..=MAX_PER_ROW).text("per row"))
        .on_hover_text(
            "How many cards fit across; they grow and shrink with the window. Holding Ctrl and              scrolling over the cards changes it too.",
        );
}

/// The line under the grid: how many entries show out of how many, and a progress bar
/// while `progress` says background work is going on, with `extra` after them.
pub(super) fn status_line(
    ui: &mut egui::Ui,
    noun: &str,
    shown: usize,
    total: Option<usize>,
    progress: Option<(f32, String)>,
    extra: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        let counts = match total {
            Some(total) if total == shown => format!("{} {noun}", thousands(total)),
            Some(total) => format!("{} of {} {noun}", thousands(shown), thousands(total)),
            None => format!("Listing {noun}..."),
        };
        ui.label(RichText::new(counts).small().color(muted(ui)));
        if let Some((fraction, text)) = progress {
            ui.separator();
            ui.add(
                egui::ProgressBar::new(fraction)
                    .desired_width(150.0)
                    .desired_height(12.0)
                    .text(RichText::new(text).small()),
            );
        }
        extra(ui);
    });
}

/// The folder the grid is narrowed to, as a trail of folders above the grid: clicking
/// one narrows the list to it, and the first shows everything.
pub(super) fn path_bar(ui: &mut egui::Ui, root: &str, folder: &mut Option<String>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let crumb = |ui: &mut egui::Ui, text: &str, last: bool| {
            let text = if last {
                RichText::new(text).strong()
            } else {
                RichText::new(text).color(muted(ui))
            };
            ui.add(egui::Button::new(text).frame(false)).clicked()
        };
        if crumb(ui, &format!("{} {root}", icon::HOUSE), folder.is_none()) {
            *folder = None;
        }
        let Some(chosen) = folder.clone() else { return };
        let parts: Vec<&str> = chosen.split('/').collect();
        for (depth, part) in parts.iter().enumerate() {
            ui.label(RichText::new(icon::CARET_RIGHT).small().color(muted(ui)));
            if crumb(ui, part, depth + 1 == parts.len()) {
                *folder = Some(parts[..=depth].join("/"));
            }
        }
    });
}

/// The filters every browser has, which the empty grid offers to undo.
pub(super) struct Narrowing<'a> {
    pub(super) filter: &'a mut String,
    pub(super) hidden: &'a mut HashSet<usize>,
    pub(super) folder: &'a mut Option<String>,
    pub(super) used_only: &'a mut bool,
}

/// What the grid shows when nothing matches: why, and a button for each filter that
/// narrows the list, with `extra` adding the browser's own.
pub(super) fn empty_state(ui: &mut egui::Ui, noun: &str, narrowing: Narrowing, extra: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(icon::MAGNIFYING_GLASS).size(40.0).color(muted(ui)));
        ui.add_space(4.0);
        ui.label(RichText::new(format!("No {noun} match")).heading());
        ui.label(RichText::new("Widen the list with one of these:").color(muted(ui)));
        ui.add_space(6.0);
    });
    ui.horizontal_wrapped(|ui| {
        let width = ui.available_width();
        ui.add_space((width * 0.5 - 260.0).max(0.0));
        if !narrowing.filter.is_empty() && ui.button(format!("{} Clear the search", icon::X)).clicked() {
            narrowing.filter.clear();
        }
        if narrowing.folder.is_some() && ui.button(format!("{} All folders", icon::FOLDERS)).clicked() {
            *narrowing.folder = None;
        }
        if !narrowing.hidden.is_empty() && ui.button(format!("{} Every source", icon::STACK)).clicked() {
            narrowing.hidden.clear();
        }
        if *narrowing.used_only && ui.button("Unused ones too").clicked() {
            *narrowing.used_only = false;
        }
        extra(ui);
    });
}

/// Puts `name` first in a Recent list, keeping the list short.
pub(super) fn remember(recent: &mut Vec<String>, name: &str) {
    recent.retain(|old| old != name);
    recent.insert(0, name.to_owned());
    recent.truncate(RECENT_LIMIT);
}

/// Stars `name`, or takes its star away when it has one.
pub(super) fn toggle_favourite(favourites: &mut Vec<String>, name: &str) {
    if favourites.iter().any(|old| old == name) {
        favourites.retain(|old| old != name);
    } else {
        favourites.push(name.to_owned());
    }
}

/// What was done on the Recent and Favourites shelf.
pub(super) enum ShelfAction {
    /// Clicked: show it as the selected entry.
    Select(String),
    /// Double-clicked: use it.
    Use(String),
}

/// The shelf above the grid: the entries used last or the starred ones, as small
/// pictures. `short` gives an entry's name as the browser shows it.
pub(super) struct Shelf<'a> {
    pub(super) recent: &'a [String],
    pub(super) favourites: &'a [String],
    pub(super) selected: Option<&'a str>,
    pub(super) short: fn(&str) -> &str,
    pub(super) upscale: bool,
}

impl Shelf<'_> {
    /// Draws the shelf, nothing while both lists are empty. `show_favourites` picks the
    /// list shown.
    pub(super) fn show(
        &self,
        ui: &mut egui::Ui,
        thumbnails: &mut Thumbnails,
        loading: bool,
        show_favourites: &mut bool,
    ) -> Option<ShelfAction> {
        if self.recent.is_empty() && self.favourites.is_empty() {
            return None;
        }
        let mut action = None;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                if chip(
                    ui,
                    !*show_favourites,
                    &format!("{} Recent", icon::CLOCK_COUNTER_CLOCKWISE),
                    None,
                )
                .clicked()
                {
                    *show_favourites = false;
                }
                if chip(ui, *show_favourites, &format!("{} Starred", icon::STAR), None).clicked() {
                    *show_favourites = true;
                }
            });
            let names = if *show_favourites { self.favourites } else { self.recent };
            if names.is_empty() {
                let hint = if *show_favourites {
                    "Right-click a card and choose Star to keep it here."
                } else {
                    "What you use shows up here."
                };
                ui.label(RichText::new(hint).color(muted(ui)));
                return;
            }
            egui::ScrollArea::horizontal()
                .id_salt("browser_shelf")
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for name in names {
                            ui.push_id(name, |ui| {
                                let response = self.item(ui, thumbnails, loading, name);
                                if response.double_clicked() {
                                    action = Some(ShelfAction::Use(name.clone()));
                                } else if response.clicked() {
                                    action = Some(ShelfAction::Select(name.clone()));
                                }
                            });
                        }
                    });
                });
        });
        ui.separator();
        action
    }

    /// One small picture on the shelf.
    fn item(&self, ui: &mut egui::Ui, thumbnails: &mut Thumbnails, loading: bool, name: &str) -> egui::Response {
        thumbnails.show(name, loading);
        let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(SHELF_SIDE), egui::Sense::click());
        let selected = self.selected == Some(name);
        let shade = if selected || response.hovered() { 0.14 } else { 0.05 };
        ui.painter().rect_filled(rect, 4, tint(ui.visuals(), shade));
        paint_thumbnail(ui, ui.painter(), rect, thumbnails, name, self.upscale);
        if selected {
            ui.painter().rect_stroke(
                rect,
                4,
                egui::Stroke::new(1.5_f32, ui.visuals().strong_text_color()),
                egui::StrokeKind::Inside,
            );
        }
        response.on_hover_text((self.short)(name))
    }
}

/// How the keys pressed this frame move the selection in a grid: by `step` entries,
/// and whether Enter asks to use it.
#[derive(Clone, Copy, Default)]
pub(super) struct KeyMove {
    step: Moves,
    pub(super) enter: bool,
}

/// The arrow, page, Home and End keys pressed, in a form that needs the grid's width
/// to turn into a number of entries.
#[derive(Clone, Copy, Default)]
struct Moves {
    across: i64,
    rows: i64,
    ends: i64,
}

impl KeyMove {
    /// Reads and uses up the keys while the pointer is over the browser's window from
    /// last frame and no text box takes them, so nothing behind the window acts on them.
    pub(super) fn read(ctx: &egui::Context, window: Option<egui::Rect>) -> Self {
        let over = ctx
            .pointer_hover_pos()
            .zip(window)
            .is_some_and(|(pointer, window)| window.contains(pointer));
        if !over || ctx.wants_keyboard_input() {
            return Self::default();
        }
        ctx.input_mut(|input| {
            let mut count = |key| input.count_and_consume_key(egui::Modifiers::NONE, key) as i64;
            let step = Moves {
                across: count(egui::Key::ArrowRight) - count(egui::Key::ArrowLeft),
                rows: count(egui::Key::ArrowDown) - count(egui::Key::ArrowUp)
                    + PAGE_ROWS * (count(egui::Key::PageDown) - count(egui::Key::PageUp)),
                ends: count(egui::Key::End) - count(egui::Key::Home),
            };
            let enter = count(egui::Key::Enter) > 0;
            Self { step, enter }
        })
    }

    /// Whether any key moves the selection.
    pub(super) fn moves(&self) -> bool {
        self.step.across != 0 || self.step.rows != 0 || self.step.ends != 0
    }

    /// The index the selection moves to from `from` in a grid of `count` entries,
    /// `columns` across; from nothing selected it starts at the first entry.
    pub(super) fn target(&self, from: Option<usize>, count: usize, columns: usize) -> Option<usize> {
        if count == 0 {
            return None;
        }
        let Some(from) = from else { return Some(0) };
        let last = count as i64 - 1;
        let moved = match self.step.ends {
            ends if ends > 0 => last,
            ends if ends < 0 => 0,
            _ => from as i64 + self.step.across + self.step.rows * columns as i64,
        };
        Some(moved.clamp(0, last) as usize)
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

/// A checkerboard in the theme's colours, so transparent parts of a picture show.
pub(super) fn checkerboard(ui: &egui::Ui, painter: &egui::Painter, rect: egui::Rect) {
    let square_fill = tint(ui.visuals(), 0.14);
    let cell = 8.0;
    let columns = (rect.width() / cell).ceil() as usize;
    let rows = (rect.height() / cell).ceil() as usize;
    for row in 0..rows {
        for column in 0..columns {
            if (row + column) % 2 == 0 {
                let min = rect.min + egui::vec2(column as f32 * cell, row as f32 * cell);
                let square = egui::Rect::from_min_size(min, egui::Vec2::splat(cell)).intersect(rect);
                painter.rect_filled(square, 0, square_fill);
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
    fn keys_move_the_selection_across_and_down_the_grid() {
        let keys = |across, rows, ends| KeyMove {
            step: Moves { across, rows, ends },
            enter: false,
        };
        assert_eq!(keys(1, 0, 0).target(None, 10, 4), Some(0));
        assert_eq!(keys(1, 0, 0).target(Some(2), 10, 4), Some(3));
        assert_eq!(keys(0, 1, 0).target(Some(2), 10, 4), Some(6));
        assert_eq!(keys(0, 1, 0).target(Some(8), 10, 4), Some(9));
        assert_eq!(keys(0, -1, 0).target(Some(1), 10, 4), Some(0));
        assert_eq!(keys(0, 0, 1).target(Some(1), 10, 4), Some(9));
        assert_eq!(keys(0, 0, -1).target(Some(7), 10, 4), Some(0));
        assert_eq!(keys(1, 0, 0).target(Some(0), 0, 4), None);
    }

    #[test]
    fn cards_share_the_width_evenly() {
        assert_eq!(grid_columns(16.0 + 4.0 * 100.0 + 3.0 * CARD_GAP, 4), (4, 100.0));
        assert_eq!(grid_columns(1000.0, 1).0, MIN_PER_ROW);
        assert_eq!(grid_columns(1000.0, 99).0, MAX_PER_ROW);
        assert_eq!(grid_columns(10.0, 4).1, 32.0);
    }

    #[test]
    fn recent_entries_come_first_and_stars_toggle() {
        let mut recent = vec!["a".to_owned(), "b".to_owned()];
        remember(&mut recent, "b");
        assert_eq!(recent, ["b", "a"]);
        let mut starred = Vec::new();
        toggle_favourite(&mut starred, "a");
        assert_eq!(starred, ["a"]);
        toggle_favourite(&mut starred, "a");
        assert!(starred.is_empty());
    }

    #[test]
    fn a_search_takes_wildcards_and_leaves_words_out() {
        let search = Search::parse("props_*/crate?? !gib");
        assert!(search.matches("props_junk/crate01"));
        assert!(!search.matches("props_junk/crate01_gib"));
        assert!(!search.matches("props_junk/barrel01"));
    }
}
