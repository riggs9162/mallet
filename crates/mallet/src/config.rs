//! Settings kept between runs, stored as JSON in `%APPDATA%\Mallet\config.json`, and the
//! programs Mallet runs to compile maps.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Game folders tried, in order, inside each Steam library when no game is configured.
const KNOWN_GAMES: [&str; 5] = [
    "GarrysMod/garrysmod",
    "Half-Life 2/hl2",
    "Source SDK Base 2013 Singleplayer/hl2",
    "Source SDK Base 2013 Multiplayer/hl2mp",
    "Counter-Strike Source/cstrike",
];

/// Where Steam is installed unless Mallet Options says otherwise.
#[cfg(windows)]
pub const DEFAULT_STEAM_DIR: &str = "C:/Program Files (x86)/Steam";

/// The Steam folders tried when `steam_dir` is not set, in order.
#[cfg(windows)]
pub fn default_steam_dirs() -> Vec<PathBuf> {
    vec![PathBuf::from(DEFAULT_STEAM_DIR)]
}

/// Where Steam is installed unless Mallet Options says otherwise: the link that Linux
/// installs of Steam create in the home folder.
#[cfg(not(windows))]
pub const DEFAULT_STEAM_DIR: &str = "~/.steam/steam";

/// The Steam folders tried when `steam_dir` is not set, in order: the link in the home
/// folder, then the folder a manual install uses.
#[cfg(not(windows))]
pub fn default_steam_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".steam/steam"));
        dirs.push(home.join(".local/share/Steam"));
    }
    dirs
}

/// The folder Mallet's own folder sits in: `%APPDATA%`.
#[cfg(windows)]
fn platform_data_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

/// The folder Mallet's own folder sits in: `$XDG_CONFIG_HOME`, or `~/.config` when that
/// is not set.
#[cfg(not(windows))]
fn platform_data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
}

/// Settings kept between runs. Missing fields take their defaults, so older files load.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The game folder holding `gameinfo.txt`, such as `.../GarrysMod/garrysmod`.
    pub game_dir: Option<PathBuf>,
    /// Steam's install folder, searched with its libraries for a game to use; `None` for
    /// the usual place.
    pub steam_dir: Option<PathBuf>,
    /// FGD files chosen instead of the game's default one; empty to use the default.
    pub fgds: Vec<PathBuf>,
    /// Games listed in Garry's Mod's `cfg/mount.cfg` that Mallet leaves unmounted.
    pub unmounted_games: Vec<String>,
    /// Maps opened or saved lately, the latest first.
    pub recent_maps: Vec<PathBuf>,
    /// Programs chosen for each compile tool; a missing one is found automatically.
    pub tools: ToolPaths,
    pub compile: CompileOptions,
    pub view: ViewOptions,
    pub colors: ColorOptions,
    pub editing: EditingOptions,
    pub files: FileOptions,
    pub texture_browser: TextureBrowserOptions,
    /// What the texture browser shows, as it was last left.
    pub texture_browser_view: TextureBrowserView,
    pub interface: InterfaceOptions,
    pub agents: AgentOptions,
    pub transform: TransformOptions,
    pub lighting: LightingOptions,
}

