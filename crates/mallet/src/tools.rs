//! The tools the MCP server exposes, run against the shared [`Editor`].

use std::path::PathBuf;

use glam::{DVec2, DVec3};
use mallet_content::{ClassKind, EntityClass};
use mallet_vmf::clip::Keep;
use mallet_vmf::edit::Edit;
use mallet_vmf::geometry::Bounds;
use mallet_vmf::light::{Falloff, CATALOGUE};
use mallet_vmf::{Connection, Entity, EntityItem};
use serde_json::{json, Value};

use eframe::egui::Pos2;

use crate::compile::{self, Outcome, Preset, Severity};
use crate::config::{self, Config, ToolKind};
use crate::editor::{Editor, SelectMode, ShotTarget, Tool, ViewMode, MAX_GRID, MIN_GRID};
use crate::face_pick::PickMode;
use crate::faces::{FaceChange, FaceInfo, FaceValues};
use crate::input::{self, InputFrame};
use crate::mcp::ToolResult;
use crate::prop_brushes::{PropBrushOptions, PropShape};
use crate::scene::Owner;
use mallet_vmf::texture::Justify;

/// How many entities `list_entities` returns when no limit is given.
const DEFAULT_LIST_LIMIT: u64 = 200;
const MAX_PITCH: f64 = 89.0;

