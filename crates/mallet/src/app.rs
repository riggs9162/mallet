//! Top-level editor window: menus, Hammer's four-viewport layout and the status bar. The
//! toolbar, map tabs, tool palette and side panel are in `chrome`.

mod camera_tool;
mod chrome;
mod compile_window;
mod context_menu;
mod brush_tools;
mod decal_tools;
mod displacement_tool;
mod entity_report;
mod entity_tools;
pub(crate) use compile_window::count;
mod map_dialogs;
mod model_browser;
mod new_objects;
mod options;
mod paint_select;
mod paste_special;
mod problems_window;
mod properties;
mod run_map;
mod texture_browser;
mod texture_tool;
mod transform_window;
mod vertex_tool;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Id, Modifiers, PointerButton, Pos2, Rect, Response, Sense, ViewportCommand};
use glam::{DVec2, DVec3};
use mallet_vmf::geometry::Bounds;

use eframe::egui_wgpu;

use crate::branding;
use crate::canvas::{Canvas, PainterCanvas};
use crate::config::{self, Config};
use crate::document::MapFormat;
use crate::editor::{self, Editor, SelectMode, Tool, ViewMode, WindowShot, VIEWPORT_GAP};
use crate::gizmo::{Change as GizmoChange, Gizmo, GizmoDrag, Part as GizmoPart};
use crate::keymap::{self, Action, KeyMap};
use crate::netgraph::{NetGraph, NetGraphExtra};
use crate::presence::{Presence, Status};
use crate::render3d::{CameraStyle, LightingInput, Renderer3D};
use crate::scene::Remap;
use crate::transform::{self, HandleMode};
use crate::view2d::{Axis2D, Handle, View2D};
use crate::view3d::Camera3D;
use crate::{mcp, tools};

/// How close, in pixels, a press has to be to a clip point to drag it.
const CLIP_POINT_REACH_PIXELS: f32 = 8.0;
/// How close, in pixels, a press has to be to the pivot to drag it.
const PIVOT_REACH_PIXELS: f32 = 7.0;
/// How far, in pixels, an arrow key pans a 2D view with nothing to nudge.
const PAN_PIXELS: f64 = 64.0;
/// How long `send_input` waits for the window to handle its input.
const INPUT_TIMEOUT: Duration = Duration::from_secs(5);

/// Actions checked against the keyboard every frame.
const KEY_ACTIONS: &[Action] = &[
    Action::NewFile,
    Action::CloseTab,
    Action::NextTab,
    Action::PreviousTab,
    Action::OpenFile,
    Action::SaveFile,
    Action::Undo,
    Action::Redo,
    Action::Cut,
    Action::Copy,
    Action::Paste,
    Action::PasteSpecial,
    Action::GridSmaller,
    Action::GridLarger,
    Action::ToggleSnapToGrid,
    Action::ToggleShowGrid,
    Action::SelectionTool,
    Action::CameraTool,
    Action::MagnifyTool,
    Action::PreviousCamera,
    Action::NextCamera,
    Action::BlockTool,
    Action::EntityTool,
    Action::TextureTool,
    Action::ApplyCurrentTexture,
    Action::ClipTool,
    Action::ClipThreePoint,
    Action::ClipMeasurements,
    Action::VertexTool,
    Action::OverlayTool,
    Action::DecalTool,
    Action::SplitFace,
    Action::Properties,
    Action::TieToEntity,
    Action::MoveToWorld,
    Action::Carve,
    Action::Group,
    Action::Ungroup,
    Action::Compile,
    Action::CompileAgain,
    Action::SelectAll,
    Action::ClearSelection,
    Action::IgnoreGroups,
    Action::DeleteSelection,
    Action::HideSelection,
    Action::HideUnselected,
    Action::ShowAllHidden,
    Action::CreateBlock,
    Action::CenterViews2D,
    Action::CenterView3D,
    Action::MaximizeView,
    Action::ViewTop,
    Action::ViewSide,
    Action::ViewFront,
    Action::ViewTextured,
    Action::ViewLighting,
    Action::GoToBrush,
    Action::CheckForProblems,
    Action::FindEntities,
    Action::Transform,
    Action::SnapToGrid,
    Action::SnapToGridIndividually,
    Action::FlipHorizontal,
    Action::FlipVertical,
    Action::ToggleTextureLock,
    Action::CycleGizmoMode,
    Action::ToggleRotationSnap,
];

/// A left-button drag in progress in one 2D view. `start` is the 2D world point where the
/// button went down; `bounds` is the box being moved or resized as it was at that moment,
/// and `anchor` the point a move snaps to the grid.
#[derive(Clone, Copy, Debug)]
enum Drag {
    /// Moving the selection, or a clone of it when Shift is held.
    Move { view: usize, start: DVec2, anchor: DVec3 },
    /// Resizing the selection with one of its handles.
    Resize {
        view: usize,
        start: DVec2,
        bounds: Bounds,
        handle: Handle,
    },
    /// Rotating the selection from a corner handle around `pivot`.
    Rotate { view: usize, start: DVec2, pivot: DVec3 },
    /// Skewing the selection from an edge handle.
    Skew {
        view: usize,
        start: DVec2,
        bounds: Bounds,
        handle: Handle,
    },
    /// Moving the pivot rotations turn around.
    Pivot { view: usize },
    /// Drawing a selection rectangle; with `add`, set when Ctrl was held at the press,
    /// what it covers joins the selection instead of replacing it.
    Marquee { view: usize, start: DVec2, add: bool },
    /// Drawing a new block, from a grid-snapped corner.
    NewBlock { view: usize, start: DVec2 },
    /// Moving the pending block.
    MoveBlock { view: usize, start: DVec2, bounds: Bounds },
    /// Resizing the pending block with one of its handles.
    ResizeBlock {
        view: usize,
        start: DVec2,
        bounds: Bounds,
        handle: Handle,
    },
    /// Placing or moving one of the clipping tool's points. With `grabbed` the drag began
    /// on a point that was already placed, so holding Ctrl moves all
    /// of them together by the grabbed one.
    ClipPoint { view: usize, point: usize, grabbed: bool },
}

impl Drag {
    /// Whether holding Shift makes this drag change a clone of the selection instead,
    /// as in Hammer: moving, resizing, rotating and skewing all clone.
    fn clones(self) -> bool {
        matches!(
            self,
            Self::Move { .. } | Self::Resize { .. } | Self::Rotate { .. } | Self::Skew { .. }
        )
    }

    fn view(self) -> usize {
        match self {
            Self::Move { view, .. }
            | Self::Resize { view, .. }
            | Self::Rotate { view, .. }
            | Self::Skew { view, .. }
            | Self::Pivot { view }
            | Self::Marquee { view, .. }
            | Self::NewBlock { view, .. }
            | Self::MoveBlock { view, .. }
            | Self::ResizeBlock { view, .. }
            | Self::ClipPoint { view, .. } => view,
        }
    }
}

/// Pointer and keyboard state a 2D tool needs for one frame.
struct PointerState {
    position: Option<Pos2>,
    press_origin: Option<Pos2>,
    modifiers: Modifiers,
}

/// The editor window. Map and view state live in the shared [`Editor`].
pub struct MalletApp {
    editor: Arc<Mutex<Editor>>,
    keymap: KeyMap,
    cursor: Option<DVec2>,
    title: String,
    mcp_url: Option<String>,
    drag: Option<Drag>,
    /// Whether Shift was held when the current 2D or gizmo drag began, so it clones.
    clone_drag: bool,
    /// Each viewport slot's GPU image as registered with egui.
    slot_textures: [Option<egui::TextureId>; 4],
    /// Where the cursor was when mouselook started, to put it back afterwards.
    look_return: Option<Pos2>,
    /// The tab whose "unsaved changes" question is showing.
    closing: Option<usize>,
    /// Material previews for the side panel.
    texture_previews: chrome::TexturePreviews,
    /// Frame timing overlay in the camera viewport's bottom-right corner.
    net_graph: NetGraph,
    /// The compile window's preset, tab and other state.
    compile: compile_window::CompileWindowState,
    /// The Mallet Options window's section.
    options: options::OptionsState,
    /// Mallet's icon, for the menu bar and the About window; loaded on the first frame.
    logo: Option<egui::TextureHandle>,
    /// Face Edit and texture browser state for the texture tool.
    face_edit: texture_tool::FaceEditState,
    texture_browser: texture_browser::TextureBrowserState,
    /// The model browser's listing, thumbnails and preview.
    model_browser: model_browser::ModelBrowserState,
    /// The Object Properties window's tab, drafts and other state.
    properties: properties::PropertiesState,
    /// The interface scale last applied from Mallet Options, so the window's own zoom
    /// keys keep working until the setting changes.
    applied_scale: Option<f32>,
    /// The Transform dialog's mode and numbers.
    transform: transform_window::TransformState,
    /// The 3D gizmo part being dragged, if any.
    gizmo_drag: Option<crate::gizmo::GizmoDrag>,
    /// The viewport right-click menu's remembered choices.
    context_menu: context_menu::ContextMenuState,
    /// The vertex tool's drag in progress, if any.
    vertex_drag: Option<vertex_tool::VertexPointer>,
    /// The overlay corner being dragged with the Overlay tool.
    overlay_drag: Option<decal_tools::CornerPointer>,
    /// The displacement paint stroke in progress, if any.
    paint_stroke: Option<displacement_tool::PaintStroke>,
    /// The Selection tool's Shift paint select stroke in progress, if any.
    paint_select: Option<paint_select::PaintSelect>,
    /// Where the displacement paint brush sits under the mouse.
    brush_spot: Option<displacement_tool::BrushSpot>,
    /// The displacement sculpt stroke in progress, if any.
    sculpt_stroke: Option<displacement_tool::SculptStroke>,
    /// The right-button drag resizing the sculpt brush, if any.
    brush_resize: Option<displacement_tool::BrushResize>,
    /// The viewport slot and point the sculpt brush is drawn at.
    sculpt_brush: Option<(usize, Pos2)>,
    /// The Paste Special dialog's settings, kept between pastes as Hammer does.
    paste_options: editor::PasteOptions,
    /// The modifiers held on their own and when they went down, for the key hints.
    hint_modifiers: Option<(egui::Modifiers, f64)>,
    /// The top edge of the status bar, which the key hints sit just above.
    status_bar_top: f32,
    /// Tie to Entity's question when the selection already holds a brush entity.
    tie_prompt: Option<entity_tools::TiePrompt>,
    /// The Make Hollow dialog and the wall width it remembers.
    hollow: brush_tools::HollowDialog,
    /// The Arch Properties dialog and the settings it remembers.
    arch: brush_tools::ArchDialog,
    /// Go to Brush Number, Go to Coordinates and Show Information.
    map_dialogs: map_dialogs::MapDialogs,
    /// The Camera tool's drag in a 2D view, if any.
    camera_drag: Option<camera_tool::CameraDrag>,
    /// Whether the normal Run Map dialog is open.
    run_map_open: bool,
    /// The Check for Problems dialog's list.
    problems: problems_window::ProblemsDialog,
    /// The Entity Report's filters.
    entity_report: entity_report::EntityReport,
    /// What Discord shows about this Mallet.
    presence: Presence,
}

