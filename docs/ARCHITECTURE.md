# Mallet architecture

Mallet is a Hammer-style editor for Source engine VMF maps, written in Rust. It aims to
match Hammer's viewports, tools and default key bindings, and to let AI agents work on
maps through a built-in MCP server.

## Crates

| Crate | Purpose |
| --- | --- |
| `mallet-kv` | Reads and writes Valve KeyValues text. Keeps every key, block and duplicate in order and writes Hammer's exact layout (tabs, CRLF). |
| `mallet-vmf` | Typed VMF model on top of `mallet-kv`, brush geometry, the edit/undo layer, Source's lighting rules (`light`) and where VBSP puts a face's luxels (`lightmap`). No UI dependencies. |
| `mallet-format` | The `.mallet` map format: the `Vmf` model as compact binary, with a checksum per section and room for the editor's own state. |
| `mallet-content` | Game content: `gameinfo.txt` search paths, VPK archives, VMT materials, VTF textures, MDL models and FGD entity classes. |
| `mallet` | The editor application: window, viewports, key bindings and the MCP server. |

The icon lives in `branding/`: `make_icons.py` draws the mallet once and writes the SVG,
PNGs and the multi-size `.ico` (with a simpler drawing at 24 pixels and below). `build.rs`
embeds the `.ico` and version details in the executable on Windows; `branding.rs` gives the
window, menu bar, About window and MCP server's `serverInfo` the same picture.

## Lossless loading and saving

A VMF is parsed into `Vmf`, whose world, entities, solids and sides are typed while every
block Mallet does not understand yet (`versioninfo`, `visgroups`, `dispinfo`, `connections`,
`hidden`, cordons and so on) is kept verbatim. Saving writes the same structure back, so
opening and saving a map changes nothing except number formatting that parses back to the
same values. Files that are not valid UTF-8 are read byte-for-byte and written back the same
way.

The one thing saving adds is where the map was viewed from, as Hammer does: the 3D camera
goes into the active entry of the `cameras` block (other cameras are kept) and the grid
size, Snap to Grid and Show Grid into `viewsettings`. Opening a map puts the camera and grid back, in Mallet and in
Hammer. This is not an edit, so it neither dirties the map nor enters the undo history.

## The .mallet format

`mallet-format` writes the `Vmf` model straight to bytes, without going through KeyValues
text. Sides are fixed records: their id, plane points and texture axes are numbers (whole
numbers as zigzag varints of one to three bytes, anything else as the exact `f64`), and
every key, value, block name and material is an index into a string table that holds each
string once. Everything `Vmf` keeps verbatim is kept the same way, along with where a
side's extra items sat among its typed fields, so VMF to `.mallet` to VMF gives the same
bytes. The map section is LZ4 compressed.

A file is a signature (`89 'MALL' 0D 0A 1A`, which catches text-mode copies), a format
version and a list of sections, each tagged, sized and checksummed with CRC-32. `MAP ` holds
the map; `META` is JSON that `Document` fills in, currently the writing Mallet's version and
the text encoding the map had as a VMF, so exporting it again writes the same bytes.
Readers skip sections they do not know, and refuse a newer format version with a clear
message.

`Document::load` tells the formats apart by the signature, and `Document::save` and
`Document::export` choose by the extension. On City 17 (28.5 MB of VMF, 9,289 brushes and
9,504 entities) a release build reads the map in 119 ms against 328 ms for VMF and writes
it in 140 ms against 554 ms, and the file is 2.8 MB. `MALLET_BIG_VMF=<map> cargo test
--release -p mallet-format big -- --nocapture` measures it on any map.

The compilers read VMF only. For a `.mallet` map, `Editor::compile_context` points the
compile at `mallet_compile/<name>.vmf` beside the map (or the folder Mallet Options names), and `start_compile` writes that copy
of the map as it is, saved or not, before the first step.

`shell.rs` sets up the Windows file types from Mallet Options > Files, for the current
user only (`HKEY_CURRENT_USER\Software\Classes`, no administrator rights): `.mallet` points
at the `Mallet.Map` program id, and `Mallet.Vmf` is added to `.vmf\OpenWithProgids` so the
default program for VMF files is left alone. Both open `"<mallet.exe>" "%1"`. When Mallet
starts with a map, `handoff.rs` first offers it to a Mallet already listening on the MCP
port (`open_map` with `focus`, after `AllowSetForegroundWindow` so that copy may come to
the front) and exits if one takes it, so double-clicking maps fills tabs of one window.
Copies with their own `MALLET_MCP_PORT` stay apart.

