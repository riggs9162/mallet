# Optimisation, sealing and compiling

Optimisation is not a stage that starts at Stage 7. It is decided at every stage:

| Stage | Optimisation decision made there |
| --- | --- |
| 1 Layout | Where sight ends: corners, offsets and height changes between areas; which areas become areaportal areas |
| 2 Blockout | Sealed, on-grid world brushes; nodraw on unseen faces; sky lid at the roofline |
| 4 Architecture | Everything that is not structure becomes `func_detail`; nodraw world cores kept inside detailed buildings |
| 6 Detail | Prop class, fade distances and collision set as each prop is placed |
| 7 Optimisation | Measured pass: hints, areaportals, occluders, `farz`, clips |

A map built without the first four cannot be rescued by the fifth; optimisation bolted on
at the end is mostly rebuilding. When the user asks to optimise an existing map, check
the earlier rows first and say which ones fail.

## Contents

- How visibility works
- The recipe
- Sealing and leaks
- Tool textures
- func_detail
- Hints
- Sight blockers
- Areaportals and occluders
- Large open maps
- Props
- Displacements
- Water
- Clipping
- Navigation
- Compile presets
- Compile errors and fixes
- Measuring in game

## How visibility works

VBSP cuts the sealed space inside the map into convex volumes called visleaves, using the
faces of **world brushes**. VVIS then works out which leaves can see which. At run time
the engine draws everything in every leaf visible from the leaf the camera is in. It is
leaf to leaf, not eye to object: if any part of leaf A can see any part of leaf B, all of
B's contents draw.

So performance comes from making leaves large and simple, and placing their boundaries
where sight should end: at corners, doors and the ends of corridors.

## The recipe

1. Seal the playable space with simple, on-grid world brushes that form the true
   structure: outer walls, floors, ceilings, the big dividers between areas.
2. Make everything else `func_detail`, so it neither cuts leaves nor costs VVIS time.
3. Design the layout to block sight: corners, offsets, door frames and height changes
   between areas. No tool fixes a layout where everything sees everything.
4. Put hints where a leaf boundary would stop one area seeing another around a corner.
5. Put areaportals in doors and windows between areas.
6. Fade props by distance, and cut draw distance with fog and `farz` on open maps.
7. Compile fully, then measure in game and fix what the numbers show, not what you guess.

## Sealing and leaks

- The inside of the map must be closed off from the void by world brushes. Sky is closed
  with `tools/toolsskybox` brushes.
- These do **not** seal: `func_detail`, any brush entity, displacements, props,
  translucent materials, water, and brushes textured only with `tools/toolsskip`.
- A leak means VVIS has nothing valid to work on: no visibility, and VRAD will not light
  properly. Never ignore a leak, and never test lighting on a leaking map.
- Find it: `compile_status` reports the leak, `load_pointfile` draws the line from an
  entity out to the void and moves the camera there. Follow the line to the first gap,
  close it with a world brush, recompile. One leak is reported per compile, so repeat.
- Usual causes: a gap between brushes off the grid; a sealing brush that was tied to an
  entity; a displacement used as the only floor; an entity placed outside the map;
  an areaportal that does not fill its opening.
- **Never wrap the whole map in a big hollow skybox to stop leaks.** It makes the entire
  map one visible space, draws everything always, lights every hidden outside face and
  balloons compile times. Fix the gap.
- Sky brushes follow the roofline: bring the sky ceiling down to just above the tallest
  thing players see, and step it with the buildings. A flat sky lid far overhead lets
  every area see every other over the rooftops.
- Cordon compiles box off a region to iterate quickly on one area. They hide leaks
  elsewhere, so always finish with an uncordoned compile.

## Tool textures

| Material | Effect | Use |
| --- | --- | --- |
| `tools/toolsnodraw` | Not drawn, no lightmap, still solid and sealing, still casts shadow | Every face no player can ever see: backs, tops of walls against sky brushes, faces buried in others |
| `tools/toolsskybox` | Draws the sky and 3D skybox; seals | The sky enclosure |
| `tools/toolsskybox2d` | Sky without the 3D skybox | Rare |
| `tools/toolsplayerclip` | Blocks players only | Smoothing and out-of-bounds |
| `tools/toolsnpcclip` | Blocks NPCs only | Keeping NPCs off places |
| `tools/toolsclip` | Blocks players and NPCs | General collision; bullets and props pass |
| `tools/toolsinvisible` | Solid to everything including bullets, not drawn | Blocking where a clip is not enough |
| `tools/toolshint` | Marks the face that cuts leaves | Hint brushes |
| `tools/toolsskip` | Discarded by the compiler | The other faces of a hint brush |
| `tools/toolstrigger` | Marks trigger volumes | All `trigger_*` brush entities |
| `tools/toolsareaportal` | Marks areaportal brushes | `func_areaportal`, `func_areaportalwindow` |
| `tools/toolsoccluder` | Marks occluder faces | `func_occluder` |
| `tools/toolsblocklight` | Blocks light, not drawn | Stopping light leaks |
| `tools/toolsblockbullets` | Stops bullets, not players | Protecting spawn areas, sealing gaps in prop collision |
| `tools/toolsinvisibleladder` | Climbable in games with brush ladders | Ladders |
| `tools/toolsblack` | Drawn black, unlit | Far ends of unreachable dark openings |