/// The `tools/list` entries for every tool [`run`] understands.
pub fn definitions() -> Value {
    let vec3 = json!({ "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 });
    let vec2 = json!({ "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2 });
    let id = json!({ "type": "integer", "minimum": 0, "description": "Entity id; the world is usually 1." });
    json!([
        tool(
            "get_status",
            "Show the open map, its brush and entity counts and bounds, the grid size, the 3D camera, \
             the 2D view positions, and whether undo or redo is available.",
            json!({}),
            &[],
        ),
        tool(
            "set_game_dir",
            "Mount a Source game's content so the 3D view shows textures, and remember it for next time. \
             The folder is the one that contains gameinfo.txt, such as .../GarrysMod/garrysmod or \
             .../Half-Life 2/hl2.",
            json!({ "path": { "type": "string", "description": "Absolute path to the game folder." } }),
            &["path"],
        ),
        tool(
            "new_map",
            "Open a new tab with an empty, unsaved map and switch to it. Save it with save_map and a path.",
            json!({}),
            &[],
        ),
        tool(
            "open_map",
            "Open a map, a .vmf or a .mallet file, in a new tab and switch to it. An untouched empty map in \
             the current tab is replaced instead.",
            json!({
                "path": { "type": "string", "description": "Absolute path to the .vmf or .mallet file." },
                "focus": { "type": "boolean", "description": "Also bring the Mallet window to the front; default false." }
            }),
            &["path"],
        ),
        tool(
            "list_tabs",
            "List the open map tabs, which one is active, and which have unsaved changes.",
            json!({}),
            &[]
        ),
        tool(
            "switch_tab",
            "Make another map tab the one every other tool works on.",
            json!({ "index": { "type": "integer", "minimum": 0 } }),
            &["index"],
        ),
        tool(
            "close_tab",
            "Close a map tab. A tab with unsaved changes is only closed with discard set to true.",
            json!({
                "index": { "type": "integer", "minimum": 0, "description": "Default: the active tab." },
                "discard": { "type": "boolean", "description": "Close even with unsaved changes." }
            }),
            &[],
        ),
        tool(
            "save_map",
            "Save the open map. Overwrites its file unless a new path is given, which then becomes the map's \
             path. A new map needs a path. The extension picks the format: .mallet is Mallet's own binary \
             format (much faster to open and save, about a tenth of the size), anything else is VMF text.",
            json!({ "path": { "type": "string", "description": "Optional absolute path to save to instead." } }),
            &[],
        ),
        tool(
            "export_map",
            "Write a copy of the open map to another file without making it the map's file, such as a VMF \
             of a .mallet map for another tool. The extension picks the format, as for save_map.",
            json!({ "path": { "type": "string", "description": "Absolute path of the copy, ending .vmf or .mallet." } }),
            &["path"],
        ),
        tool(
            "list_entities",
            "List entities with their id, classname, targetname, origin and brush count. Includes the world.",
            json!({
                "classname": {
                    "type": "string",
                    "description": "Only list this classname. A trailing * matches a prefix, such as trigger_*."
                },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum entities to return; default 200." }
            }),
            &[],
        ),
        tool(
            "get_entity",
            "Show every key, output connection and brush id of one entity.",
            json!({ "id": id }),
            &["id"],
        ),
        tool(
            "set_entity_key",
            "Set a key on an entity, adding it if missing. Undoable.",
            json!({ "id": id, "key": { "type": "string" }, "value": { "type": "string" } }),
            &["id", "key", "value"],
        ),
        tool(
            "remove_entity_key",
            "Remove a key from an entity. Undoable.",
            json!({ "id": id, "key": { "type": "string" } }),
            &["id", "key"],
        ),
        tool(
            "get_selection",
            "List what is selected and the selection's bounds. World brushes are selected one by one; \
             entities are selected whole.",
            json!({}),
            &[],
        ),
        tool(
            "select",
            "Change the selection, the same one the editor window shows. Unknown ids are ignored.",
            json!({
                "solids": { "type": "array", "items": { "type": "integer" }, "description": "World brush ids." },
                "entities": { "type": "array", "items": { "type": "integer" }, "description": "Entity ids." },
                "mode": {
                    "type": "string",
                    "enum": ["replace", "add", "remove", "toggle"],
                    "description": "How to combine with the current selection; default replace."
                }
            }),
            &[],
        ),
        tool("clear_selection", "Deselect everything.", json!({}), &[]),
        tool(
            "move_selection",
            "Move the selection with texture lock. Entity origins move too, and while texture lock is on, so do \
             overlays whose faces are all on the moved brushes. Undoable.",
            json!({ "offset": vec3 }),
            &["offset"],
        ),
        tool(
            "clone_selection",
            "Copy the selection with fresh ids, move the copies by offset and select them, like Shift-dragging \
             in Hammer. An overlay cloned with its brushes lists the clones' faces in sides. Undoable.",
            json!({ "offset": vec3 }),
            &["offset"],
        ),
        tool(
            "props_to_brushes",
            "Mesh to Geometry: rebuild the selected props as brushes with the current material, then select the \
             new brushes. shape \"collision\" uses each prop's collision model (.phy), one brush per convex \
             piece; \"mesh\" splits the visual mesh into nearly convex parts, closer to how the prop looks but \
             with more brushes. Brushes go into a func_detail per prop unless detail is false, and the props are \
             removed unless keep_props is true. Undoable.",
            json!({
                "shape": { "type": "string", "enum": ["collision", "mesh"] },
                "detail": { "type": "boolean" },
                "keep_props": { "type": "boolean" }
            }),
            &["shape"],
        ),
        tool(
            "copy_selection",
            "Copy the selected brushes and entities to Mallet's clipboard, like Ctrl+C in Hammer. The clipboard \
             belongs to no tab, so the objects paste into any open map; they also go on the system clipboard as \
             VMF text.",
            json!({}),
            &[],
        ),
        tool(
            "cut_selection",
            "Copy the selection to the clipboard, then delete it, like Ctrl+X in Hammer. Undoable.",
            json!({}),
            &[],
        ),
        tool(
            "paste",
            "Paste the clipboard into the open map with fresh ids and select the copies, as one undoable step. \
             With no options this is Hammer's Paste: one copy. The other options are Hammer's Paste Special: \
             copies, each moved by offset and turned by rotation (degrees about X, Y, Z) more than the one \
             before, around the copies' centre or, with around_pivot, the pivot placed when they were copied; \
             prefix and suffix for entity names; unique_names numbers names no other entity has, with outputs \
             between pasted entities following; group puts each copy in a group of its own. An overlay pasted \
             with its brushes lists the pasted faces in sides; faces that were not pasted leave the list.",
            json!({
                "at": { "description": "\"original\" (default): where the objects were copied from; \"cursor\": \
                    under the mouse in a 2D view, or the middle of the 2D view used last, snapped to the grid; or \
                    [x, y, z] to centre the first copy's start there." },
                "copies": { "type": "integer", "minimum": 1, "maximum": crate::editor::MAX_COPIES },
                "offset": vec3,
                "rotation": vec3,
                "around_pivot": { "type": "boolean" },
                "prefix": { "type": "string" },
                "suffix": { "type": "string" },
                "unique_names": { "type": "boolean" },
                "group": { "type": "boolean" }
            }),
            &[],
        ),
        tool(
            "resize_selection",
            "Stretch the selection so its bounding box becomes min..max, like dragging Hammer's selection \
             handles. Brushes scale along each axis; entity origins move proportionally; textures follow the \
             texture lock settings (by default they keep their scale, pinned at the box's minimum corner). \
             Undoable.",
            json!({ "min": vec3, "max": vec3 }),
            &["min", "max"],
        ),
        tool(
            "transform_selection",
            "Scale, then rotate, then move the selection as one undoable step, like Hammer's Transform dialog \
             (Ctrl+M). rotate is degrees about X, then Y, then Z; scale is a factor per axis (negative mirrors); \
             both work around pivot: \"center\" of the selection (default), \"pivot\" placed with set_pivot, \
             \"origin\", or a point. Rotations and mirrors turn entity angles too. texture_lock and \
             texture_scale_lock default to the toolbar's settings.",
            json!({
                "rotate": vec3,
                "scale": vec3,
                "move": vec3,
                "pivot": { "description": "\"center\", \"pivot\", \"origin\" or [x, y, z]." },
                "texture_lock": { "type": "boolean" },
                "texture_scale_lock": { "type": "boolean" }
            }),
            &[],
        ),
        tool(
            "flip_selection",
            "Mirror the selection along a world axis through its centre, like Hammer's Flip Objects. Entity \
             angles are mirrored too. Undoable.",
            json!({ "axis": { "type": "string", "enum": ["x", "y", "z"] } }),
            &["axis"],
        ),
        tool(
            "snap_selection_to_grid",
            "Move the selection so its bounds' minimum corner lands on the grid (Ctrl+B), or each selected \
             object on its own with individually (Ctrl+Shift+B). Undoable.",
            json!({ "individually": { "type": "boolean" } }),
            &[],
        ),
        tool(
            "align_selection",
            "Line every selected object up with the minimum or maximum side of the selection's bounds along a \
             world axis, like Hammer's Align Objects. Undoable.",
            json!({
                "axis": { "type": "string", "enum": ["x", "y", "z"] },
                "side": { "type": "string", "enum": ["min", "max"] }
            }),
            &["axis", "side"],
        ),
        tool(
            "set_pivot",
            "Place the pivot that rotations, the rotation handles and the 3D gizmo turn around, or pass no point \
             to put it back at the selection's centre. Changing the selection resets it.",
            json!({ "point": vec3 }),
            &[],
        ),
        tool(
            "delete_selection",
            "Delete the selected brushes and entities. Undoable.",
            json!({}),
            &[]
        ),
        tool(
            "create_block",
            "Create an axis-aligned box brush in the world between two corners and select it. Undoable. \
             Returns the new brush id.",
            json!({
                "min": vec3,
                "max": vec3,
                "material": { "type": "string", "description": "Material for every face; default is the editor's current material." }
            }),
            &["min", "max"],
        ),
        tool(
            "clip_selection",
            "Cut the selected brushes, and the brushes of selected entities, along the plane through \
             three points, like the Clipping tool. The points are wound like a VMF side's plane: the \
             front is the side (a - b) x (c - b) points to. Brushes the plane misses are left alone and \
             displacements are skipped. The new faces get the editor's current material. Overlays on \
             cut faces follow them. Undoable as one step. Returns how many brushes were cut.",
            json!({
                "points": {
                    "type": "array",
                    "items": vec3,
                    "minItems": 3,
                    "maxItems": 3,
                    "description": "Three points on the clip plane."
                },
                "keep": {
                    "type": "string",
                    "enum": ["both", "front", "back"],
                    "description": "Which halves survive; default both, as two brushes."
                }
            }),
            &["points"],
        ),
        tool(
            "create_entity",
            "Create a point entity such as info_player_start or light at a position and select it. \
             Undoable. It gets an id and \"angles\" \"0 0 0\" unless keys override them. Returns the new \
             entity id.",
            json!({
                "classname": { "type": "string" },
                "origin": vec3,
                "keys": {
                    "type": "object",
                    "additionalProperties": { "type": "string" },
                    "description": "Extra keyvalues, such as {\"model\": \"models/props_c17/oildrum001.mdl\"}."
                }
            }),
            &["classname", "origin"],
        ),
        tool(
            "create_overlay",
            "Place an info_overlay, a picture such as a poster, stain or road marking, on a brush face and \
             select it. It is centred on point, pushed onto the face, upright on walls and with its top to the \
             north on floors. Its sides list the face and the faces in the same plane the picture reaches, and \
             its basis keys are filled in, so it compiles as placed. Moving, rotating or resizing the selection \
             carries overlays along. Undoable. Returns the new entity id and the sides it lists.",
            json!({
                "point": { "allOf": [vec3], "description": "Where the overlay's centre goes, on or near the face." },
                "side": { "type": "integer", "description": "Side id of the face, from get_faces or pick_face; default: the brush face nearest to point, within 8 units." },
                "material": { "type": "string", "description": "The overlay's material, such as decals/decal_posters001a; default is the editor's current material." },
                "size": { "allOf": [vec2], "description": "Width and height in units; default is the texture's size at 0.25 units per texel." }
            }),
            &["point"],
        ),
        tool(
            "create_decal",
            "Place an infodecal at a point on a brush face and select it. The game projects its texture onto \
             that face at the texture's own size, upright on walls. Undoable. Returns the new entity id.",
            json!({
                "point": { "allOf": [vec3], "description": "A point on a brush face, or within 8 units of one." },
                "material": { "type": "string", "description": "The decal's texture, such as decals/decalgraffiti001c; default is the editor's current material." }
            }),
            &["point"],
        ),
        tool(
            "set_entity_outputs",
            "Replace all outputs (I/O connections) of an entity. Undoable. Each output fires `input` on the \
             entities named `target` when `output` happens.",
            json!({
                "id": id,
                "outputs": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "output": { "type": "string" },
                            "target": { "type": "string" },
                            "input": { "type": "string" },
                            "parameter": { "type": "string" },
                            "delay": { "type": "number" },
                            "refires": { "type": "integer", "description": "-1 (default) fires every time." }
                        },
                        "required": ["output", "target", "input"]
                    }
                }
            }),
            &["id", "outputs"],
        ),
        tool(
            "describe_entity_class",
            "Describe an entity class from the loaded FGD files: its keyvalues with types, defaults, help and \
             choices, its spawnflags, inputs and outputs, and editor size, colour and model.",
            json!({ "classname": { "type": "string" } }),
            &["classname"],
        ),
        tool(
            "list_entity_classes",
            "List entity class names from the loaded FGD files.",
            json!({
                "kind": { "type": "string", "enum": ["point", "solid"], "description": "Default point." },
                "filter": { "type": "string", "description": "Only names containing this text." }
            }),
            &[],
        ),
        tool(
            "set_fgds",
            "Read entity classes from these FGD files instead of the game's default FGD, and remember the choice.",
            json!({ "paths": { "type": "array", "items": { "type": "string" } } }),
            &["paths"],
        ),
        tool(
            "get_settings",
            "Read Mallet Options: game and Steam folders, FGD files, compile tool paths (with the programs found \
             on this computer), compile behaviour, viewport options and colours, editing defaults, files, the \
             texture browser, the interface and the MCP server.",
            json!({}),
            &[],
        ),
        tool(
            "set_settings",
            "Change Mallet Options and save them. Pass only what changes, in the shape get_settings returns under \
             \"config\", e.g. {\"tools\": {\"sourcecubemap\": \"D:/scm/sourcecubemap.exe\"}} or \
             {\"view\": {\"camera_speed\": 600}}. A tool path of null means find it automatically.",
            json!({ "settings": { "type": "object" } }),
            &["settings"],
        ),
        tool(
            "list_compile_presets",
            "List compile presets with their steps and options, and the command each enabled step would run for \
             the open map.",
            json!({}),
            &[],
        ),
        tool(
            "set_compile_preset",
            "Create or replace a compile preset (matched by name). Steps have kind (vbsp, vvis, vrad, vbspinfo, \
             source_cubemap, repack, copy_to_game, run_game, custom), enabled, options [{flag, value}], \
             extra_args, and for custom steps name, program, args and working_dir. Arguments may use {map}, {vmf}, \
             {bsp}, {mapdir}, {gamedir}, {gamemaps} and {bindir}.",
            json!({ "preset": { "type": "object" } }),
            &["preset"],
        ),
        tool(
            "delete_compile_preset",
            "Delete a compile preset by name.",
            json!({ "name": { "type": "string" } }),
            &["name"],
        ),
        tool(
            "compile_map",
            "Save the open map and compile it with a preset (default: the one used last). Returns at once with \
             the plan, or waits up to wait_seconds for the compile to finish and returns its result. Check on a \
             running compile with compile_status.",
            json!({
                "preset": { "type": "string" },
                "wait_seconds": { "type": "number", "minimum": 0, "maximum": 3600 }
            }),
            &[],
        ),
        tool(
            "compile_status",
            "The running or last compile: each step's status and time, progress, errors and warnings with the \
             brush or position they name, whether the map leaked, and the last lines of output.",
            json!({ "lines": { "type": "integer", "minimum": 0, "description": "Output lines to include; default 40." } }),
            &[],
        ),
        tool("cancel_compile", "Stop the running compile.", json!({}), &[]),
        tool(
            "show_compile",
            "Open or close the compile window (F9), optionally on a tab.",
            json!({
                "open": { "type": "boolean" },
                "tab": { "type": "string", "enum": ["steps", "output", "problems"] }
            }),
            &[],
        ),
        tool(
            "show_options",
            "Open or close Mallet Options, optionally on a section.",
            json!({
                "open": { "type": "boolean" },
                "section": { "type": "string", "enum": ["game", "tools", "compiling", "viewports", "lighting", "colors", "editing", "files", "texture_browser", "interface", "keyboard", "agents"] }
            }),
            &[],
        ),
        tool(
            "list_materials",
            "List the game's materials, such as brick/brickwall001a, for the texture tool. Model, particle and \
             interface materials are left out unless all_folders is set.",
            json!({
                "filter": { "type": "string", "description": "Words that must all appear in the name." },
                "used_only": { "type": "boolean", "description": "Only materials the map's brushes use, with counts." },
                "all_folders": { "type": "boolean" },
                "limit": { "type": "integer", "minimum": 1, "description": "Default 200." }
            }),
            &[],
        ),
        tool(
            "get_faces",
            "Brush faces with their side id, material, normal, centre, texture axes, scale, shift, rotation and \
             lightmap scale. Pass solids or sides, or nothing for the faces the texture tool has selected.",
            json!({
                "solids": { "type": "array", "items": { "type": "integer" } },
                "sides": { "type": "array", "items": { "type": "integer" } }
            }),
            &[],
        ),
        tool(
            "select_faces",
            "Select brush faces for the texture tool, by side id or every face of some brushes. Switches to the \
             texture tool (Shift+A).",
            json!({
                "sides": { "type": "array", "items": { "type": "integer" } },
                "solids": { "type": "array", "items": { "type": "integer" } },
                "mode": { "type": "string", "enum": ["replace", "add", "remove", "toggle"] }
            }),
            &[],
        ),
        tool(
            "pick_face",
            "The brush face under a point of the camera viewport, in pixels from its top-left corner, as a \
             screenshot of the camera shows it. Pass entities to pick that face for a keyvalue listing brush \
             faces, as the Pick button of Object Properties does: key is the side list, such as sides on \
             info_overlay or env_cubemap and sides2 on info_overlay_transition (default sides), and mode \
             replace (default) lists only this face, add or remove changes the list, and toggle takes it off \
             when every entity lists it and adds it otherwise. Picking for entities sees through entities, \
             props, overlays and decals. Undoable.",
            json!({
                "point": vec2,
                "entities": { "type": "array", "items": { "type": "integer" } },
                "key": { "type": "string" },
                "mode": { "type": "string", "enum": ["replace", "add", "remove", "toggle"] }
            }),
            &["point"],
        ),
        tool(
            "set_faces",
            "Change brush faces like Face Edit: material, scale, shift, rotation, lightmap scale, alignment to \
             world or face, and justify (left, right, top, bottom, center, fit). Fit on a hotspot texture (one \
             whose VMT names a .rect file with %rectanglemap) fits each face into its best matching rectangle, \
             as in Hammer++. Applies to the given sides, the selected faces, or every face of the selected \
             brushes. Undoable.",
            json!({
                "sides": { "type": "array", "items": { "type": "integer" } },
                "material": { "type": "string" },
                "scale": vec2,
                "shift": vec2,
                "rotation": { "type": "number" },
                "lightmap_scale": { "type": "integer", "minimum": 1 },
                "align": { "type": "string", "enum": ["world", "face"] },
                "justify": { "type": "string", "enum": ["left", "right", "top", "bottom", "center", "fit", "fit_width", "fit_height"] },
                "treat_as_one": { "type": "boolean", "description": "Justify the faces together." },
                "alternates": { "type": "boolean", "description": "With justify fit on a hotspot texture, use only its alternate rectangles." }
            }),
            &[],
        ),
        tool(
            "replace_material",
            "Replace one material with another on every brush face in the map, or only on the selected faces. \
             Undoable.",
            json!({
                "find": { "type": "string" },
                "replace": { "type": "string" },
                "selected_only": { "type": "boolean" }
            }),
            &["find", "replace"],
        ),
        tool(
            "show_texture_browser",
            "Open or close the texture browser, optionally searching for something.",
            json!({ "open": { "type": "boolean" }, "filter": { "type": "string" } }),
            &[],
        ),
        tool(
            "show_model_browser",
            "Open or close the model browser, optionally searching for something. A model picked there goes on \
             the selected entities, or on new props when none is selected.",
            json!({ "open": { "type": "boolean" }, "filter": { "type": "string" } }),
            &[],
        ),
        tool(
            "list_models",
            "List the game's models, such as models/props_c17/oildrum001.mdl, to use as an entity's model key.",
            json!({
                "filter": {
                    "type": "string",
                    "description": "Words that must all appear in the path. A word starting with ! must not appear."
                },
                "used_only": { "type": "boolean", "description": "Only models the map's entities use, with counts." },
                "limit": { "type": "integer", "minimum": 1, "description": "Default 200." }
            }),
            &[],
        ),
        tool(
            "list_recent_maps",
            "The maps opened or saved lately, the latest first, as File > Recent Maps lists them. Open one \
             with open_map.",
            json!({}),
            &[],
        ),
        tool(
            "show_about",
            "Open or close Help > About Mallet.",
            json!({ "open": { "type": "boolean" } }),
            &[],
        ),
        tool(
            "load_pointfile",
            "Show a VBSP leak line (.lin) in the views and move the camera to it. Defaults to the open map's.",
            json!({ "path": { "type": "string" } }),
            &[],
        ),
        tool("clear_pointfile", "Hide the leak line.", json!({}), &[]),
        tool(
            "show_properties",
            "Open or close the Object Properties window for the current selection, as Alt+Enter does.",
            json!({
                "open": { "type": "boolean", "description": "Default true." },
                "tab": { "type": "string", "enum": ["keyvalues", "outputs", "inputs", "flags"] }
            }),
            &[],
        ),
        tool(
            "send_input",
            "Feed keyboard and mouse input to the editor window, frame by frame, as if a user did it, then \
             wait until it has been handled. Use it to test tools and shortcuts. The window must not be \
             minimized. Each action is one of: {\"key\": \"Enter\"} using egui key names such as A, Enter, \
             Escape, Delete, OpenBracket; {\"click\": [x, y]}; {\"drag\": [[x1, y1], [x2, y2]]}. Clicks and \
             drags need \"viewport\" (camera, top, front, side or window); in 2D views coordinates are world \
             units on the view's axes, in the camera view they are pixels from its top-left corner, and for \
             window they are pixels of a window screenshot, to reach menus, panels and dialogs. Any action may add \
             \"modifiers\" (ctrl, shift, alt) and clicks and drags may add \"button\" (primary, secondary, middle).",
            json!({
                "actions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "key": { "type": "string" },
                            "hold_frames": { "type": "integer", "minimum": 1, "description": "Frames to hold the key before releasing it; default 1." },
                            "modifier_frames": { "type": "integer", "minimum": 1, "description": "Frames the modifiers stay held, to let go of them before the key; default until the key is released." },
                            "click": vec2,
                            "drag": { "type": "array", "items": vec2, "minItems": 2, "maxItems": 2 },
                            "viewport": { "type": "string", "enum": ["camera", "top", "front", "side", "window"] },
                            "modifiers": { "type": "array", "items": { "type": "string", "enum": ["ctrl", "shift", "alt"] } },
                            "button": { "type": "string", "enum": ["primary", "secondary", "middle"] }
                        }
                    }
                }
            }),
            &["actions"],
        ),
        tool(
            "undo",
            "Undo the last edit, whether it was made by hand or by a tool.",
            json!({}),
            &[]
        ),
        tool("redo", "Redo the last undone edit.", json!({}), &[]),
        tool(
            "set_grid",
            "Set the 2D grid size in world units, and turn snapping to the grid (Hammer's Snap to Grid, \
             Shift+W) and drawing it in the 2D views (Show Grid, Shift+R) on or off.",
            json!({
                "size": { "type": "number", "description": "A power of two from 1 to 512." },
                "snap": { "type": "boolean", "description": "Whether drags and new points snap to the grid." },
                "show": { "type": "boolean", "description": "Whether the 2D views draw the grid." }
            }),
            &[],
        ),
        tool(
            "center_views",
            "Fit the whole map into the three 2D views and move the 3D camera back until the map is in view.",
            json!({}),
            &[],
        ),
        tool(
            "set_camera",
            "Move or aim the 3D camera. Source coordinates: Z is up, yaw 0 looks along +X, positive pitch looks up.",
            json!({
                "position": vec3,
                "yaw": { "type": "number", "description": "Degrees." },
                "pitch": { "type": "number", "description": "Degrees, clamped to -89..89." },
                "look_at": { "allOf": [vec3], "description": "Aim at this point after moving; overrides yaw and pitch." },
                "draw_distance": { "type": "number", "description": "How far the 3D view draws, like Hammer's back clipping plane; default 8192." }
            }),
            &[],
        ),
        tool(
            "set_view_2d",
            "Pan or zoom one 2D view. Top shows X/Y, front shows Y/Z, side shows X/Z.",
            json!({
                "view": { "type": "string", "enum": ["top", "front", "side"] },
                "center": { "allOf": [vec2], "description": "World position at the view centre, in the view's two axes." },
                "pixels_per_unit": { "type": "number", "description": "Zoom; 1 means one pixel per world unit." }
            }),
            &["view"],
        ),
        tool(
            "set_viewport_type",
            "Change what one of the four viewports shows, like clicking its label. Slots are 0 top-left, \
             1 top-right, 2 bottom-left, 3 bottom-right. lighting is the 3D Lighting Preview: the map lit by its \
             lights with shadows, sky light, texture lights, the skybox and fog, set up in Mallet Options > \
             Lighting preview (get_settings and set_settings under \"lighting\").",
            json!({
                "slot": { "type": "integer", "minimum": 0, "maximum": 3 },
                "type": { "type": "string", "enum": ["textured", "wireframe", "lighting", "top", "front", "side"] }
            }),
            &["slot", "type"],
        ),
        tool(
            "list_lights",
            "The open map's lighting as the lighting preview reads it, with VRAD's maths: every light entity's \
             class, id, kind, position, direction, colour and brightness for LDR and HDR compiles (1 shows a \
             texture at full brightness), falloff, how far it reaches, cone, appearance, and whether it starts \
             dark, casts shadows or is dynamic; light_environment's sky light, the fog, the sky, glows and \
             spotlight beams; the materials of the map that lights.rad makes texture lights; and what the \
             latest preview frame drew. With catalogue, also every lighting entity class Mallet knows.",
            json!({
                "classname": { "type": "string", "description": "Only lights of this class; light_* matches a prefix." },
                "catalogue": { "type": "boolean", "description": "Also list the lighting entity classes." }
            }),
            &[],
        ),
        tool(
            "screenshot",
            "Render viewports to a PNG at their on-screen size. Works while the window is covered or minimized. \
             With viewport all, the layout is camera top-left, top top-right, front bottom-left, side bottom-right; \
             viewport labels are not drawn.",
            json!({
                "viewport": {
                    "type": "string",
                    "enum": ["all", "camera", "top", "front", "side", "window"],
                    "description": "Which viewport to render; default all four. \"window\" captures the whole editor window with its menus, panels and dialogs such as Object Properties; it needs the window to be visible."
                }
            }),
            &[],
        ),
    ])
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        }
    })
}