`presence.rs` shows Mallet on the user's Discord profile through the Discord client's
local IPC pipe. A background thread owns the connection; each frame the window hands it
the wanted status (the map's name, and whether it is compiling), only sending it on when
it changes. While Discord is not running the thread retries every 15 seconds, and
turning presence off in Mallet Options clears the activity at once.

## Edits and undo

Every change to a map is an `Edit` (`mallet_vmf::edit`). Applying an edit returns its
inverse, and `History` keeps undo and redo stacks of those inverses. The keyboard, menus and
MCP tools all go through this one path, so an edit made by an agent can be undone with
Ctrl+Z and the other way round. New editing features add `Edit` variants rather than
mutating the map directly.

Edits that add brushes or entities carry them with ids already assigned by
`IdAllocator`, which starts above every id in the map, including ids inside blocks kept
verbatim. Moving a brush shifts its texture offsets so textures stay locked to it, and
moves a displacement's start position with it. Resizing (`Edit::Rescale`) maps every plane
point from the old bounding box to the new one; its undo is `Edit::ReplaceSolids` holding
the original brushes, so undo is exact instead of scaling back with rounding errors.

Rotations, skews, mirrors and the editor's own resizes are `Edit::Transform`: an affine
matrix applied to every plane point (`mallet_vmf::transform`). A mirroring matrix reverses
each side's plane points so normals still point out. Texture lock carries each texture
axis through the inverse transpose of the matrix's linear part; `TextureLock::Full` also
lets the texture stretch, `Align` keeps its scale and pins it at an anchor point, and
`Off` leaves it in world space. Displacement normals, distances and offsets go through
the linear part, and coordinates within a millionth of a whole number snap to it, so a
90° turn stays on the grid. Its undo is `ReplaceSolids`, like resizing.

`crates/mallet/src/transform.rs` builds the matrices for the editor's commands (around
the pivot, the selection's centre or the world origin) and turns entity `angles` by
carrying their forward and up directions through the matrix and squaring them up again,
which also handles mirrors. The 2D views preview a drag through `Overlay::transform`, now
a general matrix, so rotations and skews show live in every view, models included.
`gizmo.rs` places the 3D gizmo at a fixed size on screen, hit-tests its parts in screen
space, and turns a drag into a move (the mouse ray's closest point on an axis, or its hit
on a plane), a rotation (the mouse's angle around the gizmo's centre on screen) or a
scale; the result goes through the same commands as the 2D handles.

## Selection

The side panel's selection mode (`PickMode`) decides what a click picks, as in Hammer.
Groups, the default, picks the whole VMF group a brush or entity is in (its `groupid`),
and clicking any part of an entity selects the whole entity. Objects picks one brush, or one whole entity, even inside a group. Solids picks one brush
even inside a brush entity. Ctrl+W cycles the three modes. `Scene::pick_2d`, `pick_ray` and `owners_inside` take a
`solids` flag for Solids mode; `Editor::select_picked` expands groups in Groups mode, and
`select` keeps an entity's brushes out of the selection while the entity is selected. The
selection lives in `Editor` as a set of `Owner`s, so the window and MCP tools share it. 2D clicks pick, like Hammer, a brush's
centre × first and then its outline, preferring the smallest brush; empty space inside a
brush picks nothing. 3D clicks cast a ray against each brush's planes, skipping a brush
the camera is inside; in mouselook the ray goes through the crosshair. Props are tested
against their model's triangles, in model space through the inverse of their placement,
so a click through a gap in a fence picks the wall behind it; only when the ray hits
nothing at all does a prop's bounding box count, so thin or hollow models can still be
picked. The texture tool uses the same test, so props block it only where they are. The camera smooths
raw mouse movement over two frames and eases its speed, using real frame times.

QuickHide (H, Ctrl+H and U) hides objects the way Hammer saves them: `Edit::SetHidden` wraps a
brush or entity in a `hidden` block where it stands, so the map keeps and saves it while the scene,
the views and the selection skip it, and unwraps it again to show it. Each hide or show is one undo
step, and `edit::hidden_ids` finds the hidden objects again.

## Map tabs

Several maps can be open at once, one per tab. The active tab's map state (document and
undo history, scene, views, camera, selection, tool previews) lives directly in `Editor`,
so all other code only ever deals with one map; switching tabs parks that state in a
`MapState` and moves the other tab's state in. Settings, the game's materials and the GPU
renderer are shared, so textures load once for every tab. GPU caches are keyed by the
scene's generation numbers, which are unique across tabs, so a switch simply redraws.

## Window layout

The chrome around the viewports lives in `app/chrome.rs`, laid out like Hammer++: an icon
toolbar (Phosphor icons) under the menus, the tab strip, the tool palette on the left and
the object panel on the right. Keyboard shortcuts are ignored while a text field has
focus.

## Shared editor state

`Editor` (in `crates/mallet/src/editor.rs`) owns the open document, the wireframe scene,
the camera, the 2D views and the grid. It sits behind a mutex shared by:

- the window, which locks it once per frame to handle input and draw;
- the MCP server thread, which locks it once per tool call.

Tool calls therefore never wait for a frame. They keep working while the window is covered
or minimized, because Windows sends no repaints to a minimized window.

## Game content

`GameFs` mounts the `game` search paths of a game's `gameinfo.txt` in priority order:
loose folders, `folder/*` wildcards such as GMod's addons, and VPK archives (only their
directory trees are read up front). After them come the games Garry's Mod lists in
`cfg/mount.cfg`, each folder with every `*_dir.vpk` in it, unless Mallet Options turns
them off. Every mount belongs to a `Source` (a game VPK or folder, a mounted game, an
addon or downloads), and listings say which source each file is read from, so the texture
browser can filter by them.

A game with its addons and mounted games is a hundred or more mounts listing hundreds of
thousands of files, and a map looks thousands of files up, so lookups are kept cheap. A
mounted folder remembers which of its directories exist and is skipped without touching
the disk when it lacks the file's directory; a directory found missing is looked for
again after five seconds, so content added while Mallet runs shows up. A VPK's tree is
kept as folders sorted by path, each with its files sorted by name and all names in one
string, which takes half the memory of a table of whole paths and finds a file with two
binary searches.

