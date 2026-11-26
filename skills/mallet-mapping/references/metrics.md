# Metrics: scale, grid and dimensions

Wrong scale is the hardest fault to fix late and the easiest to prevent early. Build to
these numbers from the first brush, and keep a player-sized block in every screenshot of
the blockout.

## Contents

- Units
- The player
- Movement limits
- Architecture
- Grid discipline
- Dev textures
- Layout numbers
- Engine limits

## Units

- Architecture scale: **16 units = 1 foot**, 1 unit = 0.75 inch = 1.905 cm, about 52.5
  units to the metre. Use this for buildings, streets and terrain.
- Character and prop models are authored nearer 12 units to the foot, so the 72 unit
  player is a six-foot person who looks short beside real-world architecture measured at
  16 per foot. This is why Source doors and ceilings are built oversize. When reference
  measurements and the player disagree, the player wins.
- Z is up. Yaw 0 looks along +X.

## The player

| Measure | Units |
| --- | --- |
| Hull, standing | 32 x 32 x 72 |
| Hull, crouched | 32 x 32 x 36 |
| Eye height, standing | 64 |
| Eye height, crouched | 28 |
| Step height (walk up without jumping) | 18 |
| Steepest walkable slope | about 45 degrees |

Garry's Mod, Half-Life 2 and Counter-Strike: Source share this hull. Team Fortress 2
uses a wider hull (49 x 49 x 83). Take screenshots for review from 64 above the floor.

## Movement limits

Jump heights differ by game and gamemode, and a gamemode can change them. Treat these as
starting points and test the real number in game before relying on a jump.

| Game | Jump | Crouch-jump |
| --- | --- | --- |
| Half-Life 2 | 21 | about 56 |
| Garry's Mod sandbox (jump power 200) | about 33 | about 64 |
| Counter-Strike: Source and GO | about 54 | about 64 |
| Team Fortress 2 | about 45 | about 70 |

Design rules that survive those differences:

- A ledge meant to be walked up: 16 or less (18 is the limit; 16 stays on the grid).
- A ledge meant to be jumped onto without skill: 32 or less in Half-Life 2 and Garry's
  Mod, 48 in Counter-Strike.
- A barrier meant to stop players: 96 or more, and clip above it. Players stack on props
  and on each other, so a wall that only just blocks a jump does not block.
- A gap a player must not fit through: narrower than 32 wide or lower than 36 high.
  A gap a crouched player should fit through: at least 40 high and 40 wide.
- Fall damage starts near 240 units of drop in Half-Life 2 rules; long drops need water,
  a slope or a ladder.

## Architecture

| Element | Units | Notes |
| --- | --- | --- |
| Door opening | 48 to 56 wide, 108 to 112 high | The stock door models are 48 x 108 |
| Double door | 96 to 112 wide | |
| Minimum corridor | 64 wide | One player; feels tight |
| Comfortable corridor | 96 to 128 wide | Two players pass |
| Main route or street lane | 192 to 256 wide | Vehicles need 256 or more |
| Interior ceiling | 112 to 128 | 96 feels low; 144 to 192 for halls |
| Storey height, floor to floor | 128 to 160 | Ceiling plus slab |
| Stair step | rise 8, run 12 | Up to rise 8, run 16 for grand stairs; never rise above 16 |
| Stair width | 64 minimum, 96 to 128 comfortable | |
| Ramp | 1 up for 2 along | Never steeper than 1 for 1 |
| Railing and low cover | 32 to 40 | Crouch cover is 36 to 48 |
| Standing cover | 72 to 80 | |
| Window sill | 32 to 40 | Window head near door height |
| Table | 30 to 32 | Counter 36 to 40 |
| Interior wall | 8 to 16 thick | |
| Exterior and structural wall | 16 to 32 thick | |
| Floor slab | 16 to 32 thick | |
| Ladder | 32 wide | `func_useableladder` or a ladder brush by game |

Rooms sized from reference at 16 units per foot often feel right for volume but too small
for combat or crowds. For roleplay and sandbox maps, add about a quarter to interior
floor area over the real-world size, and keep ceilings generous.

## Grid discipline

- Work on powers of two. Layout on 64 and 32; architecture on 16 and 8; trims and detail
  on 4 and 2; grid 1 only for a deliberate exception. `set_grid` sets it.
- Every vertex of a world brush sits on the grid. Off-grid vertices cause micro gaps,
  leaks, invalid solids and lighting seams, and they multiply when copied.
  `snap_selection_to_grid` repairs moved brushes; rotated brushes need rebuilding.