/// Runs one tool call. Failures come back as error results the agent can read.
///
/// While Mallet Options keeps agents in their own view the call runs there, apart from
/// the window. A map handed over by a second copy of Mallet (`open_map` with `focus`)
/// is the user's own and always opens in the window.
pub fn run(editor: &mut Editor, name: &str, args: &Value) -> ToolResult {
    let handed_over = name == "open_map" && args.get("focus").and_then(Value::as_bool) == Some(true);
    let result = if editor.config.agents.own_view && !handed_over {
        run_in_own_view(editor, name, args)
    } else {
        run_checked(editor, name, args)
    };
    result.unwrap_or_else(ToolResult::error)
}

/// Tools that drive the window, which an agent in its own view may not use.
const WINDOW_TOOLS: [&str; 7] = [
    "send_input",
    "show_compile",
    "show_options",
    "show_about",
    "show_properties",
    "show_texture_browser",
    "show_model_browser",
];

/// Runs a tool call in the agent's own view. Tab tools move only the agent between
/// tabs, and tools that would drive the window are refused.
fn run_in_own_view(editor: &mut Editor, name: &str, args: &Value) -> Result<ToolResult, String> {
    if WINDOW_TOOLS.contains(&name) {
        return Err(format!(
            "{name} would take over the user's window, and Mallet Options > AI agents keeps agents in their own view; ask the user to turn off \"Agents work in their own view\" if this is needed"
        ));
    }
    match name {
        "new_map" => {
            editor.agent_new_tab();
            Ok(ToolResult::json(&editor.with_agent_view(|editor| status_json(editor))))
        }
        "open_map" => {
            editor.agent_open(&PathBuf::from(string_arg(args, "path")?))?;
            Ok(ToolResult::json(&editor.with_agent_view(|editor| status_json(editor))))
        }
        "list_tabs" => Ok(ToolResult::json(&tabs_json(editor))),
        "switch_tab" => {
            let index = args
                .get("index")
                .and_then(Value::as_u64)
                .ok_or("\"index\" must be a whole number")?;
            editor.agent_switch_tab(index as usize)?;
            Ok(ToolResult::json(&tabs_json(editor)))
        }
        "close_tab" => {
            let index = args
                .get("index")
                .and_then(Value::as_u64)
                .map_or(editor.agent_tab(), |index| index as usize);
            check_discard(editor, index, args)?;
            editor.agent_close_tab(index)?;
            Ok(ToolResult::json(&tabs_json(editor)))
        }
        "set_settings" if changes_agent_settings(args) => Err(
            "the AI agents settings are the user's to change in Mallet Options while agents work in their own view"
                .to_owned(),
        ),
        _ => editor.with_agent_view(|editor| run_checked(editor, name, args)),
    }
}

/// Whether a `set_settings` call touches the AI agents section.
fn changes_agent_settings(args: &Value) -> bool {
    args.get("settings")
        .and_then(|settings| settings.get("agents"))
        .is_some()
}

/// Refuses to close a tab with unsaved changes unless the call passes `discard`.
fn check_discard(editor: &Editor, index: usize, args: &Value) -> Result<(), String> {
    let discard = args.get("discard").and_then(Value::as_bool).unwrap_or(false);
    let dirty = editor
        .tabs()
        .get(index)
        .ok_or_else(|| format!("there is no tab {index}"))?
        .dirty;
    if dirty && !discard {
        return Err(format!(
            "tab {index} has unsaved changes; save it first or pass discard: true"
        ));
    }
    Ok(())
}

