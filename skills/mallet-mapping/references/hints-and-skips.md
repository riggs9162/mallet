# Hints and skips

Hints are the one tool that lets the mapper, not the compiler, decide where visibility
ends. They are also the most misplaced tool in Source mapping: a hint in the wrong place
costs compile time and hides nothing. Read `optimization.md` first for how visibility
works; this file is the full treatment of hint and skip.

## Contents

- What hint and skip are
- The three facts that decide every hint
- When a hint is worth placing
- The sightline method
- Pattern: the corner
- Pattern: offset doors
- Pattern: the roofline
- Pattern: stairwells, pits and balconies
- Pattern: tidying a messy split
- When a hint cannot help
- Building hints in Mallet
- Workflow: one hint at a time
- Reading the result
- Mistakes
- Skip on its own

## What hint and skip are

- `tools/toolshint` on a brush face tells VBSP: cut the visleaves along this plane, and
  make this cut before the cuts chosen from ordinary world faces.
- `tools/toolsskip` on a brush face tells VBSP: this face does not exist. It makes no cut,
  no surface and no collision.
- A **hint brush** is a world brush whose faces carry only those two materials: hint on
  the face or faces that should cut, skip on all the rest.
- A hint brush is not drawn, not solid, casts no shadow, does not seal the map and does
  not block sight. It changes nothing but the shape of the leaves.

A hint does not hide anything by itself. It moves a leaf boundary, and the engine's
leaf-to-leaf test then gives a different answer. Every hint is a bet about that test, and
the bet is checked in game.

## The three facts that decide every hint

1. **Visibility is leaf to leaf.** If any point of leaf A has a straight line to any
   point of leaf C through the openings between them, everything in C draws while the
   camera is anywhere in A. One corner of A that pokes around a bend exposes all of A.
2. **Neighbours always see each other.** Two leaves that share a boundary see each other,
   whatever shape they are. A hint cannot hide a leaf from the leaf next to it. It works
   on the third leaf in a chain, A to B to C, by shaping B and the two boundaries so that
   no straight line passes through both.
3. **Only world geometry blocks.** The line between two leaves is blocked only by world
   brushes. `func_detail`, displacements, props and brush entities are transparent to
   VVIS. A hint next to a wall made of `func_detail` separates nothing.

From these follows the question to ask before any hint: *which leaf should stop seeing
which other leaf, and what world brush stands between them?* If the answer has no world
brush in it, the fix is a blocker, not a hint.

## When a hint is worth placing

Place a hint when all of these hold:

- Measurement shows a problem: `mat_wireframe 1` at a real player position draws a costly
  area that the player cannot see from there.
- A world brush stands between the player and that area, so a correct leaf shape would
  hide it.
- The costly area is worth hiding: a room full of props, a street, a long vista. Hiding
  an empty corridor saves nothing.
- A door between the two areas would not do the job better. A door that is usually shut
  wants an areaportal; a permanent open corner wants a hint. Many places want both.

Do not place hints by habit at every corner. Each hint adds leaves and portals, and VVIS
time rises faster than the portal count. A map with a dozen well-placed hints beats one
with two hundred.

## The sightline method

This is the one method behind every pattern below.

1. Stand, in plan view, in the area that should be hidden (the target), at the edge of
   its opening that is furthest from the viewer.
2. Draw a straight line from that edge past the **occluding edge**: the corner of the
   world brush that blocks the view.
3. Carry the line on until it hits a wall on the viewer's side.

That line is the limit of sight. Everything on the viewer's side of it, away from the
occluding edge, cannot see the target. A hint face on the plane of that line splits the
viewer's space into a part that sees the target and a part that does not, and only the
first part still draws it.

The hint face must reach from floor to ceiling and from wall to wall of the space it
cuts. A hint that stops short leaves the old leaf joined around its end, and the cut does
nothing.

## Pattern: the corner

An L-shaped corridor joins room A to room C. Without a hint VBSP cuts along one of the
wall planes, and one arm's leaf swallows the whole corner. Since the corner can see down
both arms, a player at the far end of that arm draws room C.

```
            |  room C  |
            +--+    +--+
               |    |
               | B  |              occluding edge = inner corner (o)
               |    |              limit of sight = line from the far edge of
   +-----------o    |                C's opening, through o, to the south wall
   |  A1     /      |
   | room A /   A2  |              hint on that line: A1 cannot see room C,
   +-------/--------+                A2 (the part that really sees it) still does
```

- Find the limit of sight with `trace_line` from the far edge of the target's opening
  through the inner corner; where it lands on the opposite wall is the other end of the
  hint.
- The well-known shortcut is a 45 degree hint from the inner corner to the outer corner.
  It is on the grid and easy to build, and it is right when both arms are about as long
  as they are wide. For a long arm the true line is steeper than 45 degrees; build to the
  line, snapped to the nearest grid points, erring towards the viewer's side.