Nodraw discipline: build structural brushes in nodraw and texture only the seen faces
(`create_block` `side_materials`). Audit with `list_solids` filtered by material, and by
flying outside the map: any real texture visible from the void is wasted.

## func_detail

`func_detail` brushes are solid and lit like the world but do not cut leaves or block
sight. `tie_to_entity` makes one (it is the default class).

Detail:

- Anything small or complex: trims, frames, beams, columns, stairs, railings, furniture
  made of brushes, pipes, ledges, window bars, signs.
- Anything angled, cylindrical or arched: it would cut leaves into slivers.
- Anything that does not span floor to ceiling and wall to wall.

Keep as world:

- Walls, floors and ceilings that separate areas and seal the map.
- Big simple blockers that should stop sight, such as a building in a street.

Notes:

- A world brush hidden behind a detail brush still draws; nodraw the faces it covers.
- Detail brushes touching world faces can chop those faces up. Sink a detail brush flush
  against the world, not partly into it, and keep heavy clusters of intersecting detail
  down, or VBSP reports too many T-junctions.
- Group many detail brushes of one object into one `func_detail` with `into`.

## Hints

A hint brush adds a leaf boundary where VBSP would not put one: `tools/toolshint` on the
cutting face, `tools/toolsskip` on every other face, left as a world brush.
**`hints-and-skips.md` is the full treatment; read it before placing any.** In short:

- A hint hides nothing by itself. It reshapes leaves so the leaf-to-leaf test fails.
- Neighbouring leaves always see each other; a hint works on the third leaf in a chain.
- Only world brushes block sight. No world brush between two areas, no hint will help.
- Place the hint on the limit of sight: the line from the far edge of the hidden area's
  opening through the corner that blocks the view.
- Outdoors, one horizontal hint at the roofline stops every street seeing every other
  over the rooftops. It is usually the best single hint on a map.
- Add one at a time, compile fully, and keep it only if `mat_wireframe` shows a gain.

## Sight blockers

Hints and VVIS only count world brushes. A map detailed properly is mostly `func_detail`,
displacements and props, so the things that look solid often block nothing.

- Inside every building made of detail brushes and props, keep a simple world brush core
  in `tools/toolsnodraw`: one box per building, a few units inside the visible walls,
  floor to roof.
- Under displacement hills and cliffs that should hide what is behind them, build a
  nodraw world brush that follows the mass of the hill, kept below the surface.
- Inside thick walls of detail brushwork, run one plain nodraw world brush.
- Blockers are big, on grid and few. Their faces cut leaves, so a blocker with many small
  faces does more harm than good.
- A window, a gap under a door or a grate in a world wall joins the leaves each side.
  Close it with an areaportal, or accept that the two sides see each other.

## Areaportals and occluders

An areaportal does what a hint cannot: it culls at run time by the shape of the opening,
and shuts completely with its door.

- `func_areaportal`: a brush filling a door or window completely, textured
  `tools/toolsareaportal` on every face, thin (1 to 8 units), a simple rectangle. When
  open it only draws what is seen **through the opening**; when closed it hides
  everything beyond.
- Its edges must sink into **world** brushes all round. A frame of `func_detail`, a
  displacement or a prop leaves a gap the compiler sees as a leak through the portal.
- The portal must separate two **areas** that are otherwise sealed from each other. If
  any other path connects the two sides without a portal, the compile reports that the
  portal does not touch two areas, or leaks through it. Every route between two areas
  needs its own portal: every door, window, vent and stairwell.
- Plan areas, not portals. Mark the map into a handful of areas (each building interior,
  each courtyard, the outdoors) and portal every opening on each boundary. One missed
  window undoes the whole area.
- Link a portal to its door by name (`target`) so it closes with the door. Doors that
  stay shut most of the time make the best portals. A portal with no door starts open and
  still culls by the shape of the opening.
