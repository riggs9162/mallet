---
name: mallet-mapping
description: "Design, build, review and finish Source engine maps to an exceptional standard using the Mallet editor's MCP tools (mcp__mallet__*). Runs a staged pipeline with quality gates: brief, layout, dev-texture blockout, gameplay, architecture, lighting, detail, optimisation and release. Use whenever the user wants to make, extend, block out, greybox, detail, light, optimise, compile, playtest, critique or release a map or level for Half-Life 2, Garry's Mod (sandbox, roleplay, HL2RP, DarkRP, TTT), Counter-Strike: Source or Team Fortress 2; mentions Mallet, Hammer, a .vmf or .bsp, brushes, dev textures, func_detail, visleaves, hint and skip brushes, areaportals, occluders, vis or VVIS, frame rate, displacements, lightmaps, cubemaps, soundscapes or a 3D skybox; asks why a map looks bland, runs badly or leaks; or asks for a room, building, street or area to be added to a map, even when they do not name a stage."
---

# Mapping for Source with Mallet

Exceptional maps are not made by adding more. They are made in order: a layout proven in
grey before any art, architecture before props, light before clutter, and a gate passed
before each next step. Most weak maps are a late stage laid over an unfinished early one.
This skill is that order, the standard for each step, and how to do it through Mallet.

## The one rule

**Work at the earliest unfinished stage, and do not start the next until its gate
passes.** When the user asks for late-stage work on a map whose earlier stage is not
done (props in a room with broken scale, lighting on a leaking map), say so in a
sentence, then either fix the earlier stage first or do what was asked and name the risk.
The user decides; they are never left unaware.

## The stages

| # | Stage | Looks like | Gate to pass |
| --- | --- | --- | --- |
| 0 | Brief and reference | A sentence, three pillars, reference images | User agrees the brief |
| 1 | Layout on paper | Top-down plan: areas, routes, landmarks, sizes | User approves the plan |
| 2 | Blockout: the dev-texture cycle | Orange walls, grey floors, true scale, sealed, no detail | Compiles clean, to metric, played, layout locked |
| 3 | Gameplay | Spawns, doors, triggers, objectives, all in grey | Fully playable in dev textures |
| 4 | Architecture | Real materials, wall thickness, structure, trims, terrain | No dev textures; passes the amateur-tells scan |
| 5 | Lighting | Sun, sky, fog, motivated lights, tonemap | Contrast, direction, colour; judged on a full bake |
| 6 | Detail and ambience | Props, overlays, sound, motion, cubemaps, 3D skybox | Story reads; detail uneven on purpose; sound everywhere |
| 7 | Optimisation and clip | Hints, areaportals, fades, clips, nav | Worst view holds frame rate; no way out of the world |
| 8 | Final compile and release | Final bake, cubemaps, packed, verified | Release checklist complete |

Versions follow the stages: `name_a1` through Stages 2 and 3, `name_b1` through 4 to 6,
`name_rc1` after 7, final after 8. Never overwrite a released version.

Read `references/stages.md` before starting or resuming a map: it has the work, the
Mallet calls, the full gate and the usual failures for every stage.

## How to work

1. **Orient.** `get_status`, `list_entities` with `summary`, `list_materials` with
   `used_only`, and a `screenshot`. Work out which stage the map is at and tell the user.
2. **Agree the target.** New map: Stage 0. Existing map: which stage and which area.
   Ask only what cannot be found in the map or decided sensibly.
3. **Build in batches.** One logical change per `batch`, `on_error: "undo"`, so it is one
   undo step and never half-made.
4. **Look after every batch.** `screenshot` from player eye height (floor + 64) the way a
   player arrives. Judge the picture against the stage gate. Never report work you have
   not looked at.
5. **Measure, do not guess.** `trace_line` for floors, heights, widths and sightlines;
   `describe_model` and `describe_material` before using an asset.
6. **Compile to confirm.** `check_for_problems`, then a fast preset while iterating and a
   full preset at each gate. Read `compile_status` to the end.
7. **Report against the gate.** Say what passes, what does not, and what you could not
   verify (above all, anything only testable by playing).