/// How the 3D Lighting Preview viewport lights the map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LightingOptions {
    /// Read `_lightHDR`, `_ambientHDR` and the HDR scales, as an HDR compile does; off
    /// reads the LDR values.
    pub hdr: bool,
    /// Brightness of the picture in stops, from -6 to 6: +1 doubles it.
    pub exposure: f64,
    /// How light brighter than full white looks.
    pub tonemap: crate::lighting::Tonemap,
    /// How finely brush faces are lit: per luxel at their lightmap scale, or per pixel.
    pub detail: crate::lighting::LightingDetail,
    /// Light every surface gets, from 0 to 1, so unlit corners are not black.
    pub fill: f64,
    /// Fakes light bouncing off walls: each light also adds this share of its light,
    /// without turning towards surfaces, wherever it reaches. 0 turns it off.
    pub bounce: f64,
    /// light_environment and light_directional sunlight.
    pub sun: bool,
    /// light_environment's ambient sky light.
    pub sky_light: bool,
    /// Directions sky light is traced from to find what can see the sky, from 0 to 32;
    /// 0 lets sky light reach everywhere.
    pub sky_samples: u32,
    /// light_dynamic and env_projectedtexture.
    pub dynamic_lights: bool,
    /// Faces whose material is listed in `lights.rad` give off light, as VRAD's texture
    /// lights do.
    pub texture_lights: bool,
    /// `$selfillum` materials glow.
    pub self_illum: bool,
    /// Show lights that start dark (the "Initially dark" flag) as if they were on.
    pub dark_lights: bool,
    /// Play lights' appearances (flicker, pulse, strobe and custom patterns).
    pub animate: bool,
    /// Shadows from the sun and directional lights.
    pub sun_shadows: bool,
    /// Pixels across the sun's shadow map, which covers the whole map: larger is sharper.
    pub sun_shadow_size: u32,
    /// Shadows from point, spot and projected lights.
    pub light_shadows: bool,
    /// How many of those lights cast shadows, those nearest and brightest around the
    /// camera first.
    pub shadowed_lights: u32,
    /// Pixels across each light's shadow maps.
    pub shadow_size: u32,
    /// How many lights past those get small shadow maps, drawn a few at a time, so they
    /// do not shine through walls either.
    pub more_shadowed_lights: u32,
    /// Pixels across each small shadow map.
    pub small_shadow_size: u32,
    /// How soft shadow edges are, in shadow map pixels.
    pub shadow_softness: f64,
    /// Props cast shadows.
    pub prop_shadows: bool,
    /// What sky faces show.
    pub sky: crate::lighting::SkyMode,
    /// env_fog_controller's fog.
    pub fog: bool,
    /// Glow sprites, light glows, the sun's glow and spotlight beams.
    pub glows: bool,
    /// Hide faces that are invisible in the game: nodraw, clip, trigger, hint, skip and
    /// the other tool textures.
    pub hide_tools: bool,
    /// Point entity boxes, and the editor models some classes show instead.
    pub entity_boxes: bool,
    /// Which lights show their range, cone or direction.
    pub helpers: crate::lighting::HelperMode,
    /// Lights end where they are dimmer than this share of full brightness: lower is more
    /// exact but slower.
    pub cutoff: f64,
    /// The most lights that may light one spot; the brightest are kept.
    pub lights_per_area: u32,
}

impl Default for LightingOptions {
    fn default() -> Self {
        Self {
            hdr: true,
            exposure: 0.0,
            tonemap: crate::lighting::Tonemap::Clamp,
            detail: crate::lighting::LightingDetail::Luxels,
            fill: 0.0,
            bounce: 0.15,
            sun: true,
            sky_light: true,
            sky_samples: 12,
            dynamic_lights: true,
            texture_lights: true,
            self_illum: true,
            dark_lights: false,
            animate: false,
            sun_shadows: true,
            sun_shadow_size: 2048,
            light_shadows: true,
            shadowed_lights: 16,
            shadow_size: 512,
            more_shadowed_lights: 240,
            small_shadow_size: 128,
            shadow_softness: 1.0,
            prop_shadows: true,
            sky: crate::lighting::SkyMode::Skybox,
            fog: true,
            glows: true,
            hide_tools: true,
            entity_boxes: true,
            helpers: crate::lighting::HelperMode::Selected,
            cutoff: 0.004,
            lights_per_area: 64,
        }
    }
}

impl LightingOptions {
    /// The exposure as a multiplier, from 1/64 to 64.
    pub fn exposure_scale(&self) -> f64 {
        2f64.powf(finite_or(self.exposure, 0.0).clamp(-6.0, 6.0))
    }

    /// The fill light, from 0 to 1.
    pub fn fill_light(&self) -> f64 {
        finite_or(self.fill, 0.0).clamp(0.0, 1.0)
    }

    /// The fake bounce share, from 0 to 1.
    pub fn bounce_share(&self) -> f64 {
        finite_or(self.bounce, 0.0).clamp(0.0, 1.0)
    }

    /// Sky light directions, from 0 to 32.
    pub fn sky_sample_count(&self) -> u32 {
        self.sky_samples.min(32)
    }

    /// The sun's shadow map size, a power of two from 512 to 8192.
    pub fn sun_map_size(&self) -> u32 {
        self.sun_shadow_size.clamp(512, 8192).next_power_of_two().min(8192)
    }

    /// Lights with shadows, from 0 to 64.
    pub fn shadowed_light_count(&self) -> usize {
        self.shadowed_lights.min(64) as usize
    }