impl MalletApp {
    /// Creates the window, starts the MCP server and opens `path` if one was given.
    ///
    /// The server listens on the port set in Mallet Options (7414 unless changed), or on
    /// `MALLET_MCP_PORT` when set; a value of `0` or `off` disables it, as does turning it
    /// off in Mallet Options.
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        // Always dark, like Hammer++, whatever Windows uses or a saved layout says.
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        let mut app = Self {
            editor: Arc::new(Mutex::new(Editor::default())),
            keymap: KeyMap::hammer_plus_plus_defaults(),
            cursor: None,
            title: String::new(),
            mcp_url: None,
            drag: None,
            clone_drag: false,
            slot_textures: [None; 4],
            look_return: None,
            closing: None,
            texture_previews: chrome::TexturePreviews::new(),
            net_graph: NetGraph::default(),
            compile: compile_window::CompileWindowState::default(),
            logo: None,
            face_edit: texture_tool::FaceEditState::default(),
            texture_browser: texture_browser::TextureBrowserState::default(),
            model_browser: model_browser::ModelBrowserState::default(),
            options: options::OptionsState::default(),
            properties: properties::PropertiesState::default(),
            applied_scale: None,
            transform: transform_window::TransformState::default(),
            gizmo_drag: None,
            context_menu: context_menu::ContextMenuState::default(),
            vertex_drag: None,
            overlay_drag: None,
            paint_stroke: None,
            paint_select: None,
            brush_spot: None,
            sculpt_stroke: None,
            brush_resize: None,
            sculpt_brush: None,
            paste_options: editor::PasteOptions::default(),
            hint_modifiers: None,
            status_bar_top: 0.0,
            tie_prompt: None,
            hollow: brush_tools::HollowDialog::default(),
            arch: brush_tools::ArchDialog::default(),
            map_dialogs: map_dialogs::MapDialogs::default(),
            camera_drag: None,
            run_map_open: false,
            problems: problems_window::ProblemsDialog::default(),
            entity_report: entity_report::EntityReport::default(),
            presence: Presence::start(),
        };
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        cc.egui_ctx.set_fonts(fonts);
        app.start_mcp(&cc.egui_ctx);
        let mut editor = editor::lock(&app.editor);
        load_game_content(&mut editor);
        open_or_new(&mut editor, path);
        drop(editor);
        app
    }

    fn start_mcp(&mut self, ctx: &egui::Context) {
        let port = match mcp::configured_port(&Config::load().agents) {
            Ok(Some(port)) => port,
            Ok(None) => return,
            Err(message) => {
                editor::lock(&self.editor).message = message;
                return;
            }
        };
        let shared = Arc::clone(&self.editor);
        let ctx = ctx.clone();
        let handler = move |name: &str, arguments: &serde_json::Value| {
            if name == "screenshot" && arguments.get("viewport").and_then(serde_json::Value::as_str) == Some("window") {
                return match window_screenshot(&shared, &ctx) {
                    Ok(png) => mcp::ToolResult::png(png),
                    Err(message) => mcp::ToolResult::error(message),
                };
            }
            let result = tools::run(&mut editor::lock(&shared), name, arguments);
            ctx.request_repaint();
            // A map handed over by a second copy, opened from Explorer: come to the front.
            let focus = arguments.get("focus").and_then(serde_json::Value::as_bool) == Some(true);
            if name == "open_map" && focus {
                ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
            if name == "compile_map" && !result.is_error {
                let wait = arguments
                    .get("wait_seconds")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0);
                if wait > 0.0 {
                    wait_for_compile(&shared, Duration::from_secs_f64(wait.min(3600.0)));
                    let mut editor = editor::lock(&shared);
                    editor.poll_compile();
                    return mcp::ToolResult::json(&tools::compile_status_json(&editor, 40));
                }
            }
            if name == "send_input" && !result.is_error {
                if let Err(message) = wait_for_input(&shared, &ctx) {
                    return mcp::ToolResult::error(message);
                }
            }
            result
        };
        match mcp::start(port, tools::definitions(), handler) {
            Ok(url) => self.mcp_url = Some(url),
            Err(error) => {
                editor::lock(&self.editor).message = format!("MCP server could not start on port {port}: {error}");
            }
        }
    }

    /// Sizes the interface as Mallet Options says, whenever that setting changes.
    fn apply_interface_options(&mut self, ctx: &egui::Context, editor: &Editor) {
        let scale = editor.config.interface.zoom();
        let dragging = ctx.input(|input| input.pointer.any_down());
        if self.applied_scale != Some(scale) && !dragging {
            ctx.set_zoom_factor(scale);
            self.applied_scale = Some(scale);
        }
    }

    fn handle_input(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        let keymap = &self.keymap;
        let triggered = ctx.input(|input| {
            KEY_ACTIONS
                .iter()
                .copied()
                .filter(|action| keymap.pressed(input, *action))
                .collect::<Vec<_>>()
        });
        if ctx.wants_keyboard_input() {
            return;
        }
        let clipboard = ctx.input(|input| {
            input
                .events
                .iter()
                .filter_map(|event| {
                    let action = keymap::clipboard_action(event, &input.modifiers)?;
                    let text = match event {
                        egui::Event::Paste(text) => Some(text.clone()),
                        _ => None,
                    };
                    Some((action, text))
                })
                .collect::<Vec<_>>()
        });
        for (action, text) in clipboard {
            if let Some(text) = text {
                editor.read_system_clipboard(&text);
            }
            self.dispatch(action, editor);
        }
        let alt = ctx.input(|input| input.modifiers.alt);
        for action in triggered {
            let crouching = alt && self.keymap.is_crouch_chord(action);
            if editor.camera.mouse_look() && (action == Action::ClearSelection || crouching) {
                continue;
            }
            self.dispatch(action, editor);
        }
        let dropped = ctx.input(|input| input.raw.dropped_files.iter().find_map(|file| file.path.clone()));
        if let Some(path) = dropped {
            let _ = editor.open_in_tab(&path);
        }
    }

    fn update_title(&mut self, ctx: &egui::Context, editor: &Editor) {
        let title = match &editor.document {
            Some(document) => format!("Mallet - {}", document.file_name()),
            None => "Mallet".to_owned(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    fn menu_bar(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        let mut chosen = None;
        let mut save_as = false;
        let mut export = false;
        let mut choose_game = false;
        let mut choose_fgds = false;
        let mut browse_models = false;
        let mut recent = RecentChoice::None;
        let mut show_net_graph = editor.config.view.net_graph;
        let mut draw_distance = editor.config.view.draw_distance;
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                let logo = self.logo.get_or_insert_with(|| branding::logo_texture(ctx)).id();
                ui.add(egui::Image::new((logo, egui::Vec2::splat(16.0))))
                    .on_hover_text(format!("Mallet {}", env!("CARGO_PKG_VERSION")));
                ui.menu_button("File", |ui| {
                    menu_item(ui, &self.keymap, "New", Action::NewFile, &mut chosen);
                    menu_item(ui, &self.keymap, "Open...", Action::OpenFile, &mut chosen);
                    recent_maps_menu(ui, &editor.config.recent_maps, &mut recent);
                    menu_item(ui, &self.keymap, "Save", Action::SaveFile, &mut chosen);
                    menu_item(ui, &self.keymap, "Close Tab", Action::CloseTab, &mut chosen);
                    if ui.button("Save As...").clicked() {
                        save_as = true;
                        ui.close_menu();
                    }
                    let open = editor.document.is_some();
                    let export_button = ui
                        .add_enabled(open, egui::Button::new("Export a Copy..."))
                        .on_hover_text(
                            "Write the map to another file, as a VMF or a .mallet map, and keep working on this one.",
                        );
                    if export_button.clicked() {
                        export = true;
                        ui.close_menu();
                    }
                    ui.separator();
                    menu_item(ui, &self.keymap, "Run Map...", Action::Compile, &mut chosen);
                    menu_item(ui, &self.keymap, "Compile Again", Action::CompileAgain, &mut chosen);
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                });
                ui.menu_button("Edit", |ui| {
                    menu_item(ui, &self.keymap, "Undo", Action::Undo, &mut chosen);
                    menu_item(ui, &self.keymap, "Redo", Action::Redo, &mut chosen);
                    ui.separator();
                    menu_item(ui, &self.keymap, "Cut", Action::Cut, &mut chosen);
                    menu_item(ui, &self.keymap, "Copy", Action::Copy, &mut chosen);
                    menu_item(ui, &self.keymap, "Paste", Action::Paste, &mut chosen);
                    menu_item(ui, &self.keymap, "Paste Special...", Action::PasteSpecial, &mut chosen);
                    menu_item(ui, &self.keymap, "Delete", Action::DeleteSelection, &mut chosen);
                    menu_item(ui, &self.keymap, "Select All", Action::SelectAll, &mut chosen);
                    menu_item(ui, &self.keymap, "Select None", Action::ClearSelection, &mut chosen);
                    menu_item(ui, &self.keymap, "Cycle Select Mode", Action::IgnoreGroups, &mut chosen);
                    menu_item(ui, &self.keymap, "Find Entities...", Action::FindEntities, &mut chosen);
                    ui.separator();
                    menu_item(
                        ui,
                        &self.keymap,
                        "Apply Current Texture",
                        Action::ApplyCurrentTexture,
                        &mut chosen,
                    );
                    ui.separator();
                    menu_item(ui, &self.keymap, "Properties", Action::Properties, &mut chosen);
                    ui.separator();
                    menu_item(ui, &self.keymap, "Mallet Options...", Action::Options, &mut chosen);
                });
                ui.menu_button("Map", |ui| {
                    let keymap = &self.keymap;
                    menu_toggle(ui, keymap, "Snap to Grid", editor.snap_to_grid, Action::ToggleSnapToGrid, &mut chosen);
                    menu_toggle(ui, keymap, "Show Grid", editor.show_grid, Action::ToggleShowGrid, &mut chosen);
                    ui.menu_button("Grid Settings", |ui| {
                        menu_item(ui, keymap, "Smaller Grid", Action::GridSmaller, &mut chosen);
                        menu_item(ui, keymap, "Larger Grid", Action::GridLarger, &mut chosen);
                    });
                    ui.separator();
                    menu_item(ui, keymap, "Entity Report...", Action::EntityReport, &mut chosen);
                    menu_item(ui, keymap, "Check for Problems...", Action::CheckForProblems, &mut chosen);
                    menu_item(ui, keymap, "Show Information...", Action::MapInformation, &mut chosen);
                    menu_item(ui, keymap, "Map Properties...", Action::MapProperties, &mut chosen);
                });
                ui.menu_button("View", |ui| {
                    let keymap = &self.keymap;
                    menu_toggle(
                        ui,
                        keymap,
                        "Maximize Active View",
                        editor.maximized.is_some(),
                        Action::MaximizeView,
                        &mut chosen,
                    );
                    ui.menu_button("Active View", |ui| {
                        menu_item(ui, keymap, "2D Top (x/y)", Action::ViewTop, &mut chosen);
                        menu_item(ui, keymap, "2D Front (y/z)", Action::ViewFront, &mut chosen);
                        menu_item(ui, keymap, "2D Side (x/z)", Action::ViewSide, &mut chosen);
                        menu_item(ui, keymap, "Next 2D View", Action::NextViewType, &mut chosen);
                        ui.separator();
                        menu_item(ui, keymap, "3D Textured", Action::ViewTextured, &mut chosen);
                        menu_item(ui, keymap, "3D Lighting Preview", Action::ViewLighting, &mut chosen);
                    });
                    ui.separator();
                    menu_item(ui, keymap, "Center Views on Selection", Action::CenterViews2D, &mut chosen);
                    menu_item(ui, keymap, "Center 3D Views on Selection", Action::CenterView3D, &mut chosen);
                    menu_item(ui, keymap, "Go to Coordinates...", Action::GoToCoordinates, &mut chosen);
                    menu_item(ui, keymap, "Go to Brush Number...", Action::GoToBrush, &mut chosen);
                    ui.separator();
                    menu_item(ui, keymap, "QuickHide Objects", Action::HideSelection, &mut chosen);
                    menu_item(ui, keymap, "QuickHide Unselected Objects", Action::HideUnselected, &mut chosen);
                    menu_item(ui, keymap, "Unhide QuickHide Objects", Action::ShowAllHidden, &mut chosen);
                    ui.separator();
                    menu_toggle(
                        ui,
                        keymap,
                        "Displacement Edge Collapse",
                        editor.config.view.collapsed_vertices,
                        Action::ToggleDisplacementEdgeCollapse,
                        &mut chosen,
                    );
                    if ui.checkbox(&mut show_net_graph, "Net Graph").clicked() {
                        ui.close_menu();
                    }
                    let mut gizmo = editor.config.transform.gizmo;
                    if ui.checkbox(&mut gizmo, "3D Transform Gizmo").clicked() {
                        chosen = Some(Action::ToggleGizmo);
                        ui.close_menu();
                    }
                    let gizmo_mode = editor.config.transform.gizmo_mode.label();
                    let cycle_label = format!("Next Gizmo Mode (now {gizmo_mode})");
                    menu_item(ui, &self.keymap, &cycle_label, Action::CycleGizmoMode, &mut chosen);
                    ui.horizontal(|ui| {
                        ui.label("3D draw distance");
                        ui.add(
                            egui::Slider::new(&mut draw_distance, 1024.0..=65536.0)
                                .logarithmic(true)
                                .integer(),
                        );
                    });
                });
                ui.menu_button("Tools", |ui| {
                    if ui.button("Game Folder...").clicked() {
                        choose_game = true;
                        ui.close_menu();
                    }
                    if ui.button("Game Data Files (FGD)...").clicked() {
                        choose_fgds = true;
                        ui.close_menu();
                    }
                    ui.separator();
                    menu_item(ui, &self.keymap, "Selection Tool", Action::SelectionTool, &mut chosen);
                    menu_item(ui, &self.keymap, "Magnify Tool", Action::MagnifyTool, &mut chosen);
                    menu_item(ui, &self.keymap, "Camera Tool", Action::CameraTool, &mut chosen);
                    menu_item(ui, &self.keymap, "Block Tool", Action::BlockTool, &mut chosen);
                    menu_item(ui, &self.keymap, "Entity Tool", Action::EntityTool, &mut chosen);
                    menu_item(
                        ui,
                        &self.keymap,
                        "Texture Application",
                        Action::TextureTool,
                        &mut chosen,
                    );
                    menu_item(ui, &self.keymap, "Clipping Tool", Action::ClipTool, &mut chosen);
                    menu_item(ui, &self.keymap, "Vertex Tool", Action::VertexTool, &mut chosen);
                    menu_item(ui, &self.keymap, "Overlay Tool", Action::OverlayTool, &mut chosen);
                    menu_item(ui, &self.keymap, "Decal Tool", Action::DecalTool, &mut chosen);
                    menu_item(ui, &self.keymap, "Split Face", Action::SplitFace, &mut chosen);
                    menu_item(ui, &self.keymap, "Create Block", Action::CreateBlock, &mut chosen);
                    ui.separator();
                    menu_item(ui, &self.keymap, "Carve", Action::Carve, &mut chosen);
                    menu_item(ui, &self.keymap, "Group", Action::Group, &mut chosen);
                    menu_item(ui, &self.keymap, "Ungroup", Action::Ungroup, &mut chosen);
                    menu_item(ui, &self.keymap, "Make Hollow...", Action::MakeHollow, &mut chosen);
                    menu_item(ui, &self.keymap, "Tie to Entity", Action::TieToEntity, &mut chosen);
                    menu_item(ui, &self.keymap, "Move to World", Action::MoveToWorld, &mut chosen);
                    ui.separator();
                    if ui
                        .button("Model Browser...")
                        .on_hover_text("Pick a model for the selected entities, or for new props")
                        .clicked()
                    {
                        browse_models = true;
                        ui.close_menu();
                    }
                    menu_item(ui, &self.keymap, "Transform...", Action::Transform, &mut chosen);
                    menu_item(
                        ui,
                        &self.keymap,
                        "Snap Selected to Grid",
                        Action::SnapToGrid,
                        &mut chosen,
                    );
                    menu_item(
                        ui,
                        &self.keymap,
                        "Snap Selected to Grid Individually",
                        Action::SnapToGridIndividually,
                        &mut chosen,
                    );
                    ui.menu_button("Flip Objects", |ui| {
                        menu_item(ui, &self.keymap, "Horizontally", Action::FlipHorizontal, &mut chosen);
                        menu_item(ui, &self.keymap, "Vertically", Action::FlipVertical, &mut chosen);
                    });
                    ui.menu_button("Align Objects", |ui| {
                        menu_item(ui, &self.keymap, "To Left", Action::AlignLeft, &mut chosen);
                        menu_item(ui, &self.keymap, "To Right", Action::AlignRight, &mut chosen);
                        menu_item(ui, &self.keymap, "To Top", Action::AlignTop, &mut chosen);
                        menu_item(ui, &self.keymap, "To Bottom", Action::AlignBottom, &mut chosen);
                    });
                    ui.separator();
                    let transform = &editor.config.transform;
                    let keymap = &self.keymap;
                    let locks = [
                        ("Texture Lock", transform.texture_lock, Action::ToggleTextureLock),
                        (
                            "Texture Scale Lock",
                            transform.texture_scale_lock,
                            Action::ToggleTextureScaleLock,
                        ),
                        ("Rotation Snap", transform.snap_rotation, Action::ToggleRotationSnap),
                    ];
                    for (text, on, action) in locks {
                        menu_toggle(ui, keymap, text, on, action, &mut chosen);
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About Mallet").clicked() {
                        editor.about_open = true;
                        ui.close_menu();
                    }
                });
            });
        });
        if let Some(action) = chosen {
            self.dispatch(action, editor);
        }
        if save_as {
            save_as_dialog(editor);
        }
        if export {
            export_dialog(editor);
        }
        if browse_models {
            editor.model_browser_open = true;
        }
        if choose_game {
            choose_game_dir(editor);
        }
        if choose_fgds {
            choose_fgd_files(editor);
        }
        match recent {
            RecentChoice::None => {}
            RecentChoice::Open(path) => {
                let _ = editor.open_in_tab(&path);
            }
            RecentChoice::ForgetMissing => {
                editor.config.recent_maps.retain(|path| path.is_file());
                editor.save_config();
            }
            RecentChoice::Clear => {
                editor.config.recent_maps.clear();
                editor.save_config();
            }
        }
        if show_net_graph != editor.config.view.net_graph || draw_distance != editor.config.view.draw_distance {
            editor.config.view.net_graph = show_net_graph;
            editor.config.view.draw_distance = draw_distance;
            if !ctx.input(|input| input.pointer.any_down()) {
                editor.save_config();
            }
        }
    }

    /// Help > About Mallet: the icon, version, and where things are.
    fn about_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if !editor.about_open {
            return;
        }
        let logo = self.logo.get_or_insert_with(|| branding::logo_texture(ctx)).id();
        let mut open = true;
        egui::Window::new("About Mallet")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(8.0);
                    ui.add(egui::Image::new((logo, egui::Vec2::splat(112.0))));
                    ui.add_space(6.0);
                    ui.heading(egui::RichText::new("Mallet").size(26.0).strong());
                    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                    ui.label(egui::RichText::new(env!("CARGO_PKG_DESCRIPTION")).color(egui::Color32::GRAY));
                    ui.add_space(10.0);
                });
                egui::Grid::new("about_details")
                    .num_columns(2)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Game");
                        ui.label(
                            editor
                                .game_dir()
                                .map_or_else(|| "none".to_owned(), |dir| dir.display().to_string()),
                        );
                        ui.end_row();
                        ui.label("Graphics");
                        ui.label(if editor.gpu_info.is_empty() {
                            "starting"
                        } else {
                            editor.gpu_info.as_str()
                        });
                        ui.end_row();
                        ui.label("MCP server");
                        ui.label(self.mcp_url.as_deref().unwrap_or("off"));
                        ui.end_row();
                        ui.label("Settings");
                        ui.label(config::data_dir().map_or_else(String::new, |dir| dir.display().to_string()));
                        ui.end_row();
                    });
                ui.add_space(6.0);
            });
        if !open {
            editor.about_open = false;
        }
    }

    /// While Ctrl, Shift or Alt is held on its own for a moment, lists the shortcuts that
    /// use exactly those modifiers just above the status bar, as Blender does. It floats
    /// over the viewports so they keep their size, and hides as soon as another key or a
    /// mouse button goes down, so Ctrl+click and the like never flash it.
    fn key_hints(&mut self, ctx: &egui::Context, editor: &Editor) {
        const DELAY_SECONDS: f64 = 0.35;
        let (modifiers, quiet, now) = ctx.input(|input| {
            let quiet = input.keys_down.is_empty() && !input.pointer.any_down();
            (input.modifiers, quiet, input.time)
        });
        let held = modifiers.ctrl || modifiers.shift || modifiers.alt;
        if !held || !quiet || editor.camera.mouse_look() || ctx.wants_keyboard_input() {
            self.hint_modifiers = None;
            return;
        }
        let since = match self.hint_modifiers {
            Some((previous, since)) if previous == modifiers => since,
            _ => {
                self.hint_modifiers = Some((modifiers, now));
                now
            }
        };
        let waited = now - since;
        if waited < DELAY_SECONDS {
            ctx.request_repaint_after(Duration::from_secs_f64(DELAY_SECONDS - waited));
            return;
        }
        let hints = self.keymap.hints(&modifiers);
        if hints.is_empty() {
            return;
        }
        let screen = ctx.screen_rect();
        let lift = screen.bottom() - self.status_bar_top + 4.0;
        egui::Area::new(Id::new("key_hints"))
            .order(egui::Order::Foreground)
            .interactable(false)
            .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(4.0, -lift))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    // A fixed width, so the hints wrap onto more rows instead of running
                    // off the window's edge.
                    ui.set_width(screen.width() - 32.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(16.0, 4.0);
                        for (action, chord) in hints {
                            // One label per hint, so a row only ever breaks between hints.
                            let mut job = egui::text::LayoutJob::default();
                            let style = ui.style();
                            egui::RichText::new(chord.label()).monospace().strong().append_to(
                                &mut job,
                                style,
                                egui::FontSelection::Default,
                                egui::Align::Center,
                            );
                            egui::RichText::new(format!("  {}", action.name())).append_to(
                                &mut job,
                                style,
                                egui::FontSelection::Default,
                                egui::Align::Center,
                            );
                            ui.add(egui::Label::new(job).wrap_mode(egui::TextWrapMode::Extend));
                        }
                    });
                });
            });
    }

    fn status_bar(&mut self, ctx: &egui::Context, editor: &Editor) {
        let panel = egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(&editor.message);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let snap = if !editor.snap_to_grid {
                        "Off"
                    } else if editor.snap_suspended {
                        "Off (Alt)"
                    } else {
                        "On"
                    };
                    ui.label(format!("Snap: {snap}  Grid: {}", editor.grid));
                    if let Some(cursor) = self.cursor {
                        ui.label(format!("@{}, {}", cursor.x, cursor.y));
                    }
                    if let Some(summary) = editor.selection_summary() {
                        ui.label(summary);
                    }
                    if editor.document.is_some() {
                        ui.label(format!(
                            "{} solids, {} entities",
                            editor.scene.solid_count, editor.scene.entity_count
                        ));
                    }
                    if let Some(url) = &self.mcp_url {
                        ui.label(format!("MCP: {url}"));
                    }
                    compile_status(ui, editor);
                });
            });
        });
        self.status_bar_top = panel.response.rect.top();
    }

    fn viewports(&mut self, ui: &mut egui::Ui, editor: &mut Editor, frame: &eframe::Frame) {
        let area = ui.available_rect_before_wrap();
        let mut rects = editor::quad_rects(area, VIEWPORT_GAP);
        if let Some(slot) = editor.maximized {
            rects[slot] = area;
        }
        editor.sync_vertex_tool();
        editor.set_layout(rects, ui.ctx().pixels_per_point());
        let pointer = ui.input(|input| PointerState {
            position: input.pointer.latest_pos(),
            press_origin: input.pointer.press_origin(),
            modifiers: input.modifiers,
        });
        let space_held = ui.input(|input| input.key_down(egui::Key::Space));
        let mut camera_updated = false;
        let (mut camera_ms, mut views_ms) = (0.0, 0.0);
        self.cursor = None;
        editor.paste_cursor = None;
        self.end_paint_select_with_tool(editor);
        for (slot, rect) in rects.into_iter().enumerate() {
            if editor.maximized.is_some_and(|maximized| maximized != slot) {
                continue;
            }
            let started = Instant::now();
            let response = ui.interact(rect, Id::new(("viewport", slot)), Sense::click_and_drag());
            if response.hovered() {
                editor.active_slot = slot;
            }
            let mode = editor.layout[slot];
            match (mode.camera_style(), mode.view_index()) {
                (Some(style), _) => {
                    self.camera_slot(
                        ui,
                        slot,
                        style,
                        rect,
                        &response,
                        editor,
                        frame,
                        &pointer,
                        &mut camera_updated,
                    );
                    camera_ms += elapsed_ms(started);
                }
                (None, Some(view)) => {
                    self.view_slot(ui, slot, view, rect, &response, editor, frame, &pointer, space_held);
                    views_ms += elapsed_ms(started);
                }
                (None, None) => {}
            }
            viewport_menu(ui, slot, rect, editor);
        }
        editor.timings.camera_ms = camera_ms;
        editor.timings.views_2d_ms = views_ms;
    }

    /// Handles and draws a 3D viewport slot. The camera is shared, so it only takes
    /// input once per frame: in the hovered 3D slot, or the first one during mouselook.
    ///
    /// While mouselook is on the cursor is kept at the viewport's centre, so it never
    /// wanders into another viewport (where the mouse wheel would zoom that view instead),
    /// and it goes back to where it was when mouselook ends.
    #[allow(clippy::too_many_arguments)]
    fn camera_slot(
        &mut self,
        ui: &mut egui::Ui,
        slot: usize,
        style: CameraStyle,
        rect: Rect,
        response: &Response,
        editor: &mut Editor,
        frame: &eframe::Frame,
        pointer: &PointerState,
        camera_updated: &mut bool,
    ) {
        let ctx = ui.ctx().clone();
        if rect.height() >= 1.0 {
            editor.camera.aspect = f64::from(rect.width() / rect.height());
        }
        let was_looking = editor.camera.mouse_look();
        let mut moving = false;
        let typing = ctx.wants_keyboard_input() && !was_looking;
        if !*camera_updated && !typing && (response.hovered() || was_looking) {
            *camera_updated = true;
            moving = editor
                .camera
                .update(&ctx, response.hovered() || was_looking, &self.keymap);
            let looking = editor.camera.mouse_look();
            if looking && !was_looking {
                self.look_return = pointer.position;
            }
            if looking {
                let center = rect.center();
                if pointer.position.is_none_or(|position| position.distance(center) > 1.0) {
                    ctx.send_viewport_cmd(ViewportCommand::CursorPosition(center));
                }
            } else if was_looking {
                if let Some(position) = self.look_return.take() {
                    ctx.send_viewport_cmd(ViewportCommand::CursorPosition(position));
                }
            }
        }
        if response.hovered() && !typing {
            let (closer, further) = ui.input(|input| {
                let plain = !input.modifiers.ctrl && !input.modifiers.alt && !input.modifiers.shift;
                (
                    plain && input.key_pressed(egui::Key::Num1),
                    plain && input.key_pressed(egui::Key::Num2),
                )
            });
            if closer || further {
                editor.step_draw_distance(further);
                editor.save_config();
            }
        }
        let looking = editor.camera.mouse_look();
        let picking = editor.active_face_pick().is_some();
        let painting = !picking && self.paint_displacements_3d(slot, rect, response, editor, pointer);
        let gizmo_hot = !picking
            && !self.paint_selecting(slot)
            && (self.use_gizmo(slot, rect, response, editor, pointer)
                || self.vertex_tool_3d(slot, rect, response, editor, pointer)
                || self.overlay_tool_3d(slot, rect, response, editor, pointer)
                || painting);
        let selecting = !picking && self.paint_select_3d(slot, rect, response, editor, pointer, gizmo_hot);
        if picking && response.hovered() && !looking {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        let clicked = if looking {
            ui.input(|input| input.pointer.primary_pressed())
        } else {
            response.clicked_by(PointerButton::Primary)
        };
        let aim = if looking { Some(rect.center()) } else { pointer.position };
        if let (true, Some(aim), true) = (clicked, aim, picking) {
            let (origin, direction) = editor.camera.ray(rect, aim);
            editor.face_pick_click(origin, direction, pointer.modifiers.ctrl);
        } else if let (true, Some(aim), false) = (clicked, aim, gizmo_hot) {
            let (origin, direction) = editor.camera.ray(rect, aim);
            match editor.tool {
                Tool::Select if selecting => {}
                Tool::Select => {
                    let hit = editor.scene.pick_ray(origin, direction, &editor.models, editor.solid_picks());
                    pick(editor, hit, pointer.modifiers);
                    // Double-clicking opens the properties of what was picked, as in Hammer.
                    if response.double_clicked_by(PointerButton::Primary) && !editor.selection.is_empty() {
                        editor.properties_open = true;
                    }
                }
                Tool::Entity => match editor.scene.surface_hit(origin, direction) {
                    Some(point) => {
                        // Adding zero turns -0 into 0, so the origin is never written as "-0".
                        let classname = editor.entity_class.clone();
                        let keys = editor.entity_tool_keys();
                        if let Err(error) = editor.create_entity(&classname, point.round() + DVec3::ZERO, &keys) {
                            editor.message = error;
                        }
                    }
                    None => editor.message = "Click on a brush face to place the entity".to_owned(),
                },
                Tool::Texture => {
                    let hit = editor.scene.pick_face(origin, direction, &editor.models);
                    editor.texture_click(hit, pointer.modifiers);
                }
                Tool::Clip if editor.clipper.three_point && !editor.clipper.placed() => {
                    match editor.scene.surface_hit(origin, direction) {
                        Some(point) => {
                            editor.add_clip_point_3d(point);
                        }
                        None => editor.message = "Click on a brush face to place a clip point".to_owned(),
                    }
                }
                Tool::Clip => editor.cycle_clip_keep(),
                Tool::Vertex => {
                    if let Some(owner) = editor.scene.pick_ray(origin, direction, &editor.models, editor.solid_picks()) {
                        pick(editor, Some(owner), pointer.modifiers);
                    }
                }
                Tool::Overlay => editor.overlay_tool_click(origin, direction, pointer.modifiers.ctrl),
                Tool::Decal => editor.decal_tool_click(origin, direction),
                Tool::Block | Tool::Camera | Tool::Magnify => {}
            }
        }
        if editor.tool == Tool::Camera && !looking {
            self.camera_tool_3d(response, editor);
        }
        if editor.tool == Tool::Select && !looking {
            let hit = match (response.secondary_clicked(), pointer.position) {
                (true, Some(position)) => {
                    let (origin, direction) = editor.camera.ray(rect, position);
                    editor.scene.pick_ray(origin, direction, &editor.models, editor.solid_picks())
                }
                _ => None,
            };
            self.selection_context_menu(response, hit, editor);
        }
        if editor.tool == Tool::Texture && !painting {
            let applied = if looking {
                ui.input(|input| input.pointer.secondary_pressed())
            } else {
                response.clicked_by(PointerButton::Secondary)
            };
            if let (true, Some(aim)) = (applied, aim) {
                let (origin, direction) = editor.camera.ray(rect, aim);
                let hit = editor.scene.pick_face(origin, direction, &editor.models);
                editor.texture_apply_click(hit, pointer.modifiers.alt);
            }
        }
        let active = response.hovered() || moving;
        let painter = ui.painter_at(rect);
        let canvas = &mut PainterCanvas(&painter);
        match frame.wgpu_render_state() {
            Some(render_state) => {
                let texture = self.render_camera(render_state, editor, slot, style);
                let full = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                painter.image(texture, rect, full, egui::Color32::WHITE);
                let label = match style {
                    CameraStyle::Textured | CameraStyle::Lit if editor.materials.is_none() => "camera (no game set)",
                    _ => editor.layout[slot].label(),
                };
                let color = if active {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::GRAY
                };
                canvas.label(rect.left_top() + egui::vec2(6.0, 4.0), label, color);
                Camera3D::draw_crosshair(&painter, rect);
                face_pick_notice(&painter, rect, editor);
                if !picking {
                    self.draw_gizmo(&painter, slot, rect, response, editor);
                }
                vertex_tool::draw_vertex_3d(&painter, &editor.camera, rect, editor);
                displacement_tool::draw_brush(
                    &painter,
                    &editor.camera,
                    rect,
                    slot,
                    self.brush_spot,
                    editor.displacement.radius,
                );
                displacement_tool::draw_sculpt_brush(
                    &painter,
                    slot,
                    self.sculpt_brush,
                    editor.displacement.sculpt.radius,
                );
                if editor.config.view.collapsed_vertices {
                    let points = editor.collapsed_vertex_points();
                    displacement_tool::draw_collapsed_vertices(&painter, &editor.camera, rect, &points);
                }
                decal_tools::draw_overlay_handles(&painter, &editor.camera, rect, editor);
                if style == CameraStyle::Lit {
                    lighting_notice(&painter, rect, editor);
                    let animated = editor.config.lighting.animate
                        && editor.scene.lighting.lights.iter().any(|light| light.pattern.is_some());
                    if animated {
                        ctx.request_repaint_after(Duration::from_millis(100));
                    }
                    if editor.gpu.as_ref().is_some_and(Renderer3D::lighting_pending) {
                        ctx.request_repaint();
                    }
                }
                if editor.config.view.net_graph && style != CameraStyle::Wireframe {
                    let extra = editor.gpu.as_ref().map(|gpu| NetGraphExtra {
                        stats: gpu.stats,
                        reused: gpu.reused.iter().filter(|reused| **reused).count(),
                        loading: gpu.textures_loading(),
                    });
                    self.net_graph.draw(&painter, rect, &extra.unwrap_or_default());
                }
                if editor.gpu.as_ref().is_some_and(|gpu| gpu.textures_loading() > 0) {
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
                if editor.gpu.as_ref().is_some_and(|gpu| gpu.sprites_animating(slot)) {
                    ctx.request_repaint_after(Duration::from_millis(33));
                }
            }
            None => editor
                .camera
                .draw(canvas, rect, &editor.scene, active, &editor.display_overlay()),
        }
        if moving {
            ctx.request_repaint();
        }
    }

    /// The gizmo for the selection in a 3D view, while the selection tool has one.
    /// During a drag it follows the previewed move.
    fn gizmo_for(&self, slot: usize, rect: Rect, editor: &Editor) -> Option<Gizmo> {
        let shown = editor.tool == Tool::Select
            && editor.config.transform.gizmo
            && !editor.selection.is_empty()
            && !editor.camera.mouse_look();
        if !shown {
            return None;
        }
        if let Some(drag) = self.gizmo_drag.filter(|drag| drag.slot == slot) {
            let center = editor
                .overlay
                .transform
                .map_or(drag.gizmo.center, |remap| remap.apply(drag.gizmo.center));
            return Gizmo::new(&editor.camera, rect, center, drag.gizmo.mode);
        }
        Gizmo::new(&editor.camera, rect, editor.pivot(), editor.config.transform.gizmo_mode)
    }

    /// Runs the gizmo for one frame of a 3D view: starts, follows and finishes drags,
    /// and switches its mode when its centre is clicked. Returns whether the mouse is on
    /// the gizmo, so a click there does not also select.
    fn use_gizmo(
        &mut self,
        slot: usize,
        rect: Rect,
        response: &Response,
        editor: &mut Editor,
        pointer: &PointerState,
    ) -> bool {
        let Some(gizmo) = self.gizmo_for(slot, rect, editor) else {
            if self.gizmo_drag.is_some_and(|drag| drag.slot == slot) {
                self.gizmo_drag = None;
                editor.overlay.transform = None;
            }
            return false;
        };
        let camera = &editor.camera;
        let hovered = pointer
            .position
            .filter(|_| response.hovered())
            .and_then(|position| gizmo.hit(camera, rect, position));
        if response.drag_started_by(PointerButton::Primary) {
            let pressed = pointer
                .press_origin
                .and_then(|origin| gizmo.hit(camera, rect, origin).map(|part| (part, origin)));
            if let Some((part, origin)) = pressed {
                let bounds = editor.scene.selection_bounds();
                let anchor = editor.scene.selection_anchor();
                self.gizmo_drag = GizmoDrag::begin(slot, gizmo, part, camera, rect, origin, bounds, anchor);
                self.clone_drag = pointer.modifiers.shift;
            }
        }
        let Some(drag) = self.gizmo_drag.filter(|drag| drag.slot == slot) else {
            if response.clicked_by(PointerButton::Primary) && hovered == Some(GizmoPart::Center) {
                editor.cycle_gizmo_mode();
            }
            if hovered.is_some() {
                response.ctx.set_cursor_icon(egui::CursorIcon::Grab);
            }
            return hovered.is_some();
        };
        response.ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        let cloning = self.clone_drag || pointer.modifiers.shift;
        let angle_step = editor
            .config
            .transform
            .angle_step(pointer.modifiers.shift && !self.clone_drag);
        let change = pointer
            .position
            .and_then(|position| drag.change(&editor.camera, rect, position, editor.snap_step(), angle_step));
        if let Some(change) = change {
            editor.overlay.transform = Some(Remap {
                matrix: change.matrix(),
            });
            editor.overlay.cloning = cloning;
            if let GizmoChange::Rotate(matrix) = change {
                let degrees = mallet_vmf::transform::matrix_angles(mallet_vmf::transform::linear_part(&matrix));
                editor.message = format!(
                    "Rotating {}; {}",
                    mallet_vmf::format_vec3(degrees),
                    editor.config.transform.rotation_hint()
                );
            }
        }
        if response.drag_stopped() {
            self.gizmo_drag = None;
            editor.overlay.transform = None;
            let center = drag.gizmo.center;
            let result = match change {
                Some(GizmoChange::Move(offset)) if offset != DVec3::ZERO => {
                    if cloning {
                        editor.clone_selection(offset)
                    } else {
                        editor.move_selection(offset)
                    }
                }
                Some(GizmoChange::Rotate(matrix)) if matrix != glam::DMat4::IDENTITY && cloning => {
                    editor.clone_selection_and("rotated", |editor| editor.transform_selection(matrix, true, center))
                }
                Some(GizmoChange::Scale(matrix)) if matrix != glam::DMat4::IDENTITY && cloning => {
                    editor.clone_selection_and("scaled", |editor| editor.transform_selection(matrix, false, center))
                }
                Some(GizmoChange::Rotate(matrix)) if matrix != glam::DMat4::IDENTITY => editor
                    .transform_selection(matrix, true, center)
                    .map(|()| editor.message = "Rotated selection".to_owned()),
                Some(GizmoChange::Scale(matrix)) if matrix != glam::DMat4::IDENTITY => editor
                    .transform_selection(matrix, false, center)
                    .map(|()| editor.message = "Scaled selection".to_owned()),
                _ => Ok(()),
            };
            if let Err(error) = result {
                editor.message = error;
            }
        }
        true
    }

    /// Draws the gizmo over a 3D view, highlighting the part under the mouse or dragged.
    fn draw_gizmo(&self, painter: &egui::Painter, slot: usize, rect: Rect, response: &Response, editor: &Editor) {
        let Some(gizmo) = self.gizmo_for(slot, rect, editor) else {
            return;
        };
        let hot = match self.gizmo_drag.filter(|drag| drag.slot == slot) {
            Some(drag) => Some(drag.part),
            None => response
                .hover_pos()
                .and_then(|position| gizmo.hit(&editor.camera, rect, position)),
        };
        gizmo.draw(painter, &editor.camera, rect, hot);
    }

    /// Handles and draws a 2D viewport slot showing one of the editor's three 2D views.
    ///
    /// During mouselook the 2D views ignore the mouse: the cursor is only pulled back to the
    /// 3D view once per frame, so a quick flick can leave it over a 2D view for a moment, and
    /// a click there would otherwise undo the selection made through the crosshair.
    #[allow(clippy::too_many_arguments)]
    fn view_slot(
        &mut self,
        ui: &mut egui::Ui,
        slot: usize,
        view_index: usize,
        rect: Rect,
        response: &Response,
        editor: &mut Editor,
        frame: &eframe::Frame,
        pointer: &PointerState,
        space_held: bool,
    ) {
        if !editor.camera.mouse_look() {
            let zoom_step = editor.config.view.zoom_step;
            editor.views[view_index].handle_input(ui, response, rect, zoom_step);
            if response.hovered() {
                view_keys(ui, view_index, editor);
            }
            // Zooming here, before the view is cloned below, lets this frame draw the new zoom.
            if editor.tool == Tool::Magnify && !space_held {
                editor.views[view_index].handle_click_zoom(response, rect, zoom_step);
            }
        }
        let view = editor.views[view_index].clone();
        if response.hovered() && !editor.camera.mouse_look() {
            editor.active_view = view_index;
            nudge(ui, &view, editor);
        }
        if !space_held && !editor.camera.mouse_look() {
            self.use_tool_2d(slot, &view, rect, response, editor, pointer);
            if let Some(icon) = self.cursor_icon(slot, &view, rect, response, editor) {
                ui.ctx().set_cursor_icon(icon);
            }
            if editor.tool == Tool::Select {
                let hit = match (response.secondary_clicked(), pointer.position) {
                    (true, Some(position)) => {
                        let tolerance = pick_tolerance(editor) / view.scale;
                        editor.scene.pick_2d(view.axis, view.to_world(rect, position), tolerance, editor.solid_picks())
                    }
                    _ => None,
                };
                self.selection_context_menu(response, hit, editor);
            } else if editor.tool == Tool::Block {
                self.block_context_menu(response, editor);
            }
        }
        let painter = ui.painter_at(rect);
        let canvas = &mut PainterCanvas(&painter);
        let overlay = editor.display_overlay();
        match frame.wgpu_render_state() {
            Some(render_state) => {
                let texture = self.render_view(render_state, editor, slot, view_index, rect);
                let full = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                painter.image(texture, rect, full, egui::Color32::WHITE);
                view.draw_overlay(canvas, rect, &overlay, response.hovered());
                vertex_tool::draw_vertex_2d(canvas, &view, rect, editor);
            }
            None => view.draw(canvas, rect, &editor.scene, editor.shown_grid(), response.hovered(), &overlay),
        }
        camera_tool::draw_cameras(&painter, &view, rect, editor);
        if let Some(position) = response.hover_pos() {
            let point = snap(view.to_world(rect, position), editor.snap_step());
            self.cursor = Some(point);
            editor.paste_cursor = Some((view.axis, point));
        }
    }

    /// Renders a 3D slot on the GPU and returns it as an egui texture.
    fn render_camera(
        &mut self,
        render_state: &egui_wgpu::RenderState,
        editor: &mut Editor,
        slot: usize,
        style: CameraStyle,
    ) -> egui::TextureId {
        let overlay = editor.display_overlay();
        let size = editor.slot_pixels(slot);
        let lighting = LightingInput {
            options: &editor.config.lighting,
            texture_lights: &editor.texture_lights,
        };
        let renderer = editor.gpu.get_or_insert_with(|| new_renderer(render_state));
        let (view, recreated) = renderer.render(
            slot,
            style,
            &editor.scene,
            &overlay,
            &editor.camera,
            size,
            editor.materials.as_mut(),
            &editor.models,
            lighting,
        );
        show_texture(render_state, &mut self.slot_textures[slot], view, recreated)
    }

    /// Renders a 2D slot on the GPU and returns it as an egui texture.
    fn render_view(
        &mut self,
        render_state: &egui_wgpu::RenderState,
        editor: &mut Editor,
        slot: usize,
        view_index: usize,
        rect: Rect,
    ) -> egui::TextureId {
        let overlay = editor.display_overlay();
        let pixels = editor.slot_pixels(slot);
        let grid = editor.shown_grid();
        let renderer = editor.gpu.get_or_insert_with(|| new_renderer(render_state));
        let (view, recreated) = renderer.render_2d(
            slot,
            &editor.views[view_index],
            rect.size(),
            pixels,
            &editor.scene,
            &overlay,
            grid,
        );
        show_texture(render_state, &mut self.slot_textures[slot], view, recreated)
    }

    /// Chooses the drag that a left-button press at `origin` starts with the active tool:
    /// resizing from a handle, moving from inside the box, or starting a new marquee or block.
    /// With the selection tool, Ctrl always starts a marquee that adds to the selection, as in
    /// Hammer, even over the selected objects or their handles.
    fn begin_drag(index: usize, view: &View2D, rect: Rect, origin: Pos2, editor: &Editor, add: bool) -> Drag {
        let start = view.to_world(rect, origin);
        if add && editor.tool == Tool::Select {
            return Drag::Marquee { view: index, start, add };
        }
        let target = editor.handle_box();
        let mode = editor.display_overlay().handle_mode;
        let handle = target.and_then(|bounds| view.hit_handle(rect, &bounds, origin, mode));
        let inside = target.is_some_and(|bounds| box_contains(view.axis, &bounds, start));
        let on_pivot = mode == HandleMode::Rotate
            && target.is_some()
            && view.to_screen(rect, view.axis.project(editor.pivot())).distance(origin) <= PIVOT_REACH_PIXELS;
        match (editor.tool, target, handle, inside) {
            (Tool::Select, Some(_), _, _) if on_pivot => Drag::Pivot { view: index },
            (Tool::Select, Some(_), Some(_), _) if mode == HandleMode::Rotate => Drag::Rotate {
                view: index,
                start,
                pivot: editor.pivot(),
            },
            (Tool::Select, Some(bounds), Some(handle), _) if mode == HandleMode::Skew => Drag::Skew {
                view: index,
                start,
                bounds,
                handle,
            },
            (Tool::Select, Some(bounds), Some(handle), _) => Drag::Resize {
                view: index,
                start,
                bounds,
                handle,
            },
            (Tool::Select, Some(_), None, true) => Drag::Move {
                view: index,
                start,
                anchor: editor.scene.selection_anchor(),
            },
            (Tool::Select, ..) => Drag::Marquee { view: index, start, add },
            (Tool::Block, Some(bounds), Some(handle), _) => Drag::ResizeBlock {
                view: index,
                start,
                bounds,
                handle,
            },
            (Tool::Block, Some(bounds), None, true) => Drag::MoveBlock {
                view: index,
                start,
                bounds,
            },
            (
                Tool::Entity
                | Tool::Texture
                | Tool::Clip
                | Tool::Vertex
                | Tool::Overlay
                | Tool::Decal
                | Tool::Camera
                | Tool::Magnify,
                ..,
            ) => Drag::Marquee { view: index, start, add },
            (Tool::Block, ..) => Drag::NewBlock {
                view: index,
                start: snap(start, editor.snap_step()),
            },
        }
    }

    /// The resize or move cursor while hovering a handle or the inside of the box, or
    /// while dragging one.
    fn cursor_icon(
        &self,
        index: usize,
        view: &View2D,
        rect: Rect,
        response: &Response,
        editor: &Editor,
    ) -> Option<egui::CursorIcon> {
        if self.paint_selecting(index) {
            return None;
        }
        match self.drag.filter(|drag| drag.view() == index) {
            Some(Drag::Resize { handle, .. } | Drag::ResizeBlock { handle, .. }) => return Some(handle.cursor()),
            Some(Drag::Skew { handle, .. }) => return Some(handle.skew_cursor()),
            Some(Drag::Rotate { .. }) => return Some(egui::CursorIcon::Grabbing),
            Some(Drag::Move { .. } | Drag::MoveBlock { .. } | Drag::Pivot { .. }) => {
                return Some(egui::CursorIcon::Move)
            }
            Some(_) => return None,
            None => {}
        }
        let pointer = response.hover_pos()?;
        let bounds = editor.handle_box()?;
        let mode = editor.display_overlay().handle_mode;
        if mode == HandleMode::Rotate
            && view
                .to_screen(rect, view.axis.project(editor.pivot()))
                .distance(pointer)
                <= PIVOT_REACH_PIXELS
        {
            return Some(egui::CursorIcon::Move);
        }
        if let Some(handle) = view.hit_handle(rect, &bounds, pointer, mode) {
            return Some(match mode {
                HandleMode::Scale => handle.cursor(),
                HandleMode::Rotate => egui::CursorIcon::Grab,
                HandleMode::Skew => handle.skew_cursor(),
            });
        }
        box_contains(view.axis, &bounds, view.to_world(rect, pointer)).then_some(egui::CursorIcon::Move)
    }

    /// Runs the clipping tool in one 2D view, as Hammer does: a drag away from the points
    /// draws a new clip line (or, with three-point clipping, starts over with a new
    /// point), a drag from a point moves it, or with Ctrl held the whole line or every
    /// point, and a click away from the points switches which pieces are kept. A
    /// three-point clip that still lacks points takes the click as its next point.
    fn use_clip_2d(
        &mut self,
        index: usize,
        view: &View2D,
        rect: Rect,
        response: &Response,
        editor: &mut Editor,
        pointer: &PointerState,
    ) {
        let reach = f64::from(CLIP_POINT_REACH_PIXELS) / view.scale;
        if response.drag_started_by(PointerButton::Primary) {
            if let Some(origin) = pointer.press_origin {
                let world = view.to_world(rect, origin);
                let (point, grabbed) = match editor.clip_point_at(view.axis, world, reach) {
                    Some(point) => (point, true),
                    None if editor.clipper.three_point => (editor.add_clip_point_2d(view.axis, world), false),
                    None => (editor.start_clip_line(view.axis, world), false),
                };
                self.drag = Some(Drag::ClipPoint {
                    view: index,
                    point,
                    grabbed,
                });
            }
        }
        if let Some(Drag::ClipPoint {
            view: dragged,
            point,
            grabbed,
        }) = self.drag
        {
            if dragged == index {
                response.ctx.set_cursor_icon(egui::CursorIcon::Move);
                if let Some(position) = pointer.position {
                    let world = view.to_world(rect, position);
                    if grabbed && pointer.modifiers.ctrl {
                        editor.move_clip_points(point, view.axis, world);
                    } else {
                        editor.move_clip_point(point, view.axis, world);
                    }
                }
                if response.drag_stopped() {
                    self.drag = None;
                }
            }
            return;
        }
        let hovered = response
            .hover_pos()
            .map(|position| view.to_world(rect, position))
            .and_then(|world| editor.clip_point_at(view.axis, world, reach));
        if hovered.is_some() {
            response.ctx.set_cursor_icon(egui::CursorIcon::Move);
        }
        if response.clicked_by(PointerButton::Primary) && hovered.is_none() {
            if let Some(position) = response.interact_pointer_pos().or(pointer.position) {
                editor.clip_click_2d(view.axis, view.to_world(rect, position));
            }
        }
    }

    /// Runs the active tool for a left-button click or drag in one 2D view.
    fn use_tool_2d(
        &mut self,
        index: usize,
        view: &View2D,
        rect: Rect,
        response: &Response,
        editor: &mut Editor,
        pointer: &PointerState,
    ) {
        let grid = editor.snap_step();
        if editor.tool == Tool::Camera {
            self.camera_tool_2d(index, view, rect, response, editor, pointer);
            return;
        }
        if editor.tool == Tool::Magnify {
            // The Magnify tool's clicks zoom the view, which view_slot handled already.
            return;
        }
        if editor.tool == Tool::Vertex {
            self.vertex_tool_2d(index, view, rect, response, editor, pointer);
            return;
        }
        if editor.tool == Tool::Texture {
            // In 2D the texture tool picks whole brushes, as faces overlap in these views.
            if response.clicked_by(PointerButton::Primary) {
                if let Some(position) = pointer.position {
                    let tolerance = pick_tolerance(editor) / view.scale;
                    let hit = editor
                        .scene
                        .pick_2d(view.axis, view.to_world(rect, position), tolerance, editor.solid_picks());
                    editor.texture_click_2d(hit, pointer.modifiers);
                }
            }
            return;
        }
        if editor.tool == Tool::Clip {
            self.use_clip_2d(index, view, rect, response, editor, pointer);
            return;
        }
        if editor.tool == Tool::Entity {
            // Hammer's entity tool: pressing or dragging in a 2D view moves the marker
            // for the new entity; Enter creates it.
            let held = response.is_pointer_button_down_on() && response.ctx.input(|input| input.pointer.primary_down());
            if held || response.clicked_by(PointerButton::Primary) {
                if let Some(position) = pointer.position {
                    editor.place_entity_marker(view.axis, view.to_world(rect, position));
                }
            }
            return;
        }
        if editor.tool == Tool::Select && self.paint_select_2d(index, view, rect, response, editor, pointer) {
            return;
        }
        if response.drag_started_by(PointerButton::Primary) {
            if let Some(origin) = pointer.press_origin {
                self.drag = Some(Self::begin_drag(index, view, rect, origin, editor, pointer.modifiers.ctrl));
                self.clone_drag = pointer.modifiers.shift;
            }
        }

        let Some(drag) = self.drag.filter(|drag| drag.view() == index) else {
            if response.clicked_by(PointerButton::Primary) && editor.tool == Tool::Select {
                if let Some(position) = pointer.position {
                    let tolerance = pick_tolerance(editor) / view.scale;
                    let world = view.to_world(rect, position);
                    let hit = editor.scene.pick_2d(view.axis, world, tolerance, editor.solid_picks());
                    match hit {
                        Some(owner) if pointer.modifiers.shift => editor.select_picked([owner], SelectMode::Add),
                        None if pointer.modifiers.shift => {}
                        _ if !pointer.modifiers.ctrl && on_selection(editor, view.axis, world, hit) => {
                            editor.overlay.handle_mode = editor.overlay.handle_mode.next();
                            editor.message = format!("{} handles", editor.overlay.handle_mode.label());
                        }
                        _ => pick(editor, hit, pointer.modifiers),
                    }
                }
                if response.double_clicked_by(PointerButton::Primary) && !editor.selection.is_empty() {
                    editor.properties_open = true;
                }
            }
            return;
        };

        let cloning = drag.clones() && (self.clone_drag || pointer.modifiers.shift);
        editor.overlay.cloning = cloning;
        if let Some(position) = pointer.position {
            let world = view.to_world(rect, position);
            match drag {
                Drag::Move { start, anchor, .. } => {
                    let offset = snapped_offset(view.axis, anchor, world - start, grid);
                    editor.overlay.transform = Some(Remap::translate(offset));
                }
                Drag::Resize {
                    start, bounds, handle, ..
                } => {
                    let to = handle.resize(view.axis, &bounds, world - start, grid);
                    editor.overlay.transform = Some(Remap::boxes(&bounds, &to));
                }
                Drag::Rotate { start, pivot, .. } => {
                    let center = view.axis.project(pivot);
                    let (from, to) = (start - center, world - center);
                    let mut degrees = from.perp_dot(to).atan2(from.dot(to)).to_degrees();
                    let flip_snap = pointer.modifiers.shift && !self.clone_drag;
                    if let Some(step) = editor.config.transform.angle_step(flip_snap) {
                        degrees = transform::snap_angle(degrees, step);
                    }
                    let [horizontal, vertical, _] = view.axis.axes();
                    let turn = transform::plane_rotation(horizontal, vertical, degrees.to_radians());
                    editor.overlay.transform = Some(Remap {
                        matrix: mallet_vmf::transform::around(pivot, turn),
                    });
                    let hint = editor.config.transform.rotation_hint();
                    editor.message = format!("Rotating {degrees:.1}\u{b0}; {hint}");
                }
                Drag::Skew {
                    start, bounds, handle, ..
                } => {
                    editor.overlay.transform = handle
                        .skew(view.axis, &bounds, world - start, grid)
                        .map(|matrix| Remap { matrix });
                }
                Drag::Pivot { .. } => {
                    let mut pivot = editor.pivot();
                    let snapped = view.axis.lift(snap(world, grid));
                    let [horizontal, vertical, _] = view.axis.axes();
                    pivot[horizontal] = snapped[horizontal];
                    pivot[vertical] = snapped[vertical];
                    editor.set_pivot(Some(pivot));
                }
                Drag::Marquee { start, .. } => editor.overlay.marquee = Some((view.axis, start, world)),
                Drag::ClipPoint { point, .. } => editor.move_clip_point(point, view.axis, world),
                Drag::NewBlock { start, .. } => {
                    editor.overlay.block = editor.block_from_drag(view.axis, start, snap(world, grid));
                }
                Drag::MoveBlock { start, bounds, .. } => {
                    let offset = snapped_offset(view.axis, bounds.min, world - start, grid);
                    editor.overlay.block = Some(Remap::translate(offset).map_bounds(&bounds));
                }
                Drag::ResizeBlock {
                    start, bounds, handle, ..
                } => {
                    editor.overlay.block = Some(handle.resize(view.axis, &bounds, world - start, grid));
                }
            }
        }

        if response.drag_stopped() {
            self.drag = None;
            if let Err(error) = finish_drag(drag, editor, cloning) {
                editor.message = error;
            }
        }
    }
}

/// Applies a finished drag: moves, resizes, rotates or skews the selection, or with
/// `cloning` (Shift held) does that to a clone of it, or selects what the marquee covers.
/// Block drags only change the pending block, which Enter creates.
fn finish_drag(drag: Drag, editor: &mut Editor, cloning: bool) -> Result<(), String> {
    match drag {
        Drag::Move { .. } => {
            let Some(remap) = editor.overlay.transform.take() else {
                return Ok(());
            };
            let offset = remap.offset();
            match (offset == DVec3::ZERO, cloning) {
                (true, _) => Ok(()),
                (false, true) => editor.clone_selection(offset),
                (false, false) => editor.move_selection(offset),
            }
        }
        Drag::Resize { bounds, .. } => match editor.overlay.transform.take() {
            Some(remap) if remap.map_bounds(&bounds) != bounds => {
                let to = remap.map_bounds(&bounds);
                if cloning {
                    editor.clone_selection_and("resized", |editor| editor.resize_selection(to))
                } else {
                    editor.resize_selection(to)
                }
            }
            _ => Ok(()),
        },
        Drag::Rotate { pivot, .. } => match editor.overlay.transform.take() {
            Some(remap) if remap.matrix != glam::DMat4::IDENTITY => {
                let rotate = |editor: &mut Editor| editor.transform_selection(remap.matrix, true, pivot);
                if cloning {
                    editor.clone_selection_and("rotated", rotate)
                } else {
                    rotate(editor)?;
                    editor.message = "Rotated selection".to_owned();
                    Ok(())
                }
            }
            _ => Ok(()),
        },
        Drag::Skew { bounds, .. } => match editor.overlay.transform.take() {
            Some(remap) if remap.matrix != glam::DMat4::IDENTITY => {
                let skew = |editor: &mut Editor| editor.transform_selection(remap.matrix, false, bounds.min);
                if cloning {
                    editor.clone_selection_and("skewed", skew)
                } else {
                    skew(editor)?;
                    editor.message = "Skewed selection".to_owned();
                    Ok(())
                }
            }
            _ => Ok(()),
        },
        Drag::Pivot { .. } | Drag::ClipPoint { .. } => Ok(()),
        Drag::Marquee { add, .. } => {
            if let Some((axis, a, b)) = editor.overlay.marquee.take() {
                let owners = editor.scene.owners_inside(axis, a, b, editor.solid_picks());
                let mode = if add {
                    SelectMode::Add
                } else {
                    SelectMode::Replace
                };
                editor.select_picked(owners, mode);
            }
            Ok(())
        }
        Drag::NewBlock { .. } | Drag::MoveBlock { .. } | Drag::ResizeBlock { .. } => Ok(()),
    }
}

/// The world offset that moves `anchor` by a dragged distance so it lands on the grid, the
/// way Hammer snaps moves: brushes by their minimum corner, point entities by their origin.
fn snapped_offset(axis: Axis2D, anchor: DVec3, dragged: DVec2, grid: f64) -> DVec3 {
    let corner = axis.project(anchor);
    axis.lift(snap(corner + dragged, grid) - corner)
}

fn box_contains(axis: Axis2D, bounds: &Bounds, point: DVec2) -> bool {
    point.cmpge(axis.project(bounds.min)).all() && point.cmple(axis.project(bounds.max)).all()
}

/// Whether a click in a 2D view lands on the selection, which switches its handles
/// instead of picking: anywhere inside the selection's box counts, so there is no need to
/// aim for an outline or a centre cross. An object that is not selected still takes the
/// click on its own outline or centre, so it can be picked from inside the box.
fn on_selection(editor: &Editor, axis: Axis2D, point: DVec2, hit: Option<crate::scene::Owner>) -> bool {
    match hit {
        Some(owner) => editor.selection.contains(&owner),
        None => editor
            .handle_box()
            .is_some_and(|bounds| box_contains(axis, &bounds, point)),
    }
}

impl eframe::App for MalletApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let update_started = Instant::now();
        let shared = Arc::clone(&self.editor);
        let mut editor = editor::lock(&shared);
        editor.timings.previous_frame_cpu_ms = frame.info().cpu_usage.map_or(0.0, |seconds| seconds * 1000.0);
        let interval = ctx.input(|input| input.unstable_dt);
        if editor.gpu_info.is_empty() {
            if let Some(render_state) = frame.wgpu_render_state() {
                let info = render_state.adapter.get_info();
                editor.gpu_info = format!("{} ({:?})", info.name, info.backend);
            }
        }
        self.net_graph.record(editor.timings.previous_frame_cpu_ms, interval);
        let [min, average, max] = self.net_graph.interval_stats();
        editor.timings.interval_min_ms = min;
        editor.timings.interval_average_ms = average;
        editor.timings.interval_max_ms = max;
        editor.timings.recent_intervals_ms = self.net_graph.recent_intervals(40);
        editor.snap_suspended = ctx.input(|input| input.modifiers.alt);
        self.handle_input(ctx, &mut editor);
        take_window_shot(ctx, &mut editor);
        self.update_title(ctx, &editor);
        self.presence.set(presence_status(&editor));
        self.menu_bar(ctx, &mut editor);
        self.toolbar(ctx, &mut editor);
        self.tab_strip(ctx, &mut editor);
        self.status_bar(ctx, &editor);
        self.key_hints(ctx, &editor);
        self.tool_palette(ctx, &mut editor);
        self.side_panel(ctx, &mut editor);
        self.close_dialog(ctx, &mut editor);
        self.tie_prompt_window(ctx, &mut editor);
        self.hollow_window(ctx, &mut editor);
        self.arch_window(ctx, &mut editor);
        self.map_dialogs(ctx, &mut editor);
        self.problems_window(ctx, &mut editor);
        self.entity_report_window(ctx, &mut editor);
        self.properties_window(ctx, &mut editor);
        self.run_map_window(ctx, &mut editor);
        self.compile_window(ctx, &mut editor);
        self.face_edit_window(ctx, &mut editor);
        self.vertex_merge_window(ctx, &mut editor);
        self.texture_browser_window(ctx, &mut editor);
        self.model_browser_window(ctx, &mut editor);
        self.options_window(ctx, &mut editor);
        self.about_window(ctx, &mut editor);
        self.transform_window(ctx, &mut editor);
        self.paste_special_window(ctx, &mut editor);
        if let Some(text) = editor.system_clipboard.take() {
            ctx.copy_text(text);
        }
        self.finish_compile(ctx, &mut editor);
        apply_view_options(&mut editor);
        self.apply_interface_options(ctx, &editor);
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(egui::Color32::from_gray(30)))
            .show(ctx, |ui| self.viewports(ui, &mut editor, frame));
        editor.timings.update_ms = elapsed_ms(update_started);
        editor.frames_drawn += 1;
    }

    /// Feeds one frame of queued synthetic input from the `send_input` tool.
    ///
    /// A plain Tab is Hammer's next 2D view key. Unless a text field is being typed in,
    /// it is taken out of the input here, before egui would move the keyboard focus to a
    /// button of the side panel with it.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let mut editor = editor::lock(&self.editor);
        if let Some(frame) = editor.input_queue.pop_front() {
            raw_input.modifiers = frame.modifiers;
            raw_input.events.extend(frame.events);
            ctx.request_repaint();
        }
        if ctx.wants_keyboard_input() {
            return;
        }
        let before = raw_input.events.len();
        raw_input.events.retain(|event| {
            !matches!(
                event,
                egui::Event::Key { key: egui::Key::Tab, pressed: true, modifiers, .. } if modifiers.is_none()
            )
        });
        if raw_input.events.len() < before {
            let slot = editor.active_slot;
            editor.cycle_2d_view(slot);
        }
    }
}

