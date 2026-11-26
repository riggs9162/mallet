# Working in Mallet: tools and recipes

Mallet exposes the editor through its MCP server (`mcp__mallet__*`). Tool schemas
describe every argument; this file covers how to combine them well.

## Contents

- Session start
- The look loop
- Finding things
- Building brushes without carve
- Materials and faces
- Props
- Entities and I/O
- Displacements
- Lighting preview
- Compiling and reading the result
- Working beside the user

## Session start

1. `get_status`: which map is open, bounds, grid, game folder, FGDs, `own_view`.
2. `list_entities` with `summary: true`: a class census tells the map's stage at a glance
   (no lights and only dev materials means blockout; hundreds of `prop_static` means art pass).
3. `list_materials` with `used_only: true`: the current palette, with face counts.
4. `screenshot` (`viewport: "all"`, `scale: 0.5`) for a first look, then `set_camera` to
   player eye height at a spawn and take a `camera` screenshot.
5. `list_compile_presets`: learn the preset names before promising a compile.

A new map: `new_map`, then `save_map` with an absolute path inside the game's `maps` or
an addon's `maps` folder. Save again before every compile and at the end of every stage.

## The look loop

Never build blind. After every meaningful batch:

- `screenshot` with `viewport: "camera"` from **player eye height** (floor + 64), looking the
  way a player would arrive. Editor fly-cam angles flatter a map and hide scale errors.
- Switch a viewport with `set_viewport_type` to `lighting` to judge light, `textured` to judge
  materials, `wireframe` to judge brush count and overlaps.
- Use `top` for layout proportions and alignment; `front` and `side` for heights.
- Judge each picture against the stage's exit gate, name what is wrong, fix, look again.

## Finding things

- `trace_line` is the measuring tape. Floor under a point: `direction: [0,0,-1]`. Ceiling:
  `[0,0,1]`. Room width: opposite horizontal rays. Sightline check: `from` eye A `to` eye B;
  a hit before B means the line is blocked. Up to 256 rays a call.
- `list_solids` and `list_entities` filter by `box`, `material`, `classname`, `where`.
- `select` combines class, material and box; follow with `get_selection`.
- `pick_face` and `get_faces` give side ids for `set_faces`, `create_overlay` and
  `create_displacement`.

## Building brushes without carve

`batch` is the workhorse: up to 200 calls, one undo step, `on_error: "undo"` so a failed
build leaves nothing half-made. Use `results: "errors"` for large builds.

**Rooms are six separate brushes, not a hollowed box.** Build floor, ceiling and four walls
with `create_block`, each on the grid, butting rather than overlapping. `make_hollow` makes
mitred overlapping walls and `carve` shreds neighbours into slivers: both are fine for a
five-minute sketch and wrong for anything that will be kept.

**Texture as you build.** Give every structural brush `material: "tools/toolsnodraw"` and
name only the faces a player can see in `side_materials`:

```json
{"tool": "create_block", "arguments": {
  "min": [0, 0, -16], "max": [512, 512, 0],
  "material": "tools/toolsnodraw",
  "side_materials": {"top": "dev/dev_measuregeneric01b"}}}
```

A wall seen from one room uses the one side facing that room (`north`, `south`, `east`,
`west`); a free-standing wall uses `sides`.

**Openings** are made by building around them: wall segment left, wall segment right, lintel
above (and sill below for a window). Or build the full wall and cut it with
`clip_selection` (`keep: "both"`), then delete the middle piece. Never carve a door.

**Stairs**: one `create_block` per step in a `batch`, rise 8 and run 12 by default, or one
`wedge` for a ramp. Clone a flight with `clone_selection`.

**Curves**: `create_arch` for arches and curved corridors, `create_block` with
`shape: "cylinder"` for columns. Keep side counts low (8 to 16) and tie the result to
`func_detail`.

**Repeating modules**: build one bay, `group_selection`, then `clone_selection` with an
offset, or save the bay as its own `.vmf` and stamp it with `insert_map`
(`copies`, `offset`, `rotation`, `unique_names` for entity names).

**Detail brushes**: select, then `tie_to_entity` (default class `func_detail`). Add later
pieces to the same entity with `into`. `move_to_world` reverses it.

After any build, `snap_selection_to_grid` is a cheap guard, and `check_for_problems`
catches invalid solids before the compiler does.

