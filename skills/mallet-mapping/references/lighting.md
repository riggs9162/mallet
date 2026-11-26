# Lighting and atmosphere

Read this at Stage 5. Lighting does more for perceived quality than any other stage: a
plain room lit well beats a detailed room lit flat. Light on finished architecture, before
small props, so the light shapes the space rather than chasing clutter.

## Contents

- Workflow
- Sun and sky
- Artificial lights
- Lightmaps
- VRAD and final quality
- HDR and tonemapping
- Fog
- Colour correction
- Cubemaps
- Sound
- 3D skybox
- Lighting bugs and fixes
- Mistakes veterans call out

## Workflow

1. **Decide the mood in words** before placing a light: time of day, weather, warm or cold,
   safe or threatening. Find two or three reference photos for it.
2. **Pick the sky, then the sun.** The skybox texture fixes the sun's direction and colour.
   Set `light_environment` to match it, never the other way round.
3. **Global first.** Get sun, sky ambient and fog right with no other lights in the map.
   Outdoor areas should already read.
4. **Motivated sources.** Every pool of light needs something that would make it: a lamp
   prop, a window, a fire, a screen, a skylight. Place the source model first, the light
   entity second.
5. **Key, fill, accent.** One dominant light per space sets direction and shadow. Bounce
   and a weak fill keep the dark side readable. Small accents pick out the focal point,
   doors and the way onward.
6. **Contrast.** Leave real darkness between pools of light. Even light everywhere is the
   look of an unlit map. Aim for a clear brightest thing and a clear darkest thing in each
   view, with playable floor never fully black.
7. **Colour pairs.** Warm against cool reads as depth: warm interior lamps (about
   `254 216 146`) against cool sky or moonlight, or cool fluorescent or Combine light
   (about `147 226 240`) against warm sun. Never use pure `255 255 255`; it renders grey
   and dead. Nudge every light a little warm or a little cool.
8. **Guide with light.** Players move toward the brightest thing they can see. Light
   doors, exits, pickups and landmarks; keep dead ends dimmer.
9. **Check in Mallet's lighting preview** (`set_viewport_type` `lighting`, then
   `screenshot`), and read reach and brightness with `list_lights`. Compile with a fast
   preset to confirm bounce light, then judge in game: bounce, lightmap resolution and
   tonemapping only fully show there.

## Sun and sky

`light_environment`: one per map. VRAD uses the first it finds.

| Key | Meaning |
| --- | --- |
| `_light` | Direct sun: `R G B brightness`. Typical daytime 200 to 600; dusk and overcast lower |
| `_ambient` | Sky light filling the shadows: `R G B brightness`. Tint it toward the sky colour (blue by day), never the sun colour |
| `pitch` | Sun height. **Negative points down**: `-90` is straight overhead, `-20` a low sun. VRAD reads this key, not the pitch in `angles` |
| `angles` | Only the yaw (middle number) matters: the compass direction the light travels |
| `SunSpreadAngle` | Shadow softness. 0 gives razor edges; about 5 is a sound sunny default; larger for haze and overcast |
| `_lightHDR`, `_ambientHDR` | `-1 -1 -1 1` means same as LDR. Leave unless HDR needs its own balance |

- Match `env_sun` (if used) and `shadow_control` to the same direction, or the sun disc and
  dynamic shadows will disagree with the baked light.
- Shadows should be long enough to give shape: a sun between 30 and 60 degrees above the
  horizon (`pitch` -30 to -60) models buildings better than noon.
- The ambient term is what keeps shadows from going black. Too high and the map goes flat;
  start near a quarter to a half of the sun's brightness and tune by eye.
- Every face meant to show sky uses `tools/toolsskybox`, and world properties `skyname`
  names the sky. Preview a sky in game with `sv_skyname`.
- Night maps still need a "sun": a dim blue moon (`_light` near `120 150 200 40`) so shapes
  keep direction, with artificial lights doing the real work.

## Artificial lights

- `light` (omni), `light_spot` (cone), and texture lights from `lights.rad` (a material
  that emits light from every face using it; `list_lights` reports which of the map's
  materials do).
- `_light` is `R G B brightness`. With the default quadratic falloff a brightness of
  200 lights a small room; values in the thousands are a sign the falloff model is wrong
  for the job.
- **Falloff.** Pure quadratic is physically right but leaves a hot spot near the lamp and
  a short reach. Prefer the `_fifty_percent_distance` and `_zero_percent_distance` keys:
  they say where the light is at half strength and where it ends, they override the
  constant, linear and quadratic keys when set, and they make reach predictable.