/// What Discord should show for the editor's current state, or `None` when Rich
/// Presence is turned off in Mallet Options.
fn presence_status(editor: &Editor) -> Option<Status> {
    let options = &editor.config.discord;
    if !options.enabled {
        return None;
    }
    let details = match &editor.document {
        Some(document) if options.show_map_name => format!("Editing {}", document.file_name()),
        Some(_) => "Editing a map".to_owned(),
        None => "No map open".to_owned(),
    };
    let compiling = editor.compile_job.as_ref().is_some_and(|job| job.is_running());
    let open = editor.tabs().len();
    let state = if compiling {
        "Compiling".to_owned()
    } else if open > 1 {
        format!("{open} maps open")
    } else {
        String::new()
    };
    Some(Status { details, state })
}

/// What was picked in File > Recent Maps.
enum RecentChoice {
    None,
    Open(PathBuf),
    ForgetMissing,
    Clear,
}

/// File > Recent Maps: the latest first, numbered like Hammer's, with the folder beside
/// each name. Maps that no longer exist are greyed out.
fn recent_maps_menu(ui: &mut egui::Ui, maps: &[PathBuf], choice: &mut RecentChoice) {
    ui.add_enabled_ui(!maps.is_empty(), |ui| {
        ui.menu_button("Recent Maps", |ui| {
            let mut missing = false;
            for (index, path) in maps.iter().enumerate() {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                let folder = path.parent().map(short_folder).unwrap_or_default();
                let exists = path.is_file();
                missing |= !exists;
                let label = if index < 9 {
                    format!("{}  {name}", index + 1)
                } else {
                    format!("    {name}")
                };
                // One line per map: the menu widens to fit instead of squeezing the name
                // beside a long folder, which wrapped it a letter per line.
                let button = egui::Button::new(label)
                    .shortcut_text(folder)
                    .wrap_mode(egui::TextWrapMode::Extend);
                let response = ui.add_enabled(exists, button);
                if exists {
                    if response.on_hover_text(path.display().to_string()).clicked() {
                        *choice = RecentChoice::Open(path.clone());
                        ui.close_menu();
                    }
                } else {
                    response.on_disabled_hover_text(format!("{} no longer exists", path.display()));
                }
            }
            ui.separator();
            if missing && ui.button("Forget missing maps").clicked() {
                *choice = RecentChoice::ForgetMissing;
                ui.close_menu();
            }
            if ui.button("Clear the list").clicked() {
                *choice = RecentChoice::Clear;
                ui.close_menu();
            }
        });
    });
}

