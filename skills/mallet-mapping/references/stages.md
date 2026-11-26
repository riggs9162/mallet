# The stages in full

Nine stages, each with a goal, the work, the Mallet calls that do it, the gate that ends
it, and the usual ways it goes wrong. Earlier stages are cheap to change and later ones
are not, so a fault is always fixed at the earliest stage that owns it: a route that does
not work is a Stage 2 fault even when found at Stage 6, and is fixed in the layout, not
dressed over.

## Contents

- Stage 0: Brief and reference
- Stage 1: Layout on paper
- Stage 2: Blockout (the dev-texture cycle)
- Stage 3: Gameplay pass
- Stage 4: Architecture pass
- Stage 5: Lighting pass
- Stage 6: Detail and ambience pass
- Stage 7: Optimisation and clip pass
- Stage 8: Final compile and release
- Going back a stage
- Working on an existing map

## Stage 0: Brief and reference

**Goal:** know what is being built before a brush exists.

- Settle with the user: game and gamemode, player count, the setting in one sentence,
  time of day and mood, size, must-have locations, content allowed (stock only, or which
  custom packs), and the target (private server, Workshop, contest).
- Write three pillars: the things this map must be known for. Every later decision is
  tested against them.
- Gather reference: two or three images each for architecture, environment, lighting and
  props. Name what each is for. If the user has none, describe the intended look in words
  and get agreement.
- Decide hard constraints: map bounds, the entity budget, and which areas are playable.
- In Mallet: `get_status` (game folder and FGDs decide which entities and assets exist),
  `list_materials` and `preview_models` to learn what the game ships for the theme.

**Gate:** a written brief the user has agreed to: setting sentence, pillars, location
list, constraints.

**Goes wrong when:** the brief is "a cool city map". Vague briefs make generic maps.

## Stage 1: Layout on paper

**Goal:** a top-down plan that works before it costs anything to change.

- List the areas and give each a purpose, a landmark and a size in units.
- Connect them: main routes, side routes, loops. Every area has two or more exits.
  Mark choke points, sightline breaks, height levels and where players enter.
- Mark the vistas: the two or three views the map will be remembered for.
- Give sizes from `metrics.md`: corridor and street widths, room footprints, storey
  heights. Check travel time between key points.
- Present the plan to the user as a diagram or a labelled list with coordinates. A
  top-down sketch can also be built in Mallet as flat coloured floor slabs, one per
  area, and shown with a `top` screenshot.

**Gate:** the user approves the plan; every area has a name, purpose, landmark and size.

**Goes wrong when:** the plan is a grid of same-sized boxes joined by corridors, or the
map is symmetric because symmetry was easier.

## Stage 2: Blockout (the dev-texture cycle)

**Goal:** prove the layout at true scale, in 3D, with nothing but dev textures.

This is the cycle the stage is named for: **build, look, test, change, repeat**, as many
times as it takes, because every change here costs minutes and the same change after the
art pass costs days.

Build:

- Grid 64 or 32. Floors as single slabs, walls as simple boxes, 16 or 32 thick.
- Orange dev walls, grey dev floors and ceilings, colour for meaning. Nodraw on every
  unseen face from the start.
- Every area sealed with world brushes and capped with `tools/toolsskybox`. The map
  compiles without a leak from the first hour and stays that way.
- Stand-in blocks for cover, stairs as real steps or wedges, door openings at true size,
  and a tall simple shape where each landmark will be.
- A player-sized block (32 x 32 x 72) kept near the camera for scale.
- One `info_player_start`, one `light_environment` and a sky so compiles run and can be
  walked. Rough lighting only: sun direction and nothing else.
- No props, no trims, no real textures, no small brushes, no `func_detail` yet.

Look, every pass:

- `screenshot` from eye height at each entrance and along each route, both ways.
- `top` view: proportions, alignment, whether the plan still reads.
- `trace_line`: ceiling heights, corridor widths, sightline lengths against `metrics.md`.

Test:

- Fast compile, read `compile_status`. Walk it in game if possible; time the routes.
- Get the user or other players on it. Version as `name_a1`, `a2`...

Change: move walls freely. Delete areas that do not earn their place. Rework rather than
patch.

**Gate:**

- Compiles clean with no leak on a full (not fast) VVIS.
- Every dimension is to metric; nothing looks giant or cramped from eye height.
- Every area has two or more exits and a distinct shape and landmark silhouette.
- The longest sightlines are deliberate.
- Someone has played it and the routes, timings and sizes held up.
- The user says the layout is locked.

**Goes wrong when:** detail starts early ("just this one room"); the blockout is judged
only from the fly camera; rooms are oversized; the map is never played before art begins.

## Stage 3: Gameplay pass

**Goal:** everything that makes the map function, working in grey.

- Spawns, objectives and gamemode entities, weapons and items, doors, buttons, lifts,
  ladders, triggers and their I/O (`scripting-and-polish.md`).
- First clips where players clearly must not go.
- For roleplay and sandbox: every door ownable, every interior reachable, spawns spread
  and clear, build spaces flat.
- `check_for_problems` for dangling outputs and missing spawns; entities-only compiles to
  iterate on I/O.

**Gate:** the map is fully playable in dev textures: a round can be played, or a server
could open on it tonight and only look unfinished. Play-tested as the final alpha.

**Goes wrong when:** scripting is left until after the art pass and then needs walls
moved to fit a lift or a door swing.

## Stage 4: Architecture pass