fn run_checked(editor: &mut Editor, name: &str, args: &Value) -> Result<ToolResult, String> {
    match name {
        "get_status" => Ok(ToolResult::json(&status_json(editor))),
        "set_game_dir" => {
            let path = PathBuf::from(string_arg(args, "path")?);
            let mounts = editor.choose_game(&path)?;
            Ok(ToolResult::text(format!(
                "Mounted {} with {mounts} search paths.",
                path.display()
            )))
        }
        "new_map" => {
            editor.new_tab();
            Ok(ToolResult::json(&status_json(editor)))
        }
        "open_map" => {
            editor.open_in_tab(&PathBuf::from(string_arg(args, "path")?))?;
            Ok(ToolResult::json(&status_json(editor)))
        }
        "list_tabs" => Ok(ToolResult::json(&tabs_json(editor))),
        "switch_tab" => {
            let index = args
                .get("index")
                .and_then(Value::as_u64)
                .ok_or("\"index\" must be a whole number")?;
            editor.switch_tab(index as usize)?;
            Ok(ToolResult::json(&tabs_json(editor)))
        }
        "close_tab" => {
            let index = args
                .get("index")
                .and_then(Value::as_u64)
                .map_or(editor.active_tab(), |index| index as usize);
            check_discard(editor, index, args)?;
            editor.close_tab(index)?;
            Ok(ToolResult::json(&tabs_json(editor)))
        }
        "save_map" => {
            let path = optional_string(args, "path")?.map(PathBuf::from);
            let saved = editor.save(path)?;
            Ok(ToolResult::text(format!("Saved {}", saved.display())))
        }
        "export_map" => {
            let path = PathBuf::from(string_arg(args, "path")?);
            editor.export_copy(&path)?;
            Ok(ToolResult::text(format!("Exported a copy to {}", path.display())))
        }
        "list_entities" => Ok(ToolResult::json(&list_entities(editor, args)?)),
        "get_entity" => {
            let id = id_arg(args)?;
            let entity = editor
                .document
                .as_ref()
                .ok_or("no map is open")?
                .vmf
                .all_entities()
                .find(|entity| entity.id() == Some(id))
                .ok_or_else(|| format!("no entity has id {id}"))?;
            let mut details = entity_details(entity);
            let class = editor.entity_class(entity.classname());
            details["fgd_class"] = json!(class.map(|class| class.name.clone()));
            details["outputs"] = Value::Array(entity.connections().iter().map(connection_json).collect());
            details["inputs"] = Value::Array(
                editor
                    .inputs_to(id)
                    .iter()
                    .map(|link| {
                        let mut value = connection_json(&link.connection);
                        value["source"] = json!(link.source);
                        value["source_name"] = json!(link.source_name);
                        value
                    })
                    .collect(),
            );
            Ok(ToolResult::json(&details))
        }
        "set_entity_outputs" => {
            let id = id_arg(args)?;
            let outputs = args
                .get("outputs")
                .and_then(Value::as_array)
                .ok_or("\"outputs\" must be an array")?
                .iter()
                .enumerate()
                .map(|(index, value)| connection_arg(value).map_err(|error| format!("output {index}: {error}")))
                .collect::<Result<Vec<_>, _>>()?;
            edited(editor.set_outputs(id, outputs))
        }
        "describe_entity_class" => {
            let name = string_arg(args, "classname")?;
            let class = editor
                .entity_class(name)
                .ok_or_else(|| format!("no FGD class \"{name}\"; is a game folder with FGD files set?"))?;
            Ok(ToolResult::json(&class_json(class)))
        }
        "list_entity_classes" => {
            let data = editor.game_data.as_deref().ok_or("no FGD files are loaded")?;
            let kind = match optional_string(args, "kind")?.unwrap_or("point") {
                "point" => ClassKind::Point,
                "solid" => ClassKind::Solid,
                other => return Err(format!("unknown kind \"{other}\"")),
            };
            let filter = optional_string(args, "filter")?.unwrap_or("").to_ascii_lowercase();
            let names: Vec<&str> = data
                .class_names(kind)
                .into_iter()
                .filter(|name| name.to_ascii_lowercase().contains(&filter))
                .collect();
            Ok(ToolResult::json(&json!({ "count": names.len(), "classes": names })))
        }
        "set_fgds" => {
            let paths: Vec<PathBuf> = args
                .get("paths")
                .and_then(Value::as_array)
                .ok_or("\"paths\" must be an array of strings")?
                .iter()
                .filter_map(Value::as_str)
                .map(PathBuf::from)
                .collect();
            let count = editor.choose_fgds(paths)?;
            Ok(ToolResult::text(format!("Read {count} entity classes.")))
        }
        "get_settings" => {
            let game_dir = editor.game_dir().map(PathBuf::from);
            let tools: serde_json::Map<String, Value> = ToolKind::ALL
                .into_iter()
                .map(|tool| {
                    let found: Vec<String> = tool
                        .candidates(game_dir.as_deref())
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect();
                    let value = json!({
                        "using": editor.tool_path(tool).map(|path| path.display().to_string()),
                        "found": found,
                    });
                    (tool.key().to_owned(), value)
                })
                .collect();
            Ok(ToolResult::json(&json!({
                "config": serde_json::to_value(&editor.config).map_err(|error| error.to_string())?,
                "tools": tools,
                "data_dir": config::data_dir().map(|dir| dir.display().to_string()),
            })))
        }
        "set_settings" => {
            let changes = args
                .get("settings")
                .filter(|value| value.is_object())
                .ok_or("\"settings\" must be an object")?;
            let mut value = serde_json::to_value(&editor.config).map_err(|error| error.to_string())?;
            merge(&mut value, changes);
            let updated: Config =
                serde_json::from_value(value).map_err(|error| format!("invalid settings: {error}"))?;
            let game_changed = updated.game_dir != editor.config.game_dir;
            let fgds_changed = updated.fgds != editor.config.fgds;
            let (game_dir, fgds) = (updated.game_dir.clone(), updated.fgds.clone());
            editor.config = updated;
            if game_changed {
                if let Some(game_dir) = game_dir {
                    editor.choose_game(&game_dir)?;
                }
            }
            if fgds_changed || game_changed {
                editor.choose_fgds(fgds)?;
            }
            editor.config.save()?;
            Ok(ToolResult::json(
                &serde_json::to_value(&editor.config).map_err(|error| error.to_string())?,
            ))
        }
        "list_compile_presets" => {
            let presets: Vec<Value> = editor
                .compile_presets
                .iter()
                .map(|preset| {
                    let mut value = serde_json::to_value(preset).unwrap_or(Value::Null);
                    value["commands"] = match editor.preview_preset(preset) {
                        Ok(commands) => json!(commands),
                        Err(error) => json!({ "error": error }),
                    };
                    value
                })
                .collect();
            Ok(ToolResult::json(&json!({
                "last_used": editor.config.compile.last_preset,
                "presets": presets,
            })))
        }
        "set_compile_preset" => {
            let preset: Preset = serde_json::from_value(args.get("preset").cloned().unwrap_or(Value::Null))
                .map_err(|error| format!("invalid preset: {error}"))?;
            if preset.name.trim().is_empty() {
                return Err("the preset needs a name".to_owned());
            }
            let name = preset.name.clone();
            match editor
                .compile_presets
                .iter_mut()
                .find(|existing| existing.name.eq_ignore_ascii_case(&name))
            {
                Some(existing) => *existing = preset,
                None => editor.compile_presets.push(preset),
            }
            compile::save_presets(&editor.compile_presets)?;
            Ok(ToolResult::text(format!("Saved the preset \"{name}\".")))
        }
        "delete_compile_preset" => {
            let name = string_arg(args, "name")?;
            let before = editor.compile_presets.len();
            editor
                .compile_presets
                .retain(|preset| !preset.name.eq_ignore_ascii_case(name));
            if editor.compile_presets.len() == before {
                return Err(format!("there is no preset named \"{name}\""));
            }
            compile::save_presets(&editor.compile_presets)?;
            Ok(ToolResult::text(format!("Deleted the preset \"{name}\".")))
        }
        "compile_map" => {
            let preset = optional_string(args, "preset")?
                .map(str::to_owned)
                .unwrap_or_else(|| editor.config.compile.last_preset.clone());
            editor.start_compile(&preset)?;
            Ok(ToolResult::json(&compile_status_json(editor, 10)))
        }
        "compile_status" => {
            editor.poll_compile();
            let lines = optional_number(args, "lines")?.unwrap_or(40.0) as usize;
            Ok(ToolResult::json(&compile_status_json(editor, lines)))
        }
        "cancel_compile" => Ok(ToolResult::text(if editor.cancel_compile() {
            "Cancelling the compile."
        } else {
            "No compile is running."
        })),
        "show_compile" => {
            editor.compile_open = args.get("open").and_then(Value::as_bool).unwrap_or(true);
            editor.compile_tab = optional_string(args, "tab")?.map(str::to_owned);
            Ok(ToolResult::text(if editor.compile_open {
                "The compile window is open."
            } else {
                "The compile window is closed."
            }))
        }
        "show_options" => {
            editor.options_open = args.get("open").and_then(Value::as_bool).unwrap_or(true);
            editor.options_section = optional_string(args, "section")?.map(str::to_owned);
            Ok(ToolResult::text(if editor.options_open {
                "Mallet Options is open."
            } else {
                "Mallet Options is closed."
            }))
        }
        "list_materials" => {
            let limit = optional_number(args, "limit")?.unwrap_or(200.0).max(1.0) as usize;
            let terms: Vec<String> = optional_string(args, "filter")?
                .unwrap_or("")
                .split_whitespace()
                .map(str::to_ascii_lowercase)
                .collect();
            let matches = |name: &str| {
                let name = name.to_ascii_lowercase();
                terms.iter().all(|term| name.contains(term.as_str()))
            };
            if args.get("used_only").and_then(Value::as_bool).unwrap_or(false) {
                let used: Vec<Value> = editor
                    .used_materials()
                    .into_iter()
                    .filter(|(name, _)| matches(name))
                    .take(limit)
                    .map(|(name, faces)| json!({ "material": name, "faces": faces }))
                    .collect();
                return Ok(ToolResult::json(&json!({ "materials": used })));
            }
            let all_folders = args.get("all_folders").and_then(Value::as_bool).unwrap_or(false);
            let list = editor.material_list()?;
            let found: Vec<&String> = list
                .iter()
                .filter(|name| all_folders || !NON_BRUSH_FOLDERS.iter().any(|folder| name.starts_with(folder)))
                .filter(|name| matches(name))
                .collect();
            Ok(ToolResult::json(&json!({
                "total": found.len(),
                "materials": found.iter().take(limit).collect::<Vec<_>>(),
            })))
        }
        "get_faces" => {
            let solids = id_list(args, "solids")?;
            let sides = id_list(args, "sides")?;
            let faces = if !solids.is_empty() {
                editor.brush_faces(&solids)
            } else if !sides.is_empty() {
                editor.faces(&sides.into_iter().collect())
            } else {
                editor.faces(&editor.face_selection.clone())
            };
            let faces: Vec<Value> = faces.iter().map(face_json).collect();
            Ok(ToolResult::json(&json!({ "faces": faces })))
        }
        "select_faces" => {
            let mode = match optional_string(args, "mode")?.unwrap_or("replace") {
                "replace" => SelectMode::Replace,
                "add" => SelectMode::Add,
                "remove" => SelectMode::Remove,
                "toggle" => SelectMode::Toggle,
                other => return Err(format!("unknown mode \"{other}\"")),
            };
            let solids = id_list(args, "solids")?;
            let mut sides = id_list(args, "sides")?;
            sides.extend(editor.brush_faces(&solids).iter().map(|face| face.side.id));
            if editor.tool != Tool::Texture {
                editor.set_tool(Tool::Texture);
            }
            editor.select_faces(sides, mode);
            Ok(ToolResult::json(&json!({ "selected": editor.face_selection })))
        }
        "pick_face" => {
            let [x, y] = optional_numbers::<2>(args, "point")?.ok_or("\"point\" is required")?;
            let slot = editor.slot_for(ShotTarget::Camera)?;
            let rect = editor.viewport_rects[slot];
            let at = editor.viewport_point(ShotTarget::Camera, DVec2::new(x, y))?;
            let (origin, direction) = editor.camera.ray(rect, at);
            let entities = id_list(args, "entities")?;
            let hit = if entities.is_empty() {
                editor.scene.pick_face(origin, direction, &editor.models)
            } else {
                editor.scene.surface_face(origin, direction).map(|(hit, _)| hit)
            };
            let Some(hit) = hit else {
                return Ok(ToolResult::json(&json!({ "hit": null })));
            };
            let face = editor.faces(&[hit.side].into_iter().collect()).pop();
            let mut result = json!({
                "hit": {
                    "side": hit.side,
                    "solid": hit.solid,
                    "point": [hit.point.x, hit.point.y, hit.point.z],
                    "material": face.map(|face| face.side.material),
                }
            });
            if !entities.is_empty() {
                let mode = match optional_string(args, "mode")?.unwrap_or("replace") {
                    "replace" => PickMode::Replace,
                    "add" => PickMode::Add,
                    "remove" => PickMode::Remove,
                    "toggle" => PickMode::Toggle,
                    other => return Err(format!("unknown mode \"{other}\"")),
                };
                let key = optional_string(args, "key")?.unwrap_or("sides");
                let lists = editor.pick_side(&entities, key, hit.side, mode)?;
                let lists: serde_json::Map<String, Value> =
                    lists.into_iter().map(|(id, sides)| (id.to_string(), json!(sides))).collect();
                result["key"] = json!(key);
                result["lists"] = Value::Object(lists);
            }
            Ok(ToolResult::json(&result))
        }
        "set_faces" => {
            let mut sides: std::collections::BTreeSet<u32> = id_list(args, "sides")?.into_iter().collect();
            if sides.is_empty() {
                sides = editor.texture_targets();
            }
            if sides.is_empty() {
                return Err("pass sides, or select faces or brushes first".to_owned());
            }
            let align = match optional_string(args, "align")? {
                None => None,
                Some("world") => Some(false),
                Some("face") => Some(true),
                Some(other) => return Err(format!("unknown alignment \"{other}\"")),
            };
            let justify = match optional_string(args, "justify")? {
                None => None,
                Some("left") => Some(Justify::Left),
                Some("right") => Some(Justify::Right),
                Some("top") => Some(Justify::Top),
                Some("bottom") => Some(Justify::Bottom),
                Some("center") => Some(Justify::Center),
                Some("fit") => Some(Justify::Fit),
                Some("fit_width") => Some(Justify::FitWidth),
                Some("fit_height") => Some(Justify::FitHeight),
                Some(other) => return Err(format!("unknown justify \"{other}\"")),
            };
            let scale = optional_numbers::<2>(args, "scale")?;
            let shift = optional_numbers::<2>(args, "shift")?;
            let change = FaceChange {
                material: optional_string(args, "material")?.map(str::to_owned),
                align,
                values: FaceValues {
                    scale: scale.map_or([None, None], |[x, y]| [Some(x), Some(y)]),
                    shift: shift.map_or([None, None], |[x, y]| [Some(x), Some(y)]),
                    rotation: optional_number(args, "rotation")?,
                    lightmap_scale: optional_number(args, "lightmap_scale")?.map(|value| value.max(1.0) as u32),
                    ..FaceValues::default()
                },
                justify,
                as_one: args.get("treat_as_one").and_then(Value::as_bool).unwrap_or(false),
                alternates: args.get("alternates").and_then(Value::as_bool).unwrap_or(false),
            };
            if change.material.is_none()
                && change.align.is_none()
                && change.justify.is_none()
                && change.values == FaceValues::default()
            {
                return Err(
                    "nothing to change: pass material, scale, shift, rotation, lightmap_scale, align or justify"
                        .to_owned(),
                );
            }
            editor.change_faces(&sides, &change)?;
            let faces: Vec<Value> = editor.faces(&sides).iter().map(face_json).collect();
            Ok(ToolResult::json(&json!({ "faces": faces })))
        }
        "replace_material" => {
            let selected = editor.face_selection.clone();
            let within = args
                .get("selected_only")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                .then_some(&selected);
            let count = editor.replace_material(string_arg(args, "find")?, string_arg(args, "replace")?, within)?;
            Ok(ToolResult::text(format!(
                "Replaced the material on {count} faces. Undo with the undo tool."
            )))
        }
        "show_texture_browser" => {
            editor.texture_browser_open = args.get("open").and_then(Value::as_bool).unwrap_or(true);
            editor.texture_browser_filter = optional_string(args, "filter")?.map(str::to_owned);
            Ok(ToolResult::text(if editor.texture_browser_open {
                "The texture browser is open."
            } else {
                "The texture browser is closed."
            }))
        }
        "show_model_browser" => {
            editor.model_browser_open = args.get("open").and_then(Value::as_bool).unwrap_or(true);
            editor.model_browser_filter = optional_string(args, "filter")?.map(str::to_owned);
            Ok(ToolResult::text(if editor.model_browser_open {
                "The model browser is open."
            } else {
                "The model browser is closed."
            }))
        }
        "list_models" => {
            let limit = optional_number(args, "limit")?.unwrap_or(200.0).max(1.0) as usize;
            let words: Vec<String> = optional_string(args, "filter")?
                .unwrap_or("")
                .split_whitespace()
                .map(str::to_ascii_lowercase)
                .collect();
            let matches = |path: &str| {
                let path = path.replace('\\', "/").to_ascii_lowercase();
                words.iter().all(|word| match word.strip_prefix('!') {
                    Some(unwanted) => unwanted.is_empty() || !path.contains(unwanted),
                    None => path.contains(word.as_str()),
                })
            };
            if args.get("used_only").and_then(Value::as_bool).unwrap_or(false) {
                let used: Vec<Value> = editor
                    .used_models()
                    .into_iter()
                    .filter(|(path, _)| matches(path))
                    .take(limit)
                    .map(|(path, entities)| json!({ "model": path, "entities": entities }))
                    .collect();
                return Ok(ToolResult::json(&json!({ "models": used })));
            }
            let list = editor.model_list()?;
            let found: Vec<&String> = list.iter().filter(|path| matches(path)).collect();
            Ok(ToolResult::json(&json!({
                "total": found.len(),
                "models": found.iter().take(limit).collect::<Vec<_>>(),
            })))
        }
        "list_recent_maps" => {
            let maps: Vec<Value> = editor
                .config
                .recent_maps
                .iter()
                .map(|path| json!({ "path": path.display().to_string(), "exists": path.is_file() }))
                .collect();
            Ok(ToolResult::json(&json!({ "maps": maps })))
        }
        "show_about" => {
            editor.about_open = args.get("open").and_then(Value::as_bool).unwrap_or(true);
            Ok(ToolResult::text(if editor.about_open {
                "About Mallet is open."
            } else {
                "About Mallet is closed."
            }))
        }
        "load_pointfile" => {
            let path = match optional_string(args, "path")? {
                Some(path) => PathBuf::from(path),
                None => editor.compile_context()?.vmf.with_extension("lin"),
            };
            let points = compile::runner::read_pointfile(&path)
                .ok_or_else(|| format!("{} is not a pointfile", path.display()))?;
            let count = points.len();
            editor.set_pointfile(points);
            Ok(ToolResult::text(format!("Showing a leak line of {count} points.")))
        }
        "clear_pointfile" => {
            editor.clear_pointfile();
            Ok(ToolResult::text("The leak line is hidden."))
        }
        "show_properties" => {
            editor.properties_open = args.get("open").and_then(Value::as_bool).unwrap_or(true);
            editor.properties_tab = optional_string(args, "tab")?.map(str::to_owned);
            Ok(ToolResult::text(if editor.properties_open {
                "Object Properties is open."
            } else {
                "Object Properties is closed."
            }))
        }
        "set_entity_key" => edited(editor.apply_edit(&Edit::SetKey {
            entity: id_arg(args)?,
            key: string_arg(args, "key")?.to_owned(),
            value: string_arg(args, "value")?.to_owned(),
        })),
        "remove_entity_key" => edited(editor.apply_edit(&Edit::RemoveKey {
            entity: id_arg(args)?,
            key: string_arg(args, "key")?.to_owned(),
        })),
        "get_selection" => Ok(ToolResult::json(&selection_json(editor))),
        "select" => {
            let mode = match optional_string(args, "mode")?.unwrap_or("replace") {
                "replace" => SelectMode::Replace,
                "add" => SelectMode::Add,
                "remove" => SelectMode::Remove,
                "toggle" => SelectMode::Toggle,
                other => return Err(format!("unknown mode \"{other}\"")),
            };
            let solids = id_list(args, "solids")?.into_iter().map(Owner::Solid);
            let entities = id_list(args, "entities")?.into_iter().map(Owner::Entity);
            editor.select(solids.chain(entities), mode);
            Ok(ToolResult::json(&selection_json(editor)))
        }
        "clear_selection" => {
            editor.clear_selection();
            Ok(ToolResult::text("Selection cleared."))
        }
        "move_selection" => {
            editor.move_selection(required_vec3(args, "offset")?)?;
            Ok(ToolResult::json(&selection_json(editor)))
        }
        "clone_selection" => {
            editor.clone_selection(required_vec3(args, "offset")?)?;
            Ok(ToolResult::json(&selection_json(editor)))
        }
        "props_to_brushes" => {
            let shape = PropShape::parse(args.get("shape").and_then(Value::as_str).unwrap_or_default())?;
            let defaults = PropBrushOptions::default();
            let options = PropBrushOptions {
                detail: args.get("detail").and_then(Value::as_bool).unwrap_or(defaults.detail),
                keep_props: args
                    .get("keep_props")
                    .and_then(Value::as_bool)
                    .unwrap_or(defaults.keep_props),
            };
            let brushes = editor.props_to_brushes(shape, options)?;
            Ok(ToolResult::json(&json!({
                "brushes": brushes,
                "message": editor.message,
                "selection": selection_json(editor),
            })))
        }
        "copy_selection" => {
            editor.copy_selection()?;
            Ok(ToolResult::json(&selection_json(editor)))
        }
        "cut_selection" => {
            editor.cut_selection()?;
            Ok(ToolResult::json(&selection_json(editor)))
        }
        "paste" => {
            paste(editor, args)?;
            Ok(ToolResult::json(&selection_json(editor)))
        }
        "resize_selection" => {
            let to = Bounds {
                min: required_vec3(args, "min")?,
                max: required_vec3(args, "max")?,
            };
            editor.resize_selection(to)?;
            Ok(ToolResult::json(&selection_json(editor)))
        }
        "transform_selection" => {
            transform_selection(editor, args)?;
            Ok(ToolResult::json(&selection_json(editor)))
        }
        "flip_selection" => {
            editor.flip_selection(axis_arg(args, "axis")?)?;
            Ok(ToolResult::json(&selection_json(editor)))
        }
        "snap_selection_to_grid" => {
            let individually = args.get("individually").and_then(Value::as_bool).unwrap_or(false);
            editor.snap_selection_to_grid(individually)?;
            Ok(ToolResult::json(&selection_json(editor)))
        }
        "align_selection" => {
            let max = match string_arg(args, "side")? {
                "min" => false,
                "max" => true,
                other => return Err(format!("\"side\" must be min or max, not \"{other}\"")),
            };
            editor.align_selection(axis_arg(args, "axis")?, max)?;
            Ok(ToolResult::json(&selection_json(editor)))
        }
        "set_pivot" => {
            let point = optional_numbers::<3>(args, "point")?.map(DVec3::from_array);
            editor.set_pivot(point);
            Ok(ToolResult::json(&selection_json(editor)))
        }
        "delete_selection" => {
            let count = editor.delete_selection()?;
            Ok(ToolResult::text(format!(
                "Deleted {count} objects. Undo with the undo tool or Ctrl+Z."
            )))
        }
        "clip_selection" => {
            let points = args
                .get("points")
                .and_then(Value::as_array)
                .filter(|points| points.len() == 3)
                .ok_or("\"points\" must be three [x, y, z] points")?
                .iter()
                .map(|point| {
                    let numbers: Option<Vec<f64>> = point.as_array()?.iter().map(Value::as_f64).collect();
                    let [x, y, z] = numbers?.try_into().ok()?;
                    Some(DVec3::new(x, y, z))
                })
                .collect::<Option<Vec<DVec3>>>()
                .ok_or("each point must be [x, y, z]")?;
            let keep = match optional_string(args, "keep")?.unwrap_or("both") {
                "both" => Keep::Both,
                "front" => Keep::Front,
                "back" => Keep::Back,
                other => return Err(format!("\"keep\" must be both, front or back, not \"{other}\"")),
            };
            let cut = editor.clip_selection([points[0], points[1], points[2]], keep)?;
            Ok(ToolResult::json(&json!({ "clipped": cut, "message": editor.message })))
        }
        "create_block" => {
            let min = required_vec3(args, "min")?;
            let max = required_vec3(args, "max")?;
            let previous = optional_string(args, "material")?
                .map(|material| std::mem::replace(&mut editor.material, material.to_owned()));
            let result = editor.create_block(min, max);
            if let Some(previous) = previous {
                editor.material = previous;
            }
            Ok(ToolResult::json(&json!({ "solid": result? })))
        }
        "create_entity" => {
            let classname = optional_string(args, "classname")?
                .ok_or("\"classname\" is required")?
                .to_owned();
            let origin = required_vec3(args, "origin")?;
            let mut keys = Vec::new();
            if let Some(object) = args.get("keys") {
                let object = object.as_object().ok_or("\"keys\" must be an object of strings")?;
                for (key, value) in object {
                    let value = value
                        .as_str()
                        .ok_or_else(|| format!("key \"{key}\" must have a string value"))?;
                    keys.push((key.clone(), value.to_owned()));
                }
            }
            let id = editor.create_entity(&classname, origin, &keys)?;
            Ok(ToolResult::json(&json!({ "entity": id })))
        }
        "create_overlay" => {
            let point = required_vec3(args, "point")?;
            let side = match args.get("side") {
                None | Some(Value::Null) => {
                    editor
                        .scene
                        .nearest_face(point, mallet_vmf::overlay::DECAL_REACH)
                        .ok_or("no brush face is within 8 units of that point; pass a side id")?
                        .0
                }
                Some(value) => value
                    .as_u64()
                    .and_then(|side| u32::try_from(side).ok())
                    .ok_or("\"side\" must be a side id")?,
            };
            let material = optional_string(args, "material")?.map_or_else(|| editor.material.clone(), str::to_owned);
            let size = optional_numbers::<2>(args, "size")?.map(glam::DVec2::from_array);
            let id = editor.create_overlay(side, point, &material, size)?;
            let sides = editor
                .entity(id)
                .and_then(|entity| entity.get("sides"))
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|word| word.parse::<u32>().ok())
                .collect::<Vec<_>>();
            Ok(ToolResult::json(&json!({ "entity": id, "sides": sides })))
        }
        "create_decal" => {
            let point = required_vec3(args, "point")?;
            let material = optional_string(args, "material")?.map_or_else(|| editor.material.clone(), str::to_owned);
            let on_face = editor
                .scene
                .nearest_face(point, mallet_vmf::overlay::DECAL_REACH)
                .map_or(point, |(_, nearest, _)| nearest);
            let id = editor.create_decal(on_face, &material)?;
            Ok(ToolResult::json(&json!({ "entity": id })))
        }
        "send_input" => {
            let actions = args
                .get("actions")
                .and_then(Value::as_array)
                .ok_or("\"actions\" must be an array")?;
            let mut frames = Vec::new();
            for (index, action) in actions.iter().enumerate() {
                frames.extend(input_action(editor, action).map_err(|error| format!("action {index}: {error}"))?);
            }
            let count = frames.len();
            editor.input_queue.extend(frames);
            Ok(ToolResult::text(format!("Delivered {count} frames of input.")))
        }
        "undo" | "redo" => {
            let changed = editor.step_history(name == "redo")?;
            let text = match (changed, name) {
                (true, _) => "Done.",
                (false, "undo") => "Nothing to undo.",
                (false, _) => "Nothing to redo.",
            };
            Ok(ToolResult::text(text))
        }
        "set_grid" => {
            let size = optional_number(args, "size")?;
            let snap = args.get("snap").and_then(Value::as_bool);
            let show = args.get("show").and_then(Value::as_bool);
            if size.is_none() && snap.is_none() && show.is_none() {
                return Err("give \"size\", \"snap\" or \"show\"".to_owned());
            }
            if let Some(size) = size {
                let valid =
                    (MIN_GRID..=MAX_GRID).contains(&size) && size.fract() == 0.0 && (size as u32).is_power_of_two();
                if !valid {
                    return Err(format!(
                        "grid size must be a power of two from {MIN_GRID} to {MAX_GRID}"
                    ));
                }
                editor.grid = size;
            }
            if let Some(snap) = snap {
                editor.snap_to_grid = snap;
            }
            if let Some(show) = show {
                editor.show_grid = show;
            }
            Ok(ToolResult::text(format!(
                "Grid is now {}, snapping {}, {}.",
                editor.grid,
                if editor.snap_to_grid { "on" } else { "off" },
                if editor.show_grid { "shown" } else { "hidden" }
            )))
        }
        "center_views" => {
            editor.fit_2d();
            editor.fit_3d();
            Ok(ToolResult::text("Views centred on the map."))
        }
        "set_camera" => set_camera(editor, args),
        "set_viewport_type" => {
            let slot = args
                .get("slot")
                .and_then(Value::as_u64)
                .filter(|slot| *slot < 4)
                .ok_or("\"slot\" must be 0 to 3")? as usize;
            let name = string_arg(args, "type")?;
            let mode = ViewMode::ALL
                .into_iter()
                .find(|mode| mode.key() == name)
                .ok_or_else(|| format!("unknown viewport type \"{name}\""))?;
            editor.layout[slot] = mode;
            Ok(ToolResult::json(&layout_json(editor)))
        }
        "set_view_2d" => set_view_2d(editor, args),
        "list_lights" => Ok(ToolResult::json(&lights_json(editor, args)?)),
        "screenshot" => {
            let target = match optional_string(args, "viewport")?.unwrap_or("all") {
                "all" => ShotTarget::All,
                "camera" => ShotTarget::Camera,
                "top" => ShotTarget::Top,
                "front" => ShotTarget::Front,
                "side" => ShotTarget::Side,
                "window" => return Err("window screenshots are taken by the window itself".to_owned()),
                other => return Err(format!("unknown viewport \"{other}\"")),
            };
            Ok(ToolResult::png(editor.render_png(target)?))
        }
        _ => Err(format!("unknown tool \"{name}\"")),
    }
}

