//! Editor state shared by the window and the MCP server.
//!
//! The window locks it once per frame; the MCP thread locks it for each tool call. Tool
//! calls therefore never wait for a frame, and keep working while the window is covered
//! or minimized.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use eframe::egui::{Color32, Pos2, Rect, Vec2};
use glam::{DVec2, DVec3};
use mallet_vmf::build::box_solid;
use mallet_vmf::edit::{Edit, Touched};
use mallet_vmf::geometry::Bounds;
use mallet_vmf::overlay::DECAL_CLASS;
use mallet_vmf::sides::{CopiedSides, OtherFaces};
use mallet_vmf::transform::TextureLock;
use mallet_vmf::{format_vec3, Entity, IdAllocator, SavedCamera, Solid, Vmf};

use std::sync::Arc;

use mallet_content::{default_fgd, parallel_map, GameData, GameFs, Materials};

use crate::canvas::PixmapCanvas;
use crate::compile::{CompileJob, Preset};
use crate::config::Config;
use crate::document::Document;
use crate::input::InputFrame;
use crate::lighting::{load_texture_lights, TextureLights};
use crate::render3d::{CameraStyle, LightingInput, Renderer3D};
use crate::scene::{decal_key, entity_model, model_key, CameraPreview, DecalSizes, ModelCache, Overlay, Owner, Scene};
use crate::view2d::{Axis2D, View2D};
use crate::view3d::Camera3D;

mod agent;
mod brush_entities;
mod cameras;
mod carve;
mod clipboard;
mod displacements;
mod groups;
mod hollow;
mod primitives;
mod sculpt;
mod views;
mod visgroups;

pub use agent::AgentView;
pub use brush_entities::DEFAULT_SOLID_ENTITY;
pub use displacements::{
    displacement_count, effect_label, AxisChoice, DisplacementMode, DisplacementTool, DEFAULT_ALPHA,
    DEFAULT_DISTANCE, DEFAULT_POWER, DEFAULT_RADIUS,
};
pub use hollow::DEFAULT_HOLLOW_WIDTH;
pub use primitives::DEFAULT_FACES;
pub use visgroups::{GroupState, MoveOptions, UserGroupRow, VisGroupList};
pub use sculpt::{SculptDirection, SculptSettings, SculptTool, DEFAULT_SCULPT_RADIUS};

pub use clipboard::{Clipboard, PasteAt, PasteCursor, PasteOptions, PastePlacing, MAX_COPIES};

/// Material given to new brushes until a texture browser exists.
pub const DEFAULT_MATERIAL: &str = "DEV/DEV_MEASUREGENERIC01B";
/// Depth range used for the first block before any block has been created.
const DEFAULT_BLOCK: Bounds = Bounds {
    min: DVec3::ZERO,
    max: DVec3::splat(64.0),
};

/// The active editing tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Hammer's Selection tool (Shift+S).
    Select,
    /// Hammer's Camera tool (Shift+C): drag in a 3D view to look around or move the
    /// camera, and in a 2D view place, add, move and aim the map's cameras.
    Camera,
    /// Hammer's Magnify tool (Shift+G): click a 2D view to zoom in, or right-click to zoom
    /// out.
    Magnify,
    /// Hammer's Block tool (Shift+B).
    Block,
    /// Hammer's Entity tool (Shift+E).
    Entity,
    /// Hammer's Texture Application tool (Shift+A): pick faces and change their material
    /// and alignment.
    Texture,
    /// Hammer's Clipping tool (Shift+X): cut brushes along a plane.
    Clip,
    /// Hammer's Vertex tool (Shift+V): drag the corners and edges of the selected brushes.
    Vertex,
    /// Hammer's Overlay tool (Shift+O): place overlays on brush faces and drag their
    /// corners.
    Overlay,
    /// Hammer's Decal tool (Shift+D): place decals on brush faces.
    Decal,
}

/// Point entity classes offered by the entity tool's class list. Any other class name
/// can be typed in.
pub const ENTITY_CLASSES: &[&str] = &[
    "info_player_start",
    "info_player_combine",
    "info_player_rebel",
    "info_target",
    "info_node",
    "info_teleport_destination",
    "light",
    "light_spot",
    "light_environment",
    "env_sprite",
    "env_soundscape",
    "ambient_generic",
    "prop_static",
    "prop_dynamic",
    "prop_physics",
    "point_spotlight",
    "path_track",
    "logic_auto",
    "logic_relay",
    "npc_citizen",
    "npc_combine_s",
    "npc_metropolice",
];

/// How a selection change combines with what is already selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectMode {
    Replace,
    Add,
    Remove,
    Toggle,
}

/// What a click on the map picks, Hammer's selection modes in the side panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PickMode {
    /// Clicking part of a group or an entity selects the whole group or entity.
    #[default]
    Groups,
    /// Clicking selects one object: a brush, or a whole entity, even inside a group.
    Objects,
    /// Clicking selects one brush, even one that belongs to a brush entity.
    Solids,
}

impl PickMode {
    pub const ALL: [PickMode; 3] = [PickMode::Groups, PickMode::Objects, PickMode::Solids];

    pub fn label(self) -> &'static str {
        match self {
            PickMode::Groups => "Groups",
            PickMode::Objects => "Objects",
            PickMode::Solids => "Solids",
        }
    }

    /// What a click picks in this mode, for the side panel's tooltip.
    pub fn hint(self) -> &'static str {
        match self {
            PickMode::Groups => "Clicking part of a group or entity selects the whole group or entity",
            PickMode::Objects => "Clicking selects one object, even inside a group",
            PickMode::Solids => "Clicking selects one brush, even inside a brush entity",
        }
    }
}

pub const MIN_GRID: f64 = 1.0;
pub const MAX_GRID: f64 = 512.0;
const DEFAULT_GRID: f64 = 64.0;
/// What drags snap to while Snap to Grid is off or Alt is held: fine enough to move
/// freely, yet coarse enough that coordinates stay short in the saved map.
const FREE_STEP: f64 = 1.0 / 64.0;
/// The most frames a screenshot draws to let textures load, small shadow maps draw and
/// luxels bake, a batch of lights a frame.
const SCREENSHOT_ROUNDS: usize = 600;
/// Up to this many owners are looked for in the scene one by one when the selection
/// changes; more are checked against a list of everything in it, read once.
const FEW_OWNERS: usize = 4;
/// Pixels between the four viewports.
pub const VIEWPORT_GAP: f32 = 2.0;
/// Viewport area assumed until the window has laid itself out.
const DEFAULT_AREA: Vec2 = Vec2::new(1600.0, 950.0);
const GAP_COLOR: Color32 = Color32::from_gray(30);

/// What one of the four viewport slots shows. Click a viewport's label to change it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    Textured,
    Wireframe,
    /// The textured camera lit by the map's lights, as Hammer++'s 3D Lighting Preview.
    Lighting,
    Top,
    Front,
    Side,
}

impl ViewMode {
    pub const ALL: [ViewMode; 6] = [
        Self::Textured,
        Self::Wireframe,
        Self::Lighting,
        Self::Top,
        Self::Front,
        Self::Side,
    ];

    /// The label in the viewport's corner, as Hammer prints it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Textured => "camera (textured)",
            Self::Wireframe => "camera (wireframe)",
            Self::Lighting => "camera (lighting preview)",
            Self::Top => "top (x/y)",
            Self::Front => "front (y/z)",
            Self::Side => "side (x/z)",
        }
    }

    /// The entry in the viewport type menu.
    pub fn menu_label(self) -> &'static str {
        match self {
            Self::Textured => "3D Textured",
            Self::Wireframe => "3D Wireframe",
            Self::Lighting => "3D Lighting Preview",
            Self::Top => "2D Top (x/y)",
            Self::Front => "2D Front (y/z)",
            Self::Side => "2D Side (x/z)",
        }
    }

    /// Which of the editor's three 2D views this shows, if it is a 2D mode.
    pub fn view_index(self) -> Option<usize> {
        match self {
            Self::Top => Some(0),
            Self::Front => Some(1),
            Self::Side => Some(2),
            Self::Textured | Self::Wireframe | Self::Lighting => None,
        }
    }

    /// How the camera draws, if this is a 3D mode.
    pub fn camera_style(self) -> Option<CameraStyle> {
        match self {
            Self::Textured => Some(CameraStyle::Textured),
            Self::Wireframe => Some(CameraStyle::Wireframe),
            Self::Lighting => Some(CameraStyle::Lit),
            Self::Top | Self::Front | Self::Side => None,
        }
    }

    /// The name MCP tools use for this mode.
    pub fn key(self) -> &'static str {
        match self {
            Self::Textured => "textured",
            Self::Wireframe => "wireframe",
            Self::Lighting => "lighting",
            Self::Top => "top",
            Self::Front => "front",
            Self::Side => "side",
        }
    }
}

/// Part of the viewport area a screenshot covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShotTarget {
    All,
    Camera,
    Top,
    Front,
    Side,
}

/// Progress of a whole-window screenshot, which only the UI thread can take.
#[derive(Debug, Default)]
pub enum WindowShot {
    #[default]
    Idle,
    /// Asked for; the window sends the request on its next frame.
    Requested,
    /// Sent; waiting for the image.
    Waiting,
    /// The PNG, or why there is none.
    Done(Result<Vec<u8>, String>),
}

/// How long the parts of recent frames took, for finding slowdowns.
#[derive(Clone, Debug, Default)]
pub struct FrameTimings {
    /// Time in the app's update for the latest frame.
    pub update_ms: f32,
    /// Time drawing the camera viewport in the latest frame.
    pub camera_ms: f32,
    /// Time drawing the three 2D viewports in the latest frame.
    pub views_2d_ms: f32,
    /// CPU time of the whole previous frame, including egui's tessellation and painting.
    pub previous_frame_cpu_ms: f32,
    /// Time between recent back-to-back frames: shortest, average and longest.
    pub interval_min_ms: f32,
    pub interval_average_ms: f32,
    pub interval_max_ms: f32,
    /// The latest frame intervals, oldest first, for spotting uneven frame pacing.
    pub recent_intervals_ms: Vec<f32>,
}

/// One map's state while its tab is in the background. The active tab's state lives in
/// [`Editor`] itself, so everything else only ever deals with one map.
pub struct MapState {
    document: Option<Document>,
    scene: Scene,
    views: [View2D; 3],
    camera: Camera3D,
    selection: BTreeSet<Owner>,
    overlay: Overlay,
    last_block: Bounds,
    refit_on_layout: bool,
    pointfile: Option<Vec<DVec3>>,
    face_selection: BTreeSet<u32>,
}

/// What a tab shows in the tab strip.
pub struct TabInfo {
    pub title: String,
    /// Full path, or `untitled`.
    pub path: String,
    /// Whether the map has unsaved changes.
    pub dirty: bool,
}

fn default_views() -> [View2D; 3] {
    [
        View2D::new(Axis2D::Top),
        View2D::new(Axis2D::Front),
        View2D::new(Axis2D::Side),
    ]
}