    /// Each light's shadow map size, a power of two from 128 to 2048.
    pub fn light_map_size(&self) -> u32 {
        self.shadow_size.clamp(128, 2048).next_power_of_two().min(2048)
    }

    /// Lights with small shadow maps, from 0 to 1024.
    pub fn more_shadowed_count(&self) -> usize {
        self.more_shadowed_lights.min(1024) as usize
    }

    /// Each small shadow map's size, a power of two from 32 to 256.
    pub fn small_map_size(&self) -> u32 {
        self.small_shadow_size.clamp(32, 256).next_power_of_two().min(256)
    }

    /// Shadow softness in shadow map pixels, from 0 to 4.
    pub fn softness(&self) -> f64 {
        finite_or(self.shadow_softness, 1.0).clamp(0.0, 4.0)
    }

    /// The light cutoff, from 0.0005 to 0.05.
    pub fn light_cutoff(&self) -> f64 {
        finite_or(self.cutoff, 0.004).clamp(0.0005, 0.05)
    }

    /// Lights per spot, from 4 to 256.
    pub fn lights_per_spot(&self) -> usize {
        self.lights_per_area.clamp(4, 256) as usize
    }
}

/// `value`, or `fallback` when it is not a number.
fn finite_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

/// How selections are moved, rotated and scaled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransformOptions {
    /// Textures move and turn with brushes (Hammer's texture lock, Shift+L).
    pub texture_lock: bool,
    /// Textures also stretch when brushes are scaled (Hammer's texture scale lock).
    pub texture_scale_lock: bool,
    /// Degrees a snapped rotation turns in; 0 turns snapping off.
    pub rotation_snap: f64,
    /// Whether rotations snap to [`Self::rotation_snap`] without Shift. Holding Shift
    /// does the opposite.
    pub snap_rotation: bool,
    /// Whether the 3D view shows the transform gizmo on the selection.
    pub gizmo: bool,
    /// What dragging the gizmo does.
    pub gizmo_mode: crate::transform::GizmoMode,
    /// Arrow keys over a 2D view nudge the selection by one grid step (Ctrl: one unit).
    pub arrow_nudge: bool,
}

impl TransformOptions {
    /// The step a rotation snaps to, if any, with Shift held or not. Shift flips the
    /// rotation snap toggle for the length of the drag.
    pub fn angle_step(&self, shift: bool) -> Option<f64> {
        (self.snap_rotation != shift && self.rotation_snap > 0.0).then_some(self.rotation_snap)
    }

    /// The status bar hint for what Shift does to a rotation.
    pub fn rotation_hint(&self) -> &'static str {
        if self.snap_rotation {
            "hold Shift to rotate freely"
        } else {
            "hold Shift to snap"
        }
    }
}

impl Default for TransformOptions {
    fn default() -> Self {
        Self {
            texture_lock: true,
            texture_scale_lock: false,
            rotation_snap: 15.0,
            snap_rotation: true,
            gizmo: true,
            gizmo_mode: crate::transform::GizmoMode::Move,
            arrow_nudge: true,
        }
    }
}

/// How maps are saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileOptions {
    /// Save As offers Mallet's own `.mallet` format first for a map that has never been
    /// saved; when off, it offers VMF first.
    pub new_maps_as_mallet: bool,
    /// How many maps File > Recent Maps keeps.
    pub recent_maps: usize,
}

impl Default for FileOptions {
    fn default() -> Self {
        Self {
            new_maps_as_mallet: true,
            recent_maps: 20,
        }
    }
}

impl FileOptions {
    /// The recent map count, kept between 1 and [`MAX_RECENT_MAPS`].
    pub fn recent_map_count(&self) -> usize {
        self.recent_maps.clamp(1, MAX_RECENT_MAPS)
    }
}

/// What new brushes, entities and lists start with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditingOptions {
    /// The material new brushes get when Mallet starts.
    pub default_material: String,
    /// The class the Entity tool creates when Mallet starts.
    pub default_entity: String,
    /// How many recently applied textures the texture tool lists.
    pub recent_textures: usize,
}

impl Default for EditingOptions {
    fn default() -> Self {
        Self {
            default_material: crate::editor::DEFAULT_MATERIAL.to_owned(),
            default_entity: crate::editor::ENTITY_CLASSES[0].to_owned(),
            recent_textures: 24,
        }
    }
}