/// Longest folder File > Recent Maps shows beside a map, in characters.
const RECENT_FOLDER_CHARS: usize = 48;

/// A folder short enough for a menu: the drive, then as many of the last folders as fit,
/// such as `C:\…\maps_rp_city24\maps`. The full path shows when hovering.
fn short_folder(folder: &Path) -> String {
    let full = folder.display().to_string();
    if full.chars().count() <= RECENT_FOLDER_CHARS {
        return full;
    }
    let separator = std::path::MAIN_SEPARATOR;
    let root: String = folder
        .components()
        .take_while(|part| matches!(part, std::path::Component::Prefix(_) | std::path::Component::RootDir))
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    let names: Vec<String> = folder
        .components()
        .filter_map(|part| match part {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let mut tail = String::new();
    for name in names.iter().rev() {
        let longer = if tail.is_empty() {
            name.clone()
        } else {
            format!("{name}{separator}{tail}")
        };
        if root.chars().count() + 2 + longer.chars().count() > RECENT_FOLDER_CHARS && !tail.is_empty() {
            break;
        }
        tail = longer;
    }
    format!("{root}…{separator}{tail}")
}

/// Copies the viewport settings from Mallet Options onto the camera and the view style
/// every frame, so changes show at once.
fn apply_view_options(editor: &mut Editor) {
    let view = &editor.config.view;
    editor.camera.far = view.draw_distance;
    editor.camera.fov = view.fov;
    editor.camera.move_speed = view.camera_speed;
    editor.camera.look_sensitivity = view.look_sensitivity;
    editor.camera.turn_speed = view.turn_speed;
    editor.camera.acceleration = view.camera_acceleration;
    editor.camera.wheel_step = view.wheel_step;
    editor.camera.crouch_speed = view.crouch_speed;
    editor.camera.look_frames = view.look_smoothing;
    crate::style::set(crate::style::ViewStyle::from_options(&editor.config.colors, view));
}

/// How close, in pixels, a 2D click must be to a brush's outline to select it.
fn pick_tolerance(editor: &Editor) -> f64 {
    let tolerance = editor.config.view.pick_tolerance;
    if tolerance.is_finite() {
        tolerance.clamp(1.0, 32.0)
    } else {
        4.0
    }
}

/// A small progress readout of the running or last compile, in the status bar.
fn compile_status(ui: &mut egui::Ui, editor: &Editor) {
    let Some(job) = &editor.compile_job else { return };
    let job = job.lock();
    let text = match &job.outcome {
        None => {
            let step = job
                .steps
                .iter()
                .find(|step| step.status == crate::compile::StepStatus::Running)
                .map_or("Starting", |step| step.title.as_str());
            ui.add(
                egui::ProgressBar::new(job.progress())
                    .desired_width(90.0)
                    .desired_height(12.0),
            );
            format!("Compiling: {step} {:.0}%", job.progress() * 100.0)
        }
        Some(crate::compile::Outcome::Succeeded) => format!("Compiled {}", job.map),
        Some(crate::compile::Outcome::Failed(_)) => format!("Compile of {} failed", job.map),
        Some(crate::compile::Outcome::Cancelled) => "Compile cancelled".to_owned(),
    };
    ui.label(text);
}

/// Waits, without holding the editor, until the compile ends or the time is up.
fn wait_for_compile(shared: &Mutex<Editor>, limit: Duration) {
    let Some(job) = editor::lock(shared).compile_job.as_ref().map(|job| job.shared()) else {
        return;
    };
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if job.lock().is_ok_and(|state| !state.is_running()) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Asks the window for a screenshot of itself and waits for the PNG.
fn window_screenshot(shared: &Mutex<Editor>, ctx: &egui::Context) -> Result<Vec<u8>, String> {
    editor::lock(shared).window_shot = WindowShot::Requested;
    let deadline = Instant::now() + INPUT_TIMEOUT;
    while Instant::now() < deadline {
        {
            let mut editor = editor::lock(shared);
            if let WindowShot::Done(_) = editor.window_shot {
                let WindowShot::Done(result) = std::mem::take(&mut editor.window_shot) else {
                    unreachable!("matched above");
                };
                return result;
            }
        }
        ctx.request_repaint();
        thread::sleep(Duration::from_millis(5));
    }
    editor::lock(shared).window_shot = WindowShot::Idle;
    Err("the window did not take the screenshot in time; it may be minimized".to_owned())
}

/// Sends a requested window screenshot to the GPU and encodes the image when it arrives.
fn take_window_shot(ctx: &egui::Context, editor: &mut Editor) {
    match editor.window_shot {
        WindowShot::Requested => {
            ctx.send_viewport_cmd(ViewportCommand::Screenshot(egui::UserData::default()));
            editor.window_shot = WindowShot::Waiting;
            ctx.request_repaint();
        }
        WindowShot::Waiting => {
            let image = ctx.input(|input| {
                input.raw.events.iter().find_map(|event| match event {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            match image {
                Some(image) => editor.window_shot = WindowShot::Done(encode_screenshot(&image)),
                None => ctx.request_repaint(),
            }
        }
        _ => {}
    }
}

fn encode_screenshot(image: &egui::ColorImage) -> Result<Vec<u8>, String> {
    let [width, height] = image.size;
    let size = tiny_skia::IntSize::from_wh(width as u32, height as u32).ok_or("the window has no area")?;
    let bytes: Vec<u8> = image.pixels.iter().flat_map(|pixel| pixel.to_array()).collect();
    let pixmap = tiny_skia::Pixmap::from_vec(bytes, size).ok_or("the screenshot has the wrong size")?;
    pixmap.encode_png().map_err(|error| error.to_string())
}

/// Waits until the window has handled all queued synthetic input, including the frame
/// that consumed the last of it.
fn wait_for_input(shared: &Mutex<Editor>, ctx: &egui::Context) -> Result<(), String> {
    let deadline = Instant::now() + INPUT_TIMEOUT;
    let mut drained_at = None;
    while Instant::now() < deadline {
        {
            let editor = editor::lock(shared);
            match drained_at {
                None if editor.input_queue.is_empty() => drained_at = Some(editor.frames_drawn),
                Some(frame) if editor.frames_drawn > frame => return Ok(()),
                _ => {}
            }
        }
        ctx.request_repaint();
        thread::sleep(Duration::from_millis(5));
    }
    editor::lock(shared).input_queue.clear();
    Err("the window did not handle the input in time; it may be minimized".to_owned())
}

/// Mounts the configured game folder, or finds an installed Source game the first time
/// and remembers it.
fn load_game_content(editor: &mut Editor) {
    editor.config = Config::load();
    editor.grid = editor.config.view.default_grid;
    editor.material = editor.config.editing.material();
    editor.entity_class = editor.config.editing.entity();
    let config = editor.config.clone();
    let detected = config.game_dir.is_none();
    let found = || config::detect_game_dir(config.steam_dir.as_deref());
    let Some(game_dir) = config.game_dir.clone().or_else(found) else {
        return;
    };
    if editor.set_game_dir(&game_dir).is_ok() && detected {
        editor.config.game_dir = Some(game_dir);
        editor.save_config();
    }
    if !config.fgds.is_empty() {
        if let Err(error) = editor.set_fgds(&config.fgds) {
            editor.message = format!("Could not read the chosen FGD files: {error}");
        }
    }
}

/// Asks for the FGD files to read entity classes from, reads them and remembers them.
fn choose_fgd_files(editor: &mut Editor) {
    let mut dialog = rfd::FileDialog::new()
        .set_title("Choose the game's FGD files")
        .add_filter("Forge Game Data", &["fgd"]);
    if let Some(bin) = editor
        .game_dir()
        .and_then(Path::parent)
        .map(|parent| parent.join("bin"))
    {
        dialog = dialog.set_directory(bin);
    }
    let Some(paths) = dialog.pick_files() else {
        return;
    };
    if let Err(error) = editor.choose_fgds(paths) {
        editor.message = error;
    }
}

/// Asks for the game folder that holds `gameinfo.txt`, mounts it and remembers it.
fn choose_game_dir(editor: &mut Editor) {
    let Some(folder) = rfd::FileDialog::new()
        .set_title("Choose the game folder that contains gameinfo.txt")
        .pick_folder()
    else {
        return;
    };
    if let Err(error) = editor.choose_game(&folder) {
        editor.message = error;
    }
}

/// Opens the map given on the command line, or starts a new map like Hammer when there is
/// none or it fails to load. A load failure stays in the status bar.
fn open_or_new(editor: &mut Editor, path: Option<PathBuf>) {
    let Some(path) = path else {
        editor.new_map();
        return;
    };
    if editor.open(&path).is_err() {
        let failure = std::mem::take(&mut editor.message);
        editor.new_map();
        editor.message = failure;
    }
}

/// Draws a viewport's label as a button: clicking it opens a menu to switch the slot
/// between the 3D and 2D view types. A lighting preview also gets a sun button beside its
/// label with the lighting settings used most.
fn viewport_menu(ui: &mut egui::Ui, slot: usize, rect: Rect, editor: &mut Editor) {
    let mode = editor.layout[slot];
    let font = egui::FontId::proportional(13.0);
    let width = ui.fonts(|fonts| {
        fonts
            .layout_no_wrap(mode.label().to_owned(), font, egui::Color32::WHITE)
            .size()
            .x
    });
    let label = Rect::from_min_size(rect.left_top() + egui::vec2(3.0, 2.0), egui::vec2(width + 8.0, 19.0));
    let response = ui
        .interact(label, Id::new(("viewport_label", slot)), Sense::click())
        .on_hover_text("Change this viewport's type");
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter_at(rect).rect_stroke(
            label,
            2.0,
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(120)),
            egui::StrokeKind::Inside,
        );
    }
    let popup = Id::new(("viewport_menu", slot));
    if response.clicked() {
        ui.memory_mut(|memory| memory.toggle_popup(popup));
    }
    egui::popup::popup_below_widget(ui, popup, &response, egui::PopupCloseBehavior::CloseOnClick, |ui| {
        ui.set_min_width(140.0);
        for (index, choice) in ViewMode::ALL.into_iter().enumerate() {
            let first_2d =
                index > 0 && choice.view_index().is_some() && ViewMode::ALL[index - 1].view_index().is_none();
            if first_2d {
                ui.separator();
            }
            if ui.selectable_label(choice == mode, choice.menu_label()).clicked() {
                editor.layout[slot] = choice;
            }
        }
    });
    let mut beside = label;
    if mode == ViewMode::Lighting {
        beside = lighting_button(ui, slot, rect, label, editor);
    }
    if mode.view_index().is_none() {
        camera_button(ui, slot, rect, beside, editor);
    }
}

/// The camera button beside a 3D viewport's label, which opens the field of view, draw
/// distance and the camera's preview in the 2D views; Mallet Options > Viewports has
/// every viewport setting.
fn camera_button(ui: &mut egui::Ui, slot: usize, rect: Rect, beside: Rect, editor: &mut Editor) {
    let button = Rect::from_min_size(beside.right_top() + egui::vec2(2.0, 0.0), egui::vec2(20.0, 19.0));
    let response = ui
        .interact(button, Id::new(("camera_button", slot)), Sense::click())
        .on_hover_text("Camera settings");
    let painter = ui.painter_at(rect);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        painter.rect_stroke(
            button,
            2.0,
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(120)),
            egui::StrokeKind::Inside,
        );
    }
    painter.text(
        button.center(),
        egui::Align2::CENTER_CENTER,
        egui_phosphor::regular::VIDEO_CAMERA,
        egui::FontId::proportional(14.0),
        crate::style::current().camera,
    );
    let popup = Id::new(("camera_settings", slot));
    if response.clicked() {
        ui.memory_mut(|memory| memory.toggle_popup(popup));
    }
    egui::popup::popup_below_widget(
        ui,
        popup,
        &response,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(250.0);
            let mut more = false;
            let changed = options::camera_quick_settings(ui, editor, &mut more);
            if more {
                editor.options_open = true;
                editor.options_section = Some("viewports".to_owned());
                ui.memory_mut(|memory| memory.close_popup());
            }
            let unsaved = ui.memory(|memory| memory.data.get_temp::<bool>(popup).unwrap_or(false)) || changed;
            let busy = ui.input(|input| input.pointer.any_down());
            if unsaved && !busy {
                editor.save_config();
            }
            ui.memory_mut(|memory| memory.data.insert_temp(popup, unsaved && busy));
        },
    );
}

/// The sun button beside a lighting preview's label, which opens the lighting settings
/// used most; Mallet Options > Lighting preview has every one.
fn lighting_button(ui: &mut egui::Ui, slot: usize, rect: Rect, label: Rect, editor: &mut Editor) -> Rect {
    let button = Rect::from_min_size(label.right_top() + egui::vec2(2.0, 0.0), egui::vec2(20.0, 19.0));
    let response = ui
        .interact(button, Id::new(("lighting_button", slot)), Sense::click())
        .on_hover_text("Lighting preview settings");
    let painter = ui.painter_at(rect);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        painter.rect_stroke(
            button,
            2.0,
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(120)),
            egui::StrokeKind::Inside,
        );
    }
    painter.text(
        button.center(),
        egui::Align2::CENTER_CENTER,
        egui_phosphor::regular::SUN,
        egui::FontId::proportional(14.0),
        egui::Color32::from_rgb(255, 214, 120),
    );
    let popup = Id::new(("lighting_settings", slot));
    if response.clicked() {
        ui.memory_mut(|memory| memory.toggle_popup(popup));
    }
    egui::popup::popup_below_widget(
        ui,
        popup,
        &response,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(250.0);
            let before = editor.config.lighting.clone();
            let mut more = false;
            options::lighting_quick_settings(ui, editor, &mut more);
            let stats = editor.gpu.as_ref().and_then(Renderer3D::lighting_stats);
            if let Some(stats) = stats {
                ui.separator();
                let waiting = if stats.waiting > 0 {
                    format!(", {} more coming", stats.waiting)
                } else {
                    String::new()
                };
                let luxels = match (stats.baking, stats.luxels_shown) {
                    (Some(done), _) => format!("\nBaking {} luxels: {:.0}%", stats.luxels, done * 100.0),
                    (None, true) => format!("\nFaces lit from {} baked luxels", stats.luxels),
                    (None, false) => String::new(),
                };
                ui.label(
                    egui::RichText::new(format!(
                        "{} lights ({} from textures), {} with shadows and {} with small shadows{waiting}{luxels}",
                        stats.lights, stats.texture_lights, stats.shadowed, stats.small_shadowed
                    ))
                    .small()
                    .color(egui::Color32::GRAY),
                );
            }
            if more {
                editor.options_open = true;
                editor.options_section = Some("lighting".to_owned());
                ui.memory_mut(|memory| memory.close_popup());
            }
            let busy = ui.input(|input| input.pointer.any_down());
            if editor.config.lighting != before && !busy {
                editor.save_config();
            }
        },
    );
    button
}