/// Everything about the open map and how it is being viewed.
pub struct Editor {
    pub document: Option<Document>,
    pub scene: Scene,
    /// Top, front and side views, in that order. Every slot showing one of these shares
    /// its pan and zoom.
    pub views: [View2D; 3],
    /// What each of the four viewport slots shows, in the order camera top-left, then
    /// top-right, bottom-left, bottom-right in Hammer's default layout.
    pub layout: [ViewMode; 4],
    /// The slot filling the whole viewport area, when one is maximized (Shift+Z).
    pub maximized: Option<usize>,
    /// The viewport slot the mouse was over last, which the view keys act on.
    pub active_slot: usize,
    /// Whether Object Properties shows the map's worldspawn, as Map > Map Properties
    /// opens it, rather than the selection.
    pub properties_world: bool,
    pub camera: Camera3D,
    pub grid: f64,
    /// Whether drags and new points snap to the grid, as Hammer's Map > Snap to Grid.
    pub snap_to_grid: bool,
    /// Whether the 2D views draw the grid, as Hammer's Map > Show Grid.
    pub show_grid: bool,
    /// Whether Alt is held, which lets a drag move freely of the grid, as in Hammer.
    pub snap_suspended: bool,
    /// Latest status line, shown at the bottom of the window.
    pub message: String,
    pub tool: Tool,
    /// The class the entity tool creates.
    pub entity_class: String,
    /// The shape the Block tool creates, from the Objects box of the side panel.
    pub block_shape: mallet_vmf::primitive::Shape,
    /// How many sides round shapes get, from the Faces box beside it.
    pub block_faces: u32,
    /// Selected world brushes and entities.
    pub selection: BTreeSet<Owner>,
    /// The lists of the VisGroups panel, kept up to date with the map.
    pub visgroups: VisGroupList,
    /// What a click picks: whole groups, single objects or single solids.
    pub pick_mode: PickMode,
    /// Tool previews drawn over the map.
    pub overlay: Overlay,
    /// Material applied to new brushes.
    pub material: String,
    /// Synthetic input waiting to be fed to the window, one entry per frame.
    pub input_queue: VecDeque<InputFrame>,
    /// Frames the window has finished, used to wait for synthetic input to take effect.
    pub frames_drawn: u64,
    /// Where the time of recent frames went.
    pub timings: FrameTimings,
    /// The graphics adapter and backend the window draws with.
    pub gpu_info: String,
    /// Textures from the configured game, when one is set.
    pub materials: Option<Materials>,
    /// Prop models loaded so far, shared by every tab.
    pub models: ModelCache,
    /// The size of the picture each decal texture seen so far projects, shared by every
    /// tab.
    pub decal_sizes: DecalSizes,
    /// Entity classes from the game's FGD files, when they could be read.
    pub game_data: Option<Arc<GameData>>,
    /// A screenshot of the whole window asked for by the MCP `screenshot` tool.
    pub window_shot: WindowShot,
    /// Whether the Object Properties window is showing.
    pub properties_open: bool,
    /// A tab the `show_properties` tool asked the window to switch to: keyvalues,
    /// outputs, inputs or flags.
    pub properties_tab: Option<String>,
    /// Why each model in [`Self::models`] that is `None` could not be loaded.
    pub model_errors: HashMap<String, String>,
    /// Settings kept between runs, edited in Mallet Options.
    pub config: Config,
    /// Compile presets, edited in the compile window.
    pub compile_presets: Vec<Preset>,
    /// The running or last compile.
    pub compile_job: Option<CompileJob>,
    /// Whether the compile window is showing.
    pub compile_open: bool,
    /// A tab the `show_compile` tool asked the compile window to switch to.
    pub compile_tab: Option<String>,
    /// Whether the Mallet Options window is showing, and a section an MCP tool asked for.
    pub options_open: bool,
    pub options_section: Option<String>,
    /// Whether Help > About Mallet is showing.
    pub about_open: bool,
    /// Whether the Transform dialog is showing.
    pub transform_open: bool,
    /// Objects copied with Cut or Copy. It belongs to no tab, so it pastes into any map.
    pub clipboard: Clipboard,
    /// Text to put on the system clipboard, which only the window can do.
    pub system_clipboard: Option<String>,
    /// Where the mouse is over a viewport, which is where Paste puts objects.
    pub paste_cursor: Option<PasteCursor>,
    /// Whether the Paste Special dialog is showing, and where it pastes unless told to
    /// start at the original.
    pub paste_special_open: bool,
    pub paste_special_at: PasteAt,
    /// A paste made over a 3D view that is still following the mouse.
    pub paste_placing: Option<PastePlacing>,
    /// The 2D view (0 top, 1 front, 2 side) the mouse was over last, which Flip and Align
    /// work in.
    pub active_view: usize,
    /// The leak line VBSP traced, shown in the views.
    pub pointfile: Option<Vec<DVec3>>,
    /// Brush faces the texture tool has selected, by side id.
    pub face_selection: BTreeSet<u32>,
    /// The side list keyvalue that clicks in the 3D view pick faces for, if any.
    pub(crate) face_pick: Option<crate::face_pick::FacePick>,
    /// The sound position key that the next click in the 3D view places, when its
    /// button in Object Properties is on.
    pub(crate) position_pick: Option<crate::position_pick::PositionPick>,
    /// The clipping tool's points and settings.
    pub clipper: crate::clipper::Clipper,
    /// What a texture tool click does.
    pub texture_mode: crate::faces::TextureMode,
    /// The material and alignment last picked up from a face.
    pub lifted: Option<mallet_vmf::Side>,
    /// Materials applied lately, newest first, for the texture browser.
    pub recent_materials: Vec<String>,
    /// Every material of the game, listed once per game for MCP tools.
    material_list: Option<(PathBuf, Arc<Vec<String>>)>,
    /// Whether the texture browser is showing, and a filter an MCP tool asked for.
    pub texture_browser_open: bool,
    pub texture_browser_filter: Option<String>,
    /// Whether the model browser is showing, and a filter an MCP tool asked for.
    pub model_browser_open: bool,
    pub model_browser_filter: Option<String>,
    /// Whether the soundscape browser is showing, and a filter an MCP tool asked for.
    pub soundscape_browser_open: bool,
    pub soundscape_browser_filter: Option<String>,
    /// The soundscape the entity tool gives new soundscape entities; empty to keep the
    /// class's own default.
    pub new_soundscape: String,
    /// The model the entity tool gives new entities of a class that takes one, such as
    /// `models/props_c17/oildrum001.mdl`; empty to keep the class's own default.
    pub prop_model: String,
    /// Every model of the game, listed once per game for MCP tools.
    model_list: Option<(PathBuf, Arc<Vec<String>>)>,
    /// The GPU renderer for the textured camera view, created once the window has a GPU.
    pub gpu: Option<Renderer3D>,
    /// The texture lights of the game's `lights.rad` and the map's own `.rad` file, for
    /// the lighting preview.
    pub texture_lights: Arc<TextureLights>,
    /// The sound preview: the soundscape and ambient sounds heard at the 3D camera.
    pub sound: crate::sound::SoundPreview,
    /// The vertex tool's brushes and selected vertices.
    pub vertex: crate::vertex_tool::VertexState,
    /// The Displacement tab's tool and paint brush settings.
    pub displacement: DisplacementTool,
    /// Where the vertices are that Displacement Edge Collapse shows, and the geometry
    /// generation they were worked out for.
    collapsed_points: Option<(u64, Arc<Vec<DVec3>>)>,
    /// The last block created; new blocks reuse its depth.
    last_block: Bounds,
    /// Camera, top, front and side viewport rectangles from the latest window layout.
    pub(crate) viewport_rects: [Rect; 4],
    /// Physical pixels per layout point in the window.
    pixels_per_point: f32,
    refit_on_layout: bool,
    /// One entry per open map tab: `None` for the active tab, whose state is held in the
    /// fields above, and the parked state of every other tab.
    tabs: Vec<Option<MapState>>,
    /// A lasting id for each tab in [`Self::tabs`], so a tab can be found again after
    /// tabs before it open or close.
    tab_ids: Vec<u64>,
    next_tab_id: u64,
    active_tab: usize,
    /// The tab, camera, selection and tools of an MCP agent working in its own view.
    agent: Option<AgentView>,
    /// The tab the window shows, while a tool call runs in the agent's view.
    window_tab: Option<usize>,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            document: None,
            scene: Scene::default(),
            views: default_views(),
            layout: [ViewMode::Textured, ViewMode::Top, ViewMode::Front, ViewMode::Side],
            maximized: None,
            active_slot: 0,
            properties_world: false,
            camera: Camera3D::new(),
            grid: DEFAULT_GRID,
            snap_to_grid: true,
            show_grid: true,
            snap_suspended: false,
            message: "Open a map with File > Open or drop a .vmf or .mallet onto the window.".to_owned(),
            tool: Tool::Select,
            entity_class: ENTITY_CLASSES[0].to_owned(),
            block_shape: mallet_vmf::primitive::Shape::Block,
            block_faces: DEFAULT_FACES,
            selection: BTreeSet::new(),
            visgroups: VisGroupList::default(),
            pick_mode: PickMode::default(),
            overlay: Overlay::default(),
            material: DEFAULT_MATERIAL.to_owned(),
            input_queue: VecDeque::new(),
            frames_drawn: 0,
            timings: FrameTimings::default(),
            gpu_info: String::new(),
            materials: None,
            models: ModelCache::new(),
            decal_sizes: DecalSizes::new(),
            model_errors: HashMap::new(),
            game_data: None,
            properties_open: false,
            properties_tab: None,
            config: Config::default(),
            compile_presets: crate::compile::load_presets(),
            compile_job: None,
            compile_open: false,
            compile_tab: None,
            options_open: false,
            options_section: None,
            about_open: false,
            transform_open: false,
            clipboard: Clipboard::default(),
            system_clipboard: None,
            paste_cursor: None,
            paste_special_open: false,
            paste_special_at: PasteAt::Original,
            paste_placing: None,
            active_view: 0,
            pointfile: None,
            face_selection: BTreeSet::new(),
            face_pick: None,
            position_pick: None,
            clipper: crate::clipper::Clipper::default(),
            recent_materials: Vec::new(),
            texture_mode: crate::faces::TextureMode::default(),
            lifted: None,
            material_list: None,
            texture_browser_open: false,
            texture_browser_filter: None,
            model_browser_open: false,
            model_browser_filter: None,
            soundscape_browser_open: false,
            soundscape_browser_filter: None,
            new_soundscape: String::new(),
            prop_model: String::new(),
            model_list: None,
            window_shot: WindowShot::Idle,
            gpu: None,
            texture_lights: Arc::new(TextureLights::new()),
            sound: crate::sound::SoundPreview::default(),
            vertex: Default::default(),
            displacement: DisplacementTool::default(),
            collapsed_points: None,
            last_block: DEFAULT_BLOCK,
            viewport_rects: quad_rects(Rect::from_min_size(Pos2::ZERO, DEFAULT_AREA), VIEWPORT_GAP),
            pixels_per_point: 1.0,
            refit_on_layout: false,
            tabs: vec![None],
            tab_ids: vec![0],
            next_tab_id: 1,
            active_tab: 0,
            agent: None,
            window_tab: None,
        }
    }
}