`Materials` turns a material name into the texture
Hammer shows: `%tooltexture` first, then `$basetexture`, following `patch` materials. It
caches what each material is and how large its texture is, read from the texture file's
header, but no pixels: the viewports keep theirs on the graphics card. A
VTF keeps its picture at many sizes, smallest first, and only its header and the sizes no
bigger than 512 pixels are read from the file (`GameFs::open` reads a file a part at a
time). DXT pictures go to the graphics card as the file stores them when it can sample
them, which takes a quarter to an eighth of the memory of decoded pixels; other formats
are decoded, and a texture stored without small sizes is scaled down to fit. Materials
that cannot be found render as Hammer's pink and black checkers.

`load_model` reads a studio model's `.mdl`, `.dx90.vtx` and `.vvd` through `vmdl` and keeps
the first level of detail with skin 0. Each mesh's material is resolved against the
model's `$cdmaterials` folders in order, as the engine does. The parser panics on some
features it lacks (animations in separate blocks), so a panic counts as a failed load.
The editor loads every model a map uses in parallel when the map opens and keeps them in
a cache shared by all tabs, with the error for each model that failed.

## Entity classes

`GameData` reads FGD files, following `@include`, and merges each class with its base
classes: keyvalues with types, defaults, help and choices, spawnflags, inputs and outputs,
and the `size()`, `color()`, `studio()`, `iconsprite()` and `sprite()` helpers. Base classes have their
own namespace, since `base.fgd` has both a `Light` base class and a `light` point class.
Mallet reads the FGD in the game's `bin` folder named after the game folder (such as
`garrysmod.fgd`) unless Tools > Game Data Files picks others. The scene uses a class's
size and colour for point entities and shows its `studio()` model, such as the player
figure of `info_player_start`. New entities start with their class's defaults.

Outputs are `Connection`s read from and written to an entity's `connections` block, keeping
the separator the map uses, and change through the undoable `Edit::SetConnections`.

## Object Properties

Alt+Enter, Edit > Properties or a double-click opens one window for the selected entities
(the world for world brushes, and Map > Map Properties always the world). It edits every
selected entity at once and shows "(different)" where they disagree. Tabs: Keyvalues,
with an editor per FGD type (choices, colour pickers, target names from the map, a model
Browse... button, and for angles a compass with Up and Down) and SmartEdit, which turned
off shows every key as plain text; Outputs, with named-target checks and pickers for the
class's outputs and the target's inputs; Inputs, listing the outputs elsewhere that name
this entity; and Flags. Help for the hovered key shows at the bottom. Text fields commit
when they lose focus, and every change is an undoable edit; Apply commits whatever is
still being typed, as Hammer's Apply does.

Side list keyvalues (FGD type `sidelist`, such as `sides` and `sides2`) get a Pick button.
It starts a face pick (`face_pick.rs`): `Editor::face_pick` names the key, the entities and
the tab. While it is active in the tab the window shows, clicks in the 3D view go to
`face_pick_click` instead of the current tool, the gizmo and tool handles are left out, and
`refresh_face_marks` marks the listed faces in place of the texture tool's selection.
`pick_side` makes the change for every entity as one undo step; the Overlay tool's
Ctrl+click and the `pick_face` MCP tool use it too. The pick ends with Escape, the button,
closing the window, or a selection or tab change.

## Texturing

