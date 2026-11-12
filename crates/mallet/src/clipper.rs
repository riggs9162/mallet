//! Hammer's Clipping tool (Shift+X): cuts the selected brushes along a plane.
//!
//! Dragging in a 2D view draws the clip line between two grid points; the plane runs
//! through it, straight along the axis the view looks down. With three-point clipping (C,
//! as in Hammer++) the plane goes through three points placed in the 2D views or on
//! surfaces in the 3D view. As in Hammer, the selection stays red, the pieces a clip
//! keeps are outlined in white and the clip line is cyan between its two white points.
//! Shift+X, or clicking the tool again, cycles which pieces are kept: both, then one
//! side, then the other. Ctrl+dragging a point moves the whole line, O shows the size of
//! each kept piece in the 2D views, and Enter clips.

use std::collections::HashMap;

use eframe::egui::Color32;
use glam::{DVec2, DVec3};
use mallet_vmf::clip::{remap_side_list, renumber_sides, split_solid, ClipError, Keep};
use mallet_vmf::edit::Edit;
use mallet_vmf::geometry::{base_polygon, clip_polygon, solid_faces, Bounds, Plane};
use mallet_vmf::{Entity, IdAllocator, Solid, Vmf};

use crate::editor::{find_entity, Editor, SelectMode, Tool};
use crate::scene::Owner;
use crate::view2d::Axis2D;

/// How far past the selection the drawn clip plane reaches, in world units.
const PLANE_MARGIN: f64 = 16.0;
/// Half-length of the cross the 3D view draws at each clip point, in world units.
const POINT_MARKER_SIZE: f64 = 4.0;
/// The colour of the clip line and plane: cyan, as in Hammer.
pub const CLIP_PLANE_COLOR: Color32 = Color32::from_rgb(0, 255, 255);

/// The clipping tool's points and settings, kept while the tool is in use.
#[derive(Clone, Debug, Default)]
pub struct Clipper {
    /// The points placed so far: up to two in two-point mode, three in three-point mode.
    pub points: Vec<DVec3>,
    /// The 2D view a two-point line was drawn in; the plane runs along its depth axis.
    pub axis: Option<Axis2D>,
    /// Which halves Enter keeps.
    pub keep: Keep,
    /// Whether the plane goes through three points instead of a line in one view.
    pub three_point: bool,
    /// Whether the 2D views show the size of each piece a clip keeps (O in Hammer).
    pub measurements: bool,
}

/// What the views draw for the clipping tool.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipOverlay {
    /// The tool's points, drawn as handles.
    pub points: Vec<DVec3>,
    /// The axis a two-point line was drawn along, so only its view draws the line.
    pub axis: Option<Axis2D>,
    /// Edges of the pieces that clipping would keep.
    pub kept: Vec<[DVec3; 2]>,
    /// The outline of the clip plane where it crosses the selection.
    pub plane: Vec<[DVec3; 2]>,
    /// The bounds of each kept piece, while measurements are shown.
    pub measured: Vec<Bounds>,
}

impl ClipOverlay {
    /// What the 2D views draw in world space: the kept pieces in white. As in Hammer, a
    /// two-point clip shows no plane, only its line in the view it was drawn in, which
    /// [`crate::view2d`] draws with the points; three-point clipping, which has no such
    /// line, also outlines its plane.
    pub fn edges_2d(&self) -> Vec<([DVec3; 2], Color32)> {
        let mut edges = self.kept_edges();
        if self.axis.is_none() {
            edges.extend(self.plane.iter().map(|edge| (*edge, CLIP_PLANE_COLOR)));
        }
        edges
    }

    /// What the 3D view draws: the kept pieces, the plane's outline and a small cross at
    /// each point.
    pub fn edges_3d(&self) -> Vec<([DVec3; 2], Color32)> {
        let mut edges = self.kept_edges();
        edges.extend(self.plane.iter().map(|edge| (*edge, CLIP_PLANE_COLOR)));
        for point in &self.points {
            for axis in [DVec3::X, DVec3::Y, DVec3::Z] {
                let arm = axis * POINT_MARKER_SIZE;
                edges.push(([*point - arm, *point + arm], CLIP_PLANE_COLOR));
            }
        }
        edges
    }

    /// The edges of the kept pieces, in the tool colour (white by default).
    fn kept_edges(&self) -> Vec<([DVec3; 2], Color32)> {
        let tool = crate::style::current().tool;
        self.kept.iter().map(|edge| (*edge, tool)).collect()
    }
}

/// One brush the clipping tool cuts, with the entity that owns it.
struct Target {
    owner: u32,
    solid: Solid,
}