- When the view must be limited in both directions (room A hidden from the C arm too),
  place a second hint from the same inner corner along the other limit line. The two
  hints form a V with its point on the inner corner.
- The hint touches the occluding edge exactly. A hint that misses the corner by a few
  units leaves a sliver of leaf that sees both ways.

## Pattern: offset doors

Two rooms open onto a corridor through doors that do not face each other. A player deep
in room 1 cannot see into room 3, but the leaves say otherwise, because the corridor is
one leaf that touches both doors.

```
   +--------+        +-----------------+
   | room 1 |        |     room 3      |
   +--[  ]--+--------+-----[  ]--------+
   |       \   corridor   /            |      hints along the two lines that join
   |        \            /             |      opposite edges of the two doors
   +---------\----------/--------------+
```

- Draw the two lines that join opposite edges of the two openings. Between them is the
  only part of the corridor through which one room sees the other.
- Hint on those lines. The corridor now has a middle leaf carrying the cross-view and
  outer leaves that see one room each.
- When the doors are far apart and neither room can see the other at all, one hint
  across the corridor between the doors is enough.

## Pattern: the roofline

The most valuable hint on any outdoor map. Streets separated by buildings should not see
each other, but each street's leaf rises to the sky brush, and over the rooftops every
street sees every other.

```
   sky  ============================================
                      upper leaves
   hint - - - - - - - - - - - - - - - - - - - - - -     horizontal, at the roofline
            +--------+          +--------+
   street 1 | block  | street 2 | block  | street 3
   _________|________|__________|________|_________
```

- Place one horizontal hint plane at the height of the top of the blocking buildings,
  across the whole outdoor space.
- Street 1's lower leaf touches the upper leaf; so does street 2's. But a straight line
  cannot leave street 1 upward and arrive in street 2 downward, so the two streets no
  longer see each other. Each sees only the upper leaves.
- Height: the top of the **lowest** building that must block. Taller buildings pass
  through the plane, which is fine. A hint above the blockers does nothing; a hint below
  their tops is still correct, only the hidden band is lower.
- Roofs differ a lot in height? Use one plane per district at that district's roofline,
  not one compromise plane for the map.
- Players who reach the rooftops stand in the upper leaves and draw everything. Either
  clip them off the roofs or accept the cost there.
- The buildings must be world brushes up to the plane. A facade of `func_detail` and
  props needs a nodraw world core inside it; see `optimization.md`, "Sight blockers".
- Keep the sky brush low as well. The lower the sky lid, the smaller the upper leaves and
  the less they hold.

## Pattern: stairwells, pits and balconies

The roofline pattern turned on its side or shrunk.

- **Stairwell or lift shaft**: a horizontal hint at each floor level stops one floor's
  landing seeing the landings two floors away.
- **Pit, trench, canal, sunken yard**: a horizontal hint at the rim. Players in the pit
  stop seeing the ground-level areas around it, and the other way round.
- **Balcony or mezzanine over a hall**: a horizontal hint at the balcony floor height
  separates what is seen from below from what is seen from above.
- **Hill or ridge of world brush**: a hint along the limit line over the crest, built the
  same way as a corner hint but in section view.

## Pattern: tidying a messy split

Hint planes are cut first, so a hint can replace many bad cuts with one good one.

- A large open space where VBSP has sliced leaves into wedges and slivers from the edges
  of stray world brushes: first turn the stray brushes into `func_detail`. If slivers
  remain, a hint along the main wall line gives the tree a clean first cut and the
  slivers often merge.
- The automatic split every 1024 units cuts through the middle of a room: a hint on the
  doorway plane lets the room be one leaf and puts the boundary where sight ends anyway.
- A hint lying on a plane that a world face already makes usually adds nothing. Check
  with `mat_leafvis` before keeping it.

## When a hint cannot help

- **No world brush between the two areas.** Add a blocker first.
- **The two areas really can see each other.** A long straight street is one view. Bend
  it, drop something across it, or limit it with fog and `farz`.
- **The areas are neighbours.** Rule 2. Shape a leaf between them or accept it.
- **The cost is inside the visible leaf.** Hints never hide part of a leaf. Prop fade
  distances, occluders and fewer props do that.
- **The opening is a door that shuts.** An areaportal linked to the door hides everything
  beyond it and costs no leaves.
- **The space is one huge open field.** Hints add portals for no gain. Consider
  `func_viscluster` to cut VVIS time, and distance fade for the frame rate.

## Building hints in Mallet

Keep every hint in a visgroup named `hints`, so they can be hidden while working and
found again. Confirm that visgroup is shown before a compile.

**Axis-aligned hint** (roofline, floor level, doorway plane): one thin brush, skip
everywhere, hint on the cutting face.

```json
{"tool": "create_block", "arguments": {
  "min": [-2048, -2048, 320], "max": [2048, 2048, 336],
  "material": "tools/toolsskip",
  "side_materials": {"bottom": "tools/toolshint"}
}}
```

