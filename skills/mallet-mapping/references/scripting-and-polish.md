# Entities, ambience, playtesting and release

Read this at Stage 3 (gameplay entities), Stage 6 (ambience) and Stage 8 (release).

## Contents

- Entity I/O
- Doors, buttons, lifts and triggers
- Making a map feel alive
- Garry's Mod maps
- Playtesting
- Versioning
- Packing and publishing

## Entity I/O

An output on one entity fires an input on another: `OnPressed` on a button fires `Open` on
a door. Read both ends with `describe_entity_class` before wiring, since names differ by
class and game.

- **Name by area, thing, role**: `lobby_door_main`, `lobby_btn_alarm`,
  `lobby_relay_alarm_on`. Names are how you debug at two in the morning.
- **Relays are functions.** Route anything used from more than one place through a
  `logic_relay`: sources fire `Trigger` on the relay and the relay holds the list of what
  happens. Changing behaviour then means editing one entity.
- **Start-up logic** lives on one `logic_auto` (`OnMapSpawn`) per map or area.
- **Keep chains short and flat.** A button firing a relay firing ten targets is easy to
  follow; a chain of eight entities each firing the next is not.
- **Delays** belong on the output that needs them, and long timed sequences go through a
  relay so `CancelPending` can stop them. Never have a relay cancel itself.
- **Wildcards** (`lobby_light_*`) fire many entities with one output.
- **State** goes in `logic_branch`, `math_counter` or `logic_case`, not in guesses about
  whether a door is open.
- `set_entity_outputs` replaces an entity's whole list: read with `get_entity`, edit, and
  write the full list back. `check_for_problems` reports outputs that target names that
  do not exist.
- **Debug in game**: `developer 2` logs every output as it fires; `ent_messages_draw 1`
  draws them in the world; `ent_fire name Input` fires one by hand; `ent_text` shows an
  entity's state. An entities-only compile turns an I/O fix round in seconds.
- Every logic entity is a run-time entity. On servers near the limit, fewer, smarter
  relays matter.

## Doors, buttons, lifts and triggers

- **`prop_door_rotating`** is the standard hinged door: 48 x 108 models whose origin sits
  on the hinge. Leave clearance for the swing, and never put the frame's collision in
  its path. It will not work as `prop_static`.
- **`func_door`** slides, **`func_door_rotating`** swings a brush door and needs an
  `origin` brush (`tools/toolsorigin`) on the hinge line. Set a small `lip` so a sliding
  door does not vanish fully into the wall, and decide what happens when blocked: reverse,
  not crush, unless crushing is the point.
- **Door frames**: make the opening a few units wider than the door, and give thick
  frames a sloped player clip so nobody snags on the edge.
- **Areaportal** every door between areas and link it to the door.
- **`func_button`** for brush buttons and `prop_dynamic` plus a `func_button` textured
  with a tool material for model buttons. Set a `wait` so it cannot be spammed.
- **Lifts**: `func_tracktrain` on `path_track` is the robust choice for a lift with
  several floors, since each track fires `OnPass`. `func_movelinear` suits a simple
  two-position platform. Avoid parenting movers to movers. Give lifts a floor trigger and
  blocked behaviour, and make the shaft wider than the car by a few units.
- **Triggers** are brushes textured `tools/toolstrigger` on every face. Set the filter
  flags (clients, NPCs, physics) to what should fire it, and use `filter_*` entities for
  teams or names. `trigger_once` fires once; `trigger_multiple` re-fires after `wait`.
- **Breakables**: `func_breakable` and `func_breakable_surf` for glass. Give glass a
  thickness of 1 to 2 units and its own material type.
- **Moving brushes** need an origin, do not cast baked shadows well, and should be lit
  from a sensible spot. Keep them simple.
- **Ladders, water, kill and teleport volumes**: test each from every direction a player
  can arrive.

## Making a map feel alive

A still map reads as an editor screenshot. Add motion and sound in every area, sparingly.

- **Soundscapes**: one per acoustic space, covering every spawn. See `lighting.md` and
  probe with `soundscape_at`.
- **Point sounds**: `ambient_generic` on visible sources (machines, radios, drips, fires,
  flickering lamps). Keep the count modest.
- **Light glows**: `env_sprite` at each bright lamp; `point_spotlight` for a beam and
  halo on searchlights and street lamps.
- **Air**: `func_dustmotes` in shafts of sunlight; `env_steam` at vents and broken pipes;
  `func_smokevolume` or particle systems for haze; `env_fire` for flames.
- **Weather**: `func_precipitation` volumes over outdoor areas only, with matching
  soundscape, wet materials and darker sky.
- **Motion**: fans (`func_rotating`), swinging lamps, flickering or pulsing lights
  (light `style`), flags and cloth, a passing train in the 3D skybox, birds, water flow,
  screens with animated textures.
- **Wind**: one `env_wind` so foliage, ropes and dust move together.
- **Ropes and cables**: `move_rope` and `keyframe_rope` between poles; cheap and they
  sell an outdoor scene.

Each of these is cheap alone and expensive in bulk. Put the liveliest thing at the focal
point and let the rest of the area stay calm.

## Garry's Mod maps

- **Prefixes**: `gm_` sandbox, `rp_` roleplay, `ttt_` Trouble in Terrorist Town, `ph_`
  Prop Hunt, `zs_` Zombie Survival, `ze_` escape, `surf_`, `bhop_`, `dr_` deathrun.
  The file name is the map's name in game, lower case with underscores.