fn edited(result: Result<(), String>) -> Result<ToolResult, String> {
    result.map(|()| ToolResult::text("Done. Undo with the undo tool or Ctrl+Z."))
}

fn status_json(editor: &Editor) -> Value {
    let bounds = &editor.scene.bounds;
    let views: Vec<Value> = editor
        .views
        .iter()
        .map(|view| {
            json!({
                "view": view.axis.label(),
                "center": [view.center.x, view.center.y],
                "pixels_per_unit": view.scale,
            })
        })
        .collect();
    let history = editor.document.as_ref().map(|document| &document.history);
    json!({
        "map": editor.document.as_ref().map(|document| document.display_path()),
        "solids": editor.scene.solid_count,
        "entities": editor.scene.entity_count,
        "bounds": (!bounds.is_empty()).then(|| json!({
            "min": bounds.min.to_array(),
            "max": bounds.max.to_array(),
        })),
        "grid": editor.grid,
        "snap_to_grid": editor.snap_to_grid,
        "show_grid": editor.show_grid,
        "game_dir": editor.game_dir().map(|dir| dir.display().to_string()),
        "textured_3d": editor.gpu.is_some(),
        "gpu": editor.gpu_info,
        "viewports": layout_json(editor),
        "tab": editor.active_tab(),
        "tab_count": editor.tabs().len(),
        "own_view": editor.in_agent_view(),
        "window_tab": editor.window_tab(),
        "timings_ms": {
            "update": editor.timings.update_ms,
            "camera": editor.timings.camera_ms,
            "views_2d": editor.timings.views_2d_ms,
            "previous_frame_cpu": editor.timings.previous_frame_cpu_ms,
            "recent_frame_interval": {
                "min": editor.timings.interval_min_ms,
                "average": editor.timings.interval_average_ms,
                "max": editor.timings.interval_max_ms,
            },
            "recent_intervals": editor.timings.recent_intervals_ms.iter().map(|ms| (ms * 100.0).round() / 100.0).collect::<Vec<_>>(),
        },
        "camera_culling": editor.gpu.as_ref().map(|gpu| json!({
            "chunks": gpu.stats.chunks,
            "chunks_drawn": gpu.stats.chunks_drawn,
            "triangles_drawn": gpu.stats.triangles_drawn,
            "props": gpu.stats.models,
            "props_drawn": gpu.stats.models_drawn,
        })),
        "lighting_preview": editor.gpu.as_ref().and_then(|gpu| gpu.lighting_stats()).map(|stats| lighting_stats_json(&stats)),
        "models": {
            "loaded": editor.models.values().filter(|model| model.is_some()).count(),
            "failed": editor.model_errors.len(),
            "failures": model_failures(editor),
        },
        "tool": match editor.tool {
            Tool::Select => "selection",
            Tool::Camera => "camera",
            Tool::Block => "block",
            Tool::Entity => "entity",
            Tool::Texture => "texture",
            Tool::Clip => "clip",
            Tool::Vertex => "vertex",
            Tool::Overlay => "overlay",
            Tool::Decal => "decal",
        },
        "clip": {
            "points": editor.clipper.points.iter().map(|point| point.to_array()).collect::<Vec<_>>(),
            "keep": match editor.clipper.keep {
                Keep::Both => "both",
                Keep::Front => "front",
                Keep::Back => "back",
            },
            "three_point": editor.clipper.three_point,
        },
        "entity_class": editor.entity_class,
        "fgd": editor.game_data.as_deref().map(|data| json!({
            "files": data.files.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
            "classes": data.len(),
        })),
        "properties_open": editor.properties_open,
        "entity_marker": editor.overlay.entity.map(|point| [point.x, point.y, point.z]),
        "selected": editor.selection.len(),
        "material": editor.material,
        "camera": {
            "position": editor.camera.position.to_array(),
            "yaw": editor.camera.yaw,
            "pitch": editor.camera.pitch,
            "fov": editor.camera.fov,
            "draw_distance": editor.camera.far,
        },
        "views_2d": views,
        "can_undo": history.is_some_and(|history| history.can_undo()),
        "can_redo": history.is_some_and(|history| history.can_redo()),
    })
}