/// Whether entities of a class name a soundscape of the game's scripts in their
/// `soundscape` key. A `trigger_soundscape` names an entity there instead, and a proxy
/// takes its main soundscape's.
pub fn class_plays_soundscape(classname: &str) -> bool {
    let classname = classname.trim();
    classname.eq_ignore_ascii_case("env_soundscape") || classname.eq_ignore_ascii_case("env_soundscape_triggerable")
}

/// Locks shared editor state, recovering it if a previous holder panicked.
pub fn lock(editor: &Mutex<Editor>) -> MutexGuard<'_, Editor> {
    editor.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Editor {
    /// The open tabs, in order.
    pub fn tabs(&self) -> Vec<TabInfo> {
        let describe = |document: Option<&Document>| match document {
            Some(document) => TabInfo {
                title: document.file_name(),
                path: document.display_path(),
                dirty: document.dirty,
            },
            None => TabInfo {
                title: "empty".to_owned(),
                path: String::new(),
                dirty: false,
            },
        };
        self.tabs
            .iter()
            .enumerate()
            .map(|(index, parked)| match parked {
                _ if index == self.active_tab => describe(self.document.as_ref()),
                Some(map) => describe(map.document.as_ref()),
                None => describe(None),
            })
            .collect()
    }

    /// Index of the tab being edited.
    pub fn active_tab(&self) -> usize {
        self.active_tab
    }

    /// The lasting id of the tab being edited, which stays its own while tabs before it
    /// open or close.
    pub fn active_tab_id(&self) -> u64 {
        self.tab_ids[self.active_tab]
    }

    /// Brings another tab to the front.
    pub fn switch_tab(&mut self, index: usize) -> Result<(), String> {
        if index >= self.tabs.len() {
            return Err(format!("there is no tab {index}; {} are open", self.tabs.len()));
        }
        if index == self.active_tab {
            return Ok(());
        }
        self.bring_to_front(index);
        self.message = format!("Switched to {}", self.tabs()[index].title);
        Ok(())
    }

    /// Makes another tab the one being edited, parking the current one.
    fn bring_to_front(&mut self, index: usize) {
        if index == self.active_tab {
            return;
        }
        let current = self.take_map();
        self.tabs[self.active_tab] = Some(current);
        let next = self.tabs[index].take().expect("background tabs hold their state");
        self.put_map(next);
        self.active_tab = index;
    }

    /// Opens a new tab with an empty map, after the current one.
    pub fn new_tab(&mut self) {
        let current = self.take_map();
        self.tabs[self.active_tab] = Some(current);
        self.active_tab += 1;
        self.tabs.insert(self.active_tab, None);
        self.tab_ids.insert(self.active_tab, self.next_tab_id);
        self.next_tab_id += 1;
        self.new_map();
    }

    /// Closes a tab, discarding unsaved changes; ask before calling this for a dirty tab.
    /// Closing the last tab leaves a new empty map.
    pub fn close_tab(&mut self, index: usize) -> Result<(), String> {
        if index >= self.tabs.len() {
            return Err(format!("there is no tab {index}; {} are open", self.tabs.len()));
        }
        if self.tabs.len() == 1 {
            self.new_map();
            return Ok(());
        }
        if index != self.active_tab {
            self.tabs.remove(index);
            self.tab_ids.remove(index);
            if index < self.active_tab {
                self.active_tab -= 1;
            }
            return Ok(());
        }
        self.tabs.remove(index);
        self.tab_ids.remove(index);
        self.active_tab = index.min(self.tabs.len() - 1);
        let next = self.tabs[self.active_tab]
            .take()
            .expect("background tabs hold their state");
        self.put_map(next);
        Ok(())
    }

    /// Opens a map in a new tab, or in the current one when it is an untouched empty map.
    /// A map that is already open is brought to the front instead of opened twice.
    pub fn open_in_tab(&mut self, path: &Path) -> Result<(), String> {
        if let Some(index) = self.tab_of_path(path) {
            return self.switch_tab(index);
        }
        let reuse = self
            .document
            .as_ref()
            .is_none_or(|document| document.path.is_none() && !document.dirty)
            && self.scene.meshes.is_empty();
        if !reuse {
            self.new_tab();
        }
        let result = self.open(path);
        if result.is_err() && !reuse {
            let failure = self.message.clone();
            let _ = self.close_tab(self.active_tab);
            self.message = failure;
        }
        result
    }

    /// The tab that has the map at `path` open, if any.
    fn tab_of_path(&self, path: &Path) -> Option<usize> {
        let wanted = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        self.tabs().iter().position(|tab| {
            let open = Path::new(&tab.path);
            std::fs::canonicalize(open).is_ok_and(|open| open == wanted)
        })
    }

    fn take_map(&mut self) -> MapState {
        MapState {
            document: self.document.take(),
            scene: std::mem::take(&mut self.scene),
            views: std::mem::replace(&mut self.views, default_views()),
            camera: std::mem::replace(&mut self.camera, Camera3D::new()),
            selection: std::mem::take(&mut self.selection),
            overlay: std::mem::take(&mut self.overlay),
            last_block: std::mem::replace(&mut self.last_block, DEFAULT_BLOCK),
            refit_on_layout: std::mem::take(&mut self.refit_on_layout),
            pointfile: self.pointfile.take(),
            face_selection: std::mem::take(&mut self.face_selection),
        }
    }

    fn put_map(&mut self, map: MapState) {
        self.document = map.document;
        self.scene = map.scene;
        self.views = map.views;
        self.camera = map.camera;
        self.selection = map.selection;
        self.overlay = map.overlay;
        self.last_block = map.last_block;
        self.refit_on_layout = map.refit_on_layout;
        self.pointfile = map.pointfile;
        self.face_selection = map.face_selection;
        self.refresh_face_marks();
        self.clear_clip_points();
    }

    /// Opens a map and frames it in every viewport.
    pub fn open(&mut self, path: &Path) -> Result<(), String> {
        match Document::load(path) {
            Ok(document) => {
                let grid = document.vmf.grid_spacing();
                let snap = document.vmf.view_flag("bSnapToGrid");
                let show = document.vmf.view_flag("bShowGrid");
                self.set_document(document);
                self.message = format!("Loaded {}", path.display());
                self.remember_map(path);
                if let Some(grid) = grid.filter(|grid| (MIN_GRID..=MAX_GRID).contains(grid)) {
                    self.grid = grid;
                }
                self.snap_to_grid = snap.unwrap_or(true);
                self.show_grid = show.unwrap_or(true);
                self.frame_map();
                self.refit_on_layout = true;
                Ok(())
            }
            Err(error) => {
                self.message = format!("Could not open {}: {error}", path.display());
                Err(self.message.clone())
            }
        }
    }

    /// Replaces the open map with a new, empty, unsaved one, viewed from the origin.
    pub fn new_map(&mut self) {
        self.set_document(Document::untitled());
        self.views = default_views();
        self.camera = Camera3D::new();
        self.snap_to_grid = true;
        self.show_grid = true;
        self.refit_on_layout = false;
        self.message = "New map".to_owned();
    }

    fn set_document(&mut self, document: Document) {
        self.ensure_models(document.vmf.entities());
        self.ensure_decal_sizes(document.vmf.entities());
        self.scene = Scene::build(
            &document.vmf,
            &self.models,
            self.game_data.as_deref(),
            &self.decal_sizes,
        );
        self.document = Some(document);
        self.selection.clear();
        self.overlay = Overlay::default();
        self.reload_texture_lights();
    }

    /// Reads the texture lights again: the game's `lights.rad`, then the open map's own
    /// `.rad` file beside it, as VRAD reads them.
    pub fn reload_texture_lights(&mut self) {
        let filesystem = self.materials.as_ref().map(Materials::filesystem);
        let map = self.document.as_ref().and_then(|document| document.path.as_deref());
        self.texture_lights = Arc::new(load_texture_lights(filesystem, map));
    }

    /// Like Hammer, keeps where the map was viewed from and its grid in the map, so it
    /// reopens the same way. This is not an edit: it does not dirty the map or go into
    /// the undo history.
    fn store_view(&mut self) -> Result<&mut Document, String> {
        let camera = SavedCamera {
            position: self.camera.position,
            look: self.camera.look_point(),
        };
        let (grid, snap, show) = (self.grid, self.snap_to_grid, self.show_grid);
        let document = self.document.as_mut().ok_or("no map is open")?;
        document.vmf.set_active_camera(camera);
        document.vmf.set_grid_spacing(grid);
        document.vmf.set_view_flag("bSnapToGrid", snap);
        document.vmf.set_view_flag("bShowGrid", show);
        Ok(document)
    }

    /// Saves the open map, first moving it to `path` when one is given. A map that has
    /// never been saved needs a path. The extension picks the format: `.mallet` for
    /// Mallet's own, anything else for VMF.
    pub fn save(&mut self, path: Option<PathBuf>) -> Result<PathBuf, String> {
        let document = self.store_view()?;
        if let Some(path) = path {
            document.path = Some(path);
        }
        let result = document.save();
        if result.is_ok() {
            document.dirty = false;
        }
        let message = match &result {
            Ok(()) => format!("Saved {}", document.display_path()),
            Err(error) => format!("Could not save {}: {error}", document.display_path()),
        };
        let saved = document.path.clone();
        self.message = message;
        match (result, saved) {
            (Ok(()), Some(path)) => {
                self.remember_map(&path);
                Ok(path)
            }
            _ => Err(self.message.clone()),
        }
    }

    /// Writes a copy of the open map to `path`, in the format its extension names,
    /// leaving the map's own file and unsaved state alone.
    pub fn export_copy(&mut self, path: &Path) -> Result<(), String> {
        let document = self.store_view()?;
        let result = document.export(path);
        self.message = match &result {
            Ok(()) => format!("Exported a copy to {}", path.display()),
            Err(error) => format!("Could not export {}: {error}", path.display()),
        };
        result.map_err(|_| self.message.clone())
    }

    /// Whether the open map has never been saved, so saving needs a path.
    pub fn is_untitled(&self) -> bool {
        self.document.as_ref().is_some_and(|document| document.path.is_none())
    }

    /// Applies an edit through the undo history and refreshes the viewports.
    pub fn apply_edit(&mut self, edit: &Edit) -> Result<(), String> {
        let document = self.document.as_mut().ok_or("no map is open")?;
        document
            .history
            .apply(&mut document.vmf, edit)
            .map_err(|error| error.to_string())?;
        document.dirty = true;
        self.update_scene(edit.touched());
        Ok(())
    }

    /// Applies an edit that continues the previous one when both have the same `group`,
    /// as each step of a drag does, so the whole drag undoes at once.
    pub fn apply_edit_grouped(&mut self, edit: &Edit, group: u64) -> Result<(), String> {
        let document = self.document.as_mut().ok_or("no map is open")?;
        document
            .history
            .apply_grouped(&mut document.vmf, edit, group)
            .map_err(|error| error.to_string())?;
        document.dirty = true;
        self.update_scene(edit.touched());
        Ok(())
    }

    /// Undoes or redoes one step. Returns whether anything changed.
    pub fn step_history(&mut self, redo: bool) -> Result<bool, String> {
        let document = self.document.as_mut().ok_or("no map is open")?;
        let next = if redo {
            document.history.next_redo()
        } else {
            document.history.next_undo()
        };
        let touched = next.and_then(Edit::touched);
        let result = if redo {
            document.history.redo(&mut document.vmf)
        } else {
            document.history.undo(&mut document.vmf)
        };
        match result {
            Ok(changed) => {
                if changed {
                    document.dirty = true;
                    self.update_scene(touched);
                }
                Ok(changed)
            }
            Err(error) => {
                self.message = format!("Could not undo: {error}");
                Err(self.message.clone())
            }
        }
    }

    /// Runs the sound preview for the 3D camera: one update of what a player standing
    /// there would hear. `focused` says whether Mallet is the active window. Returns
    /// whether the preview is running and wants another update soon.
    pub fn update_sound(&mut self, focused: bool) -> bool {
        let map_name = self
            .document
            .as_ref()
            .and_then(|document| document.path.as_ref())
            .and_then(|path| path.file_stem())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let frame = crate::sound::SoundFrame {
            options: &self.config.sound,
            focused,
            listener: mallet_vmf::sound::Listener {
                position: self.camera.position,
                forward: self.camera.forward(),
                right: self.camera.right(),
            },
            scene: &self.scene,
            materials: self.materials.as_mut(),
            map_name,
            tab: self.tab_ids[self.active_tab],
            open: self.document.is_some(),
            browsing: self.soundscape_browser_open,
        };
        self.sound.update(frame);
        self.sound.running()
    }

    /// Brings the wireframes up to the map after an edit. `touched` is what the edit
    /// changed where it stands, as [`Edit::touched`] gives it: only those brushes and
    /// entities are built again. Without it, or when that is not enough, everything is.
    fn update_scene(&mut self, touched: Option<Touched>) {
        if !touched.is_some_and(|touched| self.patch_scene(&touched)) {
            self.rebuild_scene();
        }
    }

    /// Rebuilds the wireframes of the brushes and entities in `touched`. Returns `false`
    /// when the scene has to be built anew instead.
    fn patch_scene(&mut self, touched: &Touched) -> bool {
        if !self.scene.patches(touched) {
            return false;
        }
        let Some(document) = self.document.take() else {
            return false;
        };
        let entities = || touched.entities.iter().filter_map(|id| document.vmf.entity_by_id(*id));
        self.ensure_models(entities());
        self.ensure_decal_sizes(entities());
        let patched = self.scene.patch(
            &document.vmf,
            &self.models,
            self.game_data.as_deref(),
            &self.decal_sizes,
            touched,
        );
        self.document = Some(document);
        if patched {
            if touched.grouping {
                self.refresh_visgroups();
            }
            self.sync_selection();
        }
        patched
    }

    /// Rebuilds the wireframes after the map changed, dropping selected items that no
    /// longer exist.
    pub fn rebuild_scene(&mut self) {
        if let Some(document) = self.document.take() {
            self.ensure_models(document.vmf.entities());
            self.ensure_decal_sizes(document.vmf.entities());
            self.scene = Scene::build(
                &document.vmf,
                &self.models,
                self.game_data.as_deref(),
                &self.decal_sizes,
            );
            self.document = Some(document);
            self.add_pointfile_to_scene();
        } else {
            self.scene = Scene::default();
        }
        self.refresh_visgroups();
        self.sync_selection();
    }

    /// Drops selected brushes, entities and faces that are no longer in the map, and
    /// marks the rest as selected in the scene.
    fn sync_selection(&mut self) {
        let selectable = self.scene.selectable();
        self.selection.retain(|owner| selectable.contains(owner));
        self.scene.mark_selected(&self.selection);
        let sides: BTreeSet<u32> = self
            .scene
            .meshes
            .iter()
            .flat_map(|mesh| &mesh.faces)
            .filter_map(|face| face.side)
            .collect();
        self.face_selection.retain(|side| sides.contains(side));
        self.refresh_face_marks();
        self.refresh_clip_preview();
    }

    /// Every material of the game, listed on first use.
    pub fn material_list(&mut self) -> Result<Arc<Vec<String>>, String> {
        let materials = self.materials.as_ref().ok_or("choose a game folder first")?;
        let game = materials.filesystem().game_dir().to_owned();
        if let Some((listed, list)) = &self.material_list {
            if *listed == game {
                return Ok(Arc::clone(list));
            }
        }
        let list = Arc::new(materials.loader().list());
        self.material_list = Some((game, Arc::clone(&list)));
        Ok(list)
    }

    /// Every model the game mounts, such as `models/props_c17/oildrum001.mdl`, sorted.
    /// Listed once per game.
    pub fn model_list(&mut self) -> Result<Arc<Vec<String>>, String> {
        let materials = self.materials.as_ref().ok_or("choose a game folder first")?;
        let game = materials.filesystem().game_dir().to_owned();
        if let Some((listed, list)) = &self.model_list {
            if *listed == game {
                return Ok(Arc::clone(list));
            }
        }
        let models: Vec<String> = materials
            .loader()
            .list_models()
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        let list = Arc::new(models);
        self.model_list = Some((game, Arc::clone(&list)));
        Ok(list)
    }

    /// Whether entities of a class show a model of their own choosing: the class has a
    /// `model` key that names a studio model. Without game data, the `prop_` classes do.
    pub fn class_takes_model(&self, classname: &str) -> bool {
        match self.entity_class(classname.trim()) {
            Some(class) => class
                .property("model")
                .is_some_and(|property| property.kind == "studio"),
            None => self.game_data.is_none() && classname.trim().to_ascii_lowercase().starts_with("prop_"),
        }
    }

    /// The selected entities whose model can be changed: those with a `model` key that
    /// names a studio model, or of a class that takes one.
    pub fn model_targets(&self) -> Vec<u32> {
        self.selection
            .iter()
            .filter_map(|owner| match owner {
                Owner::Entity(id) => Some(*id),
                Owner::Solid(_) => None,
            })
            .filter(|id| {
                self.entity(*id).is_some_and(|entity| {
                    entity
                        .get("model")
                        .is_some_and(|model| model.to_ascii_lowercase().ends_with(".mdl"))
                        || self.class_takes_model(entity.classname())
                })
            })
            .collect()
    }

    /// The keys the entity tool adds to a new entity of the current class: the model
    /// picked in the model browser, when the class takes one, and the soundscape picked
    /// in the soundscape browser, when the class plays one.
    pub fn entity_tool_keys(&self) -> Vec<(String, String)> {
        let mut keys = Vec::new();
        let model = self.prop_model.trim();
        if !model.is_empty() && self.class_takes_model(&self.entity_class) {
            keys.push(("model".to_owned(), model.to_owned()));
        }
        let soundscape = self.new_soundscape.trim();
        if !soundscape.is_empty() && class_plays_soundscape(&self.entity_class) {
            keys.push(("soundscape".to_owned(), soundscape.to_owned()));
        }
        keys
    }

    /// The selected entities whose `soundscape` key names a soundscape of the game's
    /// scripts, which the soundscape browser sets.
    pub fn soundscape_targets(&self) -> Vec<u32> {
        self.selection
            .iter()
            .filter_map(|owner| match owner {
                Owner::Entity(id) => Some(*id),
                Owner::Solid(_) => None,
            })
            .filter(|id| {
                self.entity(*id)
                    .is_some_and(|entity| class_plays_soundscape(entity.classname()))
            })
            .collect()
    }

    /// Changes the selection. Owners that are not in the map are ignored. The texture
    /// tool selects faces rather than brushes, so there it selects the owners' faces.
    pub fn select(&mut self, owners: impl IntoIterator<Item = Owner>, mode: SelectMode) {
        let mut owners: Vec<Owner> = owners.into_iter().collect();
        if owners.len() <= FEW_OWNERS {
            owners.retain(|owner| self.scene.contains(*owner));
        } else {
            let selectable = self.scene.selectable();
            owners.retain(|owner| selectable.contains(owner));
        }
        if self.tool == Tool::Texture {
            return self.select_owner_faces(&owners, mode);
        }
        let before = self.selection.clone();
        match mode {
            SelectMode::Replace => self.selection = owners.into_iter().collect(),
            SelectMode::Add => self.selection.extend(owners),
            SelectMode::Remove => {
                for owner in owners {
                    self.selection.remove(&owner);
                }
            }
            SelectMode::Toggle => {
                for owner in owners {
                    if !self.selection.remove(&owner) {
                        self.selection.insert(owner);
                    }
                }
            }
        }
        self.drop_covered_solids();
        if self.selection != before {
            self.overlay.handle_mode = crate::transform::HandleMode::for_gizmo(self.config.transform.gizmo_mode);
            self.overlay.pivot = None;
            self.refresh_clip_preview();
        }
        self.scene.mark_selected(&self.selection);
        if let Some(bounds) = self.selection_block() {
            self.last_block = bounds;
        }
    }

    /// Changes the selection with what a click or a box picked: in Groups mode each owner
    /// brings its whole group along. Other callers pass owners as they are, through
    /// [`Editor::select`].
    pub fn select_picked(&mut self, owners: impl IntoIterator<Item = Owner>, mode: SelectMode) {
        let owners = self.with_groups(owners.into_iter().collect());
        self.select(owners, mode);
    }

    /// Drops solids whose entity is selected too, so an entity's brushes are never listed
    /// twice, once as the entity and once as a solid.
    fn drop_covered_solids(&mut self) {
        let covered: Vec<u32> = self
            .scene
            .meshes
            .iter()
            .filter(|mesh| {
                matches!(mesh.owner, Some(Owner::Entity(_)))
                    && mesh.owner.is_some_and(|owner| self.selection.contains(&owner))
            })
            .filter_map(|mesh| mesh.solid)
            .collect();
        for id in covered {
            self.selection.remove(&Owner::Solid(id));
        }
    }

    /// Everything a click or a box on `owners` selects: in Groups mode, each owner and
    /// the whole Hammer group it is in (world brushes and entities that share its
    /// `groupid`); otherwise, or for owners in no group, the owners alone.
    fn with_groups(&self, owners: BTreeSet<Owner>) -> BTreeSet<Owner> {
        let Some(document) = self.document.as_ref().filter(|_| self.pick_mode == PickMode::Groups) else {
            return owners;
        };
        let vmf = &document.vmf;
        let groups: HashSet<&str> = owners
            .iter()
            .filter_map(|owner| match *owner {
                Owner::Entity(id) => vmf.entity_by_id(id)?.editor()?.get("groupid"),
                Owner::Solid(id) => vmf.solid_by_id(id)?.editor()?.get("groupid"),
            })
            .collect();
        if groups.is_empty() {
            return owners;
        }
        let in_group = |editor: Option<&str>| editor.is_some_and(|group| groups.contains(group));
        let mut members = owners;
        for entity in vmf.all_entities() {
            if entity.classname().eq_ignore_ascii_case("worldspawn") {
                members.extend(
                    entity
                        .solids()
                        .filter(|solid| in_group(solid.editor().and_then(|block| block.get("groupid"))))
                        .filter_map(|solid| solid.id().map(Owner::Solid)),
                );
            } else if in_group(entity.editor().and_then(|block| block.get("groupid"))) {
                members.extend(entity.id().map(Owner::Entity));
            }
        }
        members
    }

    /// Whether clicks pick single brush solids, even inside brush entities.
    pub fn solid_picks(&self) -> bool {
        self.pick_mode == PickMode::Solids
    }

    pub fn set_pick_mode(&mut self, mode: PickMode) {
        if self.pick_mode != mode {
            self.pick_mode = mode;
            self.message = format!("Select mode: {}", mode.label());
        }
    }

    /// Ctrl+W: cycles the selection mode, Groups, then Objects, then Solids, then Groups.
    pub fn cycle_pick_mode(&mut self) {
        let mode = match self.pick_mode {
            PickMode::Groups => PickMode::Objects,
            PickMode::Objects => PickMode::Solids,
            PickMode::Solids => PickMode::Groups,
        };
        self.set_pick_mode(mode);
    }

    pub fn clear_selection(&mut self) {
        self.select([], SelectMode::Replace);
    }

    pub fn select_all(&mut self) {
        let all = self.scene.owners();
        self.select(all, SelectMode::Replace);
    }

    /// A short description of the selection for the status bar, such as
    /// `2 objects selected, 128w 64l 32h`.
    pub fn selection_summary(&self) -> Option<String> {
        if self.selection.is_empty() {
            return None;
        }
        let size = self.scene.selection_bounds().size();
        let count = self.selection.len();
        let noun = if count == 1 { "object" } else { "objects" };
        Some(format!("{count} {noun} selected, {}w {}l {}h", size.x, size.y, size.z))
    }

    /// Moves the selection, with texture lock. Entity origins move with it, and so do
    /// overlays lying on the moved brushes while texture lock is on.
    pub fn move_selection(&mut self, offset: DVec3) -> Result<(), String> {
        let (solids, origins) = self.selection_parts()?;
        let riding = if self.config.transform.texture_lock {
            let moved = (solids.clone(), glam::DMat4::from_translation(offset), TextureLock::Full);
            self.riding_overlay_edits(&[moved])
        } else {
            Vec::new()
        };
        let mut edits = vec![if self.config.transform.texture_lock {
            Edit::Translate { solids, offset }
        } else {
            Edit::Transform {
                solids,
                matrix: glam::DMat4::from_translation(offset),
                lock: TextureLock::Off,
            }
        }];
        edits.extend(origin_edits(origins, |origin| origin + offset));
        edits.extend(self.selected_overlay_edits(&glam::DMat4::from_translation(offset)));
        edits.extend(riding);
        self.apply_edit(&Edit::Batch(edits))?;
        if let Some(pivot) = self.overlay.pivot {
            self.overlay.pivot = Some(pivot + offset);
        }
        self.message = format!("Moved selection by {}", format_vec3(offset));
        Ok(())
    }

    /// Stretches the selection so its bounds become `to`, like dragging a selection handle.
    /// Entity origins are moved proportionally; textures follow the texture lock settings,
    /// pinned at the box's minimum corner when they keep their scale.
    pub fn resize_selection(&mut self, to: Bounds) -> Result<(), String> {
        let from = self.scene.selection_bounds();
        let flattened = (0..3).any(|axis| from.size()[axis] > 0.0 && to.size()[axis] <= 0.0);
        if flattened || from.is_empty() || to.is_empty() {
            return Err(mallet_vmf::edit::EditError::InvalidResize.to_string());
        }
        let matrix = crate::scene::Remap::boxes(&from, &to).matrix;
        self.transform_selection(matrix, false, from.min)?;
        let size = to.size();
        self.message = format!("Resized selection to {}w {}l {}h", size.x, size.y, size.z);
        Ok(())
    }

    /// Key edits that carry every selected overlay's plane and corners through a
    /// transform, to apply after the edits that move entity origins.
    pub(crate) fn selected_overlay_edits(&self, matrix: &glam::DMat4) -> Vec<Edit> {
        let Some(document) = &self.document else {
            return Vec::new();
        };
        self.selection
            .iter()
            .filter_map(|owner| match owner {
                Owner::Entity(id) => find_entity(&document.vmf, *id).ok(),
                Owner::Solid(_) => None,
            })
            .flat_map(|entity| overlay_edits(entity, matrix))
            .collect()
    }

    /// The brush ids and entity origins that moving or resizing the selection affects.
    fn selection_parts(&self) -> Result<(Vec<u32>, EntityOrigins), String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        if self.selection.is_empty() {
            return Err("nothing is selected".to_owned());
        }
        let mut solids = Vec::new();
        let mut origins = Vec::new();
        for owner in &self.selection {
            match *owner {
                Owner::Solid(id) => solids.push(id),
                Owner::Entity(id) => {
                    let entity = find_entity(vmf, id)?;
                    solids.extend(entity.solids().filter_map(Solid::id));
                    origins.extend(entity.origin().map(|origin| (id, origin)));
                }
            }
        }
        Ok((solids, origins))
    }

    /// The box that gets resize handles: the pending block with the Block tool, otherwise
    /// the selection, following any move or resize being dragged.
    pub fn handle_box(&self) -> Option<Bounds> {
        match self.tool {
            Tool::Block => self.overlay.block,
            Tool::Entity
            | Tool::Texture
            | Tool::Clip
            | Tool::Vertex
            | Tool::Overlay
            | Tool::Decal
            | Tool::Camera
            | Tool::Magnify => None,
            Tool::Select => {
                let bounds = self.scene.selection_bounds();
                (!bounds.is_empty()).then(|| self.overlay.transform.map_or(bounds, |remap| remap.map_bounds(&bounds)))
            }
        }
    }

    /// The 3D view's camera for the 2D views, when Mallet Options > Viewports shows it.
    pub fn view_camera_preview(&self) -> Option<CameraPreview> {
        let view = &self.config.view;
        view.camera_preview.then(|| CameraPreview {
            position: self.camera.position,
            forward: self.camera.forward(),
            fov: self.camera.fov,
            aspect: self.camera.aspect,
            length: view.camera_preview_length.max(1.0),
        })
    }

    /// The overlay to draw, including the resize handles.
    pub fn display_overlay(&self) -> Overlay {
        let handle_mode = if self.tool == Tool::Select {
            self.overlay.handle_mode
        } else {
            crate::transform::HandleMode::Scale
        };
        Overlay {
            handles: self.handle_box(),
            handle_mode,
            agent_camera: self.agent_camera_marker(),
            view_camera: self.view_camera_preview(),
            ..self.overlay.clone()
        }
    }

    /// Copies the selection with fresh ids, moves the copies by `offset` and selects them.
    ///
    /// A cloned overlay, or other entity that lists brush faces, names the clones of the
    /// faces cloned with it, and keeps the faces that were not cloned.
    pub fn clone_selection(&mut self, offset: DVec3) -> Result<(), String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        if self.selection.is_empty() {
            return Err("nothing is selected".to_owned());
        }
        let mut ids = IdAllocator::for_map(vmf);
        let mut sides = CopiedSides::default();
        let mut edits = Vec::new();
        let mut copies = Vec::new();
        for owner in &self.selection {
            match *owner {
                Owner::Solid(id) => {
                    let (entity, solid) = vmf
                        .solid_with_owner(id)
                        .ok_or_else(|| format!("no brush has id {id}"))?;
                    let entity = entity.id().ok_or("the brush's entity has no id")?;
                    let mut solid = solid.clone();
                    sides.renumber_solid(&mut solid, &mut ids);
                    solid.translate(offset);
                    copies.extend(solid.id().map(Owner::Solid));
                    edits.push(Edit::AddSolid {
                        entity,
                        index: None,
                        solid,
                    });
                }
                Owner::Entity(id) => {
                    let mut entity = find_entity(vmf, id)?.clone();
                    sides.renumber_entity(&mut entity, &mut ids);
                    entity.translate(offset);
                    copies.extend(entity.id().map(Owner::Entity));
                    edits.push(Edit::AddEntity { index: None, entity });
                }
            }
        }
        for edit in &mut edits {
            if let Edit::AddEntity { entity, .. } = edit {
                sides.remap_entity(entity, OtherFaces::Keep);
            }
        }
        self.apply_edit(&Edit::Batch(edits))?;
        self.message = format!("Cloned {} objects", copies.len());
        self.select(copies, SelectMode::Replace);
        Ok(())
    }

    /// Clones the selection where it is, then runs `change` on the copies, the way a
    /// Shift+drag of a resize, rotate or skew handle works in Hammer: the originals stay
    /// and the clone and its change undo together. When `change` fails, the clone is
    /// undone and the originals are selected again. `verb` names the change for the
    /// status bar, such as "rotated".
    pub fn clone_selection_and(
        &mut self,
        verb: &str,
        change: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> Result<(), String> {
        let originals = self.selection.clone();
        self.clone_selection(DVec3::ZERO)?;
        let count = self.selection.len();
        if let Err(error) = change(self) {
            self.step_history(false)?;
            self.select(originals, SelectMode::Replace);
            return Err(error);
        }
        if let Some(document) = self.document.as_mut() {
            document.history.join_latest(2);
        }
        let noun = if count == 1 { "object" } else { "objects" };
        self.message = format!("Cloned and {verb} {count} {noun}");
        Ok(())
    }

    /// Hides the selected brushes and entities, as Hammer's Hide Selection does. Hidden
    /// objects leave the views and the selection until shown again. Undoable. Returns how
    /// many were hidden.
    pub fn hide_selection(&mut self) -> Result<usize, String> {
        if self.selection.is_empty() {
            return Err("nothing is selected".to_owned());
        }
        let (solids, entities) = Self::split_owners(self.selection.iter().copied());
        let count = solids.len() + entities.len();
        self.apply_edit(&Edit::SetHidden {
            solids,
            entities,
            hidden: true,
        })?;
        self.message = format!("Hid {count} objects");
        Ok(count)
    }

    /// Hides every brush and entity that is not selected, as Hammer's Hide Unselected does.
    /// Undoable. Returns how many were hidden.
    pub fn hide_unselected(&mut self) -> Result<usize, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let mut owners = Vec::new();
        if let Some(world) = vmf.world() {
            owners.extend(world.solids().filter_map(|solid| solid.id()).map(Owner::Solid));
        }
        owners.extend(vmf.entities().filter_map(Entity::id).map(Owner::Entity));
        owners.retain(|owner| !self.selection.contains(owner));
        let (solids, entities) = Self::split_owners(owners);
        let count = solids.len() + entities.len();
        if count == 0 {
            self.message = "Nothing to hide".to_owned();
            return Ok(0);
        }
        self.apply_edit(&Edit::SetHidden {
            solids,
            entities,
            hidden: true,
        })?;
        self.message = format!("Hid {count} objects");
        Ok(count)
    }

    /// Shows every brush and entity QuickHide hid again, as Hammer's Unhide QuickHide
    /// Objects does. Objects a VisGroup hides stay hidden. Undoable. Returns how many
    /// were shown.
    pub fn show_all_hidden(&mut self) -> Result<usize, String> {
        if self.document.is_none() {
            return Err("no map is open".to_owned());
        }
        let (solids, entities) = self.quick_hidden_ids();
        let count = solids.len() + entities.len();
        if count == 0 {
            self.message = "QuickHide hides no objects".to_owned();
            return Ok(0);
        }
        self.apply_edit(&Edit::SetHidden {
            solids,
            entities,
            hidden: false,
        })?;
        self.message = format!("Showed {count} hidden objects");
        Ok(count)
    }

    /// Splits owners into the ids of brushes and the ids of entities.
    fn split_owners(owners: impl IntoIterator<Item = Owner>) -> (Vec<u32>, Vec<u32>) {
        let mut solids = Vec::new();
        let mut entities = Vec::new();
        for owner in owners {
            match owner {
                Owner::Solid(id) => solids.push(id),
                Owner::Entity(id) => entities.push(id),
            }
        }
        (solids, entities)
    }

    /// Deletes the selected brushes and entities. Returns how many were removed.
    ///
    /// What stands last in the map is removed first, so nothing that is still to be
    /// removed has to move up, which keeps deleting a large selection quick.
    pub fn delete_selection(&mut self) -> Result<usize, String> {
        if self.selection.is_empty() {
            return Err("nothing is selected".to_owned());
        }
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let mut placed: Vec<(Option<(usize, usize)>, Owner)> = self
            .selection
            .iter()
            .map(|owner| {
                let place = match *owner {
                    Owner::Solid(id) => vmf.solid_position(id),
                    Owner::Entity(id) => vmf.entity_position(id).map(|at| (at, usize::MAX)),
                };
                (place, *owner)
            })
            .collect();
        placed.sort_by_key(|(place, _)| std::cmp::Reverse(*place));
        let edits: Vec<Edit> = placed
            .into_iter()
            .map(|(_, owner)| match owner {
                Owner::Solid(solid) => Edit::RemoveSolid { solid },
                Owner::Entity(entity) => Edit::RemoveEntity { entity },
            })
            .collect();
        let count = edits.len();
        self.apply_edit(&Edit::Batch(edits))?;
        self.message = format!("Deleted {count} objects");
        Ok(count)
    }

    /// Creates a box brush in the world with the current material and selects it.
    /// Returns the new brush's id.
    pub fn create_block(&mut self, min: DVec3, max: DVec3) -> Result<u32, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let world_id = vmf.world().and_then(Entity::id).ok_or("the map has no world")?;
        let mut ids = IdAllocator::for_map(vmf);
        let solid = box_solid(min, max, &self.material, &mut ids).ok_or("the block has no volume")?;
        let id = solid.id().ok_or("the new block has no id")?;
        self.apply_edit(&Edit::AddSolid {
            entity: world_id,
            index: None,
            solid,
        })?;
        self.last_block = Bounds {
            min: min.min(max),
            max: min.max(max),
        };
        self.overlay.block = None;
        self.select([Owner::Solid(id)], SelectMode::Replace);
        self.message = format!("Created block {id}");
        Ok(id)
    }

    /// Creates a point entity at `origin` with an id, the class, its FGD defaults (or zero
    /// angles without game data) and any extra keys, and selects it. Undoable. Returns the new entity's id.
    pub fn create_entity(&mut self, classname: &str, origin: DVec3, keys: &[(String, String)]) -> Result<u32, String> {
        let classname = classname.trim();
        if classname.is_empty() {
            return Err("choose an entity class first".to_owned());
        }
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let id = IdAllocator::for_map(vmf).object();
        let mut entity = Entity::default();
        entity.set("id", id.to_string());
        entity.set("classname", classname);
        for (key, value) in self.class_defaults(classname) {
            entity.set(&key, value);
        }
        entity.set("origin", format_vec3(origin));
        for (key, value) in keys {
            entity.set(key, value.clone());
        }
        self.apply_edit(&Edit::AddEntity { index: None, entity })?;
        self.overlay.entity = None;
        self.select([Owner::Entity(id)], SelectMode::Replace);
        self.message = format!("Created {classname} {id}");
        Ok(id)
    }

    /// Creates an entity of the current class at the entity tool's marker.
    pub fn commit_entity(&mut self) -> Result<u32, String> {
        let origin = self
            .overlay
            .entity
            .ok_or("click in a 2D view to place the entity first")?;
        let classname = self.entity_class.clone();
        self.create_entity(&classname, origin, &self.entity_tool_keys())
    }

    /// Moves the entity tool's marker to a grid-snapped point in a 2D view. The axis
    /// the view looks along keeps the marker's previous value, so clicking in two views
    /// sets all three coordinates.
    pub fn place_entity_marker(&mut self, axis: Axis2D, point: DVec2) {
        let [horizontal, vertical, _] = axis.axes();
        let step = self.snap_step();
        let snapped = (point / step).round() * step;
        let mut marker = self.overlay.entity.unwrap_or(DVec3::ZERO);
        marker[horizontal] = snapped.x;
        marker[vertical] = snapped.y;
        self.overlay.entity = Some(marker);
    }

    /// Creates the block tool's current shape in its pending box.
    pub fn commit_block(&mut self) -> Result<u32, String> {
        let block = self.overlay.block.ok_or("draw a block in a 2D view first")?;
        self.create_primitive(self.block_shape, block.min, block.max)
    }

    /// The box a block-tool drag between two snapped 2D corners describes. As in Hammer,
    /// its depth along the view's axis comes from the selection, or when nothing is
    /// selected, from the last brush selected or block created. `None` when it has no area.
    pub fn block_from_drag(&self, axis: Axis2D, a: DVec2, b: DVec2) -> Option<Bounds> {
        let [horizontal, vertical, _] = axis.axes();
        let depth = self.selection_block().unwrap_or(self.last_block);
        let mut min = depth.min;
        let mut max = depth.max;
        min[horizontal] = a.x.min(b.x);
        max[horizontal] = a.x.max(b.x);
        min[vertical] = a.y.min(b.y);
        max[vertical] = a.y.max(b.y);
        let bounds = Bounds { min, max };
        (bounds.size().min_element() > 0.0).then_some(bounds)
    }

    /// The selection's bounds, when it has volume along every axis, for new blocks to
    /// take their depth from.
    fn selection_block(&self) -> Option<Bounds> {
        let bounds = self.scene.selection_bounds();
        (!self.selection.is_empty() && !bounds.is_empty() && bounds.size().min_element() > 0.0).then_some(bounds)
    }

    pub fn change_grid(&mut self, factor: f64) {
        self.grid = (self.grid * factor).clamp(MIN_GRID, MAX_GRID);
    }

    /// The step drags and new points snap to: the grid while Snap to Grid is on and Alt
    /// is not held, as in Hammer, and otherwise a step too fine to notice.
    pub fn snap_step(&self) -> f64 {
        if self.snap_to_grid && !self.snap_suspended {
            self.grid
        } else {
            FREE_STEP
        }
    }

    /// The grid size the 2D views draw, or 0 when Show Grid is off.
    pub fn shown_grid(&self) -> f64 {
        if self.show_grid {
            self.grid
        } else {
            0.0
        }
    }

    /// Fits the whole map into the three 2D views.
    pub fn fit_2d(&mut self) {
        for index in 0..self.views.len() {
            let rect = self.view_rect(index);
            self.views[index].fit(&self.scene.bounds, rect);
        }
    }

    /// The rectangle of the slot showing a 2D view, or of some slot when none shows it.
    fn view_rect(&self, view_index: usize) -> Rect {
        let slot = self
            .layout
            .iter()
            .position(|mode| mode.view_index() == Some(view_index))
            .unwrap_or(1);
        self.viewport_rects[slot]
    }

    /// The viewport slot a screenshot or input target refers to: `camera` is the first
    /// 3D slot, `top`, `front` and `side` the first slot showing that view.
    pub fn slot_for(&self, target: ShotTarget) -> Result<usize, String> {
        let (name, slot) = match target {
            ShotTarget::All => return Err("pick one viewport: camera, top, front or side".to_owned()),
            ShotTarget::Camera => (
                "camera",
                self.layout.iter().position(|mode| mode.camera_style().is_some()),
            ),
            ShotTarget::Top => ("top", self.layout.iter().position(|mode| *mode == ViewMode::Top)),
            ShotTarget::Front => ("front", self.layout.iter().position(|mode| *mode == ViewMode::Front)),
            ShotTarget::Side => ("side", self.layout.iter().position(|mode| *mode == ViewMode::Side)),
        };
        slot.ok_or_else(|| format!("no viewport currently shows the {name} view"))
    }

    /// Moves the 3D camera back until the whole map is in view.
    /// Fits the map into the 2D views, and puts the 3D camera where the map was saved
    /// from, or back far enough to see all of it.
    fn frame_map(&mut self) {
        self.fit_2d();
        let saved = self.document.as_ref().and_then(|document| document.vmf.active_camera());
        match saved {
            Some(camera) => self.camera.set_view(camera.position, camera.look),
            None => self.fit_3d(),
        }
    }

    pub fn fit_3d(&mut self) {
        self.camera.look_at(&self.scene.bounds);
    }

    /// Mounts a game folder, the one holding `gameinfo.txt`, so materials show their
    /// textures. Returns how many search paths were mounted.
    pub fn set_game_dir(&mut self, game_dir: &Path) -> Result<usize, String> {
        let skipped = self.config.unmounted_games.clone();
        let filesystem = GameFs::with_mounts(game_dir, &|name| {
            !skipped.iter().any(|skip| skip.eq_ignore_ascii_case(name))
        })
        .map_err(|error| error.to_string())?;
        let mounts = filesystem.mounts().len();
        self.materials = Some(Materials::new(filesystem));
        self.models.clear();
        self.decal_sizes.clear();
        self.model_errors.clear();
        if let Some(gpu) = &mut self.gpu {
            gpu.clear_textures();
        }
        let fgd = default_fgd(game_dir);
        self.game_data = fgd.and_then(|fgd| GameData::load(&[fgd]).ok()).map(Arc::new);
        self.reload_texture_lights();
        self.rebuild_scene();
        self.message = format!("Using game content from {} ({mounts} search paths)", game_dir.display());
        Ok(mounts)
    }

    /// Pans every 2D view so a point is in its middle, keeping the zoom.
    pub fn center_2d_on(&mut self, point: DVec3) {
        for view in &mut self.views {
            view.center = view.axis.project(point);
        }
    }

    /// Mounts a game folder and remembers it. Another game's FGD choice would not fit,
    /// so the new game uses its own FGD.
    pub fn choose_game(&mut self, game_dir: &Path) -> Result<usize, String> {
        let mounts = self.set_game_dir(game_dir)?;
        self.config.game_dir = Some(game_dir.to_owned());
        self.config.fgds.clear();
        self.save_config();
        Ok(mounts)
    }

    /// Reads entity classes from FGD files and remembers the choice; an empty list goes
    /// back to the game's default FGD.
    pub fn choose_fgds(&mut self, paths: Vec<PathBuf>) -> Result<usize, String> {
        let count = if paths.is_empty() {
            let fgd = self.game_dir().and_then(default_fgd);
            self.game_data = fgd.and_then(|fgd| GameData::load(&[fgd]).ok()).map(Arc::new);
            self.rebuild_scene();
            self.game_data.as_ref().map_or(0, |data| data.len())
        } else {
            self.set_fgds(&paths)?
        };
        self.config.fgds = paths;
        self.save_config();
        Ok(count)
    }

    /// Puts a map at the top of File > Recent Maps and saves the list.
    pub fn remember_map(&mut self, path: &Path) {
        self.config.remember_map(path);
        self.save_config();
    }

    /// Switches the 3D gizmo to its next mode and the 2D views' handles to match, then
    /// saves the choice.
    pub fn cycle_gizmo_mode(&mut self) {
        let mode = self.config.transform.gizmo_mode.next();
        self.config.transform.gizmo_mode = mode;
        self.overlay.handle_mode = crate::transform::HandleMode::for_gizmo(mode);
        self.message = format!("Gizmo: {}", mode.label());
        self.save_config();
    }

    /// Writes the settings, reporting a failure in the status bar.
    pub fn save_config(&mut self) {
        if let Err(error) = self.config.save() {
            self.message = format!("Could not save settings: {error}");
        }
    }

    /// Reads entity classes from FGD files instead of the game's default FGD. Returns
    /// how many classes were read.
    pub fn set_fgds(&mut self, paths: &[PathBuf]) -> Result<usize, String> {
        let data = GameData::load(paths)?;
        let count = data.len();
        self.game_data = Some(Arc::new(data));
        self.rebuild_scene();
        self.message = format!("Read {count} entity classes");
        Ok(count)
    }

    /// Loads, in parallel, every prop model the map uses that is not loaded yet.
    fn ensure_models<'a>(&mut self, entities: impl Iterator<Item = &'a Entity>) {
        let Some(materials) = &self.materials else {
            return;
        };
        let game_data = self.game_data.as_deref();
        let mut missing: Vec<String> = entities
            .filter_map(|entity| entity_model(entity, game_data.and_then(|data| data.class(entity.classname()))))
            .map(model_key)
            .filter(|key| !self.models.contains_key(key))
            .collect();
        missing.sort();
        missing.dedup();
        if missing.is_empty() {
            return;
        }
        let loader = materials.loader();
        let loaded = parallel_map(&missing, |key| loader.model(key));
        for (key, result) in missing.into_iter().zip(loaded) {
            match result {
                Ok(model) => {
                    self.models.insert(key, Some(Arc::new(model)));
                }
                Err(error) => {
                    self.model_errors.insert(key.clone(), error);
                    self.models.insert(key, None);
                }
            }
        }
    }

    /// Looks up the size of every decal texture in the map that has not been seen yet,
    /// so the scene can draw each `infodecal` as large as the game does: the texture's
    /// size times the material's `$decalscale`.
    fn ensure_decal_sizes<'a>(&mut self, entities: impl Iterator<Item = &'a Entity>) {
        let Some(materials) = &mut self.materials else {
            return;
        };
        let mut missing: Vec<String> = entities
            .filter(|entity| entity.classname().eq_ignore_ascii_case(DECAL_CLASS))
            .filter_map(|entity| entity.get("texture"))
            .map(decal_key)
            .filter(|key| !key.is_empty() && !self.decal_sizes.contains_key(key))
            .collect();
        missing.sort();
        missing.dedup();
        if missing.is_empty() {
            return;
        }
        let names: Vec<&str> = missing.iter().map(String::as_str).collect();
        let sizes: Vec<Option<DVec2>> = materials
            .materials(&names)
            .iter()
            .map(|material| picture_size(material, f64::from(material.decal_scale)))
            .collect();
        self.decal_sizes.extend(missing.into_iter().zip(sizes));
    }

    /// The mounted game folder, if any.
    pub fn game_dir(&self) -> Option<&Path> {
        self.materials
            .as_ref()
            .map(|materials| materials.filesystem().game_dir())
    }

    /// The size in physical pixels of a viewport slot.
    pub fn slot_pixels(&self, slot: usize) -> [u32; 2] {
        let size = self.viewport_rects[slot].size() * self.pixels_per_point;
        [size.x.round().max(1.0) as u32, size.y.round().max(1.0) as u32]
    }

    /// Records the window's viewport layout. A map opened before the first layout is
    /// framed again once the real viewport sizes are known.
    pub fn set_layout(&mut self, rects: [Rect; 4], pixels_per_point: f32) {
        self.viewport_rects = rects;
        self.pixels_per_point = pixels_per_point;
        if self.refit_on_layout {
            self.refit_on_layout = false;
            self.frame_map();
        }
    }

    /// A point in a window screenshot, in pixels, as a point in the window.
    pub fn window_point(&self, at: DVec2) -> Pos2 {
        let scale = self.pixels_per_point.max(0.1);
        Pos2::new(at.x as f32 / scale, at.y as f32 / scale)
    }

    /// Converts a viewport position to a window position. In the 2D views `point` is in
    /// world units on the view's two axes; in the camera view it is pixels from the
    /// viewport's top-left corner.
    pub fn viewport_point(&self, target: ShotTarget, point: DVec2) -> Result<Pos2, String> {
        let slot = self.slot_for(target)?;
        let rect = self.viewport_rects[slot];
        Ok(match self.layout[slot].view_index() {
            None => rect.min + Vec2::new(point.x as f32, point.y as f32),
            Some(view) => self.views[view].to_screen(rect, point),
        })
    }

    /// Renders viewports offscreen at their on-screen size and encodes them as PNG.
    ///
    /// Uses the GPU renderer the window uses once it exists, so screenshots match the
    /// window, and the CPU drawing code before that. Neither needs the window to be
    /// visible. Viewport labels are left out.
    pub fn render_png(&mut self, target: ShotTarget) -> Result<Vec<u8>, String> {
        if target != ShotTarget::All {
            let slot = self.slot_for(target)?;
            let (canvas, scale) = self.render_any(slot)?;
            if scale == 1.0 {
                return canvas.encode_png();
            }
            let size = self.viewport_rects[slot].size();
            let mut scaled = canvas_for(size)?;
            scaled.draw_canvas_scaled(&canvas, 0.0, 0.0, scale);
            return scaled.encode_png();
        }
        let rects = self.viewport_rects;
        let area = rects[0].union(rects[3]);
        let mut canvas = canvas_for(area.size())?;
        canvas.fill_all(GAP_COLOR);
        for (index, rect) in rects.iter().enumerate() {
            let offset = rect.min - area.min;
            let (image, scale) = self.render_any(index)?;
            canvas.draw_canvas_scaled(&image, offset.x.round(), offset.y.round(), scale);
        }
        canvas.encode_png()
    }

    /// Renders one viewport, on the GPU when possible, and returns it with the scale that
    /// brings it to layout-point size.
    fn render_any(&mut self, slot: usize) -> Result<(PixmapCanvas, f32), String> {
        let mode = self.layout[slot];
        let gpu_image = match (mode.camera_style(), mode.view_index()) {
            (Some(style), _) => self
                .render_camera_gpu(slot, style)
                .map(|image| (image, 1.0 / self.pixels_per_point)),
            (None, Some(view)) => self.render_view_gpu(slot, view).map(|image| (image, 1.0)),
            (None, None) => None,
        };
        match gpu_image {
            Some(image) => Ok(image),
            None => Ok((self.render_viewport(slot)?, 1.0)),
        }
    }

    /// Renders a 2D view on the GPU at layout-point size, with its handles and marquee
    /// drawn on top, and reads it back.
    fn render_view_gpu(&mut self, slot: usize, view: usize) -> Option<PixmapCanvas> {
        let overlay = self.display_overlay();
        let points = self.viewport_rects[slot].size();
        let pixels = [points.x.round().max(1.0) as u32, points.y.round().max(1.0) as u32];
        let grid = self.shown_grid();
        let gpu = self.gpu.as_mut()?;
        gpu.render_2d(
            slot,
            &self.views[view],
            points,
            pixels,
            &self.scene,
            &overlay,
            grid,
        );
        let (width, height, rgba) = gpu.read_pixels(slot)?;
        let mut canvas = PixmapCanvas::from_rgba(width, height, rgba)?;
        let local = Rect::from_min_size(Pos2::ZERO, points);
        self.views[view].draw_overlay(&mut canvas, local, &overlay, false);
        Some(canvas)
    }

    /// Renders a 3D slot on the GPU, when the window has created the renderer, and reads
    /// it back.
    fn render_camera_gpu(&mut self, slot: usize, style: CameraStyle) -> Option<PixmapCanvas> {
        let overlay = self.display_overlay();
        let size = self.slot_pixels(slot);
        let lighting = LightingInput {
            options: &self.config.lighting,
            texture_lights: &self.texture_lights,
        };
        let gpu = self.gpu.as_mut()?;
        gpu.render(
            slot,
            style,
            &self.scene,
            &overlay,
            &self.camera,
            size,
            self.materials.as_mut(),
            &self.models,
            lighting,
        );
        let mut rounds = 0;
        while (gpu.textures_loading() > 0 || gpu.lighting_pending()) && rounds < SCREENSHOT_ROUNDS {
            gpu.finish_loading();
            gpu.render(
                slot,
                style,
                &self.scene,
                &overlay,
                &self.camera,
                size,
                self.materials.as_mut(),
                &self.models,
                lighting,
            );
            rounds += 1;
        }
        let (width, height, rgba) = gpu.read_pixels(slot)?;
        PixmapCanvas::from_rgba(width, height, rgba)
    }

    /// Draws a slot on the CPU, the fallback before the GPU renderer exists.
    fn render_viewport(&self, slot: usize) -> Result<PixmapCanvas, String> {
        let size = self.viewport_rects[slot].size();
        let local = Rect::from_min_size(Pos2::ZERO, size);
        let mut canvas = canvas_for(size)?;
        let overlay = self.display_overlay();
        match self.layout[slot].view_index() {
            None => self.camera.draw(&mut canvas, local, &self.scene, false, &overlay),
            Some(view) => self.views[view].draw(&mut canvas, local, &self.scene, self.shown_grid(), false, &overlay),
        }
        Ok(canvas)
    }
}

