//! The model browser: every model the game mounts, as thumbnails drawn in the background
//! while you scroll, with a preview you can turn. Laid out like the texture browser: a
//! sidebar narrows the list to a game, addon or folder, and double-clicking a model puts
//! it on the selected entities or makes it the model of new props.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icon;
use mallet_content::{MaterialLoader, ModelDetails, ModelVariant, Source, SourceKind};

use super::texture_browser::{checkerboard, elide, thousands};
use super::MalletApp;
use crate::config::{ModelBrowserView, ModelKinds, TextureBrowserOptions};
use crate::editor::{Editor, Tool};
use crate::model_render::{self, LoadedModel, Orbit, Picture};

const MUTED: Color32 = Color32::from_gray(140);
const ACCENT: Color32 = Color32::from_rgb(70, 140, 230);
const WARNING: Color32 = Color32::from_rgb(235, 160, 80);
/// Requests waiting to load; older ones are dropped, as they have scrolled out of view.
const MAX_REQUESTS: usize = 192;
const SIDEBAR_WIDTH: f32 = 220.0;
const PREVIEW_WIDTH: f32 = 320.0;
const MAX_FILTER_HISTORY: usize = 12;
/// Largest side of a thumbnail, whatever the texture browser's thumbnails use: a model
/// is drawn, not read, so a larger one costs far more than a larger texture does.
const MAX_THUMBNAIL_SIDE: u32 = 256;
/// Largest side of the textures a thumbnail's model is painted with.
const THUMBNAIL_TEXTURE_SIDE: u32 = 128;
/// Largest side of the textures the preview's model is painted with.
const PREVIEW_TEXTURE_SIDE: u32 = 512;
/// Largest side of the preview picture, in pixels.
const MAX_PREVIEW_SIDE: f32 = 1024.0;
/// The name the sidebar gives models that lie in `models/` itself.
const ROOT_FOLDER: &str = "(no folder)";
/// The body groups a thumbnail shows: the first choice of each, as a new prop has.
const THUMBNAIL_VARIANT: ModelVariant = ModelVariant { skin: 0, body: Some(0) };

/// What a model picked in the browser is for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum Pick {
    /// The model the entity tool gives new props.
    #[default]
    NewEntity,
    /// A key of these entities, such as `model`.
    Keys { ids: Vec<u32>, key: String },
}

enum Thumbnail {
    Loading,
    Ready(egui::TextureHandle),
    Failed(String),
}

/// The background thumbnail painters for one game, started with these thread and size
/// settings.
struct Loaders {
    game: PathBuf,
    threads: usize,
    side: u32,
    requests: Arc<Mutex<Vec<String>>>,
    /// Each finished picture, with whether its model is a static prop.
    results: Receiver<(String, Result<(Picture, bool), String>)>,
    stop: Arc<AtomicBool>,
}

impl Drop for Loaders {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A background check of which models can be static props, for the "Static props"
/// filter. It reads only the models the other filters let through, one after another, so
/// narrowing the list first keeps it short and it never hammers a slow disk.
struct StaticCheck {
    /// The filters the checked models passed; the check is dropped when they change.
    filters: CheckFilters,
    results: Receiver<(String, bool)>,
    /// How many models are still to check.
    left: usize,
    stop: Arc<AtomicBool>,
}

impl Drop for StaticCheck {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The filters that decide which models a [`StaticCheck`] reads.
#[derive(Clone, PartialEq, Eq)]
struct CheckFilters {
    filter: String,
    folder: Option<String>,
    used_only: bool,
    hidden_sources: Vec<usize>,
}

/// A folder of models in the sidebar's tree.
#[derive(Default)]
struct Folder {
    /// Models in this folder and every folder below it.
    count: usize,
    children: BTreeMap<String, Folder>,
}

/// The game's models with where each comes from, and counts for the sidebar.
struct Listing {
    game: PathBuf,
    /// Each model's path, such as `models/props_c17/oildrum001.mdl`, with every source
    /// that has it; the first is where it loads from.
    models: Vec<(String, Vec<usize>)>,
    sources: Vec<Source>,
    per_source: HashMap<usize, usize>,
    folders: Folder,
    /// The models that have a collision model.
    collision: HashSet<String>,
}

/// What the preview's worker is asked to draw.
#[derive(Clone, Debug, PartialEq)]
struct PreviewRequest {
    path: String,
    variant: ModelVariant,
    orbit: Orbit,
    size: [u32; 2],
    /// How many times larger the picture is drawn before it is averaged down.
    samples: u32,
}

/// What the preview shows beside the picture, read once per model and look.
struct PreviewInfo {
    details: ModelDetails,
    triangles: usize,
    /// The model's size along its forward, left and up axes.
    size: [f32; 3],
    /// Each material the shown meshes use, and whether it is missing.
    materials: Vec<(String, bool)>,
}

struct PreviewResult {
    request: PreviewRequest,
    picture: Result<Picture, String>,
    info: Option<Arc<PreviewInfo>>,
}

/// The thread that draws the preview. It draws only the newest request, so turning the
/// model never queues up pictures nobody will see.
struct PreviewWorker {
    game: PathBuf,
    slot: Arc<(Mutex<Option<PreviewRequest>>, Condvar)>,
    results: Receiver<PreviewResult>,
    stop: Arc<AtomicBool>,
}

impl Drop for PreviewWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.slot.1.notify_all();
    }
}