`references/mallet-workflow.md` has the tool recipes: rooms without carve, nodraw as you
build, props placed from a trace, displacements, lighting preview, compile and leak
hunting.

## Rules that hold at every stage

These are the faults that are cheap to prevent and costly to repair.

- **Scale to the player.** Hull 32 x 32 x 72, eyes at 64, step 18, door 48 x 108,
  ceilings 112 to 128, corridors 96 to 128. Full tables in `references/metrics.md`.
- **Stay on the grid**, powers of two, 90 and 45 degree world geometry. Off-grid
  vertices become leaks, seams and invalid brushes.
- **Never carve. Never keep a hollowed box.** Build walls as separate butting brushes
  and cut openings by building around them or clipping.
- **Nodraw every face no player can see**, from the first brush.
- **Seal with world brushes; detail everything else.** Structure that separates areas is
  world geometry. Everything small, angled or decorative is `func_detail`. Displacements,
  props and brush entities never seal.
- **Never box the map in a skybox shell** to hide a leak. Find the gap with
  `load_pointfile` and close it.
- **Optimise from the layout onward, not at the end.** Decide where sight ends when
  drawing the plan, keep a nodraw world core inside every detailed building, and set prop
  class and fade as each prop is placed. Stage 7 only measures and tunes: hints on the
  limit of sight, areaportals on every opening between areas, each kept only if the game
  shows a gain.
- **Place props from a trace**, with the class `describe_model` says the model supports.
  No floating, no sinking, no guessing Z.
- **Every light has a source and a colour.** No pure white, no flat ambient.
- **Keep run-time entities lean**: `prop_static` over dynamic, few logic entities.
  Multiplayer servers share the limit with everything players spawn.
- **One stage per visgroup**, so a stage can be hidden, reviewed or redone alone.
- **The map is the user's.** Announce large edits, save before compiles, never discard
  their unsaved work, never run a preset that launches the game without asking, and
  leave publishing to them.

## Judging quality

Look for what is wrong before admiring what is right. For any view, in this order:

1. **Shape**: is the space the right size and an interesting form, or a box?
2. **Structure**: do walls have thickness, frames, trims and something holding them up?
3. **Light**: is there a brightest point, real shadow, a warm and a cool, a visible source?
4. **Palette**: few materials, aligned, one texel density, ending at a reason?
5. **Detail**: placed with a reason, clustered, uneven on purpose, out of the route?
6. **Life**: sound, motion, and a world beyond the wall?

Fix the first one that fails; later ones cannot compensate for it. A screenshot taken at
quarter scale should still read. `references/detailing.md` lists the amateur tells to
scan for, and `references/release-checklist.md` has a scorecard to give the user.

## Reference files

Read the one for the stage in hand; do not load them all.

| File | Read when |
| --- | --- |
| `references/stages.md` | Starting, resuming or reviewing a map; deciding what comes next |
| `references/mallet-workflow.md` | Before the first build in a session; any time a tool recipe is needed |
| `references/metrics.md` | Blockout; any question of size, grid, dev textures or engine limits |
| `references/detailing.md` | Architecture and detail passes; "why does this look bland" |
| `references/lighting.md` | Lighting, fog, HDR, cubemaps, soundscape placement, 3D skybox |
| `references/optimization.md` | Sealing, leaks, visibility, sight blockers, areaportals, props, displacements, compile errors, performance; any request to optimise or "why does it lag" |
| `references/hints-and-skips.md` | Before placing, moving or reviewing any hint brush; outdoor maps where areas see each other over rooftops; long VVIS times |
| `references/scripting-and-polish.md` | Entity I/O, doors and lifts, ambience, Garry's Mod needs, playtesting, packing |
| `references/release-checklist.md` | Scoring a map; Stage 8 |
| `references/sources.md` | Where this guidance comes from, and where to read further |

## When Mallet is not available

If no `mcp__mallet__*` tool is present, Mallet is not running or its MCP server is off
(Mallet Options > AI agents; default `http://127.0.0.1:7414/mcp`). Say so. The stages,
metrics and standards here still apply to planning and to reviewing screenshots or a
`.vmf` the user shares, but do not hand-write or patch `.vmf` text as a substitute for the
editor unless the user asks for that.