/// Entity ids paired with their origins.
type EntityOrigins = Vec<(u32, DVec3)>;

fn origin_edits(origins: EntityOrigins, map: impl Fn(DVec3) -> DVec3) -> impl Iterator<Item = Edit> {
    origins.into_iter().map(move |(entity, origin)| Edit::SetKey {
        entity,
        key: "origin".to_owned(),
        value: format_vec3(map(origin)),
    })
}

/// The size of a material's picture at `scale` world units per texel, or `None` when it
/// has no texture.
pub(crate) fn picture_size(material: &mallet_content::Material, scale: f64) -> Option<DVec2> {
    let [width, height] = material.size?;
    Some(DVec2::new(f64::from(width), f64::from(height)) * scale)
}

/// Key edits that carry one entity's overlay through a transform. Entities that are not
/// placed overlays give none.
pub(crate) fn overlay_edits(entity: &Entity, matrix: &glam::DMat4) -> Vec<Edit> {
    let Some(id) = entity.id() else {
        return Vec::new();
    };
    mallet_vmf::overlay::transform_keys(entity, matrix)
        .into_iter()
        .map(|(key, value)| Edit::SetKey {
            entity: id,
            key: key.to_owned(),
            value,
        })
        .collect()
}

pub(crate) fn find_entity(vmf: &Vmf, id: u32) -> Result<&Entity, String> {
    vmf.entity_by_id(id).ok_or_else(|| format!("no entity has id {id}"))
}