impl EditingOptions {
    /// The default material, or Mallet's own when the setting is blank.
    pub fn material(&self) -> String {
        non_blank(&self.default_material, crate::editor::DEFAULT_MATERIAL)
    }

    /// The default entity class, or Mallet's own when the setting is blank.
    pub fn entity(&self) -> String {
        non_blank(&self.default_entity, crate::editor::ENTITY_CLASSES[0])
    }

    /// The recent texture count, kept between 1 and 100.
    pub fn recent_texture_count(&self) -> usize {
        self.recent_textures.clamp(1, 100)
    }
}

/// How the texture browser loads its thumbnails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextureBrowserOptions {
    /// Largest side of a thumbnail in pixels; larger looks sharper but loads slower.
    pub thumbnail_size: u32,
    /// Background threads loading thumbnails.
    pub threads: usize,
    /// Thumbnails kept in memory before they are dropped and reloaded as needed.
    pub cache: usize,
}

impl Default for TextureBrowserOptions {
    fn default() -> Self {
        Self {
            thumbnail_size: 256,
            threads: 4,
            cache: 2500,
        }
    }
}

impl TextureBrowserOptions {
    /// The thumbnail size, kept between 64 and 1024 pixels.
    pub fn thumbnail_side(&self) -> u32 {
        self.thumbnail_size.clamp(64, 1024)
    }

    /// The thread count, kept between 1 and 32.
    pub fn thread_count(&self) -> usize {
        self.threads.clamp(1, 32)
    }

    /// The cache size, kept between 200 and 50000 thumbnails.
    pub fn cache_size(&self) -> usize {
        self.cache.clamp(200, 50_000)
    }
}

/// The texture browser's filters and layout, kept between runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextureBrowserView {
    /// How many cards fit across the grid.
    pub per_row: usize,
    /// Show only materials the map uses.
    pub used_only: bool,
    /// Hide model, particle, sprite and interface materials.
    pub brush_only: bool,
    /// Text the search box held.
    pub filter: String,
    /// Earlier searches, the latest first.
    pub filter_history: Vec<String>,
    /// `%keywords` every shown material must have.
    pub keywords: String,
    /// The top-level folder the list is narrowed to, if any.
    pub folder: Option<String>,
    /// Folders and archives whose materials are hidden, by path.
    pub hidden_sources: Vec<PathBuf>,
    pub show_opaque: bool,
    pub show_translucent: bool,
    pub show_self_illum: bool,
    pub show_envmap: bool,
    pub show_errors: bool,
    pub show_alpha: bool,
}

impl Default for TextureBrowserView {
    fn default() -> Self {
        Self {
            per_row: 7,
            used_only: false,
            brush_only: true,
            filter: String::new(),
            filter_history: Vec::new(),
            keywords: String::new(),
            folder: None,
            hidden_sources: Vec::new(),
            show_opaque: true,
            show_translucent: true,
            show_self_illum: true,
            show_envmap: true,
            show_errors: true,
            show_alpha: false,
        }
    }
}

/// How the window itself looks and draws.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InterfaceOptions {
    /// Size of menus, panels and text, where 1 is normal.
    pub scale: f32,
    /// Wait for the screen's refresh before showing each frame. Takes effect on restart;
    /// `MALLET_PRESENT` overrides it.
    pub vsync: bool,
}

impl Default for InterfaceOptions {
    fn default() -> Self {
        Self {
            scale: 1.0,
            vsync: true,
        }
    }
}

impl InterfaceOptions {
    /// The interface scale, kept between 0.5 and 3.
    pub fn zoom(&self) -> f32 {
        if self.scale.is_finite() {
            self.scale.clamp(0.5, 3.0)
        } else {
            1.0
        }
    }
}

/// The MCP server AI agents connect to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentOptions {
    /// Run the server. Takes effect on restart.
    pub enabled: bool,
    /// The port it listens on, on 127.0.0.1. Takes effect on restart.
    pub port: u16,
    /// Give agents their own tab, camera, views, grid, selection and tools, and refuse
    /// the tools that drive the window, so the user can keep working while an agent
    /// edits. Takes effect at once.
    pub own_view: bool,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            port: crate::mcp::DEFAULT_PORT,
            own_view: true,
        }
    }
}

/// A colour of the views, as red, green and blue from 0 to 255.
pub type Rgb = [u8; 3];

