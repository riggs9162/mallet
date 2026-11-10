<p align="center">
  <img src="branding/mallet.svg" width="128" alt="Mallet's icon: a white mallet on a black circle">
</p>

<h1 align="center">Mallet</h1>

<p align="center">A Hammer-style level editor for Source engine VMF maps, written in Rust.</p>

Mallet opens and saves VMF files without losing anything, has its own faster `.mallet`
map format, and shows maps in Hammer's four-viewport layout, with textured brushes and prop models in 3D. It has Hammer's
Selection, Block and Entity tools with scale, rotate and skew handles and a Hammer++-style
3D transform gizmo, Overlay and Decal tools that show posters, stains and graffiti where the
game will, Object Properties driven by the game's FGD, map tabs,
a CompilePal-style compile window with presets, SourceCubemap and leak lines,
Hammer's texture tool with a texture browser that loads every mounted game's materials, and
a Hammer++-style 3D Lighting Preview that lights the map with every Source light entity. A
built-in MCP server lets AI agents inspect and edit maps, compile them and take
screenshots. It is a work in progress: displacements are next.

## Running

```bash
cargo run --release -p mallet -- path/to/map.vmf
```

On Linux you need the usual wgpu system libraries first:
`libxkbcommon`, `libx11`-and-friends (X11 development packages), `libwayland`,
and the Vulkan loader — on Debian/Ubuntu:
`sudo apt install libxkbcommon-dev libwayland-dev libx11-dev libvulkan-dev`.
Both X11 and Wayland are supported.

The 3D view shows textures from a Source game. On first start Mallet looks for Garry's
Mod, Half-Life 2, the Source SDK Base 2013 games or Counter-Strike: Source in your Steam
libraries; choose another with Tools > Game Folder... (the folder that contains
`gameinfo.txt`). Steam itself is found at `C:\Program Files (x86)\Steam` on Windows, or
`~/.steam/steam` (then `~/.local/share/Steam`) on Linux, together with every library in
its `libraryfolders.vdf`. The choice is saved in `%APPDATA%\Mallet\config.json` on
Windows and `~/.config/Mallet/config.json` on Linux.

Compiling uses the game's own tools: on Windows the usual `bin/win64` exes (and the
Hammer++ builds). On Linux, Source games ship few or none of these (GMod and the
Source SDK bases have none; CSS only `bspzip`), so install the Tools++ compilers that
accompany Hammer++: their Linux builds are native binaries, and Mallet finds
`vbsp++`, `vvis++` and `vrad++` on PATH (typically `~/.local/bin`) or in the game's
`bin` folders automatically. GMod's entity definitions (`*.fgd`) are not loose on
disk; fetch `base.fgd`, `halflife2.fgd` and `garrysmod.fgd` from the Facepunch/garrysmod
repository into the `garrysmod` folder, or list them in Mallet Options.

Prop entities with a `model` key show their model in the 3D view, textured with the
default skin and placed by `origin`, `angles` and `modelscale`. Models the game does not
have stay as entity boxes; `get_status` lists why they failed.

Without a path it starts with a new, empty map, like Hammer. You can also open a map with
File > Open or by dropping a `.vmf` or `.mallet` file onto the window.

## The .mallet format

Maps can be saved as VMF, which every Source tool reads, or as `.mallet`, Mallet's own
binary format. A `.mallet` file holds the same map, so nothing is lost either way, but
stores face planes and texture axes as numbers rather than text and every repeated string
once. On a 28.5 MB VMF it is 2.8 MB, opens in about a third of the time and saves about
four times faster. Save As picks the format by the extension, File > Export a Copy writes
the map in either format without switching to the copy, and Mallet tells the formats
apart by their contents, so a renamed file still opens. The compilers only read VMF, so
compiling a `.mallet` map first writes a VMF copy to a `mallet_compile` folder beside it
(Mallet Options > Compiling can name another); the BSP and logs end up there too. The layout is described in
[crates/mallet-format](crates/mallet-format/src/lib.rs).