- **Spots.** `_inner_cone` is the full-bright core and `_cone` the outer edge, in degrees,
  up to 90. A wide gap between them gives a soft edge. Aim with `angles` and `pitch`.
  Spots suit ceiling fixtures, street lamps and wall washes; an omni light in a ceiling
  fixture lights the ceiling above itself, which looks wrong.
- **Place lights off surfaces.** Keep an omni at least 8 to 16 units from the nearest face
  or it burns a hot spot.
- **Switchable and flickering lights** (named lights, or a non-normal `style`) each add a
  lightmap layer to every face they touch. Keep them few and never let more than a couple
  overlap, or VRAD reports too many light styles on a face.
- **Show the glow.** A bright fixture needs an `env_sprite` glow or a `point_spotlight`
  beam and halo, and a self-illuminated lamp material, or the source looks unlit while the
  room is bright.
- **Dynamic lights** (`light_dynamic`, `env_projectedtexture`) cost every frame. Use them
  for a hero moment, not for general lighting. Garry's Mod players also bring their own
  flashlights and lamps.

## Lightmaps

Lightmap scale is units per luxel, per face. Default 16.

| Scale | Use |
| --- | --- |
| 4 to 8 | Faces that catch a hard, important shadow: under railings, window bars, gratings, a hero wall |
| 16 | Default. Most faces |
| 32 to 64 | Large faces with soft or no shadows: big roofs, far walls, ceilings in the dark, 3D skybox |
| 128 | Faces nobody sees closely |

- Lower numbers cost file size, memory and compile time quickly; halving the scale
  quadruples the luxels. Sharpen only where a shadow actually falls.
- Keep to powers of two, and keep neighbouring faces within one step of each other or the
  change shows as a seam.
- Set it with `set_faces` `lightmap_scale`. `check_for_problems` reports a scale of 0.

## VRAD and final quality

| Flag | Effect |
| --- | --- |
| `-fast` | Rough preview. Never release with it |
| `-final` | More sky and bounce samples: far less blotching indoors under sky light. Slow |
| `-both` | Bakes LDR and HDR. Use for release unless the game is HDR only |
| `-ldr`, `-hdr` | Bake only one |
| `-StaticPropLighting` | Per-vertex baked light on `prop_static`. Without it props are lit from one sample and look flat or black |
| `-StaticPropPolys` | Props cast shadows from their real triangles, not their collision hull |
| `-TextureShadows` | Alpha-tested textures (fences, grates, foliage) cast patterned shadows; needs `-StaticPropPolys` and the materials listed in `lights.rad` with `forcetextureshadow` |
| `-bounce N` | Light bounces; default 100 |
| `-StaticPropBounce`, `-ambientocclusion`, `-aoscale` | Extras in newer or community compilers; check the compile log accepts them |

A release bake is `-final -StaticPropLighting -StaticPropPolys -TextureShadows -both`.
Community compilers add more; keep whatever the user's final preset already carries.

## HDR and tonemapping

- Place one `env_tonemap_controller` and drive it from a `logic_auto` `OnMapSpawn`:
  `SetAutoExposureMin`, `SetAutoExposureMax` and `SetBloomScale`. Without it the engine's
  defaults (exposure 0.5 to 2, bloom 1) swing brightness as players turn toward the sky.
- Typical starting points: exposure min 0.7 to 0.9, max 1.2 to 1.6, bloom 0.2 to 0.5. A
  narrow range keeps the designed contrast; a wide range lets the eye adapt between a dark
  interior and a bright street.
- If HDR looks blown out, lower the sun's HDR brightness or the exposure maximum. Do not
  compensate by darkening every light.

## Fog

`env_fog_controller`: `fogenable`, `fogcolor`, `fogstart`, `fogend`, `fogmaxdensity`,
`farz`.

- **Colour** matches the horizon band of the skybox, sampled from the sky texture. Wrong
  fog colour draws a line where the world meets the sky.
- **Start and end** set the mood: a far, thin fog (start 1000s, max density 0.3 to 0.6)
  gives air and depth to an outdoor map; a near, dense fog gives menace and hides a short
  draw distance.
- **`farz`** stops drawing beyond a distance and is a real optimisation on large open
  maps. Set it just past `fogend` with `fogmaxdensity` 1, or geometry pops out visibly.
- The 3D skybox has its own fog on `sky_camera`. Give it the same colour, and scale its
  distances to match, or the skybox looks pasted on.