impl PreviewWorker {
    fn start(game: &PathBuf, loader: &MaterialLoader) -> Self {
        let slot: Arc<(Mutex<Option<PreviewRequest>>, Condvar)> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, results) = mpsc::channel();
        let (thread_slot, thread_stop, loader) = (Arc::clone(&slot), Arc::clone(&stop), loader.clone());
        thread::spawn(move || {
            type Loaded = Result<(LoadedModel, Arc<PreviewInfo>), String>;
            let mut loaded: Option<(String, ModelVariant, Loaded)> = None;
            while !thread_stop.load(Ordering::Relaxed) {
                let request = {
                    let (lock, wake) = &*thread_slot;
                    let Ok(mut waiting) = lock.lock() else { return };
                    if waiting.is_none() {
                        waiting = match wake.wait_timeout(waiting, Duration::from_millis(200)) {
                            Ok((waiting, _)) => waiting,
                            Err(_) => return,
                        };
                    }
                    waiting.take()
                };
                let Some(request) = request else { continue };
                let fresh = !loaded
                    .as_ref()
                    .is_some_and(|(path, variant, _)| *path == request.path && *variant == request.variant);
                if fresh {
                    let model =
                        guarded(|| LoadedModel::load(&loader, &request.path, request.variant, PREVIEW_TEXTURE_SIDE))
                            .map(|model| {
                                let mut materials: Vec<(String, bool)> = model
                                    .paints
                                    .iter()
                                    .map(|paint| (paint.material.clone(), paint.missing))
                                    .collect();
                                materials.sort();
                                materials.dedup();
                                let info = Arc::new(PreviewInfo {
                                    details: model.details.clone(),
                                    triangles: model.model.triangle_count(),
                                    size: model.size(),
                                    materials,
                                });
                                (model, info)
                            });
                    loaded = Some((request.path.clone(), request.variant, model));
                }
                let Some((_, _, model)) = &loaded else { continue };
                let result = match model {
                    Ok((model, info)) => {
                        let [width, height] = request.size;
                        let picture = guarded(|| {
                            Ok(model_render::render(
                                model,
                                request.orbit,
                                width,
                                height,
                                request.samples,
                            ))
                        });
                        PreviewResult {
                            request,
                            picture,
                            info: Some(Arc::clone(info)),
                        }
                    }
                    Err(error) => PreviewResult {
                        request,
                        picture: Err(error.clone()),
                        info: None,
                    },
                };
                if sender.send(result).is_err() {
                    return;
                }
            }
        });
        Self {
            game: game.clone(),
            slot,
            results,
            stop,
        }
    }

    fn ask(&self, request: PreviewRequest) {
        let (lock, wake) = &*self.slot;
        if let Ok(mut waiting) = lock.lock() {
            *waiting = Some(request);
            wake.notify_one();
        }
    }
}

/// Runs model work that must not take its thread down with it: a model the parser or the
/// painter chokes on becomes an error to show.
fn guarded<T>(work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
        .unwrap_or_else(|_| Err("this model could not be drawn".to_owned()))
}

/// The preview pane: the model clicked last, how it is turned and which look is chosen.
#[derive(Default)]
struct Preview {
    worker: Option<PreviewWorker>,
    /// The skin the preview shows.
    skin: usize,
    /// The choice made in each body group.
    body: Vec<usize>,
    /// Whether the skin or a body group was changed here, so that using the model also
    /// writes them to the entities.
    look_chosen: bool,
    orbit: Orbit,
    /// The request sent last, so the same picture is not asked for twice.
    asked: Option<PreviewRequest>,
    /// The picture, and the model it shows.
    texture: Option<(egui::TextureHandle, String)>,
    info: Option<(String, Arc<PreviewInfo>)>,
    error: Option<(String, String)>,
    /// A picture is being drawn.
    waiting: bool,
}

impl Preview {
    fn variant(&self) -> ModelVariant {
        let body = match &self.info {
            Some((_, info)) => info.details.body_value(&self.body),
            None => 0,
        };
        ModelVariant {
            skin: self.skin,
            body: Some(body),
        }
    }

    /// Forgets the look chosen for the last model, keeping the angle it was seen from.
    fn reset_for_new_model(&mut self) {
        self.skin = 0;
        self.body.clear();
        self.look_chosen = false;
        self.orbit.distance = 1.0;
        self.info = None;
        self.error = None;
    }
}

/// What the picked model will be used for, worked out each frame.
struct Target {
    /// The model in use now, lower case, to highlight it.
    current: Option<String>,
    /// The classes of the entities the model goes on.
    classes: Vec<String>,
    /// What the Use button does, such as "Use on 3 entities".
    action: String,
}

/// What the model browser remembers between frames.
#[derive(Default)]
pub(super) struct ModelBrowserState {
    pub(super) pick: Pick,
    /// Whether the window was open last frame, to tell when it opens.
    was_open: bool,
    /// The filters and layout, saved to the settings when they change.
    view: ModelBrowserView,
    /// Whether `view` has been read from the settings yet.
    view_loaded: bool,
    listing: Option<Listing>,
    list_receiver: Option<(PathBuf, Receiver<Listing>)>,
    hidden_sources: HashSet<usize>,
    loaders: Option<Loaders>,
    thumbnails: HashMap<String, Thumbnail>,
    /// Whether each model looked at so far can be a static prop: those drawn as a
    /// thumbnail or in the preview, and those checked for the "Static props" filter.
    static_props: HashMap<String, bool>,
    static_check: Option<StaticCheck>,
    /// The game's loader, for looking at models once they are listed.
    loader: Option<MaterialLoader>,
    /// The model clicked last, shown in the preview.
    selected: Option<String>,
    /// Whether the sidebar should open the way down to the chosen folder this frame.
    reveal_folder: bool,
    /// Models the map uses, cached per geometry generation.
    used: Option<(u64, Arc<BTreeSet<String>>)>,
    preview: Preview,
    focus_search: bool,
}

/// Something the browser asks for once it is drawn.
enum Command {
    /// Use a model with this look; the flag says whether the look was chosen by hand.
    Use(String, ModelVariant, bool),
    Copy(String),
}

impl MalletApp {
    /// Opens the model browser to pick a model for `pick`.
    pub(super) fn open_model_browser(&mut self, editor: &mut Editor, pick: Pick) {
        editor.model_browser_open = true;
        self.model_browser.pick = pick;
        self.model_browser.was_open = true;
        self.model_browser.focus_search = true;
        self.model_browser.select_current(editor);
    }