fn canvas_for(size: Vec2) -> Result<PixmapCanvas, String> {
    PixmapCanvas::new(size.x.round() as u32, size.y.round() as u32).ok_or_else(|| "the viewport has no area".to_owned())
}

/// Splits the area into Hammer's default layout: camera top-left, top view top-right,
/// front view bottom-left and side view bottom-right.
pub fn quad_rects(full: Rect, gap: f32) -> [Rect; 4] {
    let mid = full.center();
    let half = gap * 0.5;
    [
        Rect::from_min_max(full.min, Pos2::new(mid.x - half, mid.y - half)),
        Rect::from_min_max(Pos2::new(mid.x + half, full.min.y), Pos2::new(full.max.x, mid.y - half)),
        Rect::from_min_max(Pos2::new(full.min.x, mid.y + half), Pos2::new(mid.x - half, full.max.y)),
        Rect::from_min_max(Pos2::new(mid.x + half, mid.y + half), full.max),
    ]
}

#[cfg(test)]
mod pick_mode_tests {
    use super::*;

    /// Two brushes pasted as one Hammer group, selected.
    fn editor_with_group() -> (Editor, Vec<Owner>) {
        let mut editor = Editor::default();
        editor.new_map();
        editor.create_block(DVec3::splat(-64.0), DVec3::splat(64.0)).unwrap();
        editor.create_block(DVec3::splat(128.0), DVec3::splat(192.0)).unwrap();
        editor.select_all();
        editor.copy_selection().unwrap();
        let options = PasteOptions {
            copies: 1,
            offset: DVec3::new(0.0, 512.0, 0.0),
            start_at_original: true,
            group: true,
            ..PasteOptions::default()
        };
        editor.paste(PasteAt::Original, &options).unwrap();
        let pasted: Vec<Owner> = editor.selection.iter().copied().collect();
        assert_eq!(pasted.len(), 2);
        (editor, pasted)
    }