/// Turns one `send_input` action into the frames that perform it.
fn input_action(editor: &Editor, action: &Value) -> Result<Vec<InputFrame>, String> {
    let names: Vec<&str> = match action.get("modifiers") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().ok_or("\"modifiers\" must be strings"))
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("\"modifiers\" must be an array".to_owned()),
    };
    let modifiers = input::parse_modifiers(&names)?;
    if let Some(key) = optional_string(action, "key")? {
        let frames = action.get("hold_frames").and_then(Value::as_u64).unwrap_or(1) as usize;
        let modifier_frames = action
            .get("modifier_frames")
            .and_then(Value::as_u64)
            .map(|frames| frames as usize);
        return input::key_press(key, modifiers, frames, modifier_frames);
    }
    let button = input::parse_button(optional_string(action, "button")?.unwrap_or("primary"))?;
    // `None` for the whole window, in screenshot pixels.
    let viewport = match string_arg(action, "viewport")? {
        "camera" => Some(ShotTarget::Camera),
        "top" => Some(ShotTarget::Top),
        "front" => Some(ShotTarget::Front),
        "side" => Some(ShotTarget::Side),
        "window" => None,
        other => return Err(format!("unknown viewport \"{other}\"")),
    };
    let to_screen = |at: DVec2| match viewport {
        Some(viewport) => editor.viewport_point(viewport, at),
        None => Ok(editor.window_point(at)),
    };
    if let Some(at) = optional_numbers::<2>(action, "click")? {
        let at = to_screen(DVec2::from_array(at))?;
        return Ok(input::click(at, button, modifiers));
    }
    let path = match action.get("drag").and_then(Value::as_array) {
        Some(points) if points.len() == 2 => points,
        _ => return Err("an action needs \"key\", \"click\" or \"drag\" with two points".to_owned()),
    };
    let point = |value: &Value| -> Result<Pos2, String> {
        let pair = match value.as_array().map(Vec::as_slice) {
            Some([x, y]) => x.as_f64().zip(y.as_f64()),
            _ => None,
        };
        let (x, y) = pair.ok_or("drag points must be [x, y]")?;
        to_screen(DVec2::new(x, y))
    };
    Ok(input::drag(point(&path[0])?, point(&path[1])?, button, modifiers))
}

