//! The model browser: every model the game mounts, as thumbnails drawn in the background
//! while you scroll, with a preview you can turn. Laid out like the texture browser: a
//! sidebar narrows the list to a game, addon or folder, and double-clicking a model puts
//! it on the selected entities or makes it the model of new props. The parts it shares
//! with the texture browser live in [`super::asset_browser`].

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icon;
use mallet_content::{MaterialLoader, ModelDetails, ModelVariant, PropData, PropTraits, PROP_CLASSES};

use super::asset_browser::{
    self, card, checkerboard, chip, elide, empty_state, grid_columns, grid_lines, group_by_folder, hidden_paths,
    in_folder, muted, paint_thumbnail, path_bar, per_row_slider, remember, restore_hidden, source_lines, status_line,
    thousands, toggle_chip, toggle_favourite, upload, warning, Catalog, FolderPick, Grid, KeyMove, Listing, Loaders,
    Marks, Narrowing, Search, SearchBox, Shelf, ShelfAction, Shown, Thumbnail, Thumbnails, ROOT_FOLDER,
};
use super::MalletApp;
use crate::config::{ModelBrowserView, ModelKinds, TextureBrowserOptions};
use crate::editor::{Editor, Tool};
use crate::model_render::{self, LoadedModel, Orbit, Picture};

/// Requests waiting to load; older ones are dropped, as they have scrolled out of view.
const MAX_REQUESTS: usize = 192;
const SIDEBAR_WIDTH: f32 = 220.0;
const PREVIEW_WIDTH: f32 = 320.0;
/// Largest side of a thumbnail, whatever the texture browser's thumbnails use: a model
/// is drawn, not read, so a larger one costs far more than a larger texture does.
const MAX_THUMBNAIL_SIDE: u32 = 256;
/// Largest side of the textures a thumbnail's model is painted with.
const THUMBNAIL_TEXTURE_SIDE: u32 = 128;
/// Largest side of the textures the preview's model is painted with.
const PREVIEW_TEXTURE_SIDE: u32 = 512;
/// Largest side of the preview picture, in pixels.
const MAX_PREVIEW_SIDE: f32 = 1024.0;
/// The body groups a thumbnail shows: the first choice of each, as a new prop has.
const THUMBNAIL_VARIANT: ModelVariant = ModelVariant { skin: 0, body: Some(0) };

/// A finished thumbnail: the picture, with what decides which prop classes take its
/// model, or why it could not be drawn.
type Drawn = Result<(Picture, PropTraits), String>;

/// The game's models, each with every source that has it, and the models that have a
/// collision model.
type ModelListing = Listing<HashSet<String>>;

/// What a model picked in the browser is for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum Pick {
    /// The model the entity tool gives new props.
    #[default]
    NewEntity,
    /// A key of these entities, such as `model`.
    Keys { ids: Vec<u32>, key: String },
}

/// A background check of which prop classes models work as, for the "Static props" and
/// "Physics props" filters. It reads the start of only the models the other filters let
/// through, one after another, so narrowing the list first keeps it short and it never
/// hammers a slow disk.
struct TraitsCheck {
    /// The filters the checked models passed; the check is dropped when they change.
    filters: CheckFilters,
    results: Receiver<(String, PropTraits)>,
    /// How many models are still to check, out of how many.
    left: usize,
    total: usize,
    stop: Arc<AtomicBool>,
}