Edit > Mallet Options > Files chooses the format Save As offers first for new maps, and
sets up Windows for your account: `.mallet` files open in Mallet with its icon, and VMF
files can list Mallet under "Open with" while Hammer stays their default. (This file-type
setup is Windows-only; the section hides itself elsewhere.) Opening a map from Explorer
while Mallet runs adds it as a tab in that window instead of starting a second Mallet, as
Hammer does.

## Layout

The window follows Hammer++: menus with an icon toolbar underneath, browser-style map
tabs, the tool palette on the left, the four viewports, and an object panel on the right
with the selection mode, current texture and selection details. Tools that are not built
yet are shown greyed out. Each tab is its own map with its own views, camera, selection
and undo history; a dot marks unsaved changes.

## Controls

These follow Hammer++'s default keybindings.

| Keys | Action |
| --- | --- |
| Ctrl+N / Ctrl+O / Ctrl+S | New map tab / open in a tab / save (a new map asks where to save) |
| File > Recent Maps | The maps opened or saved last, the latest first (20 unless Mallet Options > Files says otherwise) |
| Ctrl+F4 / Ctrl+Tab / Ctrl+Shift+Tab | Close tab (asks about unsaved changes) / next tab / previous tab |
| Ctrl+Z / Ctrl+Y | Undo / redo |
| Ctrl+X / Ctrl+C / Ctrl+V | Cut / copy / paste the selection; Shift+Delete, Ctrl+Insert and Shift+Insert work too. Paste centres the objects under the cursor in a 2D view, snapped to the grid, or in the 2D view used last |
| Ctrl+Shift+V | Paste Special: several copies, each moved and turned from the one before, renamed or grouped |
| Shift+S / Shift+B / Shift+E / Shift+A | Selection tool / Block tool / Entity tool / Texture application |
| Shift+C | Camera tool: drag with the left button in a 3D view to turn the camera where you look, and with the right button to slide it across the view. In a 2D view, left-click zooms in and right-click zooms out, around the cursor |
| Alt+Enter or double-click | Object Properties for the selection: keyvalues, outputs, inputs and flags from the game's FGD |
| F9 / Shift+F9 | Open the compile window / compile again with the preset used last |
| Click | In 2D, click a brush's centre × or its outline to select it; in 3D, click it (or aim the crosshair at it in mouselook). Props are hit where their model is, so clicks through gaps reach what is behind; only when nothing else is hit does a prop's box count. Entities are selected whole; Ctrl+click toggles |
| Drag on empty space | Select everything fully inside the rectangle (Ctrl adds) |
| Drag the selection | Move it so its corner snaps to the grid, with texture lock; hold Shift on release to clone |
| Drag a white handle | Resize the selection or the pending block from that edge or corner |
| Click a selected object (2D) | Switch its handles between scale, rotate (round corner handles) and skew (edge handles), as in Hammer |
| Rotate handles | Drag a corner to turn the selection around the yellow pivot, which you can drag elsewhere; hold Shift to snap to 15° |
| Skew handles | Drag an edge along itself to shear the selection; the opposite edge stays put |
| Arrow keys over a 2D view | Nudge the selection one grid step along the view's axes (Ctrl: one unit) |
| Ctrl+M | Transform dialog: rotate, scale or move by typed amounts around the selection's centre, the pivot or the world origin |
| Ctrl+B / Ctrl+Shift+B | Snap the selection to the grid / snap each selected object on its own |
| Ctrl+L / Ctrl+I | Flip the selection horizontally / vertically in the 2D view used last |
| Shift+L | Toggle texture lock |
| Entity tool: click in 3D | Create an entity of the class chosen under New entity on the brush face under the cursor |
| Entity tool: click in 2D views, then Enter | Place the new entity's marker (each view sets its two axes, snapped to the grid), then create it |
| Drag with the Block tool, then Enter | Create a box brush; drag inside it to move it, drag handles in any view to resize it (its depth starts from the last block) |
| Shift+V | Vertex tool for the selected brushes: drag a white corner or yellow edge midpoint in any view to move it on the grid (in 3D across the floor, or up and down when Shift is held as the drag starts). Click or drag a rectangle to select corners, Ctrl to add; every selected corner moves together. The brush changes as you drag and each drag is one undo step |
| Ctrl+F | With the Vertex tool, split a face between two selected corners; the halves bend apart once a corner on the split moves |
| Shift+O | Overlay tool: click a brush face in the 3D view to place an overlay with the current texture there, at the texture's own shape. Click an overlay to select it, then drag one of its four corner handles to resize it on the grid (hold Shift as the drag starts to move just that corner). Ctrl+click a face to add it to the selected overlay, or take it off |
| Shift+D | Decal tool: click a brush face in the 3D view to place a decal with the current texture there; click a decal to select it |
| Delete / Shift+Q or Escape / Ctrl+Shift+A | Delete selection / deselect or cancel the block / select all |
| Numpad `-` / Numpad `+` | Smaller / larger grid |
| Ctrl+E / Ctrl+R | Centre 2D views / 3D view on the map |
| Mouse wheel | Zoom a 2D view around the cursor, or move the camera forward |
| Middle-drag or Space+drag | Pan a 2D view |
| Z | Toggle mouselook in the 3D view (Escape leaves it); the cursor stays in the view and returns afterwards |
| Click a viewport's label | Switch that viewport between 3D Textured, 3D Wireframe, 3D Lighting Preview and the 2D Top, Front and Side views |
| W A S D, arrow keys | Move the 3D camera; the arrow keys strafe left, right, up and down. A key pressed with Ctrl or Alt, such as the S of Ctrl+S, does not move it until released |
| Ctrl (in mouselook) | Crouch: fly at a third of the camera speed. Ctrl with W A S D moves instead of saving |