fn tabs_json(editor: &Editor) -> Value {
    let tabs: Vec<Value> = editor
        .tabs()
        .into_iter()
        .map(|tab| json!({ "title": tab.title, "path": tab.path, "unsaved": tab.dirty }))
        .collect();
    let agent = editor.config.agents.own_view.then(|| editor.agent_tab());
    json!({ "active": editor.window_tab(), "agent": agent, "tabs": tabs })
}

fn layout_json(editor: &Editor) -> Value {
    json!(editor.layout.iter().map(|mode| mode.label()).collect::<Vec<_>>())
}

/// Builds and applies the matrix the `transform_selection` tool describes.
fn transform_selection(editor: &mut Editor, args: &Value) -> Result<(), String> {
    use crate::transform::{rotation_xyz, Pivot};
    let pivot = match args.get("pivot") {
        None | Some(Value::Null) => Pivot::Center,
        Some(Value::String(name)) => match name.as_str() {
            "center" | "centre" => Pivot::Center,
            "pivot" => Pivot::Placed,
            "origin" => Pivot::Origin,
            other => return Err(format!("unknown pivot \"{other}\"")),
        },
        Some(_) => Pivot::Point(required_vec3(args, "pivot")?),
    };
    let rotate = optional_numbers::<3>(args, "rotate")?.map_or(DVec3::ZERO, DVec3::from_array);
    let scale = optional_numbers::<3>(args, "scale")?.map_or(DVec3::ONE, DVec3::from_array);
    let offset = optional_numbers::<3>(args, "move")?.map_or(DVec3::ZERO, DVec3::from_array);
    let center = editor.pivot_point(pivot);
    let linear = rotation_xyz(rotate) * glam::DMat3::from_diagonal(scale);
    let matrix = glam::DMat4::from_translation(offset) * mallet_vmf::transform::around(center, linear);
    let turn = rotate != DVec3::ZERO || scale.x * scale.y * scale.z < 0.0;
    let saved = editor.config.transform.clone();
    if let Some(lock) = args.get("texture_lock").and_then(Value::as_bool) {
        editor.config.transform.texture_lock = lock;
    }
    if let Some(lock) = args.get("texture_scale_lock").and_then(Value::as_bool) {
        editor.config.transform.texture_scale_lock = lock;
    }
    let result = editor.transform_selection(matrix, turn, center);
    editor.config.transform = saved;
    result?;
    editor.message = "Transformed selection".to_owned();
    Ok(())
}

/// A world axis named `x`, `y` or `z`, as an index.
/// The `paste` tool: where to paste and the Paste Special options.
fn paste(editor: &mut Editor, args: &Value) -> Result<(), String> {
    use crate::editor::{PasteAt, PasteOptions};
    let at = match args.get("at") {
        None | Some(Value::Null) => PasteAt::Original,
        Some(Value::String(name)) => match name.as_str() {
            "original" => PasteAt::Original,
            "cursor" => editor.paste_point(),
            other => return Err(format!("unknown paste point \"{other}\"")),
        },
        Some(_) => PasteAt::Center(required_vec3(args, "at")?),
    };
    let text = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    let flag = |key: &str| args.get(key).and_then(Value::as_bool).unwrap_or(false);
    let copies = match args.get("copies") {
        None | Some(Value::Null) => 1,
        Some(value) => value
            .as_u64()
            .and_then(|copies| u32::try_from(copies).ok())
            .ok_or("copies must be a whole number")?,
    };
    let options = PasteOptions {
        copies,
        start_at_original: false,
        offset: optional_numbers::<3>(args, "offset")?.map_or(DVec3::ZERO, DVec3::from_array),
        rotation: optional_numbers::<3>(args, "rotation")?.map_or(DVec3::ZERO, DVec3::from_array),
        around_pivot: flag("around_pivot"),
        prefix: text("prefix"),
        suffix: text("suffix"),
        unique_names: flag("unique_names"),
        group: flag("group"),
    };
    if options.around_pivot && editor.clipboard.pivot.is_none() {
        return Err(
            "no pivot was placed when the objects were copied; set one with set_pivot and copy again".to_owned(),
        );
    }
    editor.paste(at, &options)?;
    Ok(())
}

fn axis_arg(args: &Value, key: &str) -> Result<usize, String> {
    match string_arg(args, key)?.to_ascii_lowercase().as_str() {
        "x" => Ok(0),
        "y" => Ok(1),
        "z" => Ok(2),
        other => Err(format!("\"{key}\" must be x, y or z, not \"{other}\"")),
    }
}

fn selection_json(editor: &Editor) -> Value {
    let mut solids = Vec::new();
    let mut entities = Vec::new();
    for owner in &editor.selection {
        match *owner {
            Owner::Solid(id) => solids.push(id),
            Owner::Entity(id) => entities.push(id),
        }
    }
    let bounds = editor.scene.selection_bounds();
    json!({
        "solids": solids,
        "entities": entities,
        "bounds": (!bounds.is_empty()).then(|| json!({
            "min": bounds.min.to_array(),
            "max": bounds.max.to_array(),
        })),
        "pivot": (!bounds.is_empty()).then(|| editor.pivot().to_array()),
        "pivot_placed": editor.overlay.pivot.is_some(),
        "handles": editor.overlay.handle_mode.label().to_ascii_lowercase(),
    })
}

fn list_entities(editor: &Editor, args: &Value) -> Result<Value, String> {
    let pattern = optional_string(args, "classname")?;
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(DEFAULT_LIST_LIMIT);
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let matching: Vec<&Entity> = editor
        .document
        .as_ref()
        .ok_or("no map is open")?
        .vmf
        .all_entities()
        .filter(|entity| pattern.is_none_or(|pattern| classname_matches(pattern, entity.classname())))
        .collect();
    let entities: Vec<Value> = matching
        .iter()
        .take(limit)
        .map(|entity| entity_summary(entity))
        .collect();
    Ok(json!({
        "total": matching.len(),
        "returned": entities.len(),
        "entities": entities,
    }))
}

fn set_camera(editor: &mut Editor, args: &Value) -> Result<ToolResult, String> {
    let camera = &mut editor.camera;
    if let Some(position) = optional_numbers::<3>(args, "position")? {
        camera.position = DVec3::from_array(position);
    }
    if let Some(yaw) = optional_number(args, "yaw")? {
        camera.yaw = yaw.rem_euclid(360.0);
    }
    if let Some(pitch) = optional_number(args, "pitch")? {
        camera.pitch = pitch.clamp(-MAX_PITCH, MAX_PITCH);
    }
    if let Some(distance) = optional_number(args, "draw_distance")? {
        camera.far = distance.clamp(256.0, 1_048_576.0);
    }
    if let Some(target) = optional_numbers::<3>(args, "look_at")? {
        let direction = DVec3::from_array(target) - camera.position;
        if direction.length_squared() > 0.0 {
            camera.yaw = direction.y.atan2(direction.x).to_degrees().rem_euclid(360.0);
            camera.pitch = direction
                .z
                .atan2(direction.truncate().length())
                .to_degrees()
                .clamp(-MAX_PITCH, MAX_PITCH);
        }
    }
    Ok(ToolResult::json(&json!({
        "position": camera.position.to_array(),
        "yaw": camera.yaw,
        "pitch": camera.pitch,
        "draw_distance": camera.far,
    })))
}

fn set_view_2d(editor: &mut Editor, args: &Value) -> Result<ToolResult, String> {
    let index = match string_arg(args, "view")? {
        "top" => 0,
        "front" => 1,
        "side" => 2,
        other => return Err(format!("unknown view \"{other}\"; use top, front or side")),
    };
    let view = &mut editor.views[index];
    if let Some(center) = optional_numbers::<2>(args, "center")? {
        view.center = DVec2::from_array(center);
    }
    if let Some(scale) = optional_number(args, "pixels_per_unit")? {
        view.set_scale(scale);
    }
    Ok(ToolResult::json(&json!({
        "view": view.axis.label(),
        "center": [view.center.x, view.center.y],
        "pixels_per_unit": view.scale,
    })))
}

/// What the lighting preview drew in its latest frame.
fn lighting_stats_json(stats: &crate::render3d::LightingStats) -> Value {
    json!({
        "lights": stats.lights,
        "texture_light_patches": stats.texture_lights,
        "lit_everywhere": stats.everywhere,
        "grid_cells": stats.grid_cells,
        "most_lights_in_a_cell": stats.busiest_cell,
        "shadowed_lights": stats.shadowed,
        "lights_with_small_shadows": stats.small_shadowed,
        "lights_waiting_for_small_shadows": stats.waiting,
        "3d_skybox": stats.skybox.map(|bounds| json!({ "min": bounds.min.to_array(), "max": bounds.max.to_array() })),
        "luxels": stats.luxels,
        "luxels_coarser_than_asked": stats.luxel_scale,
        "luxel_bake_progress": stats.baking,
        "faces_show_baked_luxels": stats.luxels_shown,
        "shadow_map_layers": stats.shadow_layers,
        "shadow_maps_drawn_last_frame": stats.shadow_maps_drawn,
        "glows": stats.glows,
    })
}