- **Spawns**: plenty of `info_player_start`, upright, on the floor, with a full hull of
  clear space each and no line of fire between them. Dozens for a roleplay or sandbox
  server. Trouble in Terrorist Town wants 32 or more spawns plus its own weapon and ammo
  entities (`ttt_random_weapon`, `ttt_random_ammo`) and usually a `ttt_map_settings`.
- **Entity headroom**: the server limit is 8192 networked entities shared with every prop
  players spawn. Ship lean: static props over dynamic, few logic entities, fade
  everything. Check the count with `list_entities` `summary: true`.
- **Content**: Garry's Mod mounts Half-Life 2 content for everyone. Counter-Strike:
  Source and the episodes are only present if the player owns and mounts them. Anything
  from those games, or custom, must be packed into the map or shipped as a required
  addon, or players see pink checkers and ERROR models. `preview_materials` and
  `describe_model` report where each asset comes from.
- **Sandbox** maps need: large flat build areas, at least one big indoor space, water,
  a long straight for vehicles, height for flying, a dark room for lighting tests, and
  very little clipping. Players praise restraint: a few well-made spaces over a sprawl of
  empty ones.
- **Roleplay** maps need a town that works as a place:
  - Civic core: police or enforcement building with cells, armoury and garage; a
    government or administration building; a hospital; a fire station where relevant.
  - Homes: many apartments of different sizes, each a lockable unit with one clear front
    door, a window and room to furnish.
  - Trade: shops with a counter, a back room and a street door; a market or plaza;
    a bank; a bar or café.
  - Industry and outskirts: warehouses with large doors, a factory or workplace for jobs,
    a rail or bus station, sewers or tunnels as a second layer, outlands or slums.
  - A central plaza as the meeting point, readable from every main street.
  - Doors the gamemode can own: `prop_door_rotating` and `func_door` are recognised as
    ownable by most frameworks; avoid one-off scripted door contraptions on player homes.
  - Every interior reachable and enterable. Fake facades are fine only where clearly not
    a door.
  - Sightlines broken between districts, both for frame rate and so areas feel distinct.
  - Admin room or sit room away from play, and spots for job spawns.
  - Compact over huge: a town where players meet beats a map too wide to find anyone.
- **Half-Life 2 roleplay** adds: a train station arrival with a checkpoint, a nexus or
  citadel base with restricted tiers, ration distribution, apartment blocks, a plaza
  with a screen, an industrial district, sewers and canals, and a contrast between the
  occupier's cold blue-white metal and the old city's warm, worn brick.
- **Lua-driven entities**: custom entities from a gamemode can be placed if the FGD
  defines them (`set_fgds`), and work only on servers running that gamemode.
- **Navigation**: generate a nav mesh if nextbots or NPC addons are expected.
- **Workshop**: an addon folder with `maps/name.bsp`, a `maps/thumb/name.png` thumbnail
  and an `addon.json` of type `map` with up to two tags, packed with `gmad` and uploaded
  with `gmpublish` alongside a 512 x 512 JPEG icon. Publishing is the user's action:
  prepare the folder and stop.

## Playtesting

A stage is not passed until someone has played it. The agent's own review comes first,
real players second, and they find different things.

**Agent review, every stage:**

- Screenshots from player eye height along each route, both directions.
- `trace_line` for scale, sightlines and prop placement.
- `check_for_problems`, then a compile, then `compile_status` read in full.
- A walk in game where the tools allow it; otherwise say plainly that it was not played.

**Player tests, at the gates:**

- Blockout: can players find their way without help, where do they get lost or stuck,
  which routes go unused, how long do rounds or trips take, what feels cramped or empty.
- Art pass: can players still read the layout, are there dark spots, do props snag
  movement, does anything read as a doorway that is not.
- Release candidate: frame rate with a full server, exploits, out-of-bounds, missing
  content on a clean install, spawn problems.

Method:

- Do not explain the map. Watch where testers go and note where they hesitate.
- Change one thing between tests where you can, and keep a list of what was changed.
- Record a demo and review deaths, stalemates and ignored areas.
- Sort feedback into must fix, should fix and opinion. Three people reporting the same
  thing is a fact; one is a lead.
- Test early and often: a rough map tested ten times beats a polished one tested once.

## Versioning

Community convention, and servers refuse to load two different files of one name, so
never overwrite a released version:

- `name_a1`, `a2`...: alpha. Dev textures. Layout testing.
- `name_b1`, `b2`...: beta. Art, lighting and optimisation in progress. Layout locked.
- `name_rc1`...: release candidate. Finished unless testing finds faults.
- `name_v1` or `name`: final. Later changes become `v2`.

Compile under the release name: cubemaps and packed files are stored by map name, and a
renamed `.bsp` loses them. Keep a short changelog per version.

## Packing and publishing

- Custom materials, models, sounds, soundscape scripts, detail sprites, colour correction
  files and particle manifests must travel with the map: packed into the `.bsp` or
  shipped in the addon.
- Pack with the compile preset's packing step if it has one, or with `bspzip` and a file
  list. Compile Pal automates finding dependencies. Pack **after** cubemaps are built,
  and compress last (a `repack` step).
- Build cubemaps on the final `.bsp` before packing and compressing.
- Test the packed map on a clean install or a second machine with no source content:
  this is the only real check for missing assets.
- Release with: screenshots that are honest about the map, a thumbnail, a description of
  size, intended gamemode and player count, required games or addons, credits for every
  borrowed asset, and the changelog. Never ship the `.vmf` unless the author wants to.
- Uploading, posting and announcing are outward-facing actions. Prepare everything and
  ask the user before any of it.