impl Clipper {
    /// The three plane points, wound so their normal points to the front, once enough
    /// points are placed. The front of a two-point line is on the right of the way it was
    /// drawn, in every 2D view: below a line drawn from left to right.
    pub fn plane_points(&self) -> Option<[DVec3; 3]> {
        if self.three_point {
            let [a, b, c] = self.points.as_slice().try_into().ok()?;
            Plane::from_points([a, b, c])?;
            return Some([a, b, c]);
        }
        let [a, b] = self.points.as_slice().try_into().ok()?;
        let [horizontal, vertical, _] = self.axis?.axes();
        let (mut right, mut up) = (DVec3::ZERO, DVec3::ZERO);
        right[horizontal] = 1.0;
        up[vertical] = 1.0;
        let toward_viewer = right.cross(up);
        let points = [a, a + toward_viewer * 64.0, b];
        Plane::from_points(points)?;
        Some(points)
    }
}

impl Editor {
    /// Toggles between two-point and three-point clipping, dropping the points placed.
    pub fn toggle_three_point_clip(&mut self) {
        self.clipper.three_point = !self.clipper.three_point;
        self.clipper.points.clear();
        self.clipper.axis = None;
        self.message = if self.clipper.three_point {
            "Three-point clipping: place three points in the 2D views or on faces in the 3D view"
        } else {
            "Two-point clipping: drag a line in a 2D view"
        }
        .to_owned();
        self.refresh_clip_preview();
    }

    /// Moves on to keeping the next half, in Hammer's order: both, front, back.
    pub fn cycle_clip_keep(&mut self) {
        self.clipper.keep = self.clipper.keep.next();
        self.message = format!("Clipping keeps {}", self.clipper.keep.label());
        self.refresh_clip_preview();
    }

    /// Drops the clip points. Returns whether there were any.
    pub fn clear_clip_points(&mut self) -> bool {
        let had = !self.clipper.points.is_empty();
        self.clipper.points.clear();
        self.clipper.axis = None;
        self.refresh_clip_preview();
        had
    }

    /// Starts a two-point line at a grid point of a 2D view, replacing any line placed
    /// before. Returns the index of the point that follows the pointer.
    pub fn start_clip_line(&mut self, axis: Axis2D, at: DVec2) -> usize {
        let point = self.clip_point_in_view(axis, at, None);
        self.clipper.points = vec![point, point];
        self.clipper.axis = Some(axis);
        self.refresh_clip_preview();
        1
    }

    /// Adds a three-point clip point at a grid point of a 2D view. Its depth is the
    /// centre of the selection, or of the points already placed. Once three points exist
    /// the next one starts over. Returns its index.
    pub fn add_clip_point_2d(&mut self, axis: Axis2D, at: DVec2) -> usize {
        let point = self.clip_point_in_view(axis, at, None);
        self.add_clip_point(point)
    }

    /// Adds a three-point clip point where the 3D view's ray meets a surface, on the grid.
    pub fn add_clip_point_3d(&mut self, point: DVec3) -> usize {
        let step = self.snap_step();
        let snapped = (point / step).round() * step + DVec3::ZERO;
        self.add_clip_point(snapped)
    }

    fn add_clip_point(&mut self, point: DVec3) -> usize {
        if self.clipper.points.len() >= 3 {
            self.clipper.points.clear();
        }
        self.clipper.points.push(point);
        let count = self.clipper.points.len();
        self.message = match count {
            3 => format!("Clip plane placed; Enter clips, keeping {}", self.clipper.keep.label()),
            _ => format!("Clip point {count} of 3 placed"),
        };
        self.refresh_clip_preview();
        count - 1
    }

    /// Moves a clip point to a grid point of a 2D view, keeping its depth.
    pub fn move_clip_point(&mut self, index: usize, axis: Axis2D, at: DVec2) {
        let Some(&old) = self.clipper.points.get(index) else {
            return;
        };
        let point = self.clip_point_in_view(axis, at, Some(old));
        if point != old {
            self.clipper.points[index] = point;
            self.refresh_clip_preview();
        }
    }

    /// Moves every clip point by as much as moving point `index` to a grid point of a 2D
    /// view takes, as Ctrl+dragging a point does in Hammer. Depths stay as they are.
    pub fn move_clip_points(&mut self, index: usize, axis: Axis2D, at: DVec2) {
        let Some(&old) = self.clipper.points.get(index) else {
            return;
        };
        let offset = self.clip_point_in_view(axis, at, Some(old)) - old;
        if offset != DVec3::ZERO {
            for point in &mut self.clipper.points {
                *point += offset;
            }
            self.refresh_clip_preview();
        }
    }