    pub(super) fn model_browser_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if !self.model_browser.view_loaded {
            self.model_browser.view = editor.config.model_browser_view.clone();
            self.model_browser.view_loaded = true;
        }
        if let Some(filter) = editor.model_browser_filter.take() {
            self.model_browser.view.filter = filter;
        }
        if !editor.model_browser_open {
            if self.model_browser.was_open {
                self.model_browser.was_open = false;
                self.model_browser.close();
                if self.model_browser.view != editor.config.model_browser_view {
                    editor.config.model_browser_view = self.model_browser.view.clone();
                    editor.save_config();
                }
            }
            return;
        }
        let Some(loader) = editor.materials.as_ref().map(|materials| materials.loader()) else {
            editor.model_browser_open = false;
            editor.message = "Choose a game folder first, so there are models to browse".to_owned();
            return;
        };
        if !self.model_browser.was_open {
            // Opened from the menu or by an agent, which name no target: the model goes
            // on the selected entities that have one, or else on new props.
            let ids = editor.model_targets();
            let pick = if ids.is_empty() {
                Pick::NewEntity
            } else {
                Pick::Keys {
                    ids,
                    key: "model".to_owned(),
                }
            };
            self.open_model_browser(editor, pick);
        }
        let game = editor.game_dir().map(PathBuf::from).unwrap_or_default();
        let state = &mut self.model_browser;
        let options = editor.config.texture_browser.clone();
        state.start(&game, &loader, &options);
        state.receive(ctx);
        let used = state.used_models(editor);
        let scroll_step = editor.config.view.texture_scroll.clamp(0.25, 2.0) as f32;
        let target = state.target(editor);

        let title = match &state.pick {
            Pick::NewEntity => format!("{} Models: pick the model for new props", icon::CUBE_FOCUS),
            Pick::Keys { ids, .. } if ids.len() == 1 => format!("{} Models: pick the entity's model", icon::CUBE_FOCUS),
            Pick::Keys { ids, .. } => format!("{} Models: pick the model of {} entities", icon::CUBE_FOCUS, ids.len()),
        };
        let mut open = true;
        let mut commands = Vec::new();
        egui::Window::new(title)
            .id(egui::Id::new("model_browser"))
            .open(&mut open)
            .default_size([1180.0, 740.0])
            .max_height(ctx.screen_rect().height() - 60.0)
            .min_width(720.0)
            .min_height(380.0)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                // Panels fill exactly the window, so it never grows to fit its content.
                egui::TopBottomPanel::bottom("model_browser_bar")
                    .show_inside(ui, |ui| state.bottom_bar(ui, &target, &mut commands));
                egui::SidePanel::left("model_browser_sources")
                    .resizable(true)
                    .default_width(SIDEBAR_WIDTH)
                    .width_range(160.0..=380.0)
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("model_sidebar")
                            .auto_shrink([false, false])
                            .show(ui, |ui| state.sidebar(ui));
                    });
                if state.view.show_preview {
                    egui::SidePanel::right("model_browser_preview")
                        .resizable(true)
                        .default_width(PREVIEW_WIDTH)
                        .width_range(240.0..=640.0)
                        .show_inside(ui, |ui| state.preview_pane(ui, &game, &loader, &target, &mut commands));
                }
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    let size = [ui.available_width(), ui.available_height()];
                    state.grid(ui, &used, &target, size, scroll_step, &mut commands);
                });
            });
        state.trim(options.cache_size());
        state.save_view(ctx, editor);
        for command in commands {
            match command {
                Command::Use(model, variant, look_chosen) => {
                    let pick = self.model_browser.pick.clone();
                    let collision = self
                        .model_browser
                        .listing
                        .as_ref()
                        .is_some_and(|listing| listing.collision.contains(&model));
                    let is_static = self.model_browser.static_props.get(&model).copied();
                    let result = use_model(editor, &pick, &model, variant, look_chosen, is_static, collision);
                    if let Err(error) = result {
                        editor.message = error;
                    }
                    editor.model_browser_open = false;
                }
                Command::Copy(model) => {
                    editor.message = format!("Copied {model}");
                    editor.system_clipboard = Some(model);
                }
            }
        }
        if !open {
            editor.model_browser_open = false;
        }
    }
}

/// Puts a picked model where `pick` says. On entities it sets the key, and the skin and
/// body groups too when they were chosen in the preview and the class has them. For new
/// props it becomes the entity tool's model, and the tool switches to a prop class that
/// suits the model when its class takes none.
fn use_model(
    editor: &mut Editor,
    pick: &Pick,
    model: &str,
    variant: ModelVariant,
    look_chosen: bool,
    is_static: Option<bool>,
    collision: bool,
) -> Result<(), String> {
    match pick {
        Pick::Keys { ids, key } => {
            editor.set_key_on(ids, key, model)?;
            if look_chosen && key.eq_ignore_ascii_case("model") {
                for (look, value) in [("skin", variant.skin as u32), ("body", variant.body.unwrap_or(0))] {
                    let takers: Vec<u32> = ids
                        .iter()
                        .copied()
                        .filter(|id| {
                            editor.entity(*id).is_some_and(|entity| {
                                entity.get(look).is_some()
                                    || editor
                                        .entity_class(entity.classname())
                                        .is_some_and(|class| class.property(look).is_some())
                            })
                        })
                        .collect();
                    if !takers.is_empty() {
                        editor.set_key_on(&takers, look, &value.to_string())?;
                    }
                }
            }
            editor.message = match ids.len() {
                1 => format!("Set the model to {model}"),
                count => format!("Set the model of {count} entities to {model}"),
            };
        }
        Pick::NewEntity => {
            editor.prop_model = model.to_owned();
            if !editor.class_takes_model(&editor.entity_class) {
                editor.entity_class = match (is_static, collision) {
                    (Some(true), _) => "prop_static",
                    (_, true) => "prop_physics",
                    _ => "prop_dynamic",
                }
                .to_owned();
            }
            editor.set_tool(Tool::Entity);
            editor.message = format!("The entity tool now places {} with {model}", editor.entity_class);
        }
    }
    Ok(())
}