/// The colours the viewports are drawn with, Hammer's by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorOptions {
    /// Behind everything in every view.
    pub background: Rgb,
    /// Ordinary grid lines in the 2D views.
    pub grid: Rgb,
    /// Grid lines at every highlight spacing.
    pub grid_highlight: Rgb,
    /// Grid lines at every major spacing.
    pub grid_major: Rgb,
    /// The lines through the world origin.
    pub axis: Rgb,
    /// Selected brushes and entities.
    pub selection: Rgb,
    /// The Block tool's box, the entity marker and the selection marquee.
    pub tool: Rgb,
    /// The box and handles around the selection.
    pub handles: Rgb,
    /// The outline of faces the texture tool has selected.
    pub selected_faces: Rgb,
}

impl Default for ColorOptions {
    fn default() -> Self {
        Self {
            background: [0, 0, 0],
            grid: [38, 38, 38],
            grid_highlight: [65, 65, 65],
            grid_major: [100, 46, 0],
            axis: [0, 100, 100],
            selection: [255, 0, 0],
            tool: [255, 255, 255],
            handles: [255, 255, 255],
            selected_faces: [255, 210, 60],
        }
    }
}

/// `value` trimmed, or `fallback` when it is blank.
fn non_blank(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() { fallback } else { value }.to_owned()
}

/// Programs chosen in Mallet Options. `None` means "find it automatically".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolPaths {
    pub vbsp: Option<PathBuf>,
    pub vvis: Option<PathBuf>,
    pub vrad: Option<PathBuf>,
    pub vbspinfo: Option<PathBuf>,
    pub bspzip: Option<PathBuf>,
    pub sourcecubemap: Option<PathBuf>,
    pub game: Option<PathBuf>,
}

impl ToolPaths {
    pub fn get(&self, tool: ToolKind) -> Option<&PathBuf> {
        match tool {
            ToolKind::Vbsp => self.vbsp.as_ref(),
            ToolKind::Vvis => self.vvis.as_ref(),
            ToolKind::Vrad => self.vrad.as_ref(),
            ToolKind::Vbspinfo => self.vbspinfo.as_ref(),
            ToolKind::Bspzip => self.bspzip.as_ref(),
            ToolKind::SourceCubemap => self.sourcecubemap.as_ref(),
            ToolKind::Game => self.game.as_ref(),
        }
    }

    pub fn set(&mut self, tool: ToolKind, path: Option<PathBuf>) {
        let slot = match tool {
            ToolKind::Vbsp => &mut self.vbsp,
            ToolKind::Vvis => &mut self.vvis,
            ToolKind::Vrad => &mut self.vrad,
            ToolKind::Vbspinfo => &mut self.vbspinfo,
            ToolKind::Bspzip => &mut self.bspzip,
            ToolKind::SourceCubemap => &mut self.sourcecubemap,
            ToolKind::Game => &mut self.game,
        };
        *slot = path;
    }
}

/// How compiles behave.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompileOptions {
    /// Save the map before compiling it, so the compile sees the latest changes.
    pub save_before: bool,
    /// Run the compilers below normal priority so the editor stays responsive.
    pub low_priority: bool,
    /// Flash the taskbar button when a compile ends.
    pub notify: bool,
    /// Show the leak line in the views when VBSP finds a leak.
    pub load_pointfile: bool,
    /// Keep each compile's log in `%APPDATA%\Mallet\logs`.
    pub keep_logs: bool,
    /// The folder beside a `.mallet` map that its VMF copy, BSP and logs are compiled in.
    pub compile_folder: String,
    /// Lines of compiler output the compile window keeps; older ones are dropped.
    pub output_lines: usize,
    /// The preset the compile window starts with and Shift+F9 runs.
    pub last_preset: String,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            save_before: true,
            low_priority: true,
            notify: true,
            load_pointfile: true,
            keep_logs: true,
            compile_folder: crate::compile::COMPILE_FOLDER.to_owned(),
            output_lines: 50_000,
            last_preset: "Normal".to_owned(),
        }
    }
}

impl CompileOptions {
    /// The compile folder's name, or Mallet's own when the setting is blank or names
    /// anything but a plain folder beside the map.
    pub fn folder_name(&self) -> String {
        let name = self.compile_folder.trim();
        let plain = !name.is_empty() && !name.contains(['/', '\\', ':']) && name != "." && name != "..";
        if plain { name } else { crate::compile::COMPILE_FOLDER }.to_owned()
    }