    #[test]
    fn groups_mode_selects_the_whole_group() {
        let (mut editor, pasted) = editor_with_group();
        editor.select_picked([pasted[0]], SelectMode::Replace);
        assert_eq!(editor.selection.len(), 2, "the click brings the whole group along");
        assert!(editor.scene.meshes.iter().filter(|mesh| mesh.selected).count() >= 2);
    }

    #[test]
    fn objects_and_solids_modes_select_one_object() {
        let (mut editor, pasted) = editor_with_group();
        editor.set_pick_mode(PickMode::Objects);
        editor.select_picked([pasted[0]], SelectMode::Replace);
        assert_eq!(editor.selection.len(), 1);
        editor.set_pick_mode(PickMode::Solids);
        editor.select_picked([pasted[1]], SelectMode::Replace);
        assert_eq!(editor.selection.len(), 1);
        assert!(editor.solid_picks());
    }

    #[test]
    fn ctrl_w_cycles_through_the_three_modes() {
        let (mut editor, _) = editor_with_group();
        assert_eq!(editor.pick_mode, PickMode::Groups);
        editor.cycle_pick_mode();
        assert_eq!(editor.pick_mode, PickMode::Objects);
        editor.cycle_pick_mode();
        assert_eq!(editor.pick_mode, PickMode::Solids);
        editor.cycle_pick_mode();
        assert_eq!(editor.pick_mode, PickMode::Groups);
    }
}