- A portal named and driven by I/O (`Open`, `Close`) can seal off a whole section of the
  map while players cannot be there: behind a lift ride, a locked gate, a loading door.
- `func_areaportalwindow` fades to opaque with distance: use it for windows and openings
  seen from afar, with a brush entity as the fade target.
- Each open portal in view costs a little at run time. A wall of forty small windows wants
  one portal across the whole bank, or none, not forty.
- Do not let water or a moving brush cross a portal.
- Test with `r_portalsopenall 1` (portals ignored) against the default, and
  `mat_wireframe 1`, to see what each portal earns.
- `func_occluder` hides props (not world geometry) behind a big solid shape, at run time
  cost. Use it sparingly on open maps, for a hill or a large building that hides many
  props where no portal can go.

## Large open maps

Where sight cannot be blocked, limit distance and cost instead.

- Fog with a far clip plane: `env_fog_controller` with `farz` just beyond full fog, so
  everything past it is not drawn at all. Match the 3D skybox fog so the cut does not show.
- Fade distances on every small and medium prop; see "Props".
- `func_viscluster` over wide open space (a field, a lake, a plaza) tells VVIS to treat
  the leaves inside as seeing each other without working it out. It saves compile time,
  not frame rate, and is only right where everything inside does see everything else.
- Break the view by design: a ridge, a tree line backed by a world blocker, a bend, a
  building across the end of a street.
- Keep the sky lid low and stepped to the rooflines.

## Props

| Class | Choose when | Costs |
| --- | --- | --- |
| `prop_static` | It never moves, animates or takes input | Cheapest. Baked light and shadow. No run-time entity |
| `prop_dynamic` | It animates, moves on a parent, or takes inputs | A run-time entity; lit from one point |
| `prop_dynamic_override` | A physics model must stand still | As above |
| `prop_physics` | Players or the world must move it | A run-time entity and physics. Use few; `prop_physics_multiplayer` in multiplayer |
| `prop_detail` | Grass and ground scatter from a material's detail type | Automatic, no collision |

- `describe_model` reports which classes a model works as. A model compiled without
  `$staticprop` is removed from the map if placed as `prop_static`; one with physics data
  is removed in some games. `check_for_problems` finds these, and with `fix` corrects them.
- Fade: set `fademindist` and `fademaxdist` on small props so they fade out with distance
  (a few hundred to about 2000 units for clutter; larger for furniture; `-1` and `0`
  to never fade for big landmarks). Fading small props is the single most effective
  measure on a dense map.
- Collision: decorative props nobody touches get `solid` 0 (not solid). Large props use
  their collision model (`solid` 6). Bounding-box collision (`solid` 2) only for boxy
  things.
- Shadows: turn off `disableshadows` only where needed; many small props casting baked
  shadows cost compile time for nothing.
- Prefer one model over forty brushes for complex shapes; prefer brushes for big flat
  architecture.

## Displacements

- Power 2 has 4 segments a side, power 3 has 8, power 4 has 16. Power 3 is the general
  choice; power 2 for near-flat ground; power 4 only where shape demands it.
- Displacements do not seal, do not block sight and do not cut leaves. Keep a nodraw
  world brush beneath them.
- Make a displacement from one face of a brush; the brush's other faces vanish in game.
- Keep base brushes rectangular or triangular, on grid, and matching edge to edge. Sewing
  only works between displacements that share a whole edge.
- Sew before every compile. Cracks show the void and leak light.
- They are cheap to draw for their triangle count and expensive in collision. Do not
  displace what could be a flat brush.
- Blend materials paint two (or four) textures by vertex alpha.

## Water

- Water is a world brush with a water material on the top face and `tools/toolsnodraw` on
  the rest. Do not tie it to `func_detail`.
- All water visible at once must be at one height, or the reflection breaks. Separate
  water at different heights with solid walls and corners.
- Expensive water reflects and refracts the scene; cheap water uses a cubemap. Use cheap
  water for anything distant or small, and place an `env_cubemap` above every water body.
- A `water_lod_control` sets the distance where expensive turns to cheap.
- Seal under water with a world brush floor; a displacement bed alone will leak.

## Clipping

A final map is clipped so movement is smooth and nobody leaves the play space.

- Ramp over stairs with a `tools/toolsplayerclip` wedge only where smooth movement
  matters; many games prefer real steps.
- Smooth every small obstruction along routes: trims, door frames, pipes, rubble and
  displacement edges get a clip that slopes over them or flattens the wall.