impl ModelBrowserState {
    /// Lists the game's models, starts looking at them for the filters, and starts the
    /// thumbnail painters, once per game and again when their settings change.
    fn start(&mut self, game: &PathBuf, loader: &MaterialLoader, options: &TextureBrowserOptions) {
        self.loader = Some(loader.clone());
        let listed = self.listing.as_ref().is_some_and(|listing| &listing.game == game);
        let listing = self.list_receiver.as_ref().is_some_and(|(listing, _)| listing == game);
        if !listed && !listing {
            let (sender, receiver) = mpsc::channel();
            let (loader, listing_game) = (loader.clone(), game.clone());
            thread::spawn(move || {
                let (models, collision) = loader.list_models_and_collision();
                let mut per_source: HashMap<usize, usize> = HashMap::new();
                let mut folders = Folder::default();
                for (path, sources) in &models {
                    for source in sources {
                        *per_source.entry(*source).or_default() += 1;
                    }
                    let mut folder = &mut folders;
                    folder.count += 1;
                    let name = short_name(path);
                    let parts: Vec<&str> = match name.rsplit_once('/') {
                        Some((directory, _)) => directory.split('/').collect(),
                        None => vec![ROOT_FOLDER],
                    };
                    for part in parts {
                        folder = folder.children.entry(part.to_owned()).or_default();
                        folder.count += 1;
                    }
                }
                let _ = sender.send(Listing {
                    game: listing_game,
                    sources: loader.sources(),
                    collision,
                    models,
                    per_source,
                    folders,
                });
            });
            self.list_receiver = Some((game.clone(), receiver));
            self.listing = None;
            self.static_check = None;
            self.thumbnails.clear();
            self.static_props.clear();
        }
        let threads = options.thread_count();
        let side = options.thumbnail_side().min(MAX_THUMBNAIL_SIDE);
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
                            Some(path) => {
                                let picture = guarded(|| {
                                    let model =
                                        LoadedModel::load(&loader, &path, THUMBNAIL_VARIANT, THUMBNAIL_TEXTURE_SIDE)?;
                                    let picture = model_render::render(&model, Orbit::default(), side, side, 2);
                                    Ok((picture, model.details.static_prop))
                                });
                                if sender.send((path, picture)).is_err() {
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

    /// Stops the background work and frees the pictures while the window is closed. The
    /// listing stays, so opening the browser again is instant.
    fn close(&mut self) {
        self.loaders = None;
        self.static_check = None;
        self.thumbnails.clear();
        self.preview.worker = None;
        self.preview.texture = None;
        self.preview.asked = None;
        self.preview.waiting = false;
    }

    /// Shows the model in use now in the preview, when the browser opens.
    fn select_current(&mut self, editor: &Editor) {
        let current = match &self.pick {
            Pick::NewEntity => Some(editor.prop_model.clone()),
            Pick::Keys { ids, key } => ids
                .first()
                .and_then(|id| editor.entity(*id))
                .and_then(|entity| entity.get(key))
                .map(str::to_owned),
        };
        let current = current
            .map(|model| model.replace('\\', "/").to_ascii_lowercase())
            .filter(|model| model.ends_with(".mdl"));
        if current.is_some() && current != self.selected {
            self.selected = current;
            self.preview.reset_for_new_model();
        }
    }

    /// What the picked model will be used for.
    fn target(&self, editor: &Editor) -> Target {
        match &self.pick {
            Pick::NewEntity => Target {
                current: Some(editor.prop_model.to_ascii_lowercase()).filter(|model| !model.is_empty()),
                classes: vec![editor.entity_class.to_ascii_lowercase()],
                action: "Use for new props".to_owned(),
            },
            Pick::Keys { ids, key } => {
                let entities: Vec<_> = ids.iter().filter_map(|id| editor.entity(*id)).collect();
                let mut models: Vec<String> = entities
                    .iter()
                    .filter_map(|entity| entity.get(key))
                    .map(|model| model.replace('\\', "/").to_ascii_lowercase())
                    .collect();
                models.sort();
                models.dedup();
                let mut classes: Vec<String> = entities
                    .iter()
                    .map(|entity| entity.classname().to_ascii_lowercase())
                    .collect();
                classes.sort();
                classes.dedup();
                Target {
                    current: (models.len() == 1).then(|| models.remove(0)),
                    classes,
                    action: match ids.len() {
                        1 => "Use on the entity".to_owned(),
                        count => format!("Use on {count} entities"),
                    },
                }
            }
        }
    }

    /// Takes in the listing, what the index found out and finished thumbnails.
    fn receive(&mut self, ctx: &egui::Context) {
        if let Some((_, receiver)) = &self.list_receiver {
            match receiver.try_recv() {
                Ok(listing) => {
                    self.restore_view(&listing);
                    self.listing = Some(listing);
                    self.list_receiver = None;
                }
                Err(_) => ctx.request_repaint_after(Duration::from_millis(50)),
            }
        }
        if let Some(check) = &mut self.static_check {
            let mut finished = false;
            loop {
                match check.results.try_recv() {
                    Ok((path, is_static)) => {
                        self.static_props.insert(path, is_static);
                        check.left = check.left.saturating_sub(1);
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        finished = true;
                        break;
                    }
                }
            }
            if finished {
                self.static_check = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
        let mut received = false;
        if let Some(loaders) = &self.loaders {
            while let Ok((path, picture)) = loaders.results.try_recv() {
                received = true;
                let thumbnail = match picture {
                    Ok((picture, is_static)) => {
                        self.static_props.insert(path.clone(), is_static);
                        let options = egui::TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear));
                        let texture = ctx.load_texture(format!("model:{path}"), color_image(&picture), options);
                        Thumbnail::Ready(texture)
                    }
                    Err(error) => Thumbnail::Failed(error),
                };
                // A thumbnail nobody waits for any more was dropped to save memory.
                if self.thumbnails.contains_key(&path) {
                    self.thumbnails.insert(path, thumbnail);
                }
            }
        }
        let waiting = self
            .thumbnails
            .values()
            .any(|thumbnail| matches!(thumbnail, Thumbnail::Loading));
        if received || waiting {
            ctx.request_repaint_after(Duration::from_millis(30));
        }
        self.receive_preview(ctx);
    }

    /// Takes in the pictures the preview's worker has drawn.
    fn receive_preview(&mut self, ctx: &egui::Context) {
        let preview = &mut self.preview;
        let Some(worker) = &preview.worker else { return };
        while let Ok(result) = worker.results.try_recv() {
            let path = result.request.path.clone();
            if preview.asked.as_ref() == Some(&result.request) {
                preview.waiting = false;
            }
            if self.selected.as_ref() != Some(&path) {
                continue;
            }
            if let Some(info) = result.info {
                self.static_props.insert(path.clone(), info.details.static_prop);
                preview.body.resize(info.details.body_groups.len(), 0);
                preview.info = Some((path.clone(), info));
            }
            match result.picture {
                Ok(picture) => {
                    let image = color_image(&picture);
                    match &mut preview.texture {
                        Some((texture, shown)) => {
                            texture.set(image, egui::TextureOptions::LINEAR);
                            *shown = path;
                        }
                        None => {
                            let texture = ctx.load_texture("model_preview", image, egui::TextureOptions::LINEAR);
                            preview.texture = Some((texture, path));
                        }
                    }
                    preview.error = None;
                }
                Err(error) => preview.error = Some((path, error)),
            }
        }
        if preview.waiting {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    /// While the "Static props" filter is on, finds out in the background which of the
    /// models the other filters let through are static props, when that is not known yet.
    /// A check for other filters than the ones set now is dropped.
    fn check_static_props(&mut self, used: &BTreeSet<String>) {
        if self.view.kinds != ModelKinds::Static {
            self.static_check = None;
            return;
        }
        let mut hidden_sources: Vec<usize> = self.hidden_sources.iter().copied().collect();
        hidden_sources.sort_unstable();
        let filters = CheckFilters {
            filter: self.view.filter.clone(),
            folder: self.view.folder.clone(),
            used_only: self.view.used_only,
            hidden_sources,
        };
        if let Some(check) = &self.static_check {
            if check.filters == filters {
                return;
            }
        }
        self.static_check = None;
        let (Some(loader), Some(listing)) = (self.loader.clone(), &self.listing) else {
            return;
        };
        let search = Search::parse(&self.view.filter);
        let unknown: Vec<String> = listing
            .models
            .iter()
            .filter(|(path, sources)| self.passes(&search, used, path, sources))
            .filter(|(path, _)| !self.static_props.contains_key(path))
            .map(|(path, _)| path.clone())
            .collect();
        if unknown.is_empty() {
            return;
        }
        let (sender, results) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let (left, thread_stop) = (unknown.len(), Arc::clone(&stop));
        thread::spawn(move || {
            for path in unknown {
                if thread_stop.load(Ordering::Relaxed) {
                    return;
                }
                let is_static = loader.model_is_static(&path).unwrap_or(false);
                if sender.send((path, is_static)).is_err() {
                    return;
                }
            }
        });
        self.static_check = Some(StaticCheck {
            filters,
            results,
            left,
            stop,
        });
    }

    /// Whether a model passes the source, folder, "only used" and search filters: every
    /// filter but the kind of prop.
    fn passes(&self, search: &Search, used: &BTreeSet<String>, path: &str, sources: &[usize]) -> bool {
        sources.iter().any(|source| !self.hidden_sources.contains(source))
            && self
                .view
                .folder
                .as_deref()
                .is_none_or(|folder| in_folder(short_name(path), folder))
            && (!self.view.used_only || used.contains(path))
            && search.matches(short_name(path))
    }

    /// The models the map's entities use, recounted when the map changes.
    fn used_models(&mut self, editor: &Editor) -> Arc<BTreeSet<String>> {
        let generation = editor.scene.geometry_generation;
        if let Some((cached, used)) = &self.used {
            if *cached == generation {
                return Arc::clone(used);
            }
        }
        let used: BTreeSet<String> = editor
            .document
            .as_ref()
            .into_iter()
            .flat_map(|document| document.vmf.entities())
            .filter_map(|entity| entity.get("model"))
            .map(|model| model.replace('\\', "/").to_ascii_lowercase())
            .filter(|model| model.ends_with(".mdl"))
            .collect();
        let used = Arc::new(used);
        self.used = Some((generation, Arc::clone(&used)));
        used
    }

    /// The sidebar: sources grouped by kind, then the folder tree, each with a count.
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        let Some(listing) = &self.listing else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Listing models...");
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
                .id_salt(("model_source_group", label))
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
        ui.horizontal(|ui| {
            if ui.selectable_label(self.view.folder.is_none(), "All folders").clicked() {
                self.view.folder = None;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(thousands(listing.folders.count)).small().color(MUTED));
            });
        });
        folder_tree(ui, &listing.folders, "", &mut self.view.folder, self.reveal_folder);
        self.reveal_folder = false;
    }

    /// Indices into the listing of the models passing every filter, in name order. A model
    /// shows while any source that has it is ticked.
    fn filtered(&self, used: &BTreeSet<String>) -> Vec<usize> {
        let Some(listing) = &self.listing else {
            return Vec::new();
        };
        let search = Search::parse(&self.view.filter);
        listing
            .models
            .iter()
            .enumerate()
            .filter(|(_, (path, _))| match self.view.kinds {
                ModelKinds::All => true,
                ModelKinds::Static => self.static_props.get(path.as_str()).copied().unwrap_or(false),
                ModelKinds::Physics => listing.collision.contains(path.as_str()),
            })
            .filter(|(_, (path, sources))| self.passes(&search, used, path, sources))
            .map(|(index, _)| index)
            .collect()
    }

    /// The thumbnail grid. Cards share the width evenly, `per_row` to a row.
    fn grid(
        &mut self,
        ui: &mut egui::Ui,
        used: &BTreeSet<String>,
        target: &Target,
        [width, height]: [f32; 2],
        scroll_step: f32,
        commands: &mut Vec<Command>,
    ) {
        if self.listing.is_none() {
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Listing the game's models...");
            });
            return;
        }
        self.check_static_props(used);
        let shown = self.filtered(used);
        if shown.is_empty() {
            ui.add_space(20.0);
            if self.static_check.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Looking for static props...");
                });
            } else {
                ui.label(
                    RichText::new("No models match. Clear the search, choose All folders or turn on more sources.")
                        .color(MUTED),
                );
            }
            return;
        }
        let gap = 6.0;
        let columns = self.view.per_row.clamp(2, 14);
        let card = ((width - 16.0 - gap * (columns as f32 - 1.0)) / columns as f32).max(32.0);
        let row_height = card + 22.0 + gap;
        let rows = shown.len().div_ceil(columns);
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
        // A solid bar that is always shown, with a handle long enough to grab, as in the
        // texture browser. It fits in the 16 points the cards leave free on the right.
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
            .id_salt("model_grid")
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
                        for index in shown.iter().skip(row * columns).take(columns) {
                            let Some((path, sources)) = self.listing.as_ref().and_then(|listing| {
                                let (path, sources) = listing.models.get(*index)?;
                                let names: Vec<String> = sources
                                    .iter()
                                    .filter_map(|source| listing.sources.get(*source))
                                    .map(|source| source.name.clone())
                                    .collect();
                                Some((path.clone(), names))
                            }) else {
                                continue;
                            };
                            let current = target.current.as_deref() == Some(path.as_str());
                            let response = self.card(ui, &path, &sources, card, current);
                            if response.clicked() && self.selected.as_ref() != Some(&path) {
                                self.selected = Some(path.clone());
                                self.preview.reset_for_new_model();
                            }
                            if response.double_clicked() {
                                commands.push(Command::Use(path.clone(), ModelVariant::default(), false));
                            }
                            response.context_menu(|ui| {
                                if ui.button(&target.action).clicked() {
                                    commands.push(Command::Use(path.clone(), ModelVariant::default(), false));
                                    ui.close_menu();
                                }
                                if ui.button("Copy path").clicked() {
                                    commands.push(Command::Copy(path.clone()));
                                    ui.close_menu();
                                }
                                if ui.button("Show its folder").clicked() {
                                    self.view.folder = short_name(&path)
                                        .rsplit_once('/')
                                        .map(|(folder, _)| folder.to_owned())
                                        .or_else(|| Some(ROOT_FOLDER.to_owned()));
                                    self.reveal_folder = true;
                                    ui.close_menu();
                                }
                            });
                        }
                    });
                    ui.add_space(gap);
                }
            });
    }

    /// One card: the model's thumbnail and its name under it.
    fn card(&mut self, ui: &mut egui::Ui, path: &str, sources: &[String], size: f32, current: bool) -> egui::Response {
        self.request(path);
        let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size + 22.0), egui::Sense::click());
        let image_rect = egui::Rect::from_min_size(rect.min, egui::Vec2::splat(size));
        let selected = self.selected.as_deref() == Some(path);
        let painter = ui.painter();
        let shade = match (selected, response.hovered()) {
            (true, _) => 54,
            (false, true) => 44,
            (false, false) => 30,
        };
        painter.rect_filled(rect, 4, Color32::from_gray(shade));
        let mut failure = None;
        match self.thumbnails.get(path) {
            Some(Thumbnail::Ready(texture)) => {
                let full = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
                painter.image(texture.id(), image_rect.shrink(3.0), full, Color32::WHITE);
            }
            Some(Thumbnail::Failed(error)) => {
                failure = Some(error.clone());
                painter.text(
                    image_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    icon::WARNING,
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
        let name = short_name(path);
        let short = name.rsplit('/').next().unwrap_or(name);
        painter.text(
            label.center(),
            egui::Align2::CENTER_CENTER,
            elide(short, ((size - 8.0) / 6.0) as usize),
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
        let mut details = vec![path.to_owned()];
        if let Some((first, others)) = sources.split_first() {
            details.push(format!("loads from {first}"));
            if !others.is_empty() {
                details.push(format!("also in {}", others.join(", ")));
            }
        }
        let mut traits = Vec::new();
        match self.static_props.get(path) {
            Some(true) => traits.push("static prop"),
            Some(false) => traits.push("not a static prop"),
            None => {}
        }
        if self
            .listing
            .as_ref()
            .is_some_and(|listing| listing.collision.contains(path))
        {
            traits.push("has a collision model");
        }
        if !traits.is_empty() {
            details.push(traits.join(", "));
        }
        details.extend(failure);
        response.on_hover_text(details.join("\n"))
    }

    /// The preview: the model clicked last, turned by dragging, with its skins and body
    /// groups to try and what kind of prop it can be.
    fn preview_pane(
        &mut self,
        ui: &mut egui::Ui,
        game: &PathBuf,
        loader: &MaterialLoader,
        target: &Target,
        commands: &mut Vec<Command>,
    ) {
        let Some(path) = self.selected.clone() else {
            ui.add_space(20.0);
            ui.label(RichText::new("Click a model to look at it here.").color(MUTED));
            return;
        };
        let collision = self
            .listing
            .as_ref()
            .is_some_and(|listing| listing.collision.contains(&path));
        let is_static = self.static_props.get(&path).copied();
        let preview = &mut self.preview;
        if preview.worker.as_ref().is_none_or(|worker| &worker.game != game) {
            preview.worker = Some(PreviewWorker::start(game, loader));
            preview.asked = None;
        }

        let side = ui.available_width();
        let height = side.min((ui.available_height() * 0.5).max(160.0));
        let (rect, response) = ui.allocate_exact_size(egui::vec2(side, height), egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4, Color32::from_gray(24));
        if self.view.show_backdrop {
            checkerboard(&painter, rect.shrink(2.0));
        }
        if response.dragged() {
            let delta = response.drag_delta();
            preview.orbit.yaw -= delta.x * 0.6;
            preview.orbit.pitch += delta.y * 0.6;
        }
        if response.hovered() {
            let zoom = ui.input(|input| input.smooth_scroll_delta.y);
            if zoom != 0.0 {
                preview.orbit.distance *= (-zoom * 0.0025).exp();
                ui.ctx().input_mut(|input| {
                    input.smooth_scroll_delta = egui::Vec2::ZERO;
                    input.raw_scroll_delta = egui::Vec2::ZERO;
                });
            }
        }
        if response.double_clicked() {
            preview.orbit = Orbit::default();
        }
        preview.orbit = preview.orbit.clamped();

        let scale = ui.ctx().pixels_per_point();
        let fit = (MAX_PREVIEW_SIDE / (rect.width().max(rect.height()) * scale)).min(1.0);
        let request = PreviewRequest {
            path: path.clone(),
            variant: preview.variant(),
            orbit: preview.orbit,
            size: [
                (rect.width() * scale * fit).round().max(16.0) as u32,
                (rect.height() * scale * fit).round().max(16.0) as u32,
            ],
            samples: if response.dragged() { 1 } else { 2 },
        };
        if preview.asked.as_ref() != Some(&request) {
            if let Some(worker) = &preview.worker {
                worker.ask(request.clone());
            }
            preview.asked = Some(request);
            preview.waiting = true;
            ui.ctx().request_repaint();
        }
        let failed = preview.error.as_ref().filter(|(model, _)| *model == path);
        match (&preview.texture, failed) {
            (_, Some((_, error))) => {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("{}\n{}", icon::WARNING, elide(error, 60)),
                    egui::FontId::proportional(13.0),
                    MUTED,
                );
            }
            (Some((texture, shown)), None) if *shown == path => {
                let full = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
                painter.image(texture.id(), rect, full, Color32::WHITE);
            }
            _ => {
                ui.put(
                    egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(24.0)),
                    egui::Spinner::new(),
                );
            }
        }
        response.on_hover_text("Drag to turn, scroll to zoom, double-click to reset");

        ui.add_space(4.0);
        ui.label(RichText::new(short_name(&path)).monospace().strong());
        ui.horizontal(|ui| {
            if ui
                .button(format!("{} {}", icon::CHECK, target.action))
                .on_hover_text("Double-clicking a model does the same")
                .clicked()
            {
                commands.push(Command::Use(path.clone(), preview.variant(), preview.look_chosen));
            }
            if ui.button(icon::COPY).on_hover_text("Copy the model's path").clicked() {
                commands.push(Command::Copy(path.clone()));
            }
            if ui
                .button(icon::FOLDER_OPEN)
                .on_hover_text("Show the model's file in its folder, when it is a loose file")
                .clicked()
            {
                reveal(loader, &path);
            }
        });
        for warning in warnings(&target.classes, is_static, collision) {
            ui.label(RichText::new(format!("{} {warning}", icon::WARNING)).color(WARNING));
        }
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("model_preview_details")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let Some(info) = preview
                    .info
                    .as_ref()
                    .filter(|(model, _)| *model == path)
                    .map(|(_, info)| Arc::clone(info))
                else {
                    return;
                };
                let details = &info.details;
                if details.skins > 1 {
                    ui.horizontal(|ui| {
                        ui.label("Skin");
                        let last = details.skins - 1;
                        if ui.add(egui::Slider::new(&mut preview.skin, 0..=last)).changed() {
                            preview.look_chosen = true;
                        }
                    });
                }
                let choosable = details
                    .body_groups
                    .iter()
                    .filter(|group| group.choices.len() > 1)
                    .count();
                if choosable > 0 {
                    preview.body.resize(details.body_groups.len(), 0);
                    for (number, group) in details.body_groups.iter().enumerate() {
                        if group.choices.len() < 2 {
                            continue;
                        }
                        ui.horizontal(|ui| {
                            ui.label(format!("Body group {}", number + 1));
                            let chosen = preview.body[number].min(group.choices.len() - 1);
                            egui::ComboBox::from_id_salt(("model_body_group", number))
                                .selected_text(choice_name(&group.choices[chosen], chosen))
                                .show_ui(ui, |ui| {
                                    for (index, choice) in group.choices.iter().enumerate() {
                                        let name = choice_name(choice, index);
                                        if ui.selectable_label(index == chosen, name).clicked() && index != chosen {
                                            preview.body[number] = index;
                                            preview.look_chosen = true;
                                        }
                                    }
                                });
                        });
                    }
                    ui.label(
                        RichText::new(format!("body {}", details.body_value(&preview.body)))
                            .small()
                            .color(MUTED),
                    );
                }
                egui::Grid::new("model_details")
                    .num_columns(2)
                    .spacing([12.0, 3.0])
                    .show(ui, |ui| {
                        let mut row = |name: &str, value: String| {
                            ui.label(RichText::new(name).color(MUTED));
                            ui.label(value);
                            ui.end_row();
                        };
                        row("Triangles", thousands(info.triangles));
                        row("Vertices", thousands(details.vertices));
                        row(
                            "Size",
                            format!("{:.0} x {:.0} x {:.0} units", info.size[0], info.size[1], info.size[2]),
                        );
                        row("Static prop", if details.static_prop { "yes" } else { "no" }.to_owned());
                        row("Collision model", if collision { "yes" } else { "no" }.to_owned());
                        if !details.surface_prop.is_empty() {
                            row("Surface", details.surface_prop.clone());
                        }
                        row("Skins", details.skins.to_string());
                        row("Bones", details.bones.to_string());
                        row("Sequences", details.sequences.len().to_string());
                    });
                ui.add_space(6.0);
                egui::CollapsingHeader::new(format!("Materials ({})", info.materials.len()))
                    .id_salt("model_materials")
                    .default_open(true)
                    .show(ui, |ui| {
                        for (material, missing) in &info.materials {
                            let text = RichText::new(material).small();
                            if *missing {
                                ui.label(text.color(WARNING))
                                    .on_hover_text("This material or its texture is missing");
                            } else {
                                ui.label(text);
                            }
                        }
                    });
                if !details.sequences.is_empty() {
                    egui::CollapsingHeader::new(format!("Sequences ({})", details.sequences.len()))
                        .id_salt("model_sequences")
                        .show(ui, |ui| {
                            for sequence in &details.sequences {
                                ui.label(RichText::new(sequence).small());
                            }
                        });
                }
            });
    }

    /// The controls under the list: size, search, what to show, and the selected model.
    fn bottom_bar(&mut self, ui: &mut egui::Ui, target: &Target, commands: &mut Vec<Command>) {
        let selected = self.selected.clone();
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::Slider::new(&mut self.view.per_row, 2..=14).text("per row"))
                .on_hover_text("How many models fit across; they grow and shrink with the window");
            ui.separator();
            ui.label("Filter");
            let search = ui
                .add(
                    egui::TextEdit::singleline(&mut self.view.filter)
                        .hint_text("such as oildrum !gib")
                        .desired_width(190.0),
                )
                .on_hover_text(
                    "Every word must be in the model's path. Start a word with ! to leave out models that have \
                     it. * stands for any text and ? for one character, such as props_*/crate??",
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
            egui::ComboBox::from_id_salt("model_kinds")
                .selected_text(kinds_label(self.view.kinds))
                .show_ui(ui, |ui| {
                    for kinds in [ModelKinds::All, ModelKinds::Static, ModelKinds::Physics] {
                        ui.selectable_value(&mut self.view.kinds, kinds, kinds_label(kinds));
                    }
                })
                .response
                .on_hover_text(
                    "Static props are models prop_static can show. Physics props have a collision model, which \
                     prop_physics needs. Finding static props reads every listed model's file, so choose a \
                     folder or search first to keep it quick.",
                );
            ui.checkbox(&mut self.view.used_only, "Only used models")
                .on_hover_text("Show only the models the map's entities use");
            ui.separator();
            ui.toggle_value(&mut self.view.show_preview, format!("{} Preview", icon::CUBE_FOCUS))
                .on_hover_text("Show the clicked model in a pane you can turn it in");
            ui.toggle_value(&mut self.view.show_backdrop, "Backdrop")
                .on_hover_text("Draw a checkerboard behind the preview, so see-through parts show");
            if ui.button(format!("{} Reload", icon::ARROWS_CLOCKWISE)).clicked() {
                self.listing = None;
                self.list_receiver = None;
                self.loaders = None;
                self.static_check = None;
                self.preview.worker = None;
                self.preview.asked = None;
            }
        });
        ui.horizontal(|ui| {
            let status = match (&self.listing, &self.static_check) {
                (Some(listing), Some(check)) => format!(
                    "{} models; checking {} of them for static props",
                    thousands(listing.models.len()),
                    thousands(check.left)
                ),
                (Some(listing), None) => format!("{} models", thousands(listing.models.len())),
                (None, _) => String::new(),
            };
            ui.label(RichText::new(status).small().color(MUTED));
            if let Some(path) = &selected {
                ui.separator();
                ui.label(RichText::new(path).monospace().small());
                if !self.view.show_preview && ui.small_button(&target.action).clicked() {
                    commands.push(Command::Use(path.clone(), ModelVariant::default(), false));
                }
            }
        });
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
        let known = |folder: &String| {
            let mut node = &listing.folders;
            folder.split('/').all(|part| match node.children.get(part) {
                Some(child) => {
                    node = child;
                    true
                }
                None => false,
            })
        };
        if self.view.folder.as_ref().is_some_and(|folder| !known(folder)) {
            self.view.folder = None;
        }
        self.reveal_folder = true;
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
        if self.view == editor.config.model_browser_view || ctx.wants_keyboard_input() {
            return;
        }
        editor.config.model_browser_view = self.view.clone();
        editor.save_config();
    }

    /// Asks for a thumbnail if it is not drawn or being drawn.
    fn request(&mut self, path: &str) {
        if self.thumbnails.contains_key(path) {
            return;
        }
        let Some(loaders) = &self.loaders else { return };
        if let Ok(mut queue) = loaders.requests.lock() {
            queue.push(path.to_owned());
            let excess = queue.len().saturating_sub(MAX_REQUESTS);
            if excess > 0 {
                for dropped in queue.drain(..excess) {
                    self.thumbnails.remove(&dropped);
                }
            }
        }
        self.thumbnails.insert(path.to_owned(), Thumbnail::Loading);
    }

    /// Drops every thumbnail when there are more than `limit`, so they are drawn again as
    /// needed and browsing never fills GPU memory.
    fn trim(&mut self, limit: usize) {
        if self.thumbnails.len() > limit {
            self.thumbnails
                .retain(|_, thumbnail| matches!(thumbnail, Thumbnail::Loading));
        }
    }
}

