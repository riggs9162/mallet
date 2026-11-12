# Mallet roadmap

What Mallet still needs to match Valve's Hammer 4.x and Hammer++, grouped into milestones
in the order they are planned. Each item says where the feature comes from:

- **Hammer**: stock Hammer, as documented on the
  [Valve Developer Community](https://developer.valvesoftware.com/wiki/Category:Hammer)
  (menus, dialogs, toolbars, tools and the hotkey reference).
- **H++**: added or changed by [Hammer++](https://ficool2.github.io/HammerPlusPlus-Website/features.html).

Tick an item off when it ships, and move it to [Already in Mallet](#already-in-mallet) once
its milestone is done. Last compared against both sources on 2026-11-09.

## Milestone 1: everyday brush editing

The tools a mapper reaches for every few minutes.

### Clipboard

- [x] Cut, Copy and Paste (Ctrl+X, Ctrl+C, Ctrl+V, plus Ctrl+Insert, Shift+Insert and Shift+Del), pasting at the cursor. Hammer
- [x] Copying between map tabs, with new ids and entity names kept or made unique. Hammer
- [x] Paste Special (Ctrl+Shift+V): copies, offset, rotation, name prefix or suffix, group the copies. Hammer
- [x] Paste Special that rotates around the pivot. H++

### Clipping tool (Shift+X)

- [x] Two-point clipping in the 2D views, keeping the front, the back or both halves (Shift+X cycles). Hammer
- [x] Click away from the clip points to switch the kept side, drag there for a new line, Esc to drop it. Hammer
- [x] Ctrl+drag a clip point to move the whole line, and O to show the size of the kept pieces. Hammer
- [x] Three-point clipping, switched with C. H++
- [x] Clip plane, handles and red highlight of the brushes it cuts in the 3D view. H++
- [x] Clipping brushes that belong to entities. H++
- [x] Overlays on clipped faces keep their faces. H++

### Vertex tool (Shift+V)

- [x] Drag vertices and edge midpoints in the 2D and 3D views, with grid snapping. Hammer
- [x] Arrow keys nudge the selected vertices in the 2D views. Hammer
- [x] Splitting a face by selecting two vertices (Ctrl+F). Hammer
- [x] Merging vertices that are dragged onto each other, with a prompt. Hammer
- [x] Live update of the brush and undo inside the tool. H++
- [x] Red highlight on faces of invalid shapes. H++

### Brush operations

- [x] Carve (Ctrl+Shift+C). Hammer
- [x] Make Hollow, with a wall thickness. Hammer
- [ ] Merge brushes that form a convex shape (Ctrl+Shift+M). H++
- [ ] Polygon tool: draw a convex brush point by point in a 2D view. H++
- [x] Primitives for the Block tool: block, wedge, cylinder, spike, sphere and arch, with the Arch Properties dialog. Hammer
- [ ] Torus primitive with its properties dialog. Hammer
- [ ] Center Origins. Hammer
- [ ] Move objects to the nearest floor or ceiling, optionally aligned to the surface normal. H++

### Entities and groups

- [x] Tie to Entity (Ctrl+T), adding brushes to an existing brush entity when asked. Hammer
- [x] Move to World (Ctrl+Shift+W). Hammer
- [x] Group and Ungroup (Ctrl+G, Ctrl+U). Hammer
- [x] Ignore Groups (Ctrl+W), and the Objects and Solids selection modes in the side panel. Hammer

### Grid, views and navigation

- [x] Show Grid (Shift+R) and Snap to Grid (Shift+W) toggles; Alt held while dragging skips snapping. Hammer
- [x] `[` and `]` (and Alt+A, Alt+S) for the grid size. Hammer
- [ ] Microgrid sizes 0.125, 0.25 and 0.5, and a custom grid size (View > Set Grid Size). H++
- [ ] 3D grid on brush faces (P). Hammer, overhauled in H++
- [ ] Shift+arrow keys clone and nudge the selection. Hammer
- [x] Maximise and restore a viewport (Shift+Z). Hammer
- [ ] Resizable viewports and Autosize 4 Views (Ctrl+A). Hammer
- [x] F2 to F5 switch the active viewport's type; Tab cycles 2D views. Hammer
- [x] Preset zoom levels 1 to 9 in 2D, and Ctrl with the mouse wheel zooming every 2D view. Hammer
- [x] Center Views on Selection (Ctrl+E) and Center 3D View on Selection (Ctrl+Shift+E). Hammer
- [x] 3D view clipping plane on 1 and 2. Hammer
- [x] Go to Coordinates and Go to Brush Number (Ctrl+Shift+G). Hammer
- [x] Camera tool (Shift+C): several cameras, switched with Page Up and Page Down. Hammer
- [x] Magnify tool (Shift+G). Hammer
- [x] Drag select: with the Selection tool, holding Shift and the left button selects everything the cursor or crosshair passes over, in 2D and 3D (Hammer++ holds Ctrl). H++
- [ ] Selection size information and selected brush numbers in the status bar. Hammer
- [ ] Local or global gizmo space (Ctrl+J), gizmo axis locks, and X cycling box, gizmo and none. H++

## Milestone 2: displacements

Mallet draws displacements and has Hammer's main displacement tools; these items are left.

- [x] Draw displacement surfaces in the 3D and 2D views, with alpha blending of two-texture materials. Hammer
- [x] Create and Destroy with power 2, 3 or 4 from the Face Edit sheet. Hammer
- [x] Select displacement faces in the texture tool, with the Displacement tab. Hammer
- [x] Paint Geometry: raise, lower, smooth and set, along the face normal or an axis, soft or hard edges, distance, radius, autosew. Hammer
- [x] Paint Alpha for blended materials, with Invert Alpha. Hammer
- [x] Sculpt mode (push and pull, carve, blend). Hammer
- [x] Sew, Subdivide and Noise. Hammer
- [x] Displacement Edge Collapse. Hammer
- [x] Elevation and Scale attributes. Hammer
- [ ] Collision masks: no physics, no hull, no ray collision, and showing walkable surfaces. Hammer
- [ ] Toggle displacements on and off and a displacement selection mask on the toolbar. Hammer
- [ ] Discard power 0 and 1 displacements on load, with a warning. H++

## Milestone 3: map organisation

- [x] QuickHide: H, Ctrl+H and U. Hammer
- [ ] Convert QuickHide Objects to VisGroup. Hammer
- [ ] VisGroups panel in the side panel (it shows "Coming soon" today), with show, hide and auto VisGroups (world, entities, displacements, tool brushes). Hammer
- [ ] Edit VisGroups and New VisGroup dialogs, and Move Selection to VisGroup. Hammer
- [ ] VisGroup tab of Object Properties, for brushes and entities. Hammer
- [ ] "Hidden objects too" in Replace Textures, which is greyed out until hiding exists. Hammer
- [ ] Cordon tool: toggle cordon, edit cordon bounds, compile only inside the cordon. Hammer
- [ ] Several named cordons at once that stock VBSP can compile. H++
- [ ] Draw `func_instance` contents in place, tinted or normal, and hide all instances. Hammer
- [ ] Collapse an instance into the map, and Edit Instance (open its VMF in a tab). Hammer
- [ ] Create New Instance from Selection, placed in the right spot. Hammer, improved in H++
- [ ] Edit Instance with Preview: the parent map shown around the instance, also by middle-clicking it in 3D. H++
- [ ] Prefabs: create from selection (Ctrl+R), a prefab library and inserting prefabs with the Block tool. Hammer
- [ ] Manifests: create manifest, sub-maps and their visibility. Hammer (low priority)

## Milestone 4: entities, models and content browsers

### Object Properties

- [ ] Model tab: skin, bodygroups, sequence preview and attachments for prop entities. Hammer
- [ ] Browse buttons for model, sound, sprite, material and particle keyvalues. Hammer
- [ ] Mark, Mark + Add and the eyedropper on target name fields. Hammer
- [ ] Copy and Paste of keyvalues between entities. Hammer
- [x] Angles compass with Up and Down. Hammer
- [ ] Help button showing the class's description, keyvalues, inputs and outputs. Hammer
- [ ] Comments field. Hammer
- [ ] Right-click a keyvalue to reset it to its default. H++
- [ ] Redesigned colour picker with RGB and HSV, a palette saved per map, opened with C on a light or prop. H++

### Browsers

- [x] Model browser: thumbnails, folder tree, search with wildcards and `!` negation, model preview with skins and bodygroups. Hammer, improved in H++
- [ ] Model browser: several panels, and attachments in the preview. H++
- [ ] Draw props in the 3D views with their `skin` and `body` keys.
- [ ] Soundscape browser: the game's soundscapes by script file, what each plays, a listen button and a way to give one to the selected `env_soundscape`.
- [ ] Room effects (`dsp`) in the sound preview.
- [ ] Sound browser (Ctrl+Shift+S) with playback of WAV, MP3 and OGG, a volume slider and Reload Sounds. Hammer, improved in H++
- [ ] Particle browser with a preview of each system. Hammer (from Alien Swarm), H++
- [ ] Alpha channel display in the texture browser. H++
- [ ] Entity Gallery. Hammer (low priority)

### Entity tools and reports

- [x] Find Entities (Ctrl+Shift+F). Hammer
- [ ] Replace (Ctrl+Shift+R) for keyvalues. Hammer
- [x] Entity Report: filter by class, name or keyvalue; go to, select, properties and delete. Hammer
- [ ] Show Connections: lines between entities that send each other outputs. Hammer
- [ ] Overlays on displacements following the displaced surface. Hammer
- [ ] Overlay handles in the 2D views, and rotating an overlay from a handle. Hammer
- [ ] Path tool: place and chain path entities such as `path_track` and `path_corner`. Hammer
- [ ] Sprinkle tool with presets. Hammer (CS:GO), H++
- [ ] Physics tool: drag, explode and shoot props, ragdolls and brushes, keeping their poses. H++
- [ ] `editor_text`: 3D text placed in the map that never reaches the game. H++
- [x] Apply Current Texture to selected brushes (Shift+T). Hammer

## Milestone 5: what the viewports show

The lighting preview already shows the skybox, fog, glows and tool texture hiding; these
items bring the same to the other modes and add Hammer's and Hammer++'s other previews.

- [ ] Editor sprites for point entities (`iconsprite()`), such as logic and ambient entity icons. Hammer
- [ ] Show Helpers: spheres, cones, lines and angles the FGD's helpers describe, in every view. Hammer
- [ ] Toggles for models in 2D and entity names in 2D. Hammer
- [ ] 3D Shaded Textured, 3D Filled Polygons and Draw Materials Opaque modes. Hammer
- [ ] Toggle editor objects: tool textures (Ctrl+Shift+F2) and editor models and sprites (Alt+O) in the textured view. H++
- [ ] Show nodraw faces, collision model wireframe and detail sprites toggles. Hammer
- [ ] Model fade preview with start and end fade distances. Hammer
- [ ] 2D skybox and 3D skybox in the textured view, toggled from the toolbar. H++
- [ ] Fog preview in the textured view, with View > Set Fog Preview choosing the controller. H++
- [ ] Material proxies: animated, scrolling and other proxy-driven textures. H++
- [ ] Render effects: `rendercolor`, `renderamt`, render modes and bodygroups. H++
- [ ] Particle systems drawn for `info_particle_system`, all or only selected ones. H++
- [ ] Rope preview with simulated sag for `move_rope` and `keyframe_rope`. H++
- [ ] Selection glow around selected models. H++
- [ ] Lightmap grid view with luxels tinted by brightness, including static prop luxels. Hammer, improved in H++
- [ ] Model hotloading: models and materials reload when they change on disk. H++
- [ ] Load and show a portal file (`.prt`). Hammer

## Milestone 6: files, checks and safety

- [x] Check for Problems (Alt+P) with Go To and Fix, and Fix All. Hammer
- [x] Map Information dialog: counts of solids, faces, entities and textures. Hammer
- [x] Map Properties shortcut for worldspawn (skybox, detail material, map version). Hammer
- [ ] Autosave every few minutes into a backup folder, with a number of copies kept. Hammer
- [ ] Crash recovery: an error message and autosaved maps when Mallet crashes. H++
- [ ] Precise vertex data saved alongside planes so brushes never drift, with an option to turn it off. H++
- [ ] Diff Map File against another VMF. Hammer
- [ ] Export to DXF and OBJ, and Export Again (Alt+B). Hammer
- [ ] Open RMF and MAP files from older Hammers. Hammer (low priority)
- [ ] Undo levels setting. Hammer
- [ ] Load and save window state presets. Hammer
- [x] Discord Rich Presence, with the map name optional. H++ (low priority)

## Milestone 7: lighting and compiling

- [x] Hammer's normal Run Map dialog (F9), with Expert... for the compile window and remembering which was used. Hammer
- [ ] Bake through VRAD and read the lightmaps back into the lighting preview. Builds on the existing preview
- [ ] Static prop lighting in the preview, per vertex or per texel as VRAD does it. H++
- [ ] Raytraced preview mode. Hammer (Orange Box "3D raytrace preview", low priority)

## Not planned

- Radius culling (C): Hammer++ removed it because it slowed Hammer down and broke compiles.
- Convert WAD to VMT: only useful for GoldSrc content.
- Hammer's freezing process window: Mallet's compile window already runs the compilers on their own threads.

## Already in Mallet

Hammer and Hammer++ features Mallet has today, so the lists above stay focused on what is
left. See the [README](../README.md) for how each one works.

- Lossless VMF loading and saving, the `.mallet` format, map tabs, recent maps and Windows file types.
- Hammer's four-viewport layout with 3D Textured, 3D Wireframe, 3D Lighting Preview and 2D Top, Front and Side views.
- Selection, Block, Entity and Texture Application tools; scale, rotate and skew handles; the Hammer++ 3D gizmo and pivot.
- Overlay (Shift+O) and Decal (Shift+D) tools, with overlays and decals drawn on their faces in 3D and outlined in 2D.
- Transform dialog, Snap Selected to Grid (and individually), Flip Objects, Align Objects, texture lock and texture scale lock, arrow-key nudging.
- Object Properties from the FGD: keyvalues with typed editors, SmartEdit, Apply, outputs, inputs and flags, for several entities at once.
- Picking brush faces for side list keyvalues, such as overlay and cubemap faces, by clicking them in the 3D view.
- Face Edit sheet with Hammer++'s UV vector editing and hotspot texturing, Apply Nodraw, random shift, justify, smoothing groups and Replace Textures.
- Texture browser over every mounted game and addon, with Hammer's filters, keywords, Mark and Replace.
- Prop models in 3D and 2D, placed by `origin`, `angles` and `modelscale`.
- Hammer++'s 3D Lighting Preview, with per-luxel lighting, shadows, the 2D and 3D skybox, fog, glows and light helpers.
- A sound preview at the 3D camera: soundscapes chosen and played by the game's rules, and the ambient generics that play from the start.
- CompilePal-style compile window with presets, a separate thread, a Problems tab, SourceCubemap and leak lines (Load and Unload Pointfile).
- Editable key bindings, Mallet Options for every setting, and the MCP server for AI agents.