    /// The output line limit, kept between 1000 and 1000000.
    pub fn line_limit(&self) -> usize {
        self.output_lines.clamp(1000, 1_000_000)
    }
}

/// How the viewports look and move.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewOptions {
    pub net_graph: bool,
    /// How far the 3D view draws, in units.
    pub draw_distance: f64,
    /// Horizontal field of view of the 3D view, in degrees.
    pub fov: f64,
    /// 3D camera speed in units per second.
    pub camera_speed: f64,
    /// Degrees turned per point of mouse movement in mouselook.
    pub look_sensitivity: f64,
    /// Grid size of new maps.
    pub default_grid: f64,
    /// How far one mouse wheel notch scrolls the texture browser, as a share of a card's
    /// height, from 0.25 to 2.
    pub texture_scroll: f64,
    /// Degrees per second the turn keys turn the 3D camera. Hammer++ binds none by default.
    pub turn_speed: f64,
    /// How quickly the 3D camera reaches its speed and stops, per second; higher is
    /// snappier.
    pub camera_acceleration: f64,
    /// Units one mouse wheel notch moves the 3D camera.
    pub wheel_step: f64,
    /// The share of the camera speed flown while holding Ctrl in mouselook.
    pub crouch_speed: f64,
    /// Frames of mouse movement mouselook averages over; 1 turns smoothing off.
    pub look_smoothing: usize,
    /// How much one mouse wheel notch zooms a 2D view.
    pub zoom_step: f64,
    /// Grid lines closer than this many pixels are thinned out.
    pub grid_min_pixels: f64,
    /// Units between highlighted grid lines; 0 for none.
    pub grid_highlight: f64,
    /// Units between major grid lines; 0 for none.
    pub grid_major: f64,
    /// How close, in pixels, a click must be to a brush's outline to select it.
    pub pick_tolerance: f64,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            net_graph: true,
            draw_distance: crate::view3d::DEFAULT_DRAW_DISTANCE,
            fov: crate::view3d::DEFAULT_FOV,
            camera_speed: crate::view3d::DEFAULT_MOVE_SPEED,
            look_sensitivity: crate::view3d::DEFAULT_LOOK_SENSITIVITY,
            default_grid: 64.0,
            texture_scroll: 1.0,
            turn_speed: crate::view3d::DEFAULT_TURN_SPEED,
            camera_acceleration: crate::view3d::DEFAULT_ACCELERATION,
            wheel_step: crate::view3d::DEFAULT_WHEEL_STEP,
            crouch_speed: crate::view3d::DEFAULT_CROUCH_SPEED,
            look_smoothing: crate::view3d::DEFAULT_LOOK_FRAMES,
            zoom_step: crate::view2d::DEFAULT_ZOOM_STEP,
            grid_min_pixels: 4.0,
            grid_highlight: 64.0,
            grid_major: 1024.0,
            pick_tolerance: 4.0,
        }
    }
}

impl Config {
    /// Loads the settings, or defaults when there are none yet or they cannot be read.
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        if path.is_file() || profile().is_none() {
            return Self::load_from(&path);
        }
        // A new profile starts from the main settings, without their map history.
        let main = platform_data_dir().map(|dir| dir.join("Mallet").join("config.json"));
        let mut config = main.map_or_else(Self::default, |main| Self::load_from(&main));
        config.recent_maps.clear();
        config
    }

    /// Puts a map at the top of the recent list, keeping it short and free of repeats.
    pub fn remember_map(&mut self, path: &Path) {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
        let key = |path: &Path| path.to_string_lossy().replace('/', "\\").to_lowercase();
        let wanted = key(&path);
        self.recent_maps.retain(|recent| key(recent) != wanted);
        self.recent_maps.insert(0, path);
        self.recent_maps.truncate(self.files.recent_map_count());
    }

    /// Loads settings from a file, or defaults when it is missing or unreadable.
    pub fn load_from(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Writes the settings, creating the folder if needed.
    pub fn save(&self) -> Result<(), String> {
        let path = config_path().ok_or("Mallet's settings folder is unavailable, so settings cannot be saved")?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder).map_err(|error| error.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        fs::write(path, text).map_err(|error| error.to_string())
    }
}

/// The most maps File > Recent Maps can be set to keep.
pub const MAX_RECENT_MAPS: usize = 50;