Thickness does not matter; only the plane of the hint face does. Put the hint face on
the exact height or wall plane wanted, and let the brush run into the surrounding world
brushes so the plane spans the whole space.

**Diagonal hint** (corner, offset doors):

1. `create_block` filling the space the hint must cross, floor to ceiling, in
   `tools/toolsskip`, slightly into the walls each side.
2. `clip_selection` with three points on the limit line (two at floor height on the line,
   one directly above the first), `keep` set to the half to keep. The points sit on the
   grid.
3. `get_faces` on the brush, find the new slanted side, and `set_faces` with that side id
   and `material: "tools/toolshint"`. The clip gives the new face the editor's current
   material, which must not be left on it.
4. Check with `list_solids` `material: "tools/*"` that the brush carries hint and skip and
   nothing else.

Rules for the brush itself:

- It stays a world brush. Never tie it to an entity.
- Hint on one face is normal. Hint on two faces is right only when both planes are wanted.
  Hint on all six faces makes six cuts.
- Only the hint face may sit at an odd angle; its vertices are still on the grid.
- Passing through world brushes, `func_detail`, props and displacements is harmless.
- Do not run a hint through an areaportal brush, and do not put one on the same plane as
  an areaportal: the portal already makes that boundary.

## Workflow: one hint at a time

1. Full compile (not `-fast` VVIS) with no hints. Note `numportals` and the VVIS time
   from `compile_status`.
2. In game, go to the worst view. `mat_wireframe 1`, then `mat_leafvis 3`. Write down what
   draws that should not, and from where.
3. For each case, apply "The three facts" and pick a pattern, or decide a hint cannot
   help and note the real fix.
4. Add one hint, or one group that solves one case. Compile fully.
5. Compare: did the unwanted area stop drawing from the test position? How much did
   `numportals` and VVIS time rise?
6. Keep the hint only when it removed something worth removing. Delete it otherwise;
   a hint that changed nothing is pure cost.
7. Repeat for the next worst view.

Tell the user which of these steps you could not do. An agent can place hints and read
the compile log; the in-game check needs the game, so either run it through an in-game
tool when one is available or hand the user the positions and convars to check.

## Reading the result

| Check | What it tells you |
| --- | --- |
| `mat_leafvis 1` | Outline of the leaf the camera is in. Walk across a hint: the outline should change at the hint plane |
| `mat_leafvis 3` | Every leaf visible from here. The target's leaves should be gone |
| `mat_wireframe 1` | What is really drawn. The final proof |
| `r_lockpvs 1` | Freeze visibility, then fly to the target and see whether it was drawn |
| `r_drawportals 1` | Portal outlines, to see where the cuts landed |
| `numportals` in the VVIS log | The cost. Rising fast with little gain means too many hints |
| VVIS time | A jump from minutes to hours means leaves multiplied; remove the last hints added |

## Mistakes

- Hint brush tied to `func_detail` or another entity: it does nothing.
- A real texture or `tools/toolsnodraw` left on a face of a hint brush: the brush may
  compile as solid or draw. Hint and skip only.
- Hint on every face of the brush.
- Hint that does not reach floor to ceiling or wall to wall.
- Hint that misses the occluding corner by a few units.
- Hint beside a wall of `func_detail`, a displacement hill or a row of props, expecting
  it to block.
- Hints between neighbouring leaves, expecting one to hide the other.
- Roofline hint above the rooftops, or over buildings that are not world brushes.
- Dozens of hints placed before the first measurement.
- Judging hints on a `-fast` VVIS, or in the editor only.
- Hints visgroup hidden at compile time, so none were compiled.
- Leaving hints in place after the layout around them changed. Recheck every hint when a
  wall moves.

## Skip on its own

- Skip exists to make the unused faces of a hint brush disappear. That is its only
  routine use.
- A brush textured entirely with skip compiles to nothing. It does not seal: never use it
  as a wall, and suspect it when a leak line passes through what looks like a brush.
- Skip is not nodraw. A nodraw face is still a real face: it cuts leaves, seals, collides
  and blocks light. A skip face does none of those.

| | Cuts leaves | Seals | Solid | Drawn |
| --- | --- | --- | --- | --- |
| `tools/toolshint` face | Yes, first | No | No | No |
| `tools/toolsskip` face | No | No | No | No |
| `tools/toolsnodraw` face | Yes | Yes | Yes | No |

- Do not put skip on faces of ordinary world brushes to "save" them. Use nodraw.
- Other tool brushes have their own rule: areaportals and triggers carry their tool
  material on every face; occluders carry `tools/toolsoccluder` on the blocking faces and
  `tools/toolsnodraw` on the rest.
- Audit: `list_solids` with `material: "tools/toolsskip"` should return hint brushes only,
  each also carrying `tools/toolshint`.