- Clip off prop tops, roofs, ledges and gaps players should not reach. Assume players
  will prop-climb, boost and surf anything that is not clipped.
- Clip brushes are simple, on grid, and never leave a lip a player can stand on unless
  intended.
- In sandbox and roleplay maps clip far less: players expect to build and climb. Clip
  only real exits from the world and things that trap players.

## Navigation

- Bots and nextbots use a navigation mesh. Generate in game with `sv_cheats 1` then
  `nav_generate`, check with `nav_edit 1`, save with `nav_save`, and ship the `.nav`
  beside the `.bsp`. Generation is slow on big maps; do it once the layout is final.
- Half-Life 2 style NPCs use AI nodes: `info_node` for ground, `info_node_hint` for
  behaviour, `info_node_air` for flyers, about 100 to 200 units apart with clear lines
  between them. The node graph rebuilds when the map changes.

## Compile presets

Use the user's presets (`list_compile_presets`). Create missing ones with
`set_compile_preset`. Four are worth having:

| Preset | VBSP | VVIS | VRAD | Use |
| --- | --- | --- | --- | --- |
| Fast | default | `-fast` | `-fast` (`-ldr` to halve the time) | Layout and scripting tests, many times a day |
| Entities only | `-onlyents` | off | off | Changed only entity keys or I/O. Seconds. Not for new brushes or lights |
| Full | default | full | `-both` | Honest visibility and light; stage reviews |
| Final | default | full | `-final -StaticPropLighting -StaticPropPolys -TextureShadows -both` | Release, with cubemaps and packing |

- Never judge performance on a `-fast` VVIS: it is far too generous about what sees what.
- Never release a `-fast` compile of anything.
- Watch compile times. A VVIS that takes hours means too many leaves: too much world
  geometry that should be `func_detail`, or wide open spaces with complex world brushes.

## Compile errors and fixes

| Message | Meaning and fix |
| --- | --- |
| `**** leaked ****` | Section "Sealing and leaks" |
| `Entity ... leaked!` naming a far-off entity | An entity lies outside the map; move or delete it |
| `Brush N: areaportal brush doesn't touch two areas` | The two sides are connected some other way, or the portal is inside a wall. Seal the other route or give it a portal |
| `Too many T-junctions to fix` | Dense intersecting detail. Simplify; turn fine brushwork into props; stop detail cutting into world faces |
| `MAX_MAP_BRUSHSIDES`, `MAX_MAP_PLANES`, `MAX_MAP_BRUSHES` | Too much brushwork. Replace complex brushwork with models; delete hidden brushes |
| `Too many displacements` or `MAX_MAP_DISPINFO` | Merge small displacements; use lower counts with higher power |
| `Vis decompression overrun` or huge VVIS times | Too many leaves. More `func_detail`, simpler world brushes |
| `Max leaf faces` | Shattered faces, usually from carving. Rebuild the area cleanly |
| `WARNING: Cluster portals saw into cluster` | Often from tiny or overlapping world brushes; usually harmless, but clean the spot it names |
| `Invalid solid` or a brush with coplanar or duplicate planes | `check_for_problems`, rebuild the brush |
| `Material ... not found` or a pink and black checker | Wrong material name or missing content. `preview_materials` on the name |
| `Too many light styles on a face` | Fewer named or styled lights overlapping |
| A prop is missing in game | Wrong prop class for the model. `check_for_problems` |
| `Error loading studio model` | Wrong model path |

`compile_status` gives each warning with the brush id or position it names: select the id
or move the camera there, look, and fix the cause before recompiling.

## Measuring in game

Needs `sv_cheats 1`.

| Command | Shows |
| --- | --- |
| `mat_wireframe 1` | Everything being drawn, through walls. The honest test of visibility |
| `mat_leafvis 1` | The leaf the camera is in; `3` shows every visible leaf |
| `r_drawportals 1` | Portal outlines between leaves |
| `r_lockpvs 1` | Freezes visibility so you can walk off and see what was drawn |
| `r_novis 1` | Draws everything: the "before" picture |
| `showbudget` | Frame time by system |
| `cl_showfps 1`, `net_graph 1` | Frame rate |
| `r_drawstaticprops 0`, `r_drawdetailprops 0`, `r_drawentities 0` | Turn a class off to see what it costs |
| `mat_fullbright 1`, `mat_luxels 1` | Unlit view; lightmap grid |
| `cl_leveloverview`, `noclip`, `getpos` | Overview and positions for notes |

Stand at the worst view in the map, the one that sees the most, and measure there. Fix
that view first. A map is as fast as its slowest common view.