impl Drop for TraitsCheck {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The filters that decide which models a [`TraitsCheck`] reads.
#[derive(Clone, PartialEq, Eq)]
struct CheckFilters {
    kinds: ModelKinds,
    filter: String,
    folder: Option<String>,
    used_only: bool,
    hidden_sources: Vec<usize>,
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
pub(super) struct ModelBrowserState {
    pub(super) pick: Pick,
    /// Whether the window was open last frame, to tell when it opens.
    was_open: bool,
    /// The filters and layout, saved to the settings when they change.
    view: ModelBrowserView,
    /// Whether `view` has been read from the settings yet.
    view_loaded: bool,
    catalog: Catalog<HashSet<String>>,
    hidden_sources: HashSet<usize>,
    loaders: Option<Loaders<Drawn>>,
    thumbnails: Thumbnails,
    /// What decides which prop classes take each model looked at so far: those drawn
    /// as a thumbnail or in the preview, and those checked for the kind filters.
    prop_traits: HashMap<String, PropTraits>,
    traits_check: Option<TraitsCheck>,
    /// The game's loader, for looking at models once they are listed.
    loader: Option<MaterialLoader>,
    /// The model clicked last, shown in the preview.
    selected: Option<String>,
    /// Whether the sidebar should open the way down to the chosen folder this frame.
    reveal_folder: bool,
    /// The text the sidebar's folder tree is searched for.
    folder_search: String,
    /// Whether the shelf shows the starred models rather than the recent ones.
    shelf_starred: bool,
    /// The window's place on screen last frame, for the keys that move the selection.
    window: Option<egui::Rect>,
    /// The keys read this frame, before the editor's shortcuts could act on them.
    keys: KeyMove,
    /// Models the map uses, cached per geometry generation.
    used: Option<(u64, Arc<BTreeSet<String>>)>,
    preview: Preview,
    focus_search: bool,
}

impl Default for ModelBrowserState {
    fn default() -> Self {
        Self {
            pick: Pick::default(),
            was_open: false,
            view: ModelBrowserView::default(),
            view_loaded: false,
            catalog: Catalog::default(),
            hidden_sources: HashSet::new(),
            loaders: None,
            thumbnails: Thumbnails::new(MAX_REQUESTS),
            prop_traits: HashMap::new(),
            traits_check: None,
            loader: None,
            selected: None,
            reveal_folder: false,
            folder_search: String::new(),
            shelf_starred: false,
            window: None,
            keys: KeyMove::default(),
            used: None,
            preview: Preview::default(),
            focus_search: false,
        }
    }
}

/// Something the browser asks for once it is drawn.
enum Command {
    /// Use a model with this look; the flag says whether the look was chosen by hand.
    Use(String, ModelVariant, bool),
    Copy(String),
}

impl MalletApp {
    /// Takes the keys that move the model browser's selection, before the editor's
    /// shortcuts see them, while the browser is open and the pointer is over it.
    pub(super) fn read_model_browser_keys(&mut self, ctx: &egui::Context, editor: &Editor) {
        let state = &mut self.model_browser;
        state.keys = if editor.model_browser_open {
            KeyMove::read(ctx, state.window)
        } else {
            KeyMove::default()
        };
    }

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
        state.thumbnails.next_frame();
        let options = editor.config.texture_browser.clone();
        state.start(&game, &loader, &options);
        state.receive(ctx);
        let used = state.used_models(editor);
        let scroll_step = editor.config.view.texture_scroll.clamp(0.25, 2.0) as f32;
        let target = state.target(editor);
        let keys = std::mem::take(&mut state.keys);
        state.check_prop_traits(&used);
        let mut shown = state.filtered(&used);
        let groups = state
            .catalog
            .listing
            .as_ref()
            .filter(|_| state.view.group_by_folder)
            .map(|listing| group_by_folder(listing, &mut shown, short_name));