#[cfg(test)]
mod hide_tests {
    use super::*;

    #[test]
    fn hidden_objects_leave_the_views_and_return_when_shown() {
        let mut editor = Editor::default();
        editor.new_map();
        let first = editor.create_block(DVec3::splat(-64.0), DVec3::splat(64.0)).unwrap();
        let _second = editor.create_block(DVec3::splat(128.0), DVec3::splat(192.0)).unwrap();
        let entity = editor.create_entity("info_target", DVec3::ZERO, &[]).unwrap();

        editor.select([Owner::Solid(first)], SelectMode::Replace);
        assert_eq!(editor.hide_selection().unwrap(), 1);
        assert!(!editor.scene.owners().contains(&Owner::Solid(first)));
        assert!(editor.selection.is_empty(), "a hidden object leaves the selection");

        assert_eq!(editor.hide_unselected().unwrap(), 2);
        assert!(editor.scene.owners().is_empty());

        assert_eq!(editor.show_all_hidden().unwrap(), 3);
        let owners = editor.scene.owners();
        assert!(owners.contains(&Owner::Solid(first)));
        assert!(owners.contains(&Owner::Entity(entity)));

        assert!(editor.step_history(false).unwrap());
        assert!(editor.scene.owners().is_empty());
    }
}

#[cfg(test)]
mod scene_update_tests {
    use super::*;