/// The `list_lights` tool: the map's lighting as the preview reads it.
fn lights_json(editor: &Editor, args: &Value) -> Result<Value, String> {
    let pattern = optional_string(args, "classname")?.unwrap_or("");
    let options = &editor.config.lighting;
    let lighting = &editor.scene.lighting;
    let lights: Vec<Value> = lighting
        .lights
        .iter()
        .filter(|light| pattern.is_empty() || classname_matches(pattern, &light.classname))
        .map(|light| {
            let falloff = match light.falloff {
                Falloff::Inverse {
                    constant,
                    linear,
                    quadratic,
                    cap,
                    fade_start,
                    fade_end,
                    max_distance,
                } => json!({
                    "type": "inverse",
                    "constant": constant,
                    "linear": linear,
                    "quadratic": quadratic,
                    "cap": cap.is_finite().then_some(cap),
                    "fade": (fade_end > fade_start).then_some([fade_start, fade_end]),
                    "max_distance": (max_distance > 0.0).then_some(max_distance),
                }),
                Falloff::Projected { far } => json!({ "type": "projected", "far": far }),
                Falloff::Radius { radius } => json!({ "type": "radius", "radius": radius }),
                Falloff::None => json!({ "type": "none" }),
            };
            json!({
                "entity": light.entity,
                "classname": light.classname,
                "kind": format!("{:?}", light.kind).to_lowercase(),
                "origin": light.origin.to_array(),
                "direction": light.direction.to_array(),
                "ldr": light.ldr.to_array(),
                "hdr": light.hdr.to_array(),
                "falloff": falloff,
                "reach": light.reach(options.hdr, options.light_cutoff()),
                "cone": light.cone.map(|cone| json!({
                    "inner_degrees": cone.inner_degrees(),
                    "outer_degrees": cone.outer_degrees(),
                    "focus": cone.exponent,
                })),
                "appearance": light.pattern,
                "starts_dark": light.starts_dark,
                "casts_shadows": light.shadows,
                "dynamic": light.dynamic,
                "lights": format!("{:?}", light.receivers).to_lowercase(),
                "projection": light.projection.as_ref().map(|projection| json!({
                    "texture": projection.texture,
                    "fov": projection.fov,
                    "near": projection.near,
                })),
                "sun_spread_degrees": (light.spread > 0.0).then_some(light.spread),
            })
        })
        .collect();
    let mut used: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for face in editor.scene.meshes.iter().flat_map(|mesh| &mesh.faces) {
        let Some(material) = &face.material else {
            continue;
        };
        let key = material.replace('\\', "/").to_ascii_lowercase();
        if editor.texture_lights.contains_key(&key) {
            *used.entry(key).or_default() += 1;
        }
    }
    let texture_lights: Vec<Value> = used
        .into_iter()
        .map(|(material, faces)| {
            let light = editor.texture_lights[&material];
            json!({ "material": material, "faces": faces, "ldr": light.ldr.to_array(), "hdr": light.hdr.to_array() })
        })
        .collect();
    let mut result = json!({
        "hdr": options.hdr,
        "lights": lights,
        "sky_light": lighting.ambient.map(|ambient| json!({ "ldr": ambient.ldr.to_array(), "hdr": ambient.hdr.to_array() })),
        "fog": lighting.fog.map(|fog| json!({
            "color": fog.color.to_array(),
            "start": fog.start,
            "end": fog.end,
            "max_density": fog.max_density,
            "blend": fog.blend.map(|(color, direction)| json!({ "color": color.to_array(), "direction": direction.to_array() })),
        })),
        "sky": {
            "name": lighting.sky.name,
            "painted": lighting.sky.paint.is_some(),
            "sun_glow": lighting.sky.sun.map(|(toward, color, size)| json!({
                "toward": toward.to_array(),
                "color": color.to_array(),
                "size_degrees": size,
            })),
        },
        "glows": lighting.glows.len(),
        "beams": lighting.beams.len(),
        "texture_lights_listed": editor.texture_lights.len(),
        "texture_lights_in_map": texture_lights,
        "preview": editor.gpu.as_ref().and_then(|gpu| gpu.lighting_stats()).map(|stats| lighting_stats_json(&stats)),
    });
    if args.get("catalogue").and_then(Value::as_bool) == Some(true) {
        result["catalogue"] = CATALOGUE
            .iter()
            .map(|class| {
                json!({
                    "classname": class.classname,
                    "group": class.group.label(),
                    "description": class.description,
                    "preview": class.preview,
                })
            })
            .collect();
    }
    Ok(result)
}

fn classname_matches(pattern: &str, classname: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => classname
            .get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix)),
        None => classname.eq_ignore_ascii_case(pattern),
    }
}

fn entity_summary(entity: &Entity) -> Value {
    json!({
        "id": entity.id(),
        "classname": entity.classname(),
        "targetname": entity.get("targetname"),
        "origin": entity.origin().map(|origin| origin.to_array()),
        "solids": entity.solids().count(),
    })
}

/// Merges `changes` into `target`: objects merge key by key, anything else replaces.
fn merge(target: &mut Value, changes: &Value) {
    match (target, changes) {
        (Value::Object(target), Value::Object(changes)) => {
            for (key, change) in changes {
                merge(target.entry(key.clone()).or_insert(Value::Null), change);
            }
        }
        (target, change) => *target = change.clone(),
    }
}

/// The running or last compile, for `compile_map` and `compile_status`.
pub fn compile_status_json(editor: &Editor, lines: usize) -> Value {
    let Some(job) = &editor.compile_job else {
        return json!({ "state": "none", "message": "Nothing has been compiled yet." });
    };
    let job = job.lock();
    let (state, error) = match &job.outcome {
        None => ("running", None),
        Some(Outcome::Succeeded) => ("succeeded", None),
        Some(Outcome::Failed(error)) => ("failed", Some(error.clone())),
        Some(Outcome::Cancelled) => ("cancelled", None),
    };
    let steps: Vec<Value> = job
        .steps
        .iter()
        .map(|step| {
            json!({
                "title": step.title,
                "status": format!("{:?}", step.status).to_lowercase(),
                "seconds": step.time().map(|time| (time.as_secs_f64() * 10.0).round() / 10.0),
                "progress": step.progress,
                "command": step.command,
            })
        })
        .collect();
    let problems: Vec<Value> = job
        .problems
        .iter()
        .take(100)
        .map(|problem| {
            json!({
                "severity": if problem.severity == Severity::Error { "error" } else { "warning" },
                "step": job.steps.get(problem.step).map(|step| step.title.clone()),
                "text": problem.text,
                "brush": problem.brush,
                "position": problem.position.map(|point| [point.x, point.y, point.z]),
            })
        })
        .collect();
    let tail: Vec<&str> = job
        .lines
        .iter()
        .skip(job.lines.len().saturating_sub(lines))
        .map(|line| line.text.as_str())
        .collect();
    json!({
        "state": state,
        "error": error,
        "map": job.map,
        "preset": job.preset,
        "seconds": (job.elapsed().as_secs_f64() * 10.0).round() / 10.0,
        "progress": (job.progress() * 100.0).round() / 100.0,
        "steps": steps,
        "errors": job.errors(),
        "warnings": job.warnings(),
        "problems": problems,
        "leaked": job.leaked,
        "leak_line_shown": editor.pointfile.is_some(),
        "output": tail,
    })
}

/// Folders whose materials are not meant for brushes.
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

fn face_json(face: &FaceInfo) -> Value {
    let axis = |axis: &mallet_vmf::TexAxis| {
        json!({
            "axis": [axis.axis.x, axis.axis.y, axis.axis.z],
            "shift": axis.offset,
            "scale": axis.scale,
        })
    };
    let normal = face.normal();
    let center = face.center();
    json!({
        "side": face.side.id,
        "solid": face.solid,
        "material": face.side.material,
        "normal": [normal.x, normal.y, normal.z],
        "center": [center.x, center.y, center.z],
        "u": axis(&face.side.uaxis),
        "v": axis(&face.side.vaxis),
        "rotation": face.side.rotation.unwrap_or(0.0),
        "lightmap_scale": face.side.lightmap_scale,
    })
}

fn connection_json(connection: &Connection) -> Value {
    json!({
        "output": connection.output,
        "target": connection.target,
        "input": connection.input,
        "parameter": connection.parameter,
        "delay": connection.delay,
        "refires": connection.refires,
    })
}

fn connection_arg(value: &Value) -> Result<Connection, String> {
    let text = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
    let connection = Connection {
        output: text("output"),
        target: text("target"),
        input: text("input"),
        parameter: text("parameter"),
        delay: value.get("delay").and_then(Value::as_f64).unwrap_or(0.0),
        refires: value.get("refires").and_then(Value::as_i64).unwrap_or(-1) as i32,
    };
    if connection.output.is_empty() || connection.input.is_empty() {
        return Err("\"output\" and \"input\" are required".to_owned());
    }
    Ok(connection)
}

fn class_json(class: &EntityClass) -> Value {
    let properties: Vec<Value> = class
        .properties
        .iter()
        .map(|property| {
            let mut value = json!({
                "name": property.name,
                "type": property.kind,
                "display": property.display,
                "default": property.default,
                "description": property.description,
            });
            if !property.choices.is_empty() {
                value["choices"] = property
                    .choices
                    .iter()
                    .map(
                        |choice| json!({ "value": choice.value, "name": choice.name, "default_on": choice.default_on }),
                    )
                    .collect();
            }
            if property.read_only {
                value["read_only"] = json!(true);
            }
            value
        })
        .collect();
    let ports = |ports: &[mallet_content::Port]| -> Vec<Value> {
        ports
            .iter()
            .map(|port| json!({ "name": port.name, "type": port.kind, "description": port.description }))
            .collect()
    };
    json!({
        "name": class.name,
        "kind": match class.kind {
            ClassKind::Base => "base",
            ClassKind::Point => "point",
            ClassKind::Solid => "solid",
        },
        "description": class.description,
        "bases": class.bases,
        "size": class.size,
        "color": class.color,
        "model": class.studio,
        "uses_model_key": class.studio_from_model,
        "properties": properties,
        "inputs": ports(&class.inputs),
        "outputs": ports(&class.outputs),
    })
}

fn entity_details(entity: &Entity) -> Value {
    let keys: Vec<[&str; 2]> = entity
        .items
        .iter()
        .filter_map(|item| match item {
            EntityItem::Key(pair) => Some([pair.key.as_str(), pair.value.as_str()]),
            _ => None,
        })
        .collect();
    let connections: Vec<[&str; 2]> = entity
        .items
        .iter()
        .filter_map(|item| match item {
            EntityItem::Other(block) if block.name.eq_ignore_ascii_case("connections") => Some(block),
            _ => None,
        })
        .flat_map(|block| block.pairs().map(|pair| [pair.key.as_str(), pair.value.as_str()]))
        .collect();
    let solids: Vec<Option<u32>> = entity.solids().map(|solid| solid.id()).collect();
    json!({
        "id": entity.id(),
        "classname": entity.classname(),
        "keys": keys,
        "connections": connections,
        "solids": solids,
        "editor_color": entity.editor_color(),
    })
}

fn string_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("\"{key}\" must be a string"))
}

fn optional_string<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(format!("\"{key}\" must be a string")),
    }
}

fn optional_number(args: &Value, key: &str) -> Result<Option<f64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .map(Some)
            .ok_or_else(|| format!("\"{key}\" must be a number")),
    }
}

fn optional_numbers<const N: usize>(args: &Value, key: &str) -> Result<Option<[f64; N]>, String> {
    let error = || format!("\"{key}\" must be an array of {N} numbers");
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) if items.len() == N => {
            let mut numbers = [0.0; N];
            for (number, item) in numbers.iter_mut().zip(items) {
                *number = item.as_f64().ok_or_else(error)?;
            }
            Ok(Some(numbers))
        }
        Some(_) => Err(error()),
    }
}

fn required_vec3(args: &Value, key: &str) -> Result<DVec3, String> {
    optional_numbers::<3>(args, key)?
        .map(DVec3::from_array)
        .ok_or_else(|| format!("\"{key}\" is required"))
}

fn id_list(args: &Value, key: &str) -> Result<Vec<u32>, String> {
    let error = || format!("\"{key}\" must be an array of whole numbers");
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_u64().and_then(|id| u32::try_from(id).ok()).ok_or_else(error))
            .collect(),
        Some(_) => Err(error()),
    }
}

fn id_arg(args: &Value) -> Result<u32, String> {
    args.get("id")
        .and_then(Value::as_u64)
        .and_then(|id| u32::try_from(id).ok())
        .ok_or_else(|| "\"id\" must be a whole number".to_owned())
}

/// Up to ten models that failed to load, with the reason, sorted by path.
fn model_failures(editor: &Editor) -> Value {
    let mut failures: Vec<(&String, &String)> = editor.model_errors.iter().collect();
    failures.sort();
    failures
        .into_iter()
        .take(10)
        .map(|(path, error)| json!({ "model": path, "error": error }))
        .collect()
}