`mallet_vmf::texture` holds the Face Edit math, tested on its own: Hammer's world axes
(Quake's base axes, floors first), face axes that follow slopes, rotation about the face
normal, and justify and fit, all on `u = point · axis / scale + offset`. Hotspot fit
(`Side::hotspot`) reads `.rect` rectangles (`Hotspot::parse_rect_file`) and scores each one,
and its quarter turn when allowed, by `|ln(width ratio)| + |ln(height ratio)|` against the
face at 0.25 units per texel; scores within 0.05 of the best tie and are picked between at
random. `Materials::rectangle_file` finds the file through the VMT's `%rectanglemap`,
following `patch` includes, and caches it.

The texture tool works on faces only: switching to it trades the selected brushes for their
faces, and while it is active `Editor::select` selects, adds or removes the owners' faces
(`select_owner_faces`) and leaves the brush selection empty. Outside it,
`Editor::texture_targets` falls back to every face of the selected brushes when no face is
selected, for the texture browser and MCP.

The texture tool keeps a set of selected side ids per tab; scene faces carry their side and
brush ids, so a ray picks one face through the plane it enters. Every face change replaces
the brushes involved with `Edit::ReplaceSolids`, which undoes exactly. A drag passes a group
to `History::apply_grouped`, which keeps only the first step's inverse, so the whole drag
undoes at once.

Texture changes do not change shape, so they skip the full scene rebuild: `Scene::retexture`
rebuilds only the meshes of the brushes involved and records the step in
`Scene::retextures`. The renderer keeps every chunk's triangles on the CPU; when it can
follow the recorded steps from what it last drew to the current generation, it
re-triangulates only the chunks those meshes are in. A face edit on City 17 (9,289 brushes)
takes about 10 ms instead of 250.

The texture browser lists materials with their sources on a thread, describes every VMT in
the background for the kind filters (`MaterialInfo`), and loads thumbnails on four worker
threads, newest request first, so what is on screen loads before what scrolled past.

The model browser (`app/model_browser.rs`) works the same way for `.mdl` files. Its listing
thread also collects which models have a `.phy` beside them. Whether a model is a static
prop is a flag in its `.mdl`, learned for free when its thumbnail is drawn. Only the "Static
props" filter needs it for models not yet seen, and then one thread reads the flag of the
models the other filters let through, one after another, and stops when the filters change:
a game's files often sit on a slow disk, and reading all of them is not worth it. Thumbnails and
the preview are drawn on the CPU by `model_render.rs`, a small depth-buffered triangle
filler with perspective-correct texturing, half-Lambert lighting and alpha test and blend.
That keeps model pictures off the graphics card and away from the 3D views' renderer:
worker threads paint thumbnails newest request first, and one preview thread keeps the
selected model loaded and redraws it for the newest camera angle only, at a lower quality
while it is being dragged. `load_model_variant` loads a model with a chosen skin and body
groups and returns `ModelDetails` (skins, body groups, static prop flag, sequences) for the
preview; `load_model` still loads the first skin with every body group for the 3D views.

## Clipping

`mallet_vmf::clip::split_solid` cuts a brush along a plane the way Hammer does: each half
is the brush plus one new side on the cut (the current material, aligned to the world),
minus the sides left without area. Both halves keep the brush's id and side ids; when both
are kept, the front one is renumbered and added next to the back one in the same entity, so
brush entities stay whole. Entities that list faces in `sides` (overlays, cube maps) are
pointed at the faces the cut ones became. The whole clip is one `Edit::Batch`.

`crates/mallet/src/clipper.rs` holds the tool. Two-point clipping drags a line in a 2D view
and runs the plane along that view's depth; three-point clipping (C) takes three points from
the 2D views or from faces in the 3D view. Shift+X cycles the halves kept and Enter clips.
The preview (kept pieces and the plane's outline) is recomputed only when the points, the
selection or the map change, and travels in `Overlay::clip`, so every view draws it.

## Overlays and decals

`mallet_vmf::overlay` holds what an `info_overlay` stores and what the compilers make of
it. `Overlay` is the quad: `BasisOrigin`, the axes `BasisU` and `BasisV` its picture runs
along, `BasisNormal`, and the corners `uv0` to `uv3` measured along the axes. The corners
take the texture coordinates (`StartU`, `StartV`), (`StartU`, `EndV`), (`EndU`, `EndV`)
and (`EndU`, `StartV`).

The game only draws an overlay whose corners run clockwise seen from in front of its
faces; one wound the other way compiles into the BSP with its faces and is then left
out. Every overlay in the maps checked (769 of them, made in Hammer or decompiled) is
wound that way. Hammer gets there with U to the picture's right and V up it, so
`U x V` is the normal, the first corner at the bottom left, and `StartV` 1 with `EndV` 0
to turn the picture the right way up. `Overlay::on_surface` builds new overlays the
same, on the directions `surface_axes` gives, which follow the game's
`R_DecalComputeBasis`: upright on walls, top to the north on floors. `shows_in_game`
tests the winding, and an overlay that fails it gets no picture in Mallet either, only
its outline.

`Overlay::pieces_on` cuts the quad to one brush face. The quad is two triangles; each is
pushed along the normal onto the face, clipped by the face's polygon, and given texture
axes that are affine in world space, so a `Piece` is drawn like any face whose
coordinates run from 0 to 1. `Overlay::transformed` carries an overlay through any affine
matrix: the normal goes through the inverse transpose, the axes are squared up again, and
the corners are measured anew from the moved quad, so resizing stretches the picture. A
mirror would reverse the winding, so after one U is turned back, the corners are
numbered in reverse and `StartU` and `EndU` trade places: the picture is mirrored and
the game still draws it. Every path that moves entities calls it: `Entity::translate` for
clones, the clipboard's paste, and `Editor::selected_overlay_edits` for moves and
`Edit::Transform`, as `Edit::SetKey` edits beside the one that moves `origin`.

Texture lock takes overlays along with the brushes under them. `Editor::riding_overlay_edits`
gets the brushes each transform moves, its matrix and its `TextureLock`, and finds the
unselected overlays whose listed faces all belong to brushes moved by the same
transform; an overlay with a face on a brush left behind stays. `overlay::follow_keys`
then follows the lock: `Off` changes nothing, `Full` is `Overlay::transformed`, and
`Align` is `Overlay::carried`, which moves the origin with the matrix and turns the
overlay onto the transformed plane but keeps its corners, so it does not stretch.
Moves, `Edit::Transform` and the per-object moves of snapping and aligning all add
these edits to their batch, so they undo with the move.

`mallet_vmf::sides` keeps face lists right across copies. `CopiedSides` records each
copied face's new id as brushes are renumbered, and `remap_entity` rewrites `sides` (and
`sides2` of `info_overlay_transition`) through it. A paste drops listed faces that were
not copied, since the map pasted into may use those ids for other faces; a clone keeps
them, since they are still the same faces.

`Scene::build` ends with `add_decals`, which gives every overlay and decal a second mesh
beside its helper model: the pieces as `SceneFace`s marked `decal`, their outline as
edges, and a thin slab around the quad for picking. An overlay is cut to the faces its
`sides` list, found through an index of side ids built only when the map has overlays. A
decal lands on the nearest brush face within eight units that is not a tool face, takes
its size from `Editor::decal_sizes` (the texture's size times `$decalscale`, looked up
once per material) and runs on over faces in the same plane. Meshes marked `decal` are
skipped by `pick_face` and `surface_hit`, so the texture tool and the tools that place
things on surfaces look through them.

The renderer treats `decal` faces as blended whatever their material says: they join the
translucent faces, sorted back to front, with texture coordinates taken as they are
instead of divided by the texture's size, lifted a sixteenth of a unit off their face.
The blended pipelines also carry a small depth bias, which scales with depth precision,
so decals do not flicker far away. They take no luxels and cast no shadows in the
lighting preview. Materials with the `DecalModulate` shader multiply what is under them
in the game; `modulate_to_blend` rewrites their pixels into a dark tint or white haze of
matching alpha, which a plain blend can show.

`crates/mallet/src/decals.rs` holds the tools. `create_overlay` builds the quad on the
clicked face at the texture's size and lists the faces in that plane which a piece
actually lands on. `move_overlay_corner` is the corner drag: the mouse ray's hit on the
overlay's plane, snapped to the grid along the overlay's own axes, with the neighbouring
corners following unless Shift asked for a free corner; each step goes through
`History::apply_grouped`, so a drag is one undo step.

## Vertex editing

`mallet_vmf::vertex::BrushMesh` holds a brush as welded corners and faces that list them,
each face remembering the side its material comes from. `build` turns a mesh back into a
brush: corners on top of each other merge, faces bent out of their plane become a fan of
triangles (the first fan that stays convex), faces left in one plane become one side again,
and each side's plane is written through three of its real corners so grid-aligned brushes
stay exact. The result is refused unless every corner is behind every face and VBSP-style
clipping of the new planes gives back the same corners. Hammer++'s `vertices_plus` is
dropped from changed sides.

`crates/mallet/src/vertex_tool.rs` keeps a mesh per selected brush, read again whenever the
brush changes outside the tool, so split faces live only in the tool until a corner on the
split moves. Selected corners and edges are remembered by position, which survives
rebuilds. A drag rebuilds from the meshes it started with on every grid step and applies
`Edit::ReplaceSolids` through `History::apply_grouped`, so the brush updates live and the
whole drag undoes at once; a refused shape is drawn red and leaves the last good one in
place. `app/vertex_tool.rs` does the picking, the 3D drag planes and the merge question.

## Compiling

`compile` turns a `Preset` (a list of `Step`s with options from a per-tool catalog,
extra arguments and, for custom steps, a program, arguments and working folder) into
`PlannedStep`s: a program to run, a program to launch, or a file to copy. Variables such
as `{bsp}` are expanded here, so the plan is plain data that tests check without running
anything. Presets live in `%APPDATA%\Mallet\compile_presets.json`.

`CompileJob` runs a plan on its own thread. Each program runs without a console window
(below normal priority when the option is on), its stdout and stderr are read on two more
threads, and lines go into a shared `JobState` that the window, the status bar and MCP
read. Progress comes from the compilers' `0...1...2` counters. Lines that read as errors
or warnings become `Problem`s with the brush id or position they name. After VBSP, a new
`.lin` pointfile (or "leaked" in the output) marks a leak: the compile stops and the
editor draws the line as an ownerless scene mesh, which every view shows but nothing can
select. Cancelling kills the running program and skips the rest.

`Config` is one serde struct with defaults for every field, so older `config.json` files
load. Tool paths left empty are found next to the game (`bin/win64` first) or, for
SourceCubemap, on `PATH` and in the usual install folders.

## Drawing

Each of the four viewport slots shows a `ViewMode`: 3D Textured, 3D Wireframe, 3D Lighting
Preview, or the 2D Top, Front or Side view, switched by clicking the slot's label. 3D slots share the one
camera; 2D slots showing the same view share its pan and zoom. MCP tools name viewports
`camera`, `top`, `front` and `side` and act on the first slot showing that view.

All four viewports are drawn by `Renderer3D` with wgpu, using the device eframe already
created, into their own offscreen images. The window shows those images through egui, and
MCP screenshots read the same images back, so they match and work while the window is
minimized. egui only draws the light overlays on top: labels, handle squares, the marquee
and the net graph. The CPU drawing code behind the `Canvas` trait (egui painter or
tiny-skia image) remains as the fallback when no GPU renderer exists yet.

Geometry is cached on the GPU and rebuilt only when the scene's `geometry_generation`
changes, never for selection changes or camera moves:

- The camera's faces are fanned into triangles with Source's texture coordinates
  (`point · axis / scale + offset`, divided by the texture size) and their normal, shaded
  by direction in the shader like Hammer's textured mode, and grouped into chunks. Each frame, chunks whose
  bounds are outside the view frustum are skipped. Faces are one-sided, wound
  counter-clockwise from outside with back faces culled, so brushes look solid from
  outside and open from inside, as in Hammer.
- Materials carry their alpha mode from the VMT: `$alphatest` faces are cut out in the
  shader at `$alphatestreference`, and `$translucent` faces (or `$alpha` below 1) are kept
  out of the chunks, sorted back to front each frame and blended without writing depth.
- Props are instanced: each model is uploaded once, and every frame the props whose
  bounds pass frustum culling are written into one instance buffer of model matrices,
  grouped by model. Opaque meshes draw after the brushes, blended ones after the sorted
  translucent faces. Selected props are tinted through the instance colour, and a move or
  resize drag multiplies its transform into their matrices. In the scene a prop is an
  entity whose box is its model's rotated bounds, so picking, selection and the 2D views
  treat it like any other entity.
- The 2D views share one buffer of every brush edge in world space; each view only sets an
  orthographic matrix, so panning and zooming cost almost nothing.
- Each slot remembers a fingerprint of what its image was drawn from (camera or view,
  scene generations, overlay, size, texture uploads). When nothing changed, the image is
  shown again without drawing.
- Textures stream in: new materials load on background threads while faces show a grey
  stand-in, so opening a large map does not stall. Memory stays level however many
  materials a map has: the threads hand over one texture at a time and pause while a few
  wait to be uploaded, a frame uploads a bounded amount, and what has arrived is swapped
  into the picture a few times a second, rebuilding only the chunks that use it. A texture
  the graphics card refuses, as when its memory is full, shows as missing. MCP screenshots
  wait for the loads to finish.
- The camera has a draw distance (default 8192 units) that sets the projection's far
  plane, so frustum culling also drops everything beyond it. Chunks are 1024 units.
- Point entities whose class has `iconsprite()` show that icon facing the camera in
  place of their box. Classes with `sprite()`, such as env_sprite, show their own sprite
  as the game draws it, rebuilt every frame in `render3d::sprites`, which follows the
  Source SDK's `C_SpriteRenderer` and `Sprite_DX9`: the material's `$spriteorientation`
  and `$spriteorigin`, the render mode's blend and depth test, the client's glow fade,
  screen size and visibility test (a line to the `GlowProxySize` sphere through drawn
  brushes and props), render effects, and animation frames loaded from the VTF. `Sprite`
  materials read, tint and blend in gamma space by default (`$nosrgb`), so they are drawn
  in their own pass through a view of the image without the sRGB curve, where the
  graphics card has one; other shaders blend in linear light. Lines are drawn after them.
- The selection, its red outline and tool previews are small and rebuilt every frame so
  drags show live. In 3D the selection is drawn again over the cached geometry at equal
  depth, tinted red.

`get_status` reports per-frame timings and how many chunks and triangles the camera drew,
and View > Net Graph shows the same numbers in the camera view's bottom-right corner.

Brushes whose planes do not enclose a volume are skipped, as Hammer's Check Problems would
report them.

## Lighting

`mallet_vmf::light` is Source's lighting written down, with no GPU in it. `CATALOGUE` lists
every lighting entity class with its group, what it does, what the preview shows and the
keyvalues Source's FGDs give it, which the Entity tool's Lighting menu and new entities
without an FGD use. `MapLighting::read` turns a map's entities into `Light`s, plus the sky
light, fog, sky, glows and beams:

- Colours are read as VRAD's LightForString does: each 0 to 255 channel made linear with a
  2.2 gamma, times the brightness over 255, so `255 255 255 200` is 0.78 and 1 shows a
  texture at full brightness. `_lightHDR` replaces `_light` in HDR unless it is
  `-1 -1 -1 1`, and the HDR scales apply.
- Falloff follows SetLightFalloffParams: constant, linear and quadratic terms scaled so the
  light has its brightness at 100 units, or a quadratic fitted through fifty and zero
  percent distances with SolveInverseQuadraticMonotonic, capped and faded the way VRAD
  does, with `_hardfalloff` and `_distance`. Spotlights fade between their cones raised to
  the focus. Directions use SetupLightNormalFromProps (`pitch`, `angle`, `angles`, or the
  `target`), and projected textures and beams the engine's AngleVectors.
- light_dynamic is split the way C_DynamicLight splits it: its colour bytes times two to
  the `brightness`, without a gamma curve. Without a cone it is one light for brushes and
  models out to `distance`. With a cone, models get the cone and brushes get a round
  light of `spotlight_radius` where a trace along its middle meets a world brush,
  dimmed by `(1 - fraction)²`. `Receivers` says which surfaces a light reaches; brushes
  take dynamic lights at full strength whatever angle they face them at, as dlights
  do. Projected textures use the flashlight shader's `100 / d` with its fade near the
  far plane and its HDR and LDR scale, light only models when `lightworld` is off, and
  read `lightcolor` as UTIL_ColorStringToLinearFloatColor does. Appearances are the
  engine's light style patterns at ten letters a second.
- `parse_lights_rad` reads `lights.rad` with its `ldr:` and `hdr:` lines;
  `texture_light_scale` gives a face VRAD's brightness of one tile per texture repeat.

`Scene::build` reads the map's lighting, and the editor reads the game's `lights.rad` and
the map's own `.rad` when the game or map changes. Materials report what the compilers
make of them (`Surface`: sky, nodraw, or a tool face they remove) from their `%compile`
keys, and whether they are self-illuminated or unlit; the renderer keeps that with each
texture in its material uniform.

The 3D Lighting Preview is `CameraStyle::Lit`, drawn by `render3d::lit` from the same
cached chunks and props as the textured view with its own shaders (`lit.wgsl`):

- Lights are gathered from the scene with the settings applied (HDR, which kinds count,
  lights that start dark), and texture lights are made from faces whose material
  `lights.rad` lists, split into patches about 128 units apart. Suns and lights being
  dragged go first and light every pixel; the rest are sorted into a grid of up to 64
  cells a side, each listing the brightest lights that reach it, which pixels look up by
  where they are. Dragged lights move with the drag without sorting the grid again.
- Shadows are depth maps in texture arrays: one per sun covering the whole map, one per
  direction sky light is traced from (sky light is the share of those directions that see
  the sky, weighted by how much the surface faces them), and cube maps or single spot maps
  for the lights nearest and brightest around the camera, kept while they stay chosen.
  Many more lights past those get small shadow maps, tiles of a 16-bit depth atlas drawn
  at most 96 a frame; a light uses them once all its tiles are drawn, and screenshots
  draw every waiting tile first. Every layer and tile remembers what it was drawn from,
  so only changed ones are drawn again.
- Shadow maps draw from their own caster buffers, rebuilt from the cached faces when the
  geometry or textures change: opaque faces as bare positions in 512-unit chunks, one
  draw a chunk, alpha-tested faces with their textures, and props sorted by model so
  neighbours draw instanced. Tool faces, translucent faces and editor models let light
  through. Sky faces stop every light but the sun and sky light, as in VRAD.
- The 3D skybox is the room the first sky_camera stands in: the world brushes touching the
  one under the camera, and those touching them. Its faces and props are kept apart from
  the map's, each sun gets a second shadow map fitted to the skybox, and sky light there
  only goes by which way surfaces face, so the map and the skybox never shadow each other.
  The map's own sun and sky light maps then fit just the map.
- Sky faces show the skybox's six faces, mapped as the engine's `st_to_vec` table does,
  or Garry's Mod's painted sky; projected textures sample their texture in a texture
  array. Both load on worker threads.
- Self-illuminated materials glow by their alpha, texture light materials glow in their
  light's colour, and the result is exposed, clamped or tone mapped and fogged.
- Glows, spotlight beams and light helpers (reach spheres, cones, frusta and sun arrows)
  are built each frame around the camera. Sprites are drawn as in the textured view,
  fogged, exposed and scaled by `HDRColorScale`.
- With luxel detail, brush faces are lit from baked luxels. `mallet_vmf::lightmap` works
  out each face's luxel grid as VBSP does, along its texture axes at its lightmap scale,
  and the point each luxel lights, moved inside the face when it falls outside. A worker
  thread lays every lightmapped face's grid out in one atlas (coarser everywhere past four
  million luxels) with each luxel's point, and the shader finds a face's luxels from its
  face number, which every face vertex carries. Bakes render into a second light texture
  while the first is shown: sky light and sunlight over the whole atlas first, then each
  frame a batch of up to 48 lights, each with small shadow maps of its own in tiles kept
  free in the atlas, added over the faces they reach. Faces show the bake once it is
  done for what it was baked from; until then they are lit per pixel. Baked lights are
  marked so faces showing luxels skip them, while props and faces without luxels light
  them as before.