/// A hint at the bottom of a lighting preview whose map gives off no light at all.
fn lighting_notice(painter: &egui::Painter, rect: Rect, editor: &Editor) {
    let lighting = &editor.scene.lighting;
    let dark = lighting.lights.is_empty()
        && lighting.ambient.is_none()
        && editor.texture_lights.is_empty()
        && editor.config.lighting.fill <= 0.0;
    if !dark || editor.document.is_none() {
        return;
    }
    painter.text(
        rect.left_bottom() + egui::vec2(8.0, -8.0),
        egui::Align2::LEFT_BOTTOM,
        "No lights: place one from the side panel's Lighting menu, or raise the fill light",
        egui::FontId::proportional(12.0),
        egui::Color32::from_gray(160),
    );
}

/// Tells what clicks in the 3D view do while they pick faces for a keyvalue.
fn face_pick_notice(painter: &egui::Painter, rect: Rect, editor: &Editor) {
    let Some(pick) = editor.active_face_pick() else {
        return;
    };
    painter.text(
        rect.center_top() + egui::vec2(0.0, 26.0),
        egui::Align2::CENTER_TOP,
        format!(
            "Picking faces for {}: click lists one face, Ctrl+click adds or takes off, Escape stops",
            pick.key
        ),
        egui::FontId::proportional(13.0),
        egui::Color32::from_rgb(255, 210, 60),
    );
}