/// The sidebar's folder tree under `folder`, whose own path is `prefix`. Clicking a
/// folder's name narrows the list to it and everything below; its arrow opens it. With
/// `reveal`, the folders on the way down to the chosen one open.
fn folder_tree(ui: &mut egui::Ui, folder: &Folder, prefix: &str, selected: &mut Option<String>, reveal: bool) {
    for (name, child) in &folder.children {
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
        let id = ui.make_persistent_id(("model_folder", &path));
        let holds_selection = selected
            .as_deref()
            .is_some_and(|chosen| chosen.starts_with(&format!("{path}/")));
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
        if reveal && holds_selection {
            state.set_open(true);
        }
        state
            .show_header(ui, |ui| label(ui, selected))
            .body(|ui| folder_tree(ui, child, &path, selected, reveal));
    }
}

/// A model's path without `models/` and `.mdl`, as the browser names it.
fn short_name(path: &str) -> &str {
    let name = path.strip_prefix("models/").unwrap_or(path);
    name.strip_suffix(".mdl").unwrap_or(name)
}

/// Whether a model, by its short name, lies in a sidebar folder or below it.
fn in_folder(name: &str, folder: &str) -> bool {
    match name.rsplit_once('/') {
        None => folder == ROOT_FOLDER,
        Some((directory, _)) => {
            directory == folder || directory.strip_prefix(folder).is_some_and(|rest| rest.starts_with('/'))
        }
    }
}