View > Net Graph toggles a frame timing overlay in the 3D view's bottom-right corner:
frame rate, CPU time per frame, how many chunks and props culling left to draw, how many viewports
reused their last image, and how many textures are still loading. View > 3D draw distance
sets how far the 3D view draws, like Hammer's back clipping plane. For the best
speed on large maps, run the release build (`cargo build --release -p mallet`).

## Overlays and decals

Overlays (`info_overlay`) and decals (`infodecal`) are drawn in the 3D views the way the
game shows them. An overlay's picture lies on the brush faces it lists and is cut to their
shape; a decal's lies on the wall, floor or ceiling it sits against, at the size its
texture gives it. Both blend by the texture's alpha, show as an outline in the 2D and
wireframe views, and can be clicked like any other object, while the texture tool looks
through them to the face behind.

The Overlay tool (Shift+O) places an overlay on the face you click, upright on walls and
with its top to the north on floors, and lists that face and the faces beside it in the
same wall that the picture reaches. Pick the picture first as the current texture, from
the texture browser. Moving, rotating, flipping, resizing, cloning and pasting an overlay
all carry its picture along. An overlay only shows on the faces it lists, as in Hammer, so
after moving one onto another brush, Ctrl+click that face with the Overlay tool to add it.
Copying an overlay together with its wall keeps it on the copied wall.

Texture lock covers overlays too. While it is on, an overlay rides along with the brushes
it lies on when you move, rotate, flip, resize or snap them, without being selected
itself. With texture scale lock it stretches with a resized brush; without, it keeps its
size and only moves with the spot it was on. With texture lock off it stays where it is,
like the textures. An overlay that also lies on a brush you leave behind stays put.

The Decal tool (Shift+D) places a decal on the face you click. The game decides a decal's
size from its texture and always draws it upright, so a decal has no handles; use an
overlay when you need a set size or angle.

## Transforming

Besides moving and resizing, the selection can be rotated, skewed and mirrored, with one
undo step per change. Rotating or mirroring an entity turns its `angles` with it;
displacements turn with their brushes.