/// Makes the viewport renderer for the window's graphics card.
fn new_renderer(render_state: &egui_wgpu::RenderState) -> Renderer3D {
    let gamma_view = render_state
        .adapter
        .get_downlevel_capabilities()
        .flags
        .contains(eframe::wgpu::DownlevelFlags::VIEW_FORMATS);
    Renderer3D::new(&render_state.device, &render_state.queue, gamma_view)
}

/// Registers a GPU image with egui the first time, and refreshes egui's reference when the
/// image was recreated.
fn show_texture(
    render_state: &egui_wgpu::RenderState,
    slot: &mut Option<egui::TextureId>,
    view: &eframe::wgpu::TextureView,
    recreated: bool,
) -> egui::TextureId {
    let mut egui_renderer = render_state.renderer.write();
    let filter = eframe::wgpu::FilterMode::Nearest;
    match *slot {
        Some(id) if !recreated => id,
        Some(id) => {
            egui_renderer.update_egui_texture_from_wgpu_texture(&render_state.device, view, filter, id);
            id
        }
        None => {
            let id = egui_renderer.register_native_texture(&render_state.device, view, filter);
            *slot = Some(id);
            id
        }
    }
}

fn elapsed_ms(started: Instant) -> f32 {
    started.elapsed().as_secs_f32() * 1000.0
}