/// The `MALLET_PROFILE` in use, if any: a separate set of settings and layouts, as test
/// copies use.
pub fn profile() -> Option<String> {
    std::env::var("MALLET_PROFILE")
        .ok()
        .map(|profile| profile.trim().to_owned())
        .filter(|profile| !profile.is_empty())
}

/// The name of Mallet's folder in `%APPDATA%`: `Mallet`, or `Mallet-<profile>`.
pub fn app_folder() -> String {
    match profile() {
        Some(profile) => format!("Mallet-{profile}"),
        None => "Mallet".to_owned(),
    }
}

/// Mallet's folder holding settings, presets and compile logs.
pub fn data_dir() -> Option<PathBuf> {
    platform_data_dir().map(|dir| dir.join(app_folder()))
}

fn config_path() -> Option<PathBuf> {
    data_dir().map(|dir| dir.join("config.json"))
}

/// A program Mallet runs while compiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolKind {
    Vbsp,
    Vvis,
    Vrad,
    Vbspinfo,
    Bspzip,
    SourceCubemap,
    Game,
}

impl ToolKind {
    pub const ALL: [ToolKind; 7] = [
        ToolKind::Vbsp,
        ToolKind::Vvis,
        ToolKind::Vrad,
        ToolKind::Vbspinfo,
        ToolKind::Bspzip,
        ToolKind::SourceCubemap,
        ToolKind::Game,
    ];

    /// The name shown in Mallet Options.
    pub fn label(self) -> &'static str {
        match self {
            ToolKind::Vbsp => "VBSP",
            ToolKind::Vvis => "VVIS",
            ToolKind::Vrad => "VRAD",
            ToolKind::Vbspinfo => "VBSPInfo",
            ToolKind::Bspzip => "BSPZip",
            ToolKind::SourceCubemap => "SourceCubemap",
            ToolKind::Game => "Game",
        }
    }

    /// The name used in settings and MCP tools.
    pub fn key(self) -> &'static str {
        match self {
            ToolKind::Vbsp => "vbsp",
            ToolKind::Vvis => "vvis",
            ToolKind::Vrad => "vrad",
            ToolKind::Vbspinfo => "vbspinfo",
            ToolKind::Bspzip => "bspzip",
            ToolKind::SourceCubemap => "sourcecubemap",
            ToolKind::Game => "game",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            ToolKind::Vbsp => "Turns the VMF into a BSP: brushes, entities and props.",
            ToolKind::Vvis => "Works out what can be seen from where, so the engine draws less.",
            ToolKind::Vrad => "Bakes the lighting.",
            ToolKind::Vbspinfo => "Prints what a compiled map contains and how close it is to the limits.",
            ToolKind::Bspzip => "Packs files into a BSP.",
            ToolKind::SourceCubemap => "Builds cubemaps on the graphics card without starting the game.",
            ToolKind::Game => "The game to test the map in.",
        }
    }

    /// Every existing program that could be this tool, best first: the game's 64-bit
    /// tools, its 32-bit ones and the Hammer++ builds, or for SourceCubemap and the game,
    /// the usual install places. On Linux these are the game's own Linux tools, the
    /// Tools++ builds in its folders or on `PATH`, and the game's start script; Windows
    /// programs are left out, as they cannot be started there.
    pub fn candidates(self, game_dir: Option<&Path>) -> Vec<PathBuf> {
        let root = game_dir.and_then(Path::parent);
        let mut found: Vec<PathBuf> = Vec::new();
        let mut add = |path: PathBuf| {
            // Rebuilding from components gives one kind of slash on Windows.
            let path: PathBuf = path.components().collect();
            if path.is_file() && !found.contains(&path) {
                found.push(path);
            }
        };
        match self {
            ToolKind::SourceCubemap => {
                for folder in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
                    add(folder.join(format!("sourcecubemap{}", std::env::consts::EXE_SUFFIX)));
                }
                #[cfg(windows)]
                {
                    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
                        add(Path::new(&local).join("Programs/SourceCubemap/sourcecubemap.exe"));
                    }
                    for program_files in ["ProgramFiles", "ProgramFiles(x86)"] {
                        if let Some(folder) = std::env::var_os(program_files) {
                            add(Path::new(&folder).join("SourceCubemap/sourcecubemap.exe"));
                        }
                    }
                    if let Some(home) = std::env::var_os("USERPROFILE") {
                        add(Path::new(&home).join("SourceCubemap/sourcecubemap.exe"));
                    }
                    for drive in ["C:", "D:", "E:"] {
                        add(PathBuf::from(format!(
                            "{drive}/Github/SourceCubemap/build/release/sourcecubemap.exe"
                        )));
                        add(PathBuf::from(format!("{drive}/SourceCubemap/sourcecubemap.exe")));
                    }
                }
                #[cfg(not(windows))]
                if let Some(home) = std::env::var_os("HOME") {
                    let home = Path::new(&home);
                    add(home.join(".local/bin/sourcecubemap"));
                    add(home.join("SourceCubemap/sourcecubemap"));
                }
            }
            ToolKind::Game => {
                let Some(root) = root else { return found };
                #[cfg(windows)]
                {
                    for name in ["gmod.exe", "hl2.exe", "hl2_win64.exe", "bin/win64/gmod.exe", "csgo.exe"] {
                        add(root.join(name));
                    }
                    if let Some(game) = game_dir.and_then(Path::file_name) {
                        add(root.join(format!("{}.exe", game.to_string_lossy())));
                    }
                }
                #[cfg(not(windows))]
                add(root.join("hl2.sh"));
            }
            _ => {
                let Some(root) = root else { return found };
                let name = self.key();
                #[cfg(windows)]
                {
                    for folder in ["bin/win64", "bin"] {
                        add(root.join(folder).join(format!("{name}.exe")));
                    }
                    for folder in ["bin/win64", "bin"] {
                        add(root.join(folder).join(format!("{name}plusplus.exe")));
                    }
                }
                #[cfg(not(windows))]
                {
                    for folder in ["bin/linux64", "bin"] {
                        add(root.join(folder).join(name));
                    }
                    for folder in ["bin/linux64", "bin", "bin/win64"] {
                        add(root.join(folder).join(format!("{name}++")));
                    }
                    for folder in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
                        add(folder.join(format!("{name}++")));
                    }
                }
            }
        }
        found
    }
}