- Rotate world geometry by 90 and 45 degrees only. Arbitrary rotations put vertices off
  grid; build angled walls with wedges or the clipping tool between grid points instead,
  and keep free rotation for props.
- Butt brushes against each other; never overlap world brushes and never leave slivers.
- Avoid brushes thinner than 1 unit and faces smaller than a few units across.
- Keep brushes convex and simple. Many simple brushes beat one clever one.
- Do not carve, and do not keep hollowed boxes. See `mallet-workflow.md`.
- Build big surfaces as big brushes. A floor is one brush, not a tiled field of small
  ones, unless a split is wanted for lightmaps or visibility.

## Dev textures

The dev-texture cycle exists so layout can be judged and changed without art in the way.
Dev materials carry a measuring grid, so scale reads straight off the wall.

| Material | Use |
| --- | --- |
| `dev/dev_measuregeneric01` | Orange grid: walls |
| `dev/dev_measuregeneric01b` | Grey grid: floors and ceilings |
| `dev/dev_measurewall01a` | Wall with printed heights and a player figure: one per room as a scale check |
| `dev/dev_measuredoor01`, `dev_measurestairs01a`, `dev_measureladder01`, `dev_measurerails01` | Stand-ins that say what a brush will become |
| `dev/dev_measurecrate01`, `dev_measurecover01`, `dev_measurebarrel`, `dev_measuredesk`, `dev_measurecounter` | Cover and furniture stand-ins |
| `dev/dev_measuregeneric_blue01` and the red, green, yellow, purple, brown, white, black set | Colour coding |
| `dev/dev_measuregeneric_glass01a` | Glass stand-in |
| `dev/reflectivity_10` to `reflectivity_90` | Flat greys of known brightness for lighting tests |

- Orange walls and grey floors is the community convention inherited from Valve's own
  blockouts: it separates vertical from horizontal at a glance and gives a neutral
  mid-tone for early lighting. Keep to it unless colour is carrying meaning.
- Use colour to carry meaning and write the key in a visgroup name or a note: one colour
  per team or faction, one for interactive things, one for out-of-bounds, one for "will
  be a prop", one for water or hazard.
- Keep dev textures at scale 0.25 and world-aligned so the grid on the wall is the real
  grid. Never stretch or fit them: the grid is the measurement. One tile covers the
  texture's pixel size times the scale (a 512 pixel texture at 0.25 is 128 units; a 128
  pixel one is 32), and games ship these at different sizes, so read the size with
  `describe_material` before measuring by eye.
- List what exists with `list_materials` `filter: "dev/"`, and look with
  `preview_materials`. Games differ in which dev materials they ship.
- The blockout is done with dev textures only. The first real texture goes on after the
  layout gate passes, not before.

## Layout numbers

Starting points to test, not laws.

- A player runs about 190 to 250 units a second depending on game: 10 seconds of running
  is roughly 2000 to 2500 units. Time routes, do not measure them.
- Competitive: attackers and defenders should reach a contested point within a second or
  two of each other. Two or three routes to each objective; a fourth usually cannot be
  defended.
- A safe or transitional area should take no more than about 30 seconds to cross.
- Break any straight sightline over about 2000 units unless long-range play is intended.
  Check with `trace_line` between eye points.
- Each area has at least two exits. Dead ends need a reason (a reward, a hiding place, a
  room with a purpose).
- Add a height change at least once per area: players remember and use vertical space.
- Leaf splits fall every 1024 units; aligning big walls and floors to 1024 lines makes
  visibility cleaner for free.

## Engine limits

The map must fit in ±16384 units on each axis. Compile limits depend on the game and the
compile tools; the log prints usage against each limit, and a `vbspinfo` step lists them.
Stock Source 2013 figures, with Garry's Mod's raised figures where they differ:

| Limit | Stock | Garry's Mod |
| --- | --- | --- |
| Brushes | 8192 | 65535 |
| Brush sides | 65536 | 65536 |
| Faces per brush | 128 | 128 |
| Displacements | 2048 | 5500 |
| Overlays | 512 to 1024 | 8192 |
| Networked entities at run time (edicts) | 2048 | 8192 |
| Static prop models, texture data and others | See the compile log | Raised in several updates |

Budget, do not approach, the run-time entity limit: on a Garry's Mod server every
player-spawned prop takes from the same pool, so a map that ships using a third of it is
already heavy. `prop_static`, `func_detail` and plain lights do not use run-time
entities; `prop_physics`, `prop_dynamic`, doors, triggers, logic and named lights do.
Count with `list_entities` `summary: true`.