fn snap(point: DVec2, grid: f64) -> DVec2 {
    (point / grid).round() * grid
}

/// Applies a click: selects what was hit, or with Ctrl toggles it. Clicking empty space
/// without Ctrl clears the selection.
fn pick(editor: &mut Editor, hit: Option<crate::scene::Owner>, modifiers: Modifiers) {
    match (hit, modifiers.ctrl) {
        (Some(owner), true) => editor.select_picked([owner], SelectMode::Toggle),
        (Some(owner), false) => editor.select_picked([owner], SelectMode::Replace),
        (None, true) => {}
        (None, false) => editor.clear_selection(),
    }
}

fn run(action: Action, editor: &mut Editor) {
    match action {
        Action::OpenFile => {
            let both = [MapFormat::Mallet.extension(), MapFormat::Vmf.extension()];
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Maps", &both)
                .add_filter(MapFormat::Mallet.name(), &[MapFormat::Mallet.extension()])
                .add_filter(MapFormat::Vmf.name(), &[MapFormat::Vmf.extension()])
                .pick_file()
            {
                let _ = editor.open_in_tab(&path);
            }
        }
        Action::NewFile => editor.new_tab(),
        Action::SaveFile => {
            if editor.is_untitled() {
                save_as_dialog(editor);
            } else {
                let _ = editor.save(None);
            }
        }
        Action::Undo => {
            let _ = editor.step_history(false);
        }
        Action::Redo => {
            let _ = editor.step_history(true);
        }
        Action::Cut => report(editor.cut_selection().map(drop), editor),
        Action::Copy => report(editor.copy_selection().map(drop), editor),
        Action::Paste => {
            let at = editor.paste_point();
            report(editor.paste(at, &editor::PasteOptions::default()).map(drop), editor)
        }
        Action::PasteSpecial => report(editor.open_paste_special(), editor),
        Action::GridSmaller => editor.change_grid(0.5),
        Action::GridLarger => editor.change_grid(2.0),
        Action::ToggleSnapToGrid => {
            editor.snap_to_grid = !editor.snap_to_grid;
            editor.message = format!("Snap to grid {}", on_off(editor.snap_to_grid));
        }
        Action::ToggleShowGrid => {
            editor.show_grid = !editor.show_grid;
            editor.message = format!("Grid {}", if editor.show_grid { "shown" } else { "hidden" });
        }
        Action::ToggleDisplacementEdgeCollapse => {
            let shown = !editor.config.view.collapsed_vertices;
            editor.config.view.collapsed_vertices = shown;
            editor.save_config();
            editor.message = if shown {
                "Showing in pink the displacement vertices the compiler collapses next to lower-power neighbours"
            } else {
                "Hiding the collapsed displacement vertices"
            }
            .to_owned();
        }
        Action::SelectionTool => editor.set_tool(Tool::Select),
        Action::CameraTool => {
            editor.set_tool(Tool::Camera);
            editor.message = "Camera tool: drag in a 2D view to place the camera (Shift adds one); Page Up and Page Down switch cameras; drag in 3D to look (left) or move (right)"
                .to_owned();
        }
        Action::MagnifyTool => {
            editor.set_tool(Tool::Magnify);
            editor.message = "Magnify tool: click a 2D view to zoom in, right-click to zoom out".to_owned();
        }
        Action::PreviousCamera | Action::NextCamera if editor.tool == Tool::Camera => {
            editor.step_camera(action == Action::NextCamera);
        }
        Action::PreviousCamera | Action::NextCamera => {}
        Action::DeleteSelection if editor.tool == Tool::Camera => report(editor.delete_camera(), editor),
        Action::BlockTool => editor.set_tool(Tool::Block),
        Action::EntityTool => editor.set_tool(Tool::Entity),
        Action::TextureTool => editor.set_tool(Tool::Texture),
        Action::ApplyCurrentTexture => report(editor.apply_current_texture().map(drop), editor),
        Action::ClipTool if editor.tool == Tool::Clip => editor.cycle_clip_keep(),
        Action::ClipTool => {
            editor.set_tool(Tool::Clip);
            editor.message = editor.clip_tool_hint();
        }
        Action::ClipThreePoint if editor.tool == Tool::Clip => editor.toggle_three_point_clip(),
        Action::ClipThreePoint => {}
        Action::ClipMeasurements if editor.tool == Tool::Clip => editor.toggle_clip_measurements(),
        Action::ClipMeasurements => {}
        Action::VertexTool => {
            editor.set_tool(Tool::Vertex);
            editor.message = if editor.selection.is_empty() {
                "Vertex tool: select brushes to edit their vertices".to_owned()
            } else {
                "Vertex tool: drag corners and edges; Ctrl+F splits a face between two corners".to_owned()
            };
        }
        Action::OverlayTool => {
            editor.set_tool(Tool::Overlay);
            editor.message =
                "Overlay tool: click a face in the 3D view to place the current material; drag a corner to resize"
                    .to_owned();
        }
        Action::DecalTool => {
            editor.set_tool(Tool::Decal);
            editor.message = "Decal tool: click a face in the 3D view to place the current material".to_owned();
        }
        Action::SplitFace => report(editor.split_vertex_face(), editor),
        Action::SelectAll => editor.select_all(),
        Action::IgnoreGroups => editor.cycle_pick_mode(),
        Action::ClearSelection if editor.stop_face_pick() => {}
        Action::ClearSelection => {
            let pending = editor.overlay.block.take().is_some()
                | editor.overlay.entity.take().is_some()
                | (editor.tool == Tool::Clip && editor.clear_clip_points());
            if editor.tool == Tool::Texture {
                editor.clear_face_selection();
            } else if editor.tool == Tool::Vertex && (editor.vertex.merge_prompt || editor.clear_vertex_selection()) {
            } else if editor.tool == Tool::Clip && !pending {
                editor.set_tool(Tool::Select);
                editor.message = "Selection tool".to_owned();
            } else if !pending {
                editor.clear_selection();
            }
        }
        Action::DeleteSelection => {
            let _ = editor.delete_selection();
        }
        Action::HideSelection => {
            let _ = editor.hide_selection();
        }
        Action::HideUnselected => {
            let _ = editor.hide_unselected();
        }
        Action::ShowAllHidden => {
            let _ = editor.show_all_hidden();
        }
        Action::CreateBlock => {
            let result = match editor.tool {
                Tool::Block => editor.commit_block().map(|_| ()),
                Tool::Entity => editor.commit_entity().map(|_| ()),
                Tool::Clip => editor.apply_clip().map(|_| ()),
                Tool::Select
                | Tool::Texture
                | Tool::Vertex
                | Tool::Overlay
                | Tool::Decal
                | Tool::Camera
                | Tool::Magnify => Ok(()),
            };
            if let Err(error) = result {
                editor.message = error;
            }
        }
        Action::CenterViews2D => editor.center_2d_on_selection(),
        Action::CenterView3D => editor.center_3d_on_selection(),
        Action::MaximizeView => editor.toggle_maximized(editor.active_slot),
        Action::ViewTop => editor.set_slot_mode(editor.active_slot, ViewMode::Top),
        Action::ViewSide => editor.set_slot_mode(editor.active_slot, ViewMode::Side),
        Action::ViewFront => editor.set_slot_mode(editor.active_slot, ViewMode::Front),
        Action::ViewTextured => editor.set_slot_mode(editor.active_slot, ViewMode::Textured),
        Action::ViewLighting => editor.set_slot_mode(editor.active_slot, ViewMode::Lighting),
        Action::NextViewType => editor.cycle_2d_view(editor.active_slot),
        Action::MapProperties => {
            editor.properties_world = true;
            editor.properties_open = true;
        }
        Action::Properties => {
            editor.properties_world = false;
            editor.properties_open = true;
        }
        Action::MoveToWorld => report(editor.move_to_world().map(drop), editor),
        Action::Carve => report(editor.carve().map(drop), editor),
        Action::Group => report(editor.group_selection(), editor),
        Action::Ungroup => report(editor.ungroup_selection(), editor),
        Action::Options => editor.options_open = true,
        Action::Transform => editor.transform_open = true,
        Action::SnapToGrid | Action::SnapToGridIndividually => report(
            editor.snap_selection_to_grid(action == Action::SnapToGridIndividually),
            editor,
        ),
        Action::FlipHorizontal | Action::FlipVertical => {
            report(editor.flip_in_view(action == Action::FlipVertical), editor)
        }
        Action::AlignLeft => report(editor.align_in_view(false, false), editor),
        Action::AlignRight => report(editor.align_in_view(false, true), editor),
        Action::AlignTop => report(editor.align_in_view(true, true), editor),
        Action::AlignBottom => report(editor.align_in_view(true, false), editor),
        Action::ToggleTextureLock => {
            editor.config.transform.texture_lock = !editor.config.transform.texture_lock;
            editor.message = format!("Texture lock {}", on_off(editor.config.transform.texture_lock));
            editor.save_config();
        }
        Action::ToggleTextureScaleLock => {
            editor.config.transform.texture_scale_lock = !editor.config.transform.texture_scale_lock;
            editor.message = format!(
                "Texture scale lock {}",
                on_off(editor.config.transform.texture_scale_lock)
            );
            editor.save_config();
        }
        Action::ToggleGizmo => {
            editor.config.transform.gizmo = !editor.config.transform.gizmo;
            editor.message = format!("3D gizmo {}", on_off(editor.config.transform.gizmo));
            editor.save_config();
        }
        Action::CycleGizmoMode => editor.cycle_gizmo_mode(),
        Action::ToggleRotationSnap => {
            let transform = &mut editor.config.transform;
            transform.snap_rotation = !transform.snap_rotation;
            editor.message = format!(
                "Rotation snap {} ({}\u{b0})",
                on_off(transform.snap_rotation),
                transform.rotation_snap
            );
            editor.save_config();
        }
        _ => {}
    }
}