/// Looks for an installed Source game: the first known game folder found in the Steam
/// libraries listed by the Steam install at `steam_dir`, or in the usual place, in the
/// order Steam lists them.
pub fn detect_game_dir(steam_dir: Option<&Path>) -> Option<PathBuf> {
    let installs = match steam_dir {
        Some(steam) => vec![steam.to_path_buf()],
        None => default_steam_dirs(),
    };
    let mut libraries = installs.clone();
    for steam in &installs {
        let listing = fs::read_to_string(steam.join("steamapps").join("libraryfolders.vdf")).unwrap_or_default();
        libraries.extend(
            listing
                .lines()
                .filter(|line| line.trim_start().starts_with("\"path\""))
                .filter_map(|line| line.split('"').nth(3))
                .map(|path| PathBuf::from(path.replace("\\\\", "\\"))),
        );
    }
    libraries.dedup();
    libraries.iter().find_map(|library| {
        KNOWN_GAMES
            .iter()
            .map(|game| library.join("steamapps").join("common").join(game))
            .find(|dir| dir.join("gameinfo.txt").is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_files_load_with_defaults_and_round_trip() {
        let folder = std::env::temp_dir().join(format!("mallet-config-{}", std::process::id()));
        let path = folder.join("config.json");
        fs::create_dir_all(&folder).unwrap();
        fs::write(&path, r#"{ "game_dir": "C:/Games/hl2", "fgds": ["a.fgd"] }"#).unwrap();
        let mut config = Config::load_from(&path);
        assert_eq!(config.game_dir, Some(PathBuf::from("C:/Games/hl2")));
        assert_eq!(config.fgds, [PathBuf::from("a.fgd")]);
        assert!(config.compile.save_before);
        assert_eq!(config.tools.get(ToolKind::Vrad), None);

        config
            .tools
            .set(ToolKind::SourceCubemap, Some(PathBuf::from("D:/scm.exe")));
        for map in ["C:/maps/a.vmf", "C:/maps/b.vmf", "C:\\MAPS\\A.vmf"] {
            config.remember_map(Path::new(map));
        }
        assert_eq!(
            config.recent_maps.len(),
            2,
            "the same map in another case or slash is one map"
        );
        assert!(config.recent_maps[0]
            .to_string_lossy()
            .to_lowercase()
            .ends_with("a.vmf"));
        config.view.camera_speed = 500.0;
        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path), config);
        fs::remove_dir_all(folder).unwrap();
    }
}
