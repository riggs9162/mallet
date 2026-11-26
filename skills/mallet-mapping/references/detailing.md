# Art pass: architecture, texturing, composition, story

Read this at Stage 4 (architecture) and Stage 6 (detail). The dev-texture stages come
first for a reason: detail laid over a weak layout is wasted, and detail laid before the
layout is locked gets thrown away.

## Contents

- Three sizes of detail
- Order of operations
- Architecture that reads as built
- Texturing
- Composition and readability
- Theme and environmental storytelling
- Amateur tells
- The restraint rule

## Three sizes of detail

Work big to small, and finish each size across the whole map before starting the next.

| Size | What it is | Read from | Built with |
| --- | --- | --- | --- |
| Primary | Massing: building volumes, room shapes, skyline, the big light and dark | Across the map | World brushes, displacements |
| Secondary | Structure: columns, beams, trims, door and window frames, stairs, railings, pipe runs, large props | Across a room | `func_detail`, large `prop_static` |
| Tertiary | Surface: decals, overlays, small props, wires, litter, stains | Up close | Overlays, decals, small props |

A map with strong primary shapes and no tertiary detail looks clean. A map with rich
tertiary detail on weak primary shapes looks like a decorated box. If a screenshot at
quarter size (`screenshot` with `scale: 0.25`) does not read, no amount of small detail
will rescue it: go back up a size.

## Order of operations

1. **Plan from the player's eyes.** Stand at each entrance and each place players stop.
   Detail what is seen from there; spend almost nothing on roofs nobody reaches and backs
   nobody sees.
2. **Structure.** Thicken walls, add the supports a real building would need, frame every
   opening, break up long flat runs.
3. **Seams.** Hide every texture seam and brush joint with a trim, a frame or a pillar.
4. **Hero props**, then filler props.
5. **Overlays and decals** last, where surfaces meet and where use would wear them.
6. Tie detail brushes to `func_detail` as you make them, compile, and look in game after
   each step. Detail judged only in the editor is detail judged under the wrong light.

## Architecture that reads as built

- **Walls have thickness.** 8 to 16 units for interior partitions, 16 to 32 for exterior
  and structural walls. A doorway is a hole through that thickness with a frame around it,
  not a gap between two paper planes.
- **Show what holds it up.** Beams under long ceilings, columns under beams, brackets under
  balconies, lintels over openings. Add supports before decoration: a bland wall is fixed
  first with structure, then with stains.
- **Trim every joint.** Skirting where wall meets floor, cornice or beam where wall meets
  ceiling, a frame at every material change. Trims are thin brushes (4 to 16 units) and are
  the single cheapest step from blockout to building.
- **Break the box.** Recess a bay, chamfer a corner, step a facade forward, drop a ceiling
  section, raise a floor section. Mix 90 with 45 degree walls; keep odd angles rare, since
  they make every later brush harder to fit and keep on the grid.
- **Treat the ceiling.** Flat single-texture ceilings are the most common unfinished
  surface. Give it beams, ducts, light fixtures, a coffer, a skylight or exposed structure.
- **Vary height.** A map on one plane is flat to play and flat to look at. Give each area
  a level change seen from above and from below.
- **Give rooms a job.** Decide who uses the room and what they need in it. A building that
  has a toilet, a store room, a way to bring goods in and somewhere to put the rubbish
  feels real even when the player never notices why.
- **Silhouette.** Squint at the skyline: chimneys, antennas, water tanks, towers and
  stepped roofs give an outline worth looking at. A flat roofline is a blockout tell.
- **Scale check.** Keep a 32 x 32 x 72 reference block, or a door, in view. Oversized rooms
  that make players look tiny are a classic beginner result; when unsure, build smaller.

## Texturing

- **Scale.** The default 0.25 puts a 512 pixel texture across 128 units. Keep one texel
  density across the map: 0.25 for 512 textures, 0.125 for 1024 textures of the same
  surface, 0.5 only for far-off or 3D skybox faces. Read the pixel size with
  `describe_material` rather than guessing.
- **Alignment.** Align to `world` for walls on the grid so neighbours tile into each other
  for free. Align to `face` on slanted and rotated brushes, which world alignment stretches.
  Use `treat_as_one` to wrap one texture around a corner or along a curved wall.
- **Fit sparingly.** `justify: "fit"` is for trims, doors, signs and panels made to be seen
  whole. Fitting a tiling brick texture to a wall stretches it and breaks texel density.