/// The name a body group's choice shows: the sub-model's name, or a number for a
/// nameless one.
fn choice_name(name: &str, index: usize) -> String {
    let name = name.trim();
    if name.is_empty() {
        format!("{index}")
    } else {
        format!("{index}: {}", name.trim_end_matches(".smd"))
    }
}

fn kinds_label(kinds: ModelKinds) -> &'static str {
    match kinds {
        ModelKinds::All => "All models",
        ModelKinds::Static => "Static props",
        ModelKinds::Physics => "Physics props",
    }
}

/// What will go wrong in game when a model is used on entities of these classes.
fn warnings(classes: &[String], is_static: Option<bool>, collision: bool) -> Vec<&'static str> {
    let mut warnings = Vec::new();
    if is_static == Some(false) && classes.iter().any(|class| class == "prop_static") {
        warnings.push("Not compiled as a static prop, so prop_static will not show it in game.");
    }
    let physical = |class: &String| class.starts_with("prop_physics") || class == "prop_ragdoll";
    if !collision && classes.iter().any(physical) {
        warnings.push("No collision model, so the game removes it as a physics prop.");
    }
    warnings
}

/// The words of a search: every plain word must be in a name, and no word that started
/// with `!` may be.
struct Search {
    wanted: Vec<String>,
    unwanted: Vec<String>,
}

impl Search {
    fn parse(text: &str) -> Self {
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

    fn matches(&self, name: &str) -> bool {
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

fn color_image(picture: &Picture) -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied([picture.width as usize, picture.height as usize], &picture.rgba)
}

/// Shows a model's `.mdl` in the file manager, when it is a loose file rather than in a
/// VPK: selected in Explorer on Windows, and its folder opened elsewhere.
fn reveal(loader: &MaterialLoader, model: &str) {
    let Some(path) = loader.locate_model(model) else {
        return;
    };
    #[cfg(windows)]
    let mut command = {
        let mut command = std::process::Command::new("explorer");
        command.arg(format!("/select,{}", path.display()));
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(path.parent().unwrap_or(&path));
        command
    };
    let _ = command.spawn();
}