In the 3D view the selection tool shows a transform gizmo, as in Hammer++. In its move
mode the arrows move along one axis and the squares across a plane, snapping the
selection's corner to the grid; in rotate mode the rings turn the selection around the
pivot (Shift snaps to 15°); in scale mode the cubes stretch one axis by whole grid steps
and the centre cube scales evenly. Click the gizmo's centre, or use the buttons under
Transform in the side panel, to switch modes. Hold Shift when letting go of a move to
clone. View > 3D Transform Gizmo or the toolbar button hides it.

Tools > Align Objects lines selected objects up with an edge of the selection in the 2D
view used last. The toolbar's padlock is Hammer's texture lock: textures move and turn
with their brushes. Texture scale lock, beside it, also stretches them when brushes are
scaled; without it they keep their scale and stay pinned to the brush's corner. Mallet
Options > Editing holds both locks, the rotation snap step, arrow-key nudging and the
gizmo.

## Copying and pasting

The clipboard belongs to no tab, so objects copied in one map paste into any other, with
fresh ids. Copying also puts them on the system clipboard as VMF text, which another
Mallet window can paste. Each paste is one undo step and selects what it pasted.

Edit > Paste Special works as Hammer's: it pastes a number of copies, each moved by the
offset and turned by the rotation more than the one before it, starting from the cursor or
from where the objects were copied. Copies turn around their centre or, as in Hammer++,
around the pivot placed before copying, which makes spiral stairs a single paste. Entity
names can get a prefix or suffix, or be numbered so no other entity in the map has them;
outputs between the pasted entities follow the new names. Group each copy puts every copy
in a group of its own in the saved map.

## Lighting

Click the camera viewport's label and pick 3D Lighting Preview to see the map lit the
way the game lights it, as in Hammer++. The preview reads every light entity with VRAD's
own rules: `_light` colours and brightness (`_lightHDR` and the HDR scales when compiling
with HDR), constant, linear and quadratic falloff, fifty and zero percent distances with
hard falloff, `_distance`, spotlight cones and focus, `pitch`, `angles` and aim targets.

| Light | In the preview |
| --- | --- |
| `light`, `light_spot` | Point and spot lights with VRAD's falloff; they cast shadows |
| `light_environment` | Sunlight that shines in through sky faces, with shadows, and sky light that reaches what can see the sky |
| `light_directional` | Light from one direction with shadows, like the sun without sky light |
| `light_dynamic` | Without a cone, lights everything out to its maximum distance. With one, lights models in the cone and brushes in a round spot where its middle lands, as the game does. No shadows, as in the game |
| `env_projectedtexture` | Projects its texture through its field of view out to its far distance, with shadows |
| Texture lights | Faces whose material `lights.rad` lists give off light, split into patches like VRAD's, and glow; the map's own `.rad` file beside it counts too |
| `$selfillum` materials | Glow where their texture's alpha says |
| `point_spotlight`, `beam_spotlight`, `env_sprite`, `env_lightglow`, `env_sun` | Beams, halos and glows, and the sun's glow in the sky |
| `env_fog_controller`, `env_skypaint` | Fog, and Garry's Mod's painted sky |

Sky faces show the map's skybox, and tool textures that are invisible in the game
(nodraw, clip, trigger, hint, skip and the like) are hidden. Lights that start dark are
left off, and flickering or pulsing appearances can play. Shadows come from the sun,
from sky light and from the lights nearest and brightest around the camera, and many
more lights past those get small shadow maps, drawn a few each frame; only lights past
those shine through walls. Lights stop at sky faces, as VRAD stops them, while sunlight
comes in through them. A 3D skybox gets its own sun shadows, so it and the map never
shadow each other. Bounced light is not traced, so a "fake bounce" adds a little light
around each light instead.

Brush faces are lit per luxel by default, the way the game's lightmaps light them: once
for every luxel of the face's lightmap scale, so a face at scale 16 gets one light value
every 16 units, blended between neighbours as the engine blends them. Every light casts
shadows into the luxels, which are baked in the background a batch of lights at a time
and kept while the camera moves; after an edit, faces are lit per pixel until the new
bake is ready, which takes a few seconds on a big map. Projected textures, lights being
dragged and, while appearances play, flickering lights stay live, and props are always
lit per pixel. Set Detail to Every pixel to light faces live and sharper instead.