**Goal:** turn the blockout into buildings. Primary and secondary detail only. This
begins the beta (`name_b1`).

- Choose the palette per area (`detailing.md`) and replace dev materials with it:
  `replace_material` for the broad swap, then `set_faces` per surface.
- Give walls real thickness, frame every opening, trim every joint, add the structure
  that would hold the building up, treat ceilings, break long flat runs, shape the
  skyline.
- Replace stand-in blocks with real stairs, railings, cover and large props.
- Build terrain as displacements and blend their materials.
- Tie every non-sealing brush to `func_detail` as it is made. Keep the world brushes of
  the blockout as the sealing shell.
- Re-seal and recompile often; architecture is where leaks and off-grid brushes creep in.

**Gate:**

- No dev material left in playable space (`list_materials` `used_only: true`,
  `filter: "dev/"`).
- Every room passes the amateur-tells scan in `detailing.md` at the primary and
  secondary level, with flat default lighting.
- Texel density is consistent and no texture is stretched or misaligned.
- The layout plays as it did at the Stage 3 gate; collision has not drifted.
- Still compiles clean.

**Goes wrong when:** texturing substitutes for architecture (a brick texture on a
blockout box is still a box); small props arrive before trims and structure.

## Stage 5: Lighting pass

**Goal:** mood, readability and direction. See `lighting.md`.

- Sky, sun, ambient and fog first; then motivated sources, key and fill, accents.
- Tonemap controller. Lightmap scales raised where shadows matter and lowered where
  nothing shows.
- Work in the lighting preview viewport; confirm with fast compiles, then a full bake.
- 3D skybox roughed in, since it sets the horizon the fog and sun must match.

**Gate:**

- Each view has a clear brightest point that is the right thing to look at.
- No playable floor is black, nothing is blown out, no light lacks a source.
- Warm and cool are both present; light colour and sky agree.
- Routes and exits are readable by light alone.
- Reviewed on a full, non-fast bake in game, not only in the preview.

**Goes wrong when:** lighting is a last-day job; ambient is raised to hide a lack of
lights; the map is lit before its architecture is settled.

## Stage 6: Detail and ambience pass

**Goal:** tertiary detail, story and life.

- Props in story-telling clusters, overlays and decals for wear, signage, cables,
  foliage (`detailing.md`).
- Soundscapes, point sounds, glows, dust, steam, motion (`scripting-and-polish.md`).
- Cubemaps placed. Out-of-bounds and 3D skybox dressed. Colour correction if used.
- Every prop placed from a floor trace and checked with `describe_model` for class.

**Gate:**

- Each area tells what it is and what happened there without a sign saying so.
- Detail density is uneven on purpose: focal points rich, routes clean.
- No floating, sunk or clipping props; no repeated rows.
- Every space and every spawn has a soundscape (`soundscape_at`).
- A quarter-size screenshot of each main view still reads.

**Goes wrong when:** props are scattered to fill space; detail blocks movement; the
entity count balloons with physics and dynamic props.

## Stage 7: Optimisation and clip pass

**Goal:** it runs well and plays cleanly. See `optimization.md`.

- Audit: nodraw on unseen faces, `func_detail` on everything non-structural, prop
  classes and fade distances, lightmap scales, entity count.
- Hints, areaportals, occluders, `farz`, where measurement shows a need.
- Full clip pass for the gamemode. Navigation mesh or AI nodes.
- Full compile, then measure in game at the worst views.

**Gate:**

- Full VVIS finishes in reasonable time; no compile warnings left unexplained.
- Frame rate holds at the worst view on the target hardware and player count.
- `mat_wireframe` shows areas hiding each other as designed.
- Players cannot leave the world or get stuck, and movement along routes is smooth.
- Entity count leaves the headroom the gamemode needs.

This is the release candidate (`name_rc1`).

**Goes wrong when:** it is the first time anyone looks at visibility; the fix for a slow
view is deleting detail rather than blocking the sightline.

## Stage 8: Final compile and release

**Goal:** a file that works on someone else's machine.

Work through `release-checklist.md` in order. In short: final compile under the release
name, cubemaps, pack, compress, test on a clean install, write the notes, and hand the
publishing decision to the user.

**Gate:** every line of the checklist is ticked or has a written reason.

## Going back a stage

Going back is normal and is not failure. Rules that keep it cheap:

- Name the stage that owns the fault and fix it there.
- Changing layout after Stage 4 means re-running the gates of every stage in between for
  the changed area: architecture, lighting, detail, clips, cubemaps.
- Keep each stage's work in its own visgroup (`blockout`, `gameplay`, `detail`,
  `lighting`, `ambience`, `clips`, `optimisation`) so one stage can be hidden, reviewed
  or redone without disturbing the others.
- Keep the last file of each stage (`name_a3.vmf`, `name_b1.vmf`) so a comparison or a
  rollback is one `open_map` away.

## Working on an existing map

Most requests are not a new map. Before changing anything:

1. Find the stage the map is at: materials in use, entity census, a compile log, and
   screenshots from eye height.
2. Judge it against the gates of every stage up to that one, and tell the user which
   gates it fails. A beautiful area with broken scale has a Stage 2 problem.
3. Do the work asked for at the stage it belongs to, and match the surrounding area's
   stage and standard: a new room in a finished map is taken through all stages itself
   before it is called done.
4. Leave the rest of the map alone unless asked. Report faults found elsewhere instead of
   fixing them unasked.