- **Palette.** Pick the materials for an area before detailing: about two wall materials,
  one or two floors, one ceiling, two or three trims, one accent. Hold the whole map to a
  short list and audit it with `list_materials` `used_only: true`. Fifty one-off materials
  read as noise; twelve used with intent read as a place.
- **Materials end at a reason.** Never change material in the middle of a flat wall with
  nothing between. Put a trim, a pilaster, a pipe or a change of plane at the join.
- **Break repetition.** A tiling texture across a long wall shows its repeat. Interrupt it
  with structure every 128 to 256 units, then with overlays (stains, cracks, posters), then
  with a second related material low on the wall.
- **Ground.** Use blend materials on displacements and paint the alpha by logic: dirt where
  people walk, grass where they do not, gravel at the edges of roads, mud where water sits.
- **Overlays and decals.** `create_overlay` for anything placed with care, sized and
  spanning faces: road markings, posters, stains, signs. `create_decal` for quick
  projections at the texture's own size. Overlays work on world brushes, `func_detail` and
  displacements, not on moving brush entities or models. Grime belongs where it would
  gather: under windows, below pipes and vents, in corners, at the foot of walls.

## Composition and readability

- **One focal point per view.** From each entrance decide what the eye should land on, then
  make it the brightest, most detailed, most saturated or tallest thing in frame, and keep
  it unobstructed. Everything else in that view recedes.
- **Landmarks.** Each area needs one feature nameable in two words (clock tower, red crane,
  broken bridge) and visible from several places. Height and contrast make a landmark;
  players navigate and call positions by them.
- **Frame and lead.** Darker simple foreground, lit middle ground, hazy low-detail
  background. Pipes, railings, road lines, cables and rows of lights point where you want
  players to go.
- **Light pulls, dark hides.** Players walk toward light. Lit areas need more detail;
  shadowed areas can stay simple. See `lighting.md`.
- **Contrast between play space and dressing.** Floors, routes and cover stay clean and
  readable; ornament goes above head height and outside the route. In competitive spaces,
  remove any dark upright shape that could be mistaken for a player.
- **Density is uneven on purpose.** Roughly a third of a space carries most of the detail.
  Calm surfaces are what make the busy ones read.
- **Colour.** One dominant hue, one supporting, one accent, per area. Give neighbouring
  areas different colour themes so players know where they are without a sign.
- **Clip what you add.** Every piece of detail that sticks into a route gets a smooth
  `tools/toolsplayerclip` ramp or a non-solid setting, so nobody snags on it.

## Theme and environmental storytelling

- **Gather reference first**: architecture, environment, lighting, props. Two or three
  images per subject is enough; too many give a muddle of styles. Build from what the
  reference shows, not from memory of other games.
- **Commit to one theme** per map and a purpose per area. Write it in a sentence
  ("a river-side rail depot, half shut down, occupied by a checkpoint") and test every
  prop and material against it.
- **Every prop has a reason.** Ask who put it there, when, and why. A chair faces a
  window; a ladder leans where something is being fixed; crates sit near the door they came
  through. Clusters tell stories, rows do not: group props in odd numbers, vary rotation,
  overlap them, and leave empty floor around the group.
- **Show time.** New work against old work, repairs, boarded windows, wear paths, water
  stains below leaks, rust under metal, soot above fires.
- **Build the world past the wall.** The playable space should look like a piece of a
  larger place: a 3D skybox with the rest of the city or valley, dressed out-of-bounds
  streets behind fences, lit windows across the road, a road that continues past the
  barricade. A map that ends at a blank wall and a skybox feels like a box.

## Amateur tells

Scan every screenshot for these before calling a stage done.

- Props floating above or sunk into the floor; props clipping through walls.
- A room with one wall texture, one floor texture and a bare flat ceiling.
- Walls with no thickness at doors and windows; no frames, no trims.
- Only 90 degree corners; a perfectly symmetric layout; everything on one plane.
- Identical props in an evenly spaced row, all at the same rotation.
- Textures stretched, misaligned across a seam, or at mixed texel densities.
- A material change in the middle of a flat wall.
- Uniform brightness everywhere; light with no visible source; pure white lights.
- A large open space filled with scattered objects in place of a designed layout.
- Rooms far too large for the player; doors and stairs at giant or dwarf scale.
- A hard map edge: blank walls, visible void, skybox touching the ground.
- Detail everywhere at once, so nothing reads as important.

## The restraint rule

When a space feels unfinished, the instinct is to add props. First check the bigger
causes, in order: shape, structure, lighting, palette. Add small detail only when those
four are right, and stop as soon as the space reads. Exceptional maps are edited as much
as they are decorated.