The sun button beside the viewport's label holds the settings used most: detail,
exposure, tone mapping, fill light, fake bounce, the sky, fog, glows, light helpers and
which kinds of light count. Mallet Options > Lighting preview has all of them. Selected lights show how
far they reach, their cones or the sun's direction, and a light being dragged lights the
map from where it is.

The side panel's Lighting menu, beside Common classes, lists every Source lighting entity
for the Entity tool, grouped into baked lights, dynamic lights, glows, beams and sky, fog,
tone mapping and shadow controls, and lighting helpers, each with what it does. New
lighting entities get Source's default settings even without the game's FGD.

## Texturing

Shift+A opens the Face Edit Sheet, laid out like Hammer++'s. In the 3D view, click a face
to select it and pick up its texture (Lift+Select, as in Hammer; the Mode menu has Lift,
Select, Apply and Align to View too), Ctrl+click to add faces and Shift+click for the whole
brush. Switching to the texture tool with brushes selected lets go of the brushes and
selects all of their faces instead. Selecting brushes while the sheet is open, by clicking in
a 2D view, dragging a box or Select All, selects all of their faces, and Ctrl adds or removes a
brush's faces. With brushes
selected in any tool, picking a texture in the texture browser puts it on all their faces. Right-click puts the current texture on a face; Alt+right-click also puts the picked
up alignment on it. Scale, shift, luxel scale, rotation and the UV vectors apply as you drag,
type, scroll or click their arrows, and a drag is one undo step. The ½ and ×2 buttons halve
or double the scale of one axis while keeping the texture pinned at the face's corner, and
the dice buttons shift each face randomly, for stairs. Justify (Fit, L, R, T, B, C, and
Fit width and Fit height, which keep the texture's proportions, with Treat as one), World and Face alignment, Apply Nodraw, smoothing groups, Hide mask and
Replace Textures work as in Hammer.

Hotspot textures work as in Hammer++. A hotspot texture is a trim sheet whose VMT names a
`.rect` file of rectangles with `%rectanglemap`; the sheet says so under the current texture.
Apply it, select the faces and press Fit: each face gets the rectangle that suits its size
best, turned a quarter turn where the rectangle allows it (`rotate`), and flipped at random
where it allows that (`reflect`). Among equally good rectangles one is picked at random, so
pressing Fit again gives another look. Hold Alt while pressing Fit to use only the rectangles
marked `alt`.

The texture browser lists every material the game mounts, including the games in Garry's
Mod's `cfg/mount.cfg` and every addon. The sidebar narrows it to particular games, addons
or folders, each with a count; the bar below has Hammer's filter, keywords, "only used
textures", Mark, Replace, Open Source and the Opaque, Translucent, Selfillum, EnvMask and
error filters. The "per row" slider sets how many textures fit across, and they resize with
the window. Thumbnails load in the background while you scroll. Mounted games can be
turned off one by one in Edit > Mallet Options > Game.

## Compiling

The hammer icon on the toolbar (or F9) opens the compile window, which works like
CompilePal. A preset is a list of steps: VBSP, VVIS, VRAD, SourceCubemap, BSPZip repack,
VBSPInfo, copying the BSP to the game, starting the game, or any program you add. Tick
each tool's options, add extra arguments, reorder steps, and keep as many presets as
you like; Fast, Normal, Final, Entities only and Cubemaps come built in. Custom steps
can use `{map}`, `{vmf}`, `{bsp}`, `{mapdir}`, `{gamedir}`, `{gamemaps}` and `{bindir}`.

Every option the game's own compile tools and the Hammer++ builds (`vbspplusplus`,
`vvisplusplus`, `vradplusplus`, `bspzipplusplus`) accept is listed, with a search box.
Mallet sees which build a step runs and hides the options that build does not
understand; tick "Show Hammer++ options" (or "Show game tools options") to see them all.