        let title = match &state.pick {
            Pick::NewEntity => format!("{} Models: pick the model for new props", icon::CUBE_FOCUS),
            Pick::Keys { ids, .. } if ids.len() == 1 => format!("{} Models: pick the entity's model", icon::CUBE_FOCUS),
            Pick::Keys { ids, .. } => format!("{} Models: pick the model of {} entities", icon::CUBE_FOCUS, ids.len()),
        };
        let mut open = true;
        let mut commands = Vec::new();
        let window = egui::Window::new(title)
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
                egui::TopBottomPanel::top("model_browser_tools").show_inside(ui, |ui| state.toolbar(ui, &used));
                egui::TopBottomPanel::bottom("model_browser_status")
                    .show_inside(ui, |ui| state.status(ui, &shown, &target, &mut commands));
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
                    state.grid(
                        ui,
                        Shown {
                            entries: &shown,
                            groups: groups.as_deref(),
                        },
                        &target,
                        scroll_step,
                        keys,
                        &mut commands,
                    );
                });
            });
        state.window = window.map(|window| window.response.rect);
        state.thumbnails.trim(options.cache_size());
        state.save_view(ctx, editor);
        for command in commands {
            match command {
                Command::Use(model, variant, look_chosen) => {
                    remember(&mut self.model_browser.view.recent, &model);
                    let pick = self.model_browser.pick.clone();
                    let collision = self
                        .model_browser
                        .catalog
                        .listing
                        .as_ref()
                        .is_some_and(|listing| listing.extra.contains(&model));
                    let known = self.model_browser.prop_traits.get(&model).cloned();
                    let traits = known.or_else(|| loader.model_prop_traits(&model));
                    let result = use_model(editor, &pick, &model, variant, look_chosen, traits.as_ref(), collision);
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
/// takes the model when its own class takes none: `traits` and `collision` say which
/// classes do, and a model that could not be looked at becomes a `prop_dynamic`.
fn use_model(
    editor: &mut Editor,
    pick: &Pick,
    model: &str,
    variant: ModelVariant,
    look_chosen: bool,
    traits: Option<&PropTraits>,
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
                editor.entity_class = traits
                    .map_or("prop_dynamic", |traits| traits.default_class(collision))
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
        let (listing_loader, listing_game) = (loader.clone(), game.clone());
        let started = self.catalog.list(game, move || {
            let (models, collision) = listing_loader.list_models_and_collision();
            Listing::new(listing_game, models, listing_loader.sources(), short_name, collision)
        });
        if started {
            self.traits_check = None;
            self.thumbnails.clear();
            self.prop_traits.clear();
        }
        let threads = options.thread_count();
        let side = options.thumbnail_side().min(MAX_THUMBNAIL_SIDE);
        if self
            .loaders
            .as_ref()
            .is_none_or(|loaders| !loaders.matches(game, threads, side))
        {
            self.loaders = None;
            self.thumbnails.clear();
            let loader = loader.clone();
            self.loaders = Some(Loaders::start(game, threads, side, &self.thumbnails, move |path| {
                guarded(|| {
                    let model = LoadedModel::load(&loader, path, THUMBNAIL_VARIANT, THUMBNAIL_TEXTURE_SIDE)?;
                    let picture = model_render::render(&model, Orbit::default(), side, side, 2);
                    Ok((picture, model.details.prop_traits()))
                })
            }));
        }
    }

    /// Stops the background work and frees the pictures while the window is closed. The
    /// listing stays, so opening the browser again is instant.
    fn close(&mut self) {
        self.loaders = None;
        self.traits_check = None;
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
        if self.catalog.receive(ctx) {
            self.restore_view();
        }
        if let Some(check) = &mut self.traits_check {
            let mut finished = false;
            loop {
                match check.results.try_recv() {
                    Ok((path, traits)) => {
                        self.prop_traits.insert(path, traits);
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
                self.traits_check = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
        let now = ctx.input(|input| input.time);
        if let Some(loaders) = &self.loaders {
            for (path, picture) in loaders.finished() {
                let thumbnail = match picture {
                    Ok((picture, traits)) => {
                        self.prop_traits.insert(path.clone(), traits);
                        let size = [picture.width as usize, picture.height as usize];
                        let texture = upload(ctx, format!("model:{path}"), size, &picture.rgba);
                        Thumbnail::Ready(texture, [picture.width, picture.height])
                    }
                    Err(error) => Thumbnail::Failed(Some(error)),
                };
                self.thumbnails.finish(path, thumbnail, now);
            }
        }
        if self.thumbnails.busy(now) {
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
                self.prop_traits.insert(path.clone(), info.details.prop_traits());
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

    /// While the "Static props" or "Physics props" filter is on, finds out in the
    /// background which prop classes the models the other filters let through work as,
    /// when that is not known yet. A physics prop needs a collision model, which the
    /// listing knows already, so only models that have one are read for that filter. A
    /// check for other filters than the ones set now is dropped.
    fn check_prop_traits(&mut self, used: &BTreeSet<String>) {
        if self.view.kinds == ModelKinds::All {
            self.traits_check = None;
            return;
        }
        let mut hidden_sources: Vec<usize> = self.hidden_sources.iter().copied().collect();
        hidden_sources.sort_unstable();
        let filters = CheckFilters {
            kinds: self.view.kinds,
            filter: self.view.filter.clone(),
            folder: self.view.folder.clone(),
            used_only: self.view.used_only,
            hidden_sources,
        };
        if let Some(check) = &self.traits_check {
            if check.filters == filters {
                return;
            }
        }
        self.traits_check = None;
        let (Some(loader), Some(listing)) = (self.loader.clone(), &self.catalog.listing) else {
            return;
        };
        let search = Search::parse(&self.view.filter);
        let physics = self.view.kinds == ModelKinds::Physics;
        let unknown: Vec<String> = listing
            .entries
            .iter()
            .filter(|(path, sources)| self.passes(&search, used, path, sources))
            .filter(|(path, _)| !physics || listing.extra.contains(path))
            .filter(|(path, _)| !self.prop_traits.contains_key(path))
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
                let traits = loader.model_prop_traits(&path).unwrap_or_default();
                if sender.send((path, traits)).is_err() {
                    return;
                }
            }
        });
        self.traits_check = Some(TraitsCheck {
            filters,
            results,
            left,
            total: left,
            stop,
        });
    }

    /// Whether a model is of the kind the kind filter shows. A model that has not been
    /// looked at yet is of neither kind until it has been.
    fn is_of_kind(&self, listing: &ModelListing, path: &str) -> bool {
        let class = match self.view.kinds {
            ModelKinds::All => return true,
            ModelKinds::Static => "prop_static",
            ModelKinds::Physics => "prop_physics",
        };
        let collision = listing.extra.contains(path);
        self.prop_traits
            .get(path)
            .is_some_and(|traits| traits.works_as(class, collision))
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
        asset_browser::sidebar(
            ui,
            &self.catalog,
            "models",
            &mut self.hidden_sources,
            FolderPick {
                folder: &mut self.view.folder,
                search: &mut self.folder_search,
                reveal: self.reveal_folder,
            },
            |_| false,
        );
        self.reveal_folder = false;
    }

    /// Indices into the listing of the models passing every filter, in name order. A model
    /// shows while any source that has it is ticked.
    fn filtered(&self, used: &BTreeSet<String>) -> Vec<usize> {
        let Some(listing) = &self.catalog.listing else {
            return Vec::new();
        };
        let search = Search::parse(&self.view.filter);
        listing
            .entries
            .iter()
            .enumerate()
            .filter(|(_, (path, _))| self.is_of_kind(listing, path))
            .filter(|(_, (path, sources))| self.passes(&search, used, path, sources))
            .map(|(index, _)| index)
            .collect()
    }

    /// Makes `path` the selected model, shown in the preview.
    fn select(&mut self, path: &str) {
        if self.selected.as_deref() != Some(path) {
            self.selected = Some(path.to_owned());
            self.preview.reset_for_new_model();
        }
    }

    /// The command that uses `path`: with the look chosen in the preview when the
    /// preview shows it, else as it comes.
    fn use_command(&self, path: &str) -> Command {
        if self.view.show_preview && self.selected.as_deref() == Some(path) {
            Command::Use(path.to_owned(), self.preview.variant(), self.preview.look_chosen)
        } else {
            Command::Use(path.to_owned(), ModelVariant::default(), false)
        }
    }

    /// The grid with the folder trail and the Recent and Starred shelf above it. The
    /// arrow keys move the selection and Enter uses it.
    fn grid(
        &mut self,
        ui: &mut egui::Ui,
        shown: Shown,
        target: &Target,
        scroll_step: f32,
        keys: KeyMove,
        commands: &mut Vec<Command>,
    ) {
        let Shown { entries: shown, groups } = shown;
        path_bar(ui, "All models", &mut self.view.folder);
        let shelf = Shelf {
            recent: &self.view.recent,
            favourites: &self.view.favourites,
            selected: self.selected.as_deref(),
            short: short_name,
            upscale: true,
        };
        match shelf.show(
            ui,
            &mut self.thumbnails,
            self.loaders.is_some(),
            &mut self.shelf_starred,
        ) {
            Some(ShelfAction::Select(path)) => self.select(&path),
            Some(ShelfAction::Use(path)) => commands.push(self.use_command(&path)),
            None => {}
        }
        let Some(listing) = &self.catalog.listing else {
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Listing the game's models...");
            });
            return;
        };
        if shown.is_empty() {
            if self.traits_check.is_some() {
                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(match self.view.kinds {
                        ModelKinds::Physics => "Looking for physics props...",
                        _ => "Looking for static props...",
                    });
                });
                return;
            }
            let view = &mut self.view;
            empty_state(
                ui,
                "models",
                Narrowing {
                    filter: &mut view.filter,
                    hidden: &mut self.hidden_sources,
                    folder: &mut view.folder,
                    used_only: &mut view.used_only,
                },
                |ui| {
                    if view.kinds != ModelKinds::All && ui.button("Every kind of prop").clicked() {
                        view.kinds = ModelKinds::All;
                    }
                },
            );
            return;
        }
        let mut reveal = None;
        if keys.moves() {
            let (columns, _) = grid_columns(ui.available_width(), self.view.per_row);
            let lines = grid_lines(shown.len(), columns, groups);
            let from = self
                .selected
                .as_ref()
                .and_then(|selected| shown.iter().position(|index| listing.entries[*index].0 == *selected));
            if let Some(next) = keys.target(from, &lines) {
                let path = listing.entries[shown[next]].0.clone();
                self.select(&path);
                reveal = Some(next);
            }
        }
        if keys.enter {
            if let Some(path) = self.selected.clone() {
                commands.push(self.use_command(&path));
            }
        }
        let mut per_row = self.view.per_row;
        let grid = Grid {
            salt: "model_grid",
            count: shown.len(),
            groups,
            scroll_step,
            reveal,
        };
        let clicked = grid.show(ui, &mut per_row, |ui, index, side| {
            let Some((path, sources)) = self
                .catalog
                .listing
                .as_ref()
                .and_then(|listing| listing.source_names(shown[index]))
            else {
                return;
            };
            let current = target.current.as_deref() == Some(path.as_str());
            let response = self.card(ui, &path, &sources, side, current);
            if response.clicked() {
                self.select(&path);
            }
            if response.double_clicked() {
                commands.push(Command::Use(path.clone(), ModelVariant::default(), false));
            }
            response.context_menu(|ui| self.card_menu(ui, &path, target, commands));
        });
        if let Some(group) = clicked.and_then(|group| groups?.get(group)) {
            self.view.folder = Some(group.folder.clone());
            self.reveal_folder = true;
        }
        self.view.per_row = per_row;
    }

    /// The menu a card opens when right-clicked.
    fn card_menu(&mut self, ui: &mut egui::Ui, path: &str, target: &Target, commands: &mut Vec<Command>) {
        if ui.button(&target.action).clicked() {
            commands.push(Command::Use(path.to_owned(), ModelVariant::default(), false));
            ui.close_menu();
        }
        let starred = self.view.favourites.iter().any(|starred| starred == path);
        if ui.button(if starred { "Unstar" } else { "Star" }).clicked() {
            toggle_favourite(&mut self.view.favourites, path);
            ui.close_menu();
        }
        if ui.button("Copy path").clicked() {
            commands.push(Command::Copy(path.to_owned()));
            ui.close_menu();
        }
        if ui.button("Show its folder").clicked() {
            self.view.folder = Some(
                short_name(path)
                    .rsplit_once('/')
                    .map_or(ROOT_FOLDER, |(folder, _)| folder)
                    .to_owned(),
            );
            self.reveal_folder = true;
            ui.close_menu();
        }
    }

    /// One card: the model's thumbnail and its name under it.
    fn card(&mut self, ui: &mut egui::Ui, path: &str, sources: &[String], side: f32, current: bool) -> egui::Response {
        self.thumbnails.show(path, self.loaders.is_some());
        let marks = Marks {
            current,
            selected: self.selected.as_deref() == Some(path),
            favourite: self.view.favourites.iter().any(|starred| starred == path),
        };
        let thumbnails = &self.thumbnails;
        let response = card(ui, side, short_name(path), marks, |ui, painter, rect| {
            paint_thumbnail(ui, painter, rect, thumbnails, path, true);
        });
        let mut details = vec![path.to_owned()];
        details.extend(source_lines(sources));
        let collision = self
            .catalog
            .listing
            .as_ref()
            .is_some_and(|listing| listing.extra.contains(path));
        if let Some(traits) = self.prop_traits.get(path) {
            details.push(format!("works as {}", classes_taking(traits, collision)));
        }
        if collision {
            details.push("has a collision model".to_owned());
        }
        if let Some(Thumbnail::Failed(Some(error))) = self.thumbnails.get(path) {
            details.push(error.clone());
        }
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
            ui.label(RichText::new("Click a model to look at it here.").color(muted(ui)));
            ui.label(
                RichText::new("The arrow keys move between models, and Enter uses the one picked.")
                    .small()
                    .color(muted(ui)),
            );
            return;
        };
        let collision = self
            .catalog
            .listing
            .as_ref()
            .is_some_and(|listing| listing.extra.contains(&path));
        let traits = self.prop_traits.get(&path).cloned();
        let preview = &mut self.preview;
        if preview.worker.as_ref().is_none_or(|worker| &worker.game != game) {
            preview.worker = Some(PreviewWorker::start(game, loader));
            preview.asked = None;
        }

        let side = ui.available_width();
        let height = side.min((ui.available_height() * 0.5).max(160.0));
        let (rect, response) = ui.allocate_exact_size(egui::vec2(side, height), egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4, ui.visuals().extreme_bg_color);
        if self.view.show_backdrop {
            checkerboard(ui, &painter, rect.shrink(2.0));
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
                    muted(ui),
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
            let starred = self.view.favourites.contains(&path);
            let star = if starred { icon::STAR_HALF } else { icon::STAR };
            if chip(ui, starred, star, None)
                .on_hover_text(if starred {
                    "Unstar"
                } else {
                    "Star, to keep it on the shelf"
                })
                .clicked()
            {
                toggle_favourite(&mut self.view.favourites, &path);
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
        for problem in warnings(&target.classes, traits.as_ref(), collision) {
            ui.label(RichText::new(format!("{} {problem}", icon::WARNING)).color(warning(ui)));
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
                            .color(muted(ui)),
                    );
                }
                let spacing = egui::vec2(12.0, 3.0);
                let widest_column = (ui.available_width() - spacing.x) / 2.0;
                egui::Grid::new("model_details")
                    .num_columns(2)
                    .spacing(spacing)
                    .max_col_width(widest_column)
                    .show(ui, |ui| {
                        let mut row = |name: &str, value: String| {
                            ui.label(RichText::new(name).color(muted(ui)));
                            ui.label(value);
                            ui.end_row();
                        };
                        row("Triangles", thousands(info.triangles));
                        row("Vertices", thousands(details.vertices));
                        row(
                            "Size",
                            format!("{:.0} x {:.0} x {:.0} units", info.size[0], info.size[1], info.size[2]),
                        );
                        row("Works as", classes_taking(&details.prop_traits(), collision));
                        row("$staticprop", if details.static_prop { "yes" } else { "no" }.to_owned());
                        row("Prop data", prop_data_words(details.prop_data.as_ref()));
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
                                ui.label(text.color(warning(ui)))
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

    /// The bar along the top: the search, the card size and the preview, then what kind
    /// of prop to show.
    fn toolbar(&mut self, ui: &mut egui::Ui, used: &BTreeSet<String>) {
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            SearchBox {
                hint: "such as oildrum !gib",
                help: "Every word must be in the model's path. Start a word with ! to leave out models that have \
                       it. * stands for any text and ? for one character, such as props_*/crate??",
                width: 220.0,
            }
            .show(
                ui,
                &mut self.view.filter,
                &mut self.view.filter_history,
                &mut self.focus_search,
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
            .on_hover_text("Put the models under a heading for each folder");
            toggle_chip(
                ui,
                &mut self.view.show_preview,
                &format!("{} Preview", icon::CUBE_FOCUS),
                None,
            )
            .on_hover_text("Show the clicked model in a pane you can turn it in");
            toggle_chip(ui, &mut self.view.show_backdrop, "Backdrop", None)
                .on_hover_text("Draw a checkerboard behind the preview, so see-through parts show");
            if ui
                .button(icon::ARROWS_CLOCKWISE)
                .on_hover_text("List the game's models again")
                .clicked()
            {
                self.catalog.reset();
                self.loaders = None;
                self.traits_check = None;
                self.preview.worker = None;
                self.preview.asked = None;
            }
        });
        ui.horizontal_wrapped(|ui| {
            let help = "Static props are models prop_static can show: compiled as static props, with no prop \
                        data. Physics props are models prop_physics takes: with prop data and a collision model. \
                        Finding either reads the start of every listed model's file, so choose a folder or search \
                        first to keep it quick.";
            for kinds in [ModelKinds::All, ModelKinds::Static, ModelKinds::Physics] {
                if chip(ui, self.view.kinds == kinds, kinds_label(kinds), None)
                    .on_hover_text(help)
                    .clicked()
                {
                    self.view.kinds = kinds;
                }
            }
            ui.separator();
            toggle_chip(ui, &mut self.view.used_only, "Only used", Some(used.len()))
                .on_hover_text("Show only the models the map's entities use");
        });
        ui.add_space(2.0);
    }

    /// The line along the bottom: how many models show, how far finding what kind of
    /// prop they are has got, and the selected model while the preview is hidden.
    fn status(&mut self, ui: &mut egui::Ui, shown: &[usize], target: &Target, commands: &mut Vec<Command>) {
        let total = self.catalog.listing.as_ref().map(|listing| listing.entries.len());
        let progress = self.traits_check.as_ref().map(|check| {
            (
                1.0 - check.left as f32 / check.total.max(1) as f32,
                format!("Checking {} for their kind", thousands(check.left)),
            )
        });
        let selected = self.selected.clone().filter(|_| !self.view.show_preview);
        status_line(ui, "models", shown.len(), total, progress, |ui| {
            if let Some(path) = selected {
                ui.separator();
                ui.label(RichText::new(&path).monospace().small());
                if ui.small_button(&target.action).clicked() {
                    commands.push(Command::Use(path, ModelVariant::default(), false));
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
        if self.view == editor.config.model_browser_view || ctx.wants_keyboard_input() {
            return;
        }
        editor.config.model_browser_view = self.view.clone();
        editor.save_config();
    }
}

/// A model's path without `models/` and `.mdl`, as the browser names it.
fn short_name(path: &str) -> &str {
    let name = path.strip_prefix("models/").unwrap_or(path);
    name.strip_suffix(".mdl").unwrap_or(name)
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

/// What will go wrong in game when a model is used on entities of these classes, one
/// line for each class the compiler or the game takes the model from. Nothing is said
/// about a model that has not been looked at yet.
fn warnings(classes: &[String], traits: Option<&PropTraits>, collision: bool) -> Vec<String> {
    let Some(traits) = traits else {
        return Vec::new();
    };
    classes
        .iter()
        .filter_map(|class| Some(format!("{class}: {}", traits.class_problem(class, collision)?)))
        .collect()
}

/// The usual prop classes that take a model, in words, as the browser lists them.
fn classes_taking(traits: &PropTraits, collision: bool) -> String {
    let classes: Vec<&str> = PROP_CLASSES
        .into_iter()
        .filter(|class| traits.works_as(class, collision))
        .collect();
    if classes.is_empty() {
        "prop_dynamic_override only".to_owned()
    } else {
        classes.join(", ")
    }
}

/// A model's prop data in a few words: the entry of the game's prop data it builds on,
/// and whether it lets the model be a static prop all the same.
fn prop_data_words(data: Option<&PropData>) -> String {
    let Some(data) = data else {
        return "none".to_owned();
    };
    let base = data.base.as_deref().unwrap_or("yes");
    if data.allow_static {
        format!("{base}, static allowed")
    } else {
        base.to_owned()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A model compiled as a static prop, without prop data: a lamp post.
    fn lamp() -> PropTraits {
        PropTraits {
            static_prop: true,
            prop_data: None,
        }
    }

    /// A model compiled as a static prop that has prop data all the same: an oil drum.
    fn drum() -> PropTraits {
        PropTraits {
            static_prop: true,
            prop_data: Some(PropData {
                base: Some("Metal.Medium".to_owned()),
                allow_static: false,
            }),
        }
    }

    /// A browser that has listed these models, each with whether it has a collision
    /// model, and has looked at those given with their traits.
    fn browser(models: &[(&str, bool, Option<PropTraits>)]) -> ModelBrowserState {
        let mut state = ModelBrowserState::default();
        let entries = models
            .iter()
            .map(|(path, _, _)| ((*path).to_owned(), vec![0]))
            .collect();
        let collision = models
            .iter()
            .filter(|(_, collision, _)| *collision)
            .map(|(path, _, _)| (*path).to_owned())
            .collect();
        let listing = Listing::new(PathBuf::new(), entries, Vec::new(), short_name, collision);
        for (path, _, traits) in models {
            if let Some(traits) = traits {
                state.prop_traits.insert((*path).to_owned(), traits.clone());
            }
        }
        state.catalog.listing = Some(listing);
        state
    }

    #[test]
    fn the_kind_filters_show_the_models_each_class_takes() {
        let mut state = browser(&[
            ("models/a_lamp.mdl", false, Some(lamp())),
            ("models/b_drum.mdl", true, Some(drum())),
            ("models/c_drum_without_collision.mdl", false, Some(drum())),
            ("models/d_citizen.mdl", true, Some(PropTraits::default())),
            ("models/e_not_looked_at.mdl", true, None),
        ]);
        let used = BTreeSet::new();
        assert_eq!(state.filtered(&used), [0, 1, 2, 3, 4]);
        state.view.kinds = ModelKinds::Static;
        assert_eq!(
            state.filtered(&used),
            [0],
            "prop data keeps a model from being a static prop"
        );
        state.view.kinds = ModelKinds::Physics;
        assert_eq!(
            state.filtered(&used),
            [1],
            "a physics prop needs prop data and a collision model"
        );
    }

    #[test]
    fn warnings_name_each_class_that_loses_the_model() {
        let classes = ["prop_dynamic", "prop_physics", "prop_static"].map(str::to_owned);
        let lines = warnings(&classes, Some(&drum()), true);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].starts_with("prop_dynamic: ") && lines[0].contains("prop_dynamic_override"));
        assert!(lines[1].starts_with("prop_static: ") && lines[1].contains("VBSP"));
        assert!(warnings(&classes, Some(&drum()), false)
            .iter()
            .any(|line| line.starts_with("prop_physics: ") && line.contains("collision")));
        assert!(warnings(&classes[2..], Some(&lamp()), false).is_empty());
        assert!(warnings(&["npc_citizen".to_owned()], Some(&drum()), false).is_empty());
        assert!(
            warnings(&classes, None, true).is_empty(),
            "nothing is said about a model that was not looked at yet"
        );
    }

    #[test]
    fn what_a_model_works_as_is_put_in_words() {
        assert_eq!(classes_taking(&lamp(), false), "prop_static, prop_dynamic");
        assert_eq!(classes_taking(&drum(), true), "prop_physics");
        assert_eq!(classes_taking(&drum(), false), "prop_dynamic_override only");
        assert_eq!(classes_taking(&PropTraits::default(), true), "prop_dynamic");
        assert_eq!(prop_data_words(None), "none");
        assert_eq!(prop_data_words(drum().prop_data.as_ref()), "Metal.Medium");
        let allowed = PropData {
            base: None,
            allow_static: true,
        };
        assert_eq!(prop_data_words(Some(&allowed)), "yes, static allowed");
    }

    #[test]
    fn a_new_prop_gets_a_class_that_takes_its_model() {
        let placed_as = |class: Option<&str>, traits: Option<PropTraits>, collision: bool| {
            let mut editor = Editor::default();
            if let Some(class) = class {
                editor.entity_class = class.to_owned();
            }
            let variant = ModelVariant::default();
            use_model(
                &mut editor,
                &Pick::NewEntity,
                "models/a.mdl",
                variant,
                false,
                traits.as_ref(),
                collision,
            )
            .unwrap();
            assert_eq!(editor.prop_model, "models/a.mdl");
            editor.entity_class
        };
        assert_eq!(placed_as(None, Some(lamp()), false), "prop_static");
        assert_eq!(placed_as(None, Some(drum()), true), "prop_physics");
        assert_eq!(placed_as(None, Some(drum()), false), "prop_dynamic_override");
        assert_eq!(placed_as(None, Some(PropTraits::default()), true), "prop_dynamic");
        assert_eq!(
            placed_as(None, None, true),
            "prop_dynamic",
            "a model that could not be read"
        );
        assert_eq!(
            placed_as(Some("prop_dynamic"), Some(lamp()), false),
            "prop_dynamic",
            "a class that takes a model is kept"
        );
    }
}