/// Whether the arrow keys have something to move: the selection with the Selection tool,
/// or the selected corners and edges with the Vertex tool.
fn nudges(editor: &Editor) -> bool {
    editor.config.transform.arrow_nudge
        && (editor.nudges_vertices() || (editor.tool == Tool::Select && !editor.selection.is_empty()))
}

/// Moves the selection with the arrow keys in a hovered 2D view, along the view's axes:
/// one grid step, or one unit with Ctrl, as Hammer's nudge does. With the Vertex tool the
/// selected corners and edges move instead of the whole brushes.
fn nudge(ui: &egui::Ui, view: &View2D, editor: &mut Editor) {
    if !nudges(editor) || ui.ctx().wants_keyboard_input() {
        return;
    }
    let (step, offset) = ui.input(|input| {
        let step = if input.modifiers.ctrl { 1.0 } else { editor.grid };
        let mut offset = DVec2::ZERO;
        for (key, direction) in [
            (egui::Key::ArrowLeft, DVec2::NEG_X),
            (egui::Key::ArrowRight, DVec2::X),
            (egui::Key::ArrowUp, DVec2::Y),
            (egui::Key::ArrowDown, DVec2::NEG_Y),
        ] {
            if input.key_pressed(key) && !input.modifiers.alt && !input.modifiers.shift {
                offset += direction;
            }
        }
        (step, offset)
    });
    if offset == DVec2::ZERO {
        return;
    }
    let offset = view.axis.lift(offset * step);
    if editor.nudges_vertices() {
        editor.nudge_vertices(offset);
    } else {
        report(editor.move_selection(offset), editor);
    }
}

/// Hammer's keys over a 2D view: 1 to 9 and 0 pick a preset zoom, Ctrl with the mouse
/// wheel zooms every 2D view together, and the arrow keys pan the view when they have no
/// selection to nudge.
fn view_keys(ui: &egui::Ui, view_index: usize, editor: &mut Editor) {
    if ui.ctx().wants_keyboard_input() {
        return;
    }
    let (digit, synced, pan) = ui.input(|input| {
        let plain = !input.modifiers.ctrl && !input.modifiers.alt && !input.modifiers.shift;
        let digits = [
            egui::Key::Num0,
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
        ];
        let digit = digits
            .iter()
            .position(|key| plain && input.key_pressed(*key))
            .map(|digit| digit as u32);
        let synced = input.modifiers.ctrl && input.raw_scroll_delta.y != 0.0;
        let mut pan = DVec2::ZERO;
        for (key, direction) in [
            (egui::Key::ArrowLeft, DVec2::NEG_X),
            (egui::Key::ArrowRight, DVec2::X),
            (egui::Key::ArrowUp, DVec2::Y),
            (egui::Key::ArrowDown, DVec2::NEG_Y),
        ] {
            if plain && input.key_pressed(key) {
                pan += direction;
            }
        }
        (digit, synced, pan)
    });
    if let Some(digit) = digit {
        editor.preset_zoom(view_index, digit);
    }
    if synced {
        let scale = editor.views[view_index].scale;
        for view in &mut editor.views {
            view.set_scale(scale);
        }
    }
    if pan != DVec2::ZERO && !nudges(editor) {
        let view = &mut editor.views[view_index];
        view.center += pan * PAN_PIXELS / view.scale;
    }
}

/// Shows why a command failed in the status bar.
fn report(result: Result<(), String>, editor: &mut Editor) {
    if let Err(error) = result {
        editor.message = error;
    }
}

fn on_off(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

/// File > Save As. The map's own format is offered first (for a new map, the one chosen
/// in Mallet Options > Files), and the other one beside it, so saving a VMF as a `.mallet`
/// map is a pick away.
fn save_as_dialog(editor: &mut Editor) {
    let Some(document) = &editor.document else {
        return;
    };
    let format = if document.path.is_some() {
        document.format()
    } else if editor.config.files.new_maps_as_mallet {
        MapFormat::Mallet
    } else {
        MapFormat::Vmf
    };
    if let Some(path) = map_save_dialog(&document.file_name(), format, "Save As") {
        let _ = editor.save(Some(path));
    }
}

/// File > Export a Copy: writes the map elsewhere, in either format, without making the
/// copy the map's file. The other format is offered first, as that is the usual reason.
fn export_dialog(editor: &mut Editor) {
    let Some(document) = &editor.document else {
        return;
    };
    let format = match document.format() {
        MapFormat::Mallet => MapFormat::Vmf,
        MapFormat::Vmf => MapFormat::Mallet,
    };
    if let Some(path) = map_save_dialog(&document.file_name(), format, "Export a Copy") {
        let _ = editor.export_copy(&path);
    }
}

/// A save dialog offering both map formats, `first` preselected. A name typed without an
/// extension gets the preselected one.
fn map_save_dialog(file_name: &str, first: MapFormat, title: &str) -> Option<PathBuf> {
    let second = match first {
        MapFormat::Mallet => MapFormat::Vmf,
        MapFormat::Vmf => MapFormat::Mallet,
    };
    let stem = Path::new(file_name)
        .file_stem()
        .map_or_else(|| file_name.to_owned(), |stem| stem.to_string_lossy().into_owned());
    let path = rfd::FileDialog::new()
        .set_title(title)
        .add_filter(first.name(), &[first.extension()])
        .add_filter(second.name(), &[second.extension()])
        .set_file_name(format!("{stem}.{}", first.extension()))
        .save_file()?;
    if path.extension().is_some() {
        Some(path)
    } else {
        Some(path.with_extension(first.extension()))
    }
}

fn menu_item(ui: &mut egui::Ui, keymap: &KeyMap, text: &str, action: Action, chosen: &mut Option<Action>) {
    if ui
        .add(egui::Button::new(text).shortcut_text(keymap.label(action)))
        .clicked()
    {
        *chosen = Some(action);
        ui.close_menu();
    }
}

/// A menu entry that turns a setting on or off, ticked while it is on, with the
/// action's shortcut beside it.
fn menu_toggle(ui: &mut egui::Ui, keymap: &KeyMap, text: &str, on: bool, action: Action, chosen: &mut Option<Action>) {
    let tick = if on { egui_phosphor::regular::CHECK } else { "  " };
    let button = egui::Button::new(format!("{tick} {text}")).shortcut_text(keymap.label(action));
    if ui.add(button).clicked() {
        *chosen = Some(action);
        ui.close_menu();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn long_folders_keep_the_drive_and_their_last_folders() {
        let folder = Path::new(
            r"C:\Program Files (x86)\Steam\steamapps\common\GarrysMod\garrysmod\addons\maps_rp_city24_v4_resilience\maps",
        );
        let short = short_folder(folder);
        assert_eq!(short, r"C:\…\addons\maps_rp_city24_v4_resilience\maps");
        assert!(short.chars().count() <= RECENT_FOLDER_CHARS);
        assert_eq!(short_folder(Path::new(r"D:\maps")), r"D:\maps");
        // A single folder longer than the limit is still shown whole.
        let long = format!(r"C:\{}", "x".repeat(60));
        assert_eq!(short_folder(Path::new(&long)), format!(r"C:\…\{}", "x".repeat(60)));
    }
}