## MCP server

The editor starts an MCP server at `http://127.0.0.1:7414/mcp` (Streamable HTTP transport,
JSON responses). Mallet Options > AI agents chooses another port or turns it off;
`MALLET_MCP_PORT` overrides the port, and `0` or `off` disables it. The status bar shows the address while it runs.

Security:

- It listens on loopback only.
- Requests with a non-local `Origin` header are refused, which blocks DNS-rebinding
  attacks from web pages.
- There is no authentication, so any program on this machine can use it while Mallet runs.

Agents' own view: while Mallet Options > AI agents > "Agents work in their own view" is on
(the default), `tools::run` runs each call through `Editor::with_agent_view`
(`editor/agent.rs`). It swaps an `AgentView` into the editor for the call: the agent's
tab, camera, 2D views, viewport layout, grid, selection, face selection, overlay, tool,
clipper, vertex tool state and clipboard. Afterwards the user's state is swapped back,
selections are trimmed of objects the agent deleted, and the agent's status line and
system clipboard text are dropped. The maps and their undo history are shared, so the
agent's edits show in the window at once and Ctrl+Z can undo them.

Tabs carry lasting ids, so the agent keeps its tab while the user opens, closes or
switches tabs. In this mode `new_map` and `open_map` open a tab behind the window's,
`switch_tab` moves only the agent, `close_tab` refuses the tab the window shows,
`set_settings` refuses the `agents` section, and the tools that drive the window
(`send_input` and the `show_` tools) are refused. `get_status` reports `own_view`, the
agent's `tab` and the window's `window_tab`; `list_tabs` reports `active` (the window's)
and `agent`. A map handed over by a second copy of Mallet (`open_map` with `focus`) always
opens in the window.

Tools:

| Tool | What it does |
| --- | --- |
| `get_status` | Open map, counts, bounds, grid, camera, 2D views, undo/redo state, and what the lighting preview drew last |
| `set_game_dir` | Mount a game's content for textures and remember it |
| `new_map`, `open_map`, `save_map` | Start an empty map or load a VMF or `.mallet` map in a new tab (`focus` also brings the window to the front); save in place or to a new path, the extension picking the format |
| `export_map` | Write a copy of the map in either format without switching to it |
| `list_tabs`, `switch_tab`, `close_tab` | Work with the open map tabs; closing unsaved work needs `discard` |
| `list_entities`, `get_entity` | Browse entities, filtered by classname (`trigger_*` prefix match) |
| `set_entity_key`, `remove_entity_key` | Undoable key edits |
| `get_selection`, `select`, `clear_selection` | Read or change the selection the window shows |
| `move_selection`, `clone_selection`, `delete_selection` | Undoable selection edits; moves keep textures locked |
| `copy_selection`, `cut_selection`, `paste` | Hammer's clipboard, shared by every tab; `paste` takes Paste Special's copies, offset, rotation, naming and grouping options |
| `resize_selection` | Stretch the selection to a new bounding box, like dragging its handles |
| `transform_selection` | Scale, rotate and move the selection in one step around its centre, the pivot, the origin or a point |
| `flip_selection`, `snap_selection_to_grid`, `align_selection` | Hammer's Flip Objects, Snap Selected to Grid and Align Objects |
| `set_pivot` | Place or reset the pivot rotations turn around |
| `create_block`, `create_arch` | Box brush or one of Hammer's primitives (wedge, cylinder, spike, sphere) between two corners, like the Block tool, or an arch as the Arch Properties dialog makes it |
| `tie_to_entity`, `move_to_world` | Hammer's Tie to Entity and Move to World: make the selected brushes a brush entity, or add them to one, and put brush entities' brushes back into the world |
| `group_selection`, `ungroup_selection` | Hammer's Group and Ungroup |
| `hide_selection`, `hide_unselected`, `show_all_hidden` | Hammer's QuickHide: hide the selection or everything else, and show all again |
| `check_for_problems` | Hammer's Check for Problems, optionally fixing what it can |
| `make_hollow`, `carve` | Hammer's Make Hollow and Carve: walls of a set width in place of the selected brushes, and their shape cut out of the brushes they overlap |
| `clip_selection` | Cut the selected brushes along the plane through three points, keeping both halves, the front or the back, like the Clipping tool |
| `create_entity` | Point entity of any class at a position, with extra keyvalues, like the Entity tool |
| `create_overlay`, `create_decal` | Place an overlay or decal on a brush face, like the Overlay and Decal tools; overlays get their basis and the faces they reach filled in |
| `undo`, `redo` | Step the shared history |
| `set_grid`, `center_views` | Grid size, Snap to Grid and Show Grid, and framing |
| `set_viewport_type` | Switch a viewport slot between 3D (textured, wireframe, lighting preview) and 2D types |
| `list_lights` | The map's lights as the lighting preview reads them, the sky light, fog, sky, texture lights in use, the latest frame's lighting numbers and, optionally, the lighting entity catalogue |
| `set_camera`, `set_view_2d` | Aim the 3D camera, pan and zoom the 2D views |
| `screenshot` | PNG of one or all viewports, rendered offscreen, or of the whole window (`window`) |
| `set_entity_outputs` | Replace an entity's outputs |
| `describe_entity_class`, `list_entity_classes` | Entity classes from the FGD files |
| `set_fgds` | Read entity classes from chosen FGD files |
| `show_properties` | Open or close Object Properties on a tab |
| `get_settings`, `set_settings` | Read and change Mallet Options, with the programs found for each tool |
| `list_compile_presets`, `set_compile_preset`, `delete_compile_preset` | Compile presets, with each step's command for the open map |
| `compile_map` | Save and compile, returning at once or waiting up to `wait_seconds` |
| `compile_status`, `cancel_compile` | Steps, progress, problems, leak and output of the compile |
| `show_compile`, `show_options`, `show_about` | Open the compile window or Mallet Options on a tab, or Help > About |
| `list_recent_maps` | The maps opened or saved lately, latest first |
| `load_pointfile`, `clear_pointfile` | Show or hide a leak line |
| `list_materials` | The game's materials, filtered, or the ones the map uses with counts |
| `get_faces`, `select_faces`, `pick_face` | Brush faces: details, texture tool selection, and the face under a camera pixel, which `pick_face` can also put on an entity's side list |
| `set_faces` | Material, scale, shift, rotation, lightmap scale, alignment and justify, as one undo step |
| `replace_material` | Replace a material in the map or the selected faces |
| `show_texture_browser` | Open the texture browser with a search |
| `list_models` | The game's models, filtered, or the ones the map's entities use with counts |
| `show_model_browser` | Open the model browser with a search |
| `send_input` | Key presses, clicks and drags fed into the window as if a user made them, in a viewport or anywhere in the window (`window`, in screenshot pixels); refused while agents work in their own view |

`send_input` queues synthetic egui events that `MalletApp::raw_input_hook` feeds into the
window one frame at a time, so they take the same path as real input. The tool returns
once the window has handled them. Unlike the other tools it needs the window to be drawing,
so it fails if the window stays minimized. Use it to test tools and shortcuts end to end.

The repository's `.mcp.json` registers the server for MCP clients such as Claude Code.
Mallet has to be running for the tools to connect.

Adding a tool: describe it in `tools::definitions`, handle it in `tools::run_checked`, and
make any map change through `Editor::apply_edit` so it stays undoable.

## Roadmap

[ROADMAP.md](ROADMAP.md) lists the Hammer and Hammer++ features still to come, grouped
into milestones.