- Fog also gives atmospheric perspective for composition: distant shapes lose contrast, so
  the foreground reads.

## Colour correction

A `color_correction` entity with a `.raw` lookup table grades the final image: the last
ten percent that unifies a palette. Use a subtle grade (lift shadows toward a hue, pull
saturation slightly) and keep it weak. Grade after lighting is final, never to fix it.
`color_correction_volume` grades per area. The lookup file is custom content and must be
packed.

## Cubemaps

Reflective materials sample the nearest `env_cubemap`. Without built cubemaps, reflective
surfaces show the skybox or a pink and black checker.

- Place at **eye level, 64 units above the floor**, away from walls and props.
- One per distinct space: each room, each end and bend of a corridor, each outdoor area,
  both sides of every doorway where the light differs. Add one wherever the lighting or
  colour changes sharply.
- For a key reflective surface (a polished floor, a window), assign the entity to those
  faces with its brush faces key, so they do not pick a wrong neighbour.
- Leave the size at default unless a hero surface needs more; large sizes bloat the BSP.
- Build after the final compile and again after every recompile: in game with
  `buildcubemaps` (once in LDR and once in HDR for a `-both` map, reloading between), or
  with a cubemap compile step if the preset has one (`source_cubemap`).

## Sound

Silence makes a map feel like an editor. See `scripting-and-polish.md` for I/O; the rules
here are spatial.

- `env_soundscape` plays when the player is inside its radius **and** has line of sight to
  it; the newest one triggered keeps playing until another takes over.
- One per acoustic space, placed where a player entering sees it straight away. A
  soundscape must cover every spawn point or players spawn into silence.
- `list_soundscapes` shows what the game defines, `describe_soundscape` what one plays and
  which position entities it needs, and `soundscape_at` which soundscape a player at a
  point would hear. Probe every room and spawn with `soundscape_at` to find uncovered
  places.
- Add `ambient_generic` point sounds for sources the player can see: a humming
  transformer, a dripping pipe, a radio.

## 3D skybox

The world beyond the playable area, built at 1/16 scale in a sealed box elsewhere in the
map, around a `sky_camera` (`scale` 16).

- The `sky_camera` position corresponds to the point in the main map where the two line
  up: copy the surroundings, scale by 1/16 around a reference point, and put the camera at
  that reference point's scaled position.
- Use it for distant buildings, mountains, tree lines, the far bank of a river, cranes on
  the horizon. It costs little and removes the "floating box" look.
- Only one `sky_camera`. No players, NPCs or physics in it. Keep lightmap scale high (64).
- Faces in the main map that should show through to it use `tools/toolsskybox`.

## Lighting bugs and fixes

| Symptom | Cause and fix |
| --- | --- |
| Whole map fullbright | No light entities, or VRAD did not run or failed (a leak stops it). Fix the leak; read `compile_status` |
| Props pitch black | Compile lacks `-StaticPropLighting`, or the prop's origin is inside a brush. Add the flag; move the prop or point it at an `info_lighting` |
| Props glowing or lit wrong | The origin samples a bright or dark spot. Use `info_lighting` as the sample point |
| Light leaking under walls | Luxels straddle a wall on a continuous floor. Split the floor brush at the wall, or lower its lightmap scale, or add a `tools/toolsblocklight` brush |
| Blotchy indoor shadows | `-fast`, or no `-final`. Compile final before judging |
| Jagged, stair-stepped shadows | Lightmap scale too coarse for a hard shadow: lower the scale on that face or raise `SunSpreadAngle` |
| Black or mismatched displacement edges | Unsewn displacements, or a displacement sunk into a brush |
| Hard seams between faces | Mismatched lightmap scales, or smoothing groups. Even the scales out |
| Pink and black reflections | Cubemaps not built, or none in range |
| Screen brightness pumping | No tonemap controller; exposure range too wide |
| "Too many light styles on a face" | Too many named or styled lights overlap. Remove names or merge lights |

## Mistakes veterans call out

- Raising ambient until nothing is dark. Flat light is the mark of an unlit map.
- Pure white light, and one colour of light for a whole map.
- Light with no visible source, or a lamp prop with no light.
- Lights so bright they clip surfaces to white near the source.
- Judging light in the editor only, or from a `-fast` bake.
- Forgetting cubemaps, so every reflective surface is wrong.
- A sun that disagrees with the skybox's painted sun.
- Fog whose colour does not match the sky.
- Lighting before the architecture is settled, then moving walls and re-lighting.