## Materials and faces

- Look before choosing: `preview_materials` with a `filter` (words, `!word` to exclude)
  returns a contact sheet; `describe_material` gives size, shader, surface property and
  whether a material is a tool, sky or blend.
- `set_faces` is Face Edit: `material`, `scale`, `shift`, `rotation`, `lightmap_scale`,
  `align` (`world` or `face`), `justify` (`fit`, `center`, and so on), `treat_as_one` to
  wrap a texture across several faces.
- `replace_material` swaps a material across the map: the fast way from dev textures to a
  first art pass, and the way to audit a palette.
- `select_faces` picks faces for the texture tool; `get_faces` reads their alignment back.
- Lightmap scale is a face property: `set_faces` with `lightmap_scale`.

## Props

1. `preview_models` with a `filter` to choose by looks and size.
2. `describe_model` before placing: `use_as` and `not_as` say which of `prop_static`,
   `prop_dynamic` and `prop_physics` the model survives as, and it reports the distance from
   the origin to the model's lowest point.
3. `trace_line` down to find the floor; place the origin so the model sits on it.
4. `create_entity` with `classname`, `origin` and `keys` (`model`, `angles`, `skin`,
   fade distances).
5. `check_for_problems` reports props whose class would lose the model, and with `fix`
   gives them the class that works.

Never leave a prop floating or sunk: always derive Z from a trace, never from a guess.

## Entities and I/O

- `list_entity_classes` and `describe_entity_class` read the loaded FGDs: keyvalues with
  defaults, spawnflags, inputs and outputs. Read the class before setting keys.
- `create_entity` for point entities; `tie_to_entity` with a `classname` for brush entities
  (`func_door`, `trigger_multiple`, `func_areaportal`).
- `set_entity_keys` changes many entities at once; `set_entity_outputs` **replaces** the
  whole output list, so read with `get_entity` first and send the full list back.
- Name everything that is targeted. Use a consistent scheme such as
  `area_thing_role` (`lobby_door_main`, `lobby_relay_alarm`).

## Displacements

1. Build the base brush on the grid; `create_displacement` on the top face only
   (`power: 3` by default; 2 for flat ground, 4 only where the shape needs it).
2. Shape with `paint_displacement` (world-space points, good for programmatic terrain),
   `sculpt_displacement` (screen-space strokes on the camera view) and
   `edit_displacements` (`noise`, `subdivide`, `sew`).
3. Blend textures with `paint_displacement` `mode: "alpha"` on a blend material.
4. Always `edit_displacements` `action: "sew"` across neighbours when done.
5. Displacements do not seal the map and do not block visibility: keep a nodraw world
   brush under them.

## Lighting preview

`set_viewport_type` with `type: "lighting"` shows the map lit by its lights with shadows,
sky light, texture lights, fog and the skybox, using VRAD's maths. `list_lights` reports
every light's reach and brightness for LDR and HDR. Iterate on lights here; compile only to
confirm bounce light and lightmap resolution, which the preview approximates.

## Compiling and reading the result

- `compile_map` with a `preset` and `wait_seconds`; `compile_status` for progress, warnings
  and errors with the brush or position each names.
- On a leak: `load_pointfile` draws the leak line and moves the camera to it. Follow the line
  from the entity it starts at to the void, close that gap, `clear_pointfile`, recompile.
- Preset names are the user's. Typical shapes: a fast preset (`vvis -fast`, `vrad -fast`),
  an entities-only preset (`vbsp -onlyents`) for I/O changes, a full preset, and a final
  preset with cubemaps and repacking. Create missing ones with `set_compile_preset`.
- Presets that end in a `run_game` step launch the game. Ask before running one, or use a
  preset without that step.

## Working beside the user

- The map is shared even when the view is not. Announce large edits, keep each logical change
  to one `batch` so a single undo takes it back, and never close a tab with `discard`.
- If `get_status` reports `own_view: false`, tools that change tab, camera or selection move
  the user's window too. Work in small steps and restore the camera afterwards.
- Organise as you go: `create_visgroup` per area or per kind (`blockout`, `detail`,
  `lighting`, `clips`, `optimisation`), so stages can be hidden and reviewed apart.
- Do not edit the `.vmf` on disk while it is open. If another program wrote it, `reload_map`.