While it runs, the window shows each step's progress and time and the live output.
Errors and warnings are collected on the Problems tab, with buttons that select the brush
or show the place a message names. When the map leaks, the leak line is drawn in red in
every view and the 3D camera moves to it. Logs are kept in `%APPDATA%\Mallet\logs`, and
Shift+F9 compiles again with the last preset.

[SourceCubemap](https://github.com/riggs9162/SourceCubemap) builds cubemaps without
starting the game; Mallet runs it in place on the compiled BSP so the cubemaps keep the
map's name.

## Mallet Options

Edit > Mallet Options... (or the gear on the toolbar) holds every setting, each applied
as you change it:

- **Game**: the game folder, the Steam folder searched for an installed game, mounted
  games and FGD files.
- **Compile tools**: VBSP, VVIS, VRAD, VBSPInfo, BSPZip, SourceCubemap and the game, each
  found automatically, with a menu of every copy found, including 64-bit and Hammer++
  builds.
- **Compiling**: how compiles run, the folder `.mallet` maps compile in and how much
  compiler output the compile window keeps.
- **Viewports**: draw distance, field of view, camera speed, crouch speed and acceleration,
  how far the mouse wheel moves the camera, mouselook sensitivity and smoothing,
  how far the wheel zooms the 2D views, how close a click must be to select a brush, the
  grid of new maps, how dense the grid may get and how often grid lines are highlighted.
- **Lighting preview**: HDR or LDR light values; the sun, sky light, dynamic lights, texture
  lights, self-illumination, lights that start dark and appearances; sun and light
  shadows with their sizes, softness, how many lights cast them, how many more get small
  shadow maps and their size, and how many directions sky light is traced from; whether
  faces are lit per luxel or every pixel; exposure, tone mapping, fill light, fake bounce,
  the sky, fog, glows, tool textures, entity boxes and light helpers; and how far lights
  reach and how many may light one spot.
- **Colours**: the background, grid, axis, selection, tool, handle and texture-face
  colours of the views, Hammer's by default.
- **Editing**: the texture new brushes get, the Entity tool's class, how many recent
  textures are listed, texture lock and texture scale lock, the rotation snap step,
  arrow-key nudging and the 3D gizmo.
- **Files**: the format new maps are saved in, how many recent maps are kept, and the
  Windows file types.
- **Texture browser**: scrolling, thumbnail size, loading threads and how many
  thumbnails stay in memory.
- **Interface**: the size of menus, panels and text, and vertical sync.
- **Keyboard** and **AI agents**: the key bindings, and whether the MCP server runs and on
  which port.

Settings are saved in `%APPDATA%\Mallet\config.json` on Windows and
`~/.config/Mallet/config.json` on Linux (`$XDG_CONFIG_HOME` is honoured).

If an error ever closes Mallet, it writes what went wrong to `%APPDATA%\Mallet\crash.log`
on Windows or `~/.config/Mallet/crash.log` on Linux. Attach that file when you report the
problem.

Where you leave the main window and every panel (Face Edit, the texture browser, Compile,
Options and the rest), and their sizes, are remembered between runs in
`%APPDATA%\Mallet\data` on Windows or `~/.local/share/mallet` on Linux. Set
`MALLET_PROFILE` to keep a separate layout, as test copies do.

## AI agents

While Mallet runs, its MCP server listens on `http://127.0.0.1:7414/mcp`; Mallet Options >
AI agents can change the port or turn it off. The included
`.mcp.json` registers it for clients such as Claude Code. See
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the tool list and design notes.

Agents work in their own view by default, so you can keep looking around while one edits.
An agent has its own tab, camera, 2D views, grid, selection and tools; its changes to the
map show in your viewports straight away, but your camera, selection and tab stay where you
left them. A pink camera marker in your viewports shows where it is looking while it
works on the map you have open. It cannot click or type into the window or open panels there. Turn off "Agents
work in their own view" in Mallet Options > AI agents to let agents drive the window
itself, for example to test tools with `send_input`.
