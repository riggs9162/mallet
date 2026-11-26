# Quality bar and release checklist

Two tools: a scorecard for judging a map at any time, and the ordered checklist for
Stage 8.

## Scorecard

Score each line 0 (absent), 1 (weak), 2 (solid), 3 (exceptional), from eye-height
screenshots and a compile log, and give the user the numbers with one sentence of evidence
each. Fix anything at 0 or 1 before polishing anything at 2. A map is ready to release at
2 or above on every line; it is exceptional when most lines are 3 and the first three
are.

| Area | Question |
| --- | --- |
| Layout | Do routes, loops, sightlines and timings work, and has it been played? |
| Scale | Is every dimension to metric, and does the player look right in every space? |
| Identity | Could a player name each area, and name the map from one screenshot? |
| Architecture | Do buildings have thickness, structure, trims and silhouette? |
| Texturing | Is the palette short and coherent, aligned, at one texel density? |
| Lighting | Is there contrast, direction, colour, motivated sources and readable routes? |
| Composition | Does each main view have a focal point and a clear way onward? |
| Detail | Is detail placed with reason, uneven on purpose, and out of the way of play? |
| Atmosphere | Sound in every space, motion, fog, sky and a world beyond the walls? |
| Function | Do doors, lifts, triggers, spawns and gamemode entities all work? |
| Technical | Sealed, clean compile, on grid, nodraw, correct prop classes? |
| Performance | Does the worst view hold frame rate, with entity headroom? |
| Finish | Clipped, cubemaps built, content packed, nothing missing? |

## Release checklist

In order. Later steps depend on earlier ones.

### Before the final compile

- [ ] `check_for_problems` reports nothing, or only items with a known reason.
- [ ] No dev materials in playable space; no missing materials or models
      (`preview_materials` and `preview_models` with `used_only: true` report missing files).
- [ ] No `prop_static` using a model that cannot be static; no props floating or sunk.
- [ ] Every unseen face is nodraw; no brush is off grid; nothing was carved.
- [ ] Every non-structural brush is `func_detail`; the map is sealed by world brushes.
- [ ] Spawns: enough for the player count, clear of geometry, facing a sensible way.
- [ ] Every targeted entity is named; no output points at nothing.
- [ ] Cubemaps placed in every space; soundscapes cover every space and spawn.
- [ ] Tonemap controller, fog and sky set; 3D skybox fog matches world fog.
- [ ] Clip pass done for the gamemode; no way out of the world.
- [ ] Fade distances on small props; entity count within budget.
- [ ] The map is saved under its release name, and that name is new.

### Compile

- [ ] Full VBSP and VVIS (never `-fast`).
- [ ] VRAD with `-final`, static prop lighting, and both LDR and HDR where the game
      supports both.
- [ ] `compile_status` read to the end: no leak, no errors, every warning understood.
- [ ] Limits in the log are comfortably under their maximums.

### After the compile

- [ ] Cubemaps built on this exact `.bsp` (LDR and HDR), and reflections checked.
- [ ] Navigation mesh generated and saved, if bots or nextbots are expected.
- [ ] Custom content packed into the `.bsp` or collected for the addon.
- [ ] `.bsp` compressed or repacked last.
- [ ] Do not recompile after this without repeating every step in this section.

### Verify

- [ ] Loads on a clean install with no source content and no extra addons: no pink
      checkers, no ERROR models, no missing sounds.
- [ ] Walked end to end in game: every door, lift, button and trigger used.
- [ ] Frame rate measured at the worst views.
- [ ] Console free of repeating errors while playing.
- [ ] Tested with the real gamemode and, if possible, a full server.

### Package

- [ ] Version name follows the scheme and has never been released before.
- [ ] Honest screenshots, a thumbnail and an icon in the formats the platform needs.
- [ ] Description: gamemode, size, player count, required games and addons.
- [ ] Credits for every asset that is not the author's or stock.
- [ ] Changelog since the last version.
- [ ] The user has approved and performs, or explicitly asks for, the upload.