    /// A map with a grid of brushes and a row of lights: large enough that an edit
    /// updates the scene where it changed instead of building it anew.
    fn editor_with_grid() -> (Editor, Vec<u32>, Vec<u32>) {
        let mut editor = Editor::default();
        editor.new_map();
        let mut brushes = Vec::new();
        let mut lights = Vec::new();
        for x in 0..8 {
            for y in 0..8 {
                let min = DVec3::new(f64::from(x) * 256.0, f64::from(y) * 256.0, 0.0);
                brushes.push(editor.create_block(min, min + DVec3::splat(128.0)).unwrap());
            }
            let origin = DVec3::new(f64::from(x) * 256.0 + 64.0, 64.0, 256.0);
            lights.push(editor.create_entity("light", origin, &[]).unwrap());
        }
        editor.clear_selection();
        (editor, brushes, lights)
    }

    /// Checks that the scene an edit left behind shows the same as one built anew from
    /// the map, and that the edit did not build it anew itself.
    #[track_caller]
    fn check(editor: &Editor, step: &str) {
        let document = editor.document.as_ref().unwrap();
        let fresh = Scene::build(
            &document.vmf,
            &editor.models,
            editor.game_data.as_deref(),
            &editor.decal_sizes,
        );
        assert!(editor.scene.outline() == fresh.outline(), "the scene is out of date after {step}");
        assert_eq!(
            (editor.scene.solid_count, editor.scene.entity_count),
            (fresh.solid_count, fresh.entity_count),
            "after {step}"
        );
        assert!(!editor.scene.retextures.is_empty(), "{step} built the whole scene anew");
    }

    /// The side id of the top face of a brush and the middle of that face.
    fn top_face(editor: &Editor, brush: u32) -> (u32, DVec3) {
        let mesh = editor
            .scene
            .meshes
            .iter()
            .find(|mesh| mesh.solid == Some(brush))
            .unwrap();
        let face = mesh.faces.iter().find(|face| face.normal.z > 0.9).unwrap();
        let middle = face.vertices.iter().sum::<DVec3>() / face.vertices.len() as f64;
        (face.side.unwrap(), middle)
    }

    #[test]
    fn moves_clones_and_deletes_update_the_scene_in_place() {
        let (mut editor, brushes, _) = editor_with_grid();
        editor.select([Owner::Solid(brushes[0])], SelectMode::Replace);
        editor.move_selection(DVec3::new(16.0, 0.0, 0.0)).unwrap();
        check(&editor, "a move");
        editor.clone_selection(DVec3::new(0.0, 0.0, 256.0)).unwrap();
        check(&editor, "a clone");
        assert_eq!(editor.selection.len(), 1, "the clone is selected");
        assert_eq!(editor.delete_selection().unwrap(), 1);
        check(&editor, "a delete");
        for step in ["undoing the delete", "undoing the clone", "undoing the move"] {
            assert!(editor.step_history(false).unwrap());
            check(&editor, step);
        }
        for step in ["redoing the move", "redoing the clone", "redoing the delete"] {
            assert!(editor.step_history(true).unwrap());
            check(&editor, step);
        }
        editor.select([Owner::Solid(brushes[7])], SelectMode::Replace);
        let bounds = editor.scene.selection_bounds();
        editor
            .resize_selection(Bounds {
                min: bounds.min,
                max: bounds.max + DVec3::new(64.0, 32.0, 16.0),
            })
            .unwrap();
        check(&editor, "a resize");
    }

    #[test]
    fn brush_entities_hollows_and_hiding_update_the_scene_in_place() {
        let (mut editor, brushes, _) = editor_with_grid();
        editor.select([Owner::Solid(brushes[1]), Owner::Solid(brushes[2])], SelectMode::Replace);
        let detail = editor.tie_to_new_entity("func_detail").unwrap();
        check(&editor, "tying to an entity");
        editor.select([Owner::Entity(detail)], SelectMode::Replace);
        editor.move_selection(DVec3::new(0.0, 0.0, 64.0)).unwrap();
        check(&editor, "moving a brush entity");
        editor.move_to_world().unwrap();
        check(&editor, "moving to the world");
        assert!(editor.step_history(false).unwrap());
        check(&editor, "undoing the move to the world");

        editor.select([Owner::Solid(brushes[4]), Owner::Entity(detail)], SelectMode::Replace);
        assert_eq!(editor.hide_selection().unwrap(), 2);
        check(&editor, "hiding");
        assert_eq!(editor.show_all_hidden().unwrap(), 2);
        check(&editor, "showing");

        editor.select([Owner::Solid(brushes[3])], SelectMode::Replace);
        assert!(editor.make_hollow(8.0).unwrap() > 1);
        check(&editor, "making hollow");
        assert!(editor.step_history(false).unwrap());
        check(&editor, "undoing the hollow");
    }

    #[test]
    fn entity_keys_overlays_and_decals_update_the_scene_in_place() {
        let (mut editor, brushes, lights) = editor_with_grid();
        editor.set_key_on(&[lights[0]], "origin", "8 16 320").unwrap();
        check(&editor, "setting an origin");
        editor.set_key_on(&[lights[1]], "classname", "info_target").unwrap();
        check(&editor, "setting a classname");

        let (side, middle) = top_face(&editor, brushes[5]);
        let overlay = editor
            .create_overlay(side, middle, "decals/test", Some(glam::DVec2::splat(32.0)))
            .unwrap();
        check(&editor, "placing an overlay");
        editor.select([Owner::Solid(brushes[5])], SelectMode::Replace);
        editor.move_selection(DVec3::new(0.0, 0.0, 32.0)).unwrap();
        check(&editor, "moving a brush under an overlay");
        editor.select([Owner::Entity(overlay)], SelectMode::Replace);
        editor.move_selection(DVec3::new(16.0, 0.0, 0.0)).unwrap();
        check(&editor, "moving an overlay");

        let (_, middle) = top_face(&editor, brushes[6]);
        let decal = editor.create_decal(middle + DVec3::Z, "decals/test").unwrap();
        check(&editor, "placing a decal");
        editor.select([Owner::Solid(brushes[6])], SelectMode::Replace);
        editor.move_selection(DVec3::new(0.0, 0.0, 256.0)).unwrap();
        check(&editor, "moving a brush out from under a decal");
        assert!(editor.step_history(false).unwrap());
        check(&editor, "moving it back");
        editor.select([Owner::Entity(decal), Owner::Solid(brushes[6])], SelectMode::Replace);
        assert_eq!(editor.delete_selection().unwrap(), 2);
        check(&editor, "deleting a decal and its brush");
    }
}