    /// Shows or hides the size of each piece a clip keeps, as O does in Hammer.
    pub fn toggle_clip_measurements(&mut self) {
        self.clipper.measurements = !self.clipper.measurements;
        self.message = if self.clipper.measurements {
            "Clipping tool shows the size of the pieces it keeps"
        } else {
            "Clipping tool hides the size of the pieces it keeps"
        }
        .to_owned();
        self.refresh_clip_preview();
    }

    /// The clip point under a 2D position, within `reach` world units.
    pub fn clip_point_at(&self, axis: Axis2D, at: DVec2, reach: f64) -> Option<usize> {
        if !self.clipper.three_point && self.clipper.axis != Some(axis) {
            return None;
        }
        self.clipper
            .points
            .iter()
            .enumerate()
            .map(|(index, point)| (index, axis.project(*point).distance(at)))
            .filter(|(_, distance)| *distance <= reach)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
    }

    /// A grid point in a 2D view lifted to 3D. The depth comes from `keep_depth`, else
    /// from the points already placed, else from the centre of the selection.
    fn clip_point_in_view(&self, axis: Axis2D, at: DVec2, keep_depth: Option<DVec3>) -> DVec3 {
        let step = self.snap_step();
        let snapped = (at / step).round() * step;
        let [horizontal, vertical, depth] = axis.axes();
        let mut point = keep_depth.unwrap_or_else(|| {
            let placed = &self.clipper.points;
            if placed.is_empty() {
                let bounds = self.scene.selection_bounds();
                if bounds.is_empty() {
                    DVec3::ZERO
                } else {
                    (bounds.center() / step).round() * step
                }
            } else {
                placed.iter().sum::<DVec3>() / placed.len() as f64
            }
        });
        point[horizontal] = snapped.x;
        point[vertical] = snapped.y;
        point[depth] = point[depth].round();
        point + DVec3::ZERO
    }

    /// Clips with the tool's points, keeping the halves Shift+X chose.
    pub fn apply_clip(&mut self) -> Result<usize, String> {
        let points = self
            .clipper
            .plane_points()
            .ok_or("place the clip line in a 2D view first")?;
        let count = self.clip_selection(points, self.clipper.keep)?;
        self.clear_clip_points();
        Ok(count)
    }

    /// Cuts every selected brush, including the brushes of selected entities, along the
    /// plane through `points` (wound so their normal points to the front). Brushes the
    /// plane misses are left alone; displacements are skipped. Overlays and other
    /// entities that list cut faces in `sides` are given the faces those became. The
    /// whole clip is one undo step. Returns how many brushes were cut.
    pub fn clip_selection(&mut self, points: [DVec3; 3], keep: Keep) -> Result<usize, String> {
        Plane::from_points(points).ok_or("the clip points do not make a plane")?;
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let targets = clip_targets(vmf, &self.selection)?;
        let mut ids = IdAllocator::for_map(vmf);
        let mut edits = Vec::new();
        let mut faces: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut new_brushes = Vec::new();
        let mut cut = 0;
        let mut displacements = 0;
        for target in targets {
            let split = match split_solid(&target.solid, points, &self.material, &mut ids) {
                Ok(split) => split,
                Err(ClipError::Displacement) => {
                    displacements += 1;
                    continue;
                }
                Err(ClipError::Uncut | ClipError::NoPlane) => continue,
            };
            cut += 1;
            let originals: Vec<u32> = target.solid.sides().map(|side| side.id).collect();
            let (first, second) = match keep {
                Keep::Back => (split.back, None),
                Keep::Front => (split.front, None),
                Keep::Both => (split.back, Some(split.front)),
            };
            for id in &originals {
                let mapped = faces.entry(*id).or_default();
                if first.sides().any(|side| side.id == *id) {
                    mapped.push(*id);
                }
            }
            edits.push(Edit::ReplaceSolids(vec![first]));
            if let Some(mut second) = second {
                for (old, new) in renumber_sides(&mut second, &mut ids) {
                    if originals.contains(&old) {
                        faces.entry(old).or_default().push(new);
                    }
                }
                if target.owner == world_id(vmf)? {
                    new_brushes.extend(second.id().map(Owner::Solid));
                }
                edits.push(Edit::AddSolid {
                    entity: target.owner,
                    index: None,
                    solid: second,
                });
            }
        }
        if cut == 0 {
            return Err(match displacements {
                0 => "the clip plane does not cut any selected brush".to_owned(),
                _ => "displacements cannot be clipped".to_owned(),
            });
        }
        edits.extend(side_list_edits(vmf, &faces));
        self.apply_edit(&Edit::Batch(edits))?;
        self.select(new_brushes, SelectMode::Add);
        let noun = if cut == 1 { "brush" } else { "brushes" };
        self.message = match displacements {
            0 => format!("Clipped {cut} {noun}, keeping {}", keep.label()),
            skipped => format!("Clipped {cut} {noun}, keeping {}; skipped {skipped} displacements", keep.label()),
        };
        Ok(cut)
    }

    /// Recomputes what the views draw for the clipping tool: its points, the outline of
    /// the plane and the pieces Enter would keep.
    pub fn refresh_clip_preview(&mut self) {
        if self.tool != Tool::Clip {
            self.overlay.clip = None;
            return;
        }
        let mut preview = ClipOverlay {
            points: self.clipper.points.clone(),
            axis: (!self.clipper.three_point).then_some(self.clipper.axis).flatten(),
            ..ClipOverlay::default()
        };
        if let (Some(points), Some(document)) = (self.clipper.plane_points(), self.document.as_ref()) {
            let keep = self.clipper.keep;
            let mut ids = IdAllocator::for_map(&document.vmf);
            let targets = clip_targets(&document.vmf, &self.selection).unwrap_or_default();
            let mut cut_bounds = Bounds::EMPTY;
            for target in &targets {
                let Ok(split) = split_solid(&target.solid, points, &self.material, &mut ids) else {
                    continue;
                };
                for (piece, kept) in [(&split.back, keep.back()), (&split.front, keep.front())] {
                    let piece_faces = solid_faces(piece);
                    let mut piece_bounds = Bounds::EMPTY;
                    for face in &piece_faces {
                        for vertex in &face.vertices {
                            cut_bounds.add_point(*vertex);
                            piece_bounds.add_point(*vertex);
                        }
                        if kept {
                            preview.kept.extend(polygon_edges(&face.vertices));
                        }
                    }
                    if kept && self.clipper.measurements && !piece_bounds.is_empty() {
                        preview.measured.push(piece_bounds);
                    }
                }
            }
            let bounds = if cut_bounds.is_empty() {
                self.scene.selection_bounds()
            } else {
                cut_bounds
            };
            if let Some(plane) = Plane::from_points(points).filter(|_| !bounds.is_empty()) {
                preview.plane = polygon_edges(&plane_in_box(&plane, &bounds));
            }
        }
        self.overlay.clip = Some(preview);
    }
}

/// The brushes a clip cuts: selected world brushes and the brushes of selected entities.
fn clip_targets(vmf: &Vmf, selection: &std::collections::BTreeSet<Owner>) -> Result<Vec<Target>, String> {
    let world = vmf.world().ok_or("the map has no world")?;
    let world_id = world.id().ok_or("the world has no id")?;
    let mut targets = Vec::new();
    for owner in selection {
        match *owner {
            Owner::Solid(id) => {
                if let Some(solid) = world.solids().find(|solid| solid.id() == Some(id)) {
                    targets.push(Target {
                        owner: world_id,
                        solid: solid.clone(),
                    });
                }
            }
            Owner::Entity(id) => {
                let entity = find_entity(vmf, id)?;
                targets.extend(entity.solids().map(|solid| Target {
                    owner: id,
                    solid: solid.clone(),
                }));
            }
        }
    }
    Ok(targets)
}

fn world_id(vmf: &Vmf) -> Result<u32, String> {
    vmf.world().and_then(Entity::id).ok_or_else(|| "the world has no id".to_owned())
}

/// Key edits that point every entity listing cut faces in `sides`, such as overlays and
/// cube maps, at the faces those became.
fn side_list_edits(vmf: &Vmf, faces: &HashMap<u32, Vec<u32>>) -> Vec<Edit> {
    vmf.entities()
        .filter_map(|entity| {
            let id = entity.id()?;
            let value = remap_side_list(entity.get("sides")?, faces)?;
            Some(Edit::SetKey {
                entity: id,
                key: "sides".to_owned(),
                value,
            })
        })
        .collect()
}

/// The edges around a polygon.
fn polygon_edges(points: &[DVec3]) -> Vec<[DVec3; 2]> {
    (0..points.len())
        .map(|index| [points[index], points[(index + 1) % points.len()]])
        .collect()
}

/// The part of a plane inside a box grown by [`PLANE_MARGIN`].
fn plane_in_box(plane: &Plane, bounds: &Bounds) -> Vec<DVec3> {
    let min = bounds.min - DVec3::splat(PLANE_MARGIN);
    let max = bounds.max + DVec3::splat(PLANE_MARGIN);
    let mut polygon = base_polygon(plane);
    for axis in 0..3 {
        let mut normal = DVec3::ZERO;
        normal[axis] = 1.0;
        polygon = clip_polygon(&polygon, &Plane { normal, dist: max[axis] });
        polygon = clip_polygon(&polygon, &Plane {
            normal: -normal,
            dist: -min[axis],
        });
        if polygon.len() < 3 {
            return Vec::new();
        }
    }
    polygon
}
