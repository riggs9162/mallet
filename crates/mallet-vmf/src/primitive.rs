//! Hammer's primitives for the Block tool: wedges, cylinders, spikes, spheres and arches,
//! built inside the box drawn in the 2D views the way Hammer builds them.

use glam::{DVec2, DVec3};

use crate::build::solid_from_sides;
use crate::geometry::solid_faces;
use crate::texture::{face_axes, world_axes};
use crate::{IdAllocator, Solid};

/// The fewest sides a round primitive or an arch may have, as in Hammer.
pub const MIN_SIDES: u32 = 3;
/// The most sides a round primitive or an arch may have, as in Hammer.
pub const MAX_SIDES: u32 = 100;

/// A shape the Block tool can build, as listed in Hammer's Objects box.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Shape {
    /// A box filling the drawn box.
    #[default]
    Block,
    /// A ramp: a triangular prism rising towards the drawn box's north side.
    Wedge,
    /// An upright prism with as many sides as asked for, touching the box's sides.
    Cylinder,
    /// An upright pyramid over a base with as many sides as asked for.
    Spike,
    /// A ball made of rings of faces, inside the box.
    Sphere,
    /// Several brushes in a curve, set up in the Arch Properties dialog.
    Arch,
}

impl Shape {
    /// Every shape, in the order Hammer lists them.
    pub const ALL: [Shape; 6] = [
        Self::Block,
        Self::Wedge,
        Self::Cylinder,
        Self::Spike,
        Self::Sphere,
        Self::Arch,
    ];

    /// The name Hammer's Objects box shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Wedge => "wedge",
            Self::Cylinder => "cylinder",
            Self::Spike => "spike",
            Self::Sphere => "sphere",
            Self::Arch => "arch",
        }
    }

    /// The shape with this name, ignoring case.
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|shape| shape.label().eq_ignore_ascii_case(label.trim()))
    }

    /// Whether the number of faces (sides around) changes the shape.
    pub fn uses_faces(self) -> bool {
        matches!(self, Self::Cylinder | Self::Spike | Self::Sphere)
    }
}

/// The settings of Hammer's Arch Properties dialog.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arch {
    /// How thick each piece is, measured in from the drawn box. Negative builds outwards.
    pub wall_width: f64,
    /// How many brushes the arch is made of.
    pub sides: u32,
    /// How far round the arch goes, in degrees: 360 makes a ring.
    pub arc: f64,
    /// Where the first piece starts, in degrees counter-clockwise from east.
    pub start_angle: f64,
    /// How much higher each piece is than the one before, for spiral stairs.
    pub add_height: f64,
}

impl Default for Arch {
    fn default() -> Self {
        Self {
            wall_width: 32.0,
            sides: 8,
            arc: 360.0,
            start_angle: 0.0,
            add_height: 0.0,
        }
    }
}

impl Arch {
    /// The smallest and largest arc Hammer accepts, in degrees.
    pub const ARC_RANGE: (f64, f64) = (8.0, 360.0);

    /// The outline of each piece in the top view, as four corners: outer start, outer
    /// end, inner end and inner start, rounded to whole units as Hammer rounds them. A
    /// wall at least half as wide as the box collapses the inner corners onto its centre,
    /// as Hammer does.
    pub fn outlines(&self, min: DVec2, max: DVec2) -> Vec<[DVec2; 4]> {
        self.outlines_rounded(min, max, true)
    }

    fn outlines_rounded(&self, min: DVec2, max: DVec2, round: bool) -> Vec<[DVec2; 4]> {
        let sides = self.sides.clamp(MIN_SIDES, MAX_SIDES) as usize;
        let arc = self.arc.clamp(Self::ARC_RANGE.0, Self::ARC_RANGE.1);
        let outer = arc_points(min, max, sides, self.start_angle, arc, round);
        let width = self.wall_width;
        let size = max - min;
        let inner = if width * 2.0 + 8.0 >= size.x || width * 2.0 + 8.0 >= size.y {
            vec![(min + max) * 0.5; sides + 1]
        } else {
            let inset = DVec2::splat(width);
            arc_points(min + inset, max - inset, sides, self.start_angle, arc, round)
        };
        (0..sides)
            .map(|index| [outer[index], outer[index + 1], inner[index + 1], inner[index]])
            .collect()
    }
}

/// Points around the ellipse filling a box, from `start` degrees counter-clockwise from
/// east over `arc` degrees, `sides + 1` of them, rounded to whole units when `round` is
/// set, like Hammer's.
fn arc_points(min: DVec2, max: DVec2, sides: usize, start: f64, arc: f64, round: bool) -> Vec<DVec2> {
    let radius = (max - min) * 0.5;
    let centre = min + radius;
    (0..=sides)
        .map(|index| {
            let angle = (start + arc * index as f64 / sides as f64).to_radians();
            let point = centre + DVec2::new(angle.cos() * radius.x, angle.sin() * radius.y);
            if round {
                point.round()
            } else {
                point
            }
        })
        .collect()
}

/// Builds a primitive filling the box between `min` and `max`, with `faces` sides around
/// for the round shapes, every face using `material`. Arches have their own builder,
/// [`arch_solids`]. Returns `None` when the box is too small for the shape.
pub fn primitive_solid(
    shape: Shape,
    min: DVec3,
    max: DVec3,
    faces: u32,
    material: &str,
    ids: &mut IdAllocator,
) -> Option<Solid> {
    let (min, max) = (min.min(max), min.max(max));
    if (max - min).min_element() <= 0.0 {
        return None;
    }
    let sides = faces.clamp(MIN_SIDES, MAX_SIDES) as usize;
    let polygons = match shape {
        Shape::Block | Shape::Arch => prism(&rectangle(min, max), min.z, max.z),
        Shape::Wedge => wedge(min, max),
        Shape::Cylinder => prism(&ring(min, max, sides), min.z, max.z),
        Shape::Spike => spike(min, max, sides),
        Shape::Sphere => sphere(min, max, sides),
    };
    solid_from_polygons(&polygons, material, ids)
}

/// Builds an arch inside the box between `min` and `max`: one brush per side, as Hammer's
/// Arch Properties dialog makes them. A piece that rounding to whole units would spoil is
/// built from the exact corners instead, and pieces that come out flat are left out.
pub fn arch_solids(min: DVec3, max: DVec3, arch: &Arch, material: &str, ids: &mut IdAllocator) -> Vec<Solid> {
    let (min, max) = (min.min(max), min.max(max));
    let bottom = min.z;
    let top = if max.z - min.z < 1.0 { min.z + 1.0 } else { max.z };
    let rounded = arch.outlines_rounded(min.truncate(), max.truncate(), true);
    let exact = arch.outlines_rounded(min.truncate(), max.truncate(), false);
    rounded
        .into_iter()
        .zip(exact)
        .enumerate()
        .filter_map(|(index, (rounded, exact))| {
            let lift = arch.add_height * index as f64;
            [rounded, exact].into_iter().find_map(|outline| {
                let polygon = without_repeats(&outline);
                if polygon.len() < 3 {
                    return None;
                }
                solid_from_polygons(&prism(&polygon, bottom + lift, top + lift), material, ids)
            })
        })
        .collect()
}

/// The box's corners in the top view, counter-clockwise.
fn rectangle(min: DVec3, max: DVec3) -> Vec<DVec2> {
    vec![
        DVec2::new(min.x, min.y),
        DVec2::new(max.x, min.y),
        DVec2::new(max.x, max.y),
        DVec2::new(min.x, max.y),
    ]
}

/// Hammer's polygon inside a box: `sides` corners round the ellipse touching its sides,
/// the first at the north, going clockwise, rounded to whole units unless that would
/// spoil the shape.
fn ring(min: DVec3, max: DVec3, sides: usize) -> Vec<DVec2> {
    let radius = (max - min).truncate() * 0.5;
    let centre = min.truncate() + radius;
    let exact: Vec<DVec2> = (0..sides)
        .map(|index| {
            let angle = (360.0 * index as f64 / sides as f64).to_radians();
            centre + DVec2::new(angle.sin() * radius.x, angle.cos() * radius.y)
        })
        .collect();
    let rounded: Vec<DVec2> = exact.iter().map(|point| point.round()).collect();
    if strictly_convex(&rounded) {
        rounded
    } else {
        exact
    }
}

/// Whether every corner of a polygon turns the same way and none is straight or doubled.
fn strictly_convex(points: &[DVec2]) -> bool {
    let count = points.len();
    let turns: Vec<f64> = (0..count)
        .map(|index| {
            let (a, b, c) = (points[index], points[(index + 1) % count], points[(index + 2) % count]);
            (b - a).perp_dot(c - b)
        })
        .collect();
    count >= 3 && (turns.iter().all(|turn| *turn > 1e-9) || turns.iter().all(|turn| *turn < -1e-9))
}

/// The faces of an upright prism over a polygon in the top view.
fn prism(polygon: &[DVec2], bottom: f64, top: f64) -> Vec<Vec<DVec3>> {
    let at = |point: DVec2, z: f64| point.extend(z);
    let mut faces = vec![
        polygon.iter().map(|point| at(*point, top)).collect(),
        polygon.iter().rev().map(|point| at(*point, bottom)).collect(),
    ];
    for index in 0..polygon.len() {
        let (a, b) = (polygon[index], polygon[(index + 1) % polygon.len()]);
        faces.push(vec![at(a, bottom), at(b, bottom), at(b, top), at(a, top)]);
    }
    faces
}

/// Hammer's wedge: the floor of the box, a back wall along its north side and a slope
/// from the top of that wall down to the south edge of the floor.
fn wedge(min: DVec3, max: DVec3) -> Vec<Vec<DVec3>> {
    let p = DVec3::new;
    let (south_west, south_east) = (p(min.x, min.y, min.z), p(max.x, min.y, min.z));
    let (north_west, north_east) = (p(min.x, max.y, min.z), p(max.x, max.y, min.z));
    let (top_west, top_east) = (p(min.x, max.y, max.z), p(max.x, max.y, max.z));
    vec![
        vec![south_west, south_east, north_east, north_west],
        vec![north_west, north_east, top_east, top_west],
        vec![south_west, south_east, top_east, top_west],
        vec![south_west, north_west, top_west],
        vec![south_east, north_east, top_east],
    ]
}

/// A pyramid over Hammer's polygon, its tip at the middle of the box's top.
fn spike(min: DVec3, max: DVec3, sides: usize) -> Vec<Vec<DVec3>> {
    let base = ring(min, max, sides);
    let tip = ((min + max) * 0.5).truncate().extend(max.z);
    let mut faces = vec![base.iter().map(|point| point.extend(min.z)).collect::<Vec<_>>()];
    for index in 0..base.len() {
        let (a, b) = (base[index], base[(index + 1) % base.len()]);
        faces.push(vec![a.extend(min.z), b.extend(min.z), tip]);
    }
    faces
}

/// A ball inside the box: `sides` slices round and half as many rings from pole to pole,
/// so every face is flat.
fn sphere(min: DVec3, max: DVec3, sides: usize) -> Vec<Vec<DVec3>> {
    let radius = (max - min) * 0.5;
    let centre = min + radius;
    let rings = (sides / 2).max(2);
    let point = |ring: usize, slice: usize| {
        let latitude = (-90.0 + 180.0 * ring as f64 / rings as f64).to_radians();
        let longitude = (360.0 * slice as f64 / sides as f64).to_radians();
        centre
            + DVec3::new(
                latitude.cos() * longitude.sin() * radius.x,
                latitude.cos() * longitude.cos() * radius.y,
                latitude.sin() * radius.z,
            )
    };
    let (south, north) = (centre - DVec3::Z * radius.z, centre + DVec3::Z * radius.z);
    let mut faces = Vec::new();
    for ring in 0..rings {
        for slice in 0..sides {
            let next = (slice + 1) % sides;
            let face = if ring == 0 {
                vec![south, point(1, next), point(1, slice)]
            } else if ring == rings - 1 {
                vec![point(ring, slice), point(ring, next), north]
            } else {
                vec![point(ring, slice), point(ring, next), point(ring + 1, next), point(ring + 1, slice)]
            };
            faces.push(face);
        }
    }
    faces
}

/// The points with neighbours that repeat dropped, the last one checked against the first.
fn without_repeats(points: &[DVec2]) -> Vec<DVec2> {
    let mut kept: Vec<DVec2> = Vec::with_capacity(points.len());
    for point in points {
        if kept.last().is_none_or(|last| last.distance(*point) > 1e-6) {
            kept.push(*point);
        }
    }
    while kept.len() > 1 && kept[0].distance(kept[kept.len() - 1]) <= 1e-6 {
        kept.pop();
    }
    kept
}

/// A brush with one side per face polygon. Each polygon is turned to face away from the
/// middle of the shape, its plane goes through three of its corners spread around it, and
/// its texture is aligned to the world on faces that are, and to the face on slopes.
/// Returns `None` when the faces do not close a volume.
fn solid_from_polygons(polygons: &[Vec<DVec3>], material: &str, ids: &mut IdAllocator) -> Option<Solid> {
    let corners: Vec<DVec3> = polygons.iter().flatten().copied().collect();
    if corners.is_empty() {
        return None;
    }
    let inside = corners.iter().sum::<DVec3>() / corners.len() as f64;
    let mut planes = Vec::with_capacity(polygons.len());
    for polygon in polygons {
        if polygon.len() < 3 {
            continue;
        }
        let normal = newell_normal(polygon);
        let centre = polygon.iter().sum::<DVec3>() / polygon.len() as f64;
        let outwards = normal.dot(centre - inside) >= 0.0;
        let normal = if outwards { normal } else { -normal };
        let count = polygon.len();
        let pick = |step: usize| polygon[if outwards { step } else { (count - step) % count }];
        let (a, b, c) = (pick(0), pick(count / 3), pick(2 * count / 3));
        let (u, v) = primitive_axes(normal);
        planes.push(([c, b, a], u, v));
    }
    let mut scratch = ids.clone();
    let draft = solid_from_sides(&planes, material, &mut scratch);
    if solid_faces(&draft).len() != planes.len() {
        return None;
    }
    Some(solid_from_sides(&planes, material, ids))
}

/// The face normal of a polygon wound counter-clockwise, robust to small bends.
fn newell_normal(polygon: &[DVec3]) -> DVec3 {
    let mut normal = DVec3::ZERO;
    for (index, current) in polygon.iter().enumerate() {
        let next = polygon[(index + 1) % polygon.len()];
        normal.x += (current.y - next.y) * (current.z + next.z);
        normal.y += (current.z - next.z) * (current.x + next.x);
        normal.z += (current.x - next.x) * (current.y + next.y);
    }
    normal.normalize_or_zero()
}

/// World texture axes on faces that look straight along an axis, as Hammer gives new
/// boxes, and axes lying in the face on slopes so textures are not stretched.
fn primitive_axes(normal: DVec3) -> (DVec3, DVec3) {
    if normal.abs().max_element() > 1.0 - 1e-9 {
        world_axes(normal)
    } else {
        face_axes(normal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::faces_bounds;
    use crate::Vmf;

    fn ids() -> IdAllocator {
        IdAllocator::for_map(&Vmf::new_map())
    }

    fn bounds(solid: &Solid) -> (DVec3, DVec3) {
        let bounds = faces_bounds(&solid_faces(solid));
        (bounds.min, bounds.max)
    }

    #[test]
    fn round_shapes_fill_their_box() {
        let (min, max) = (DVec3::new(0.0, 0.0, 0.0), DVec3::new(128.0, 128.0, 256.0));
        for shape in [Shape::Cylinder, Shape::Spike, Shape::Sphere] {
            for faces in [3, 4, 8, 16, 32] {
                let solid = primitive_solid(shape, min, max, faces, "BRICK/BRICKWALL001A", &mut ids())
                    .unwrap_or_else(|| panic!("{shape:?} with {faces} faces"));
                let (low, high) = bounds(&solid);
                assert!(low.cmpge(min - DVec3::splat(0.5)).all() && high.cmple(max + DVec3::splat(0.5)).all());
                assert_eq!(high.z, 256.0, "{shape:?} {faces}");
            }
        }
        let cylinder = primitive_solid(Shape::Cylinder, min, max, 8, "X", &mut ids()).unwrap();
        assert_eq!(cylinder.sides().count(), 10);
        assert_eq!(bounds(&cylinder), (min, max));
    }

    #[test]
    fn a_wedge_rises_to_the_north() {
        let solid = primitive_solid(Shape::Wedge, DVec3::ZERO, DVec3::new(64.0, 128.0, 32.0), 4, "X", &mut ids())
            .unwrap();
        assert_eq!(solid.sides().count(), 5);
        let top: Vec<DVec3> = solid_faces(&solid)
            .into_iter()
            .flat_map(|face| face.vertices)
            .filter(|vertex| vertex.z == 32.0)
            .collect();
        assert!(top.iter().all(|vertex| vertex.y == 128.0));
    }

    #[test]
    fn an_arch_is_one_brush_per_side() {
        let arch = Arch {
            wall_width: 16.0,
            sides: 8,
            arc: 180.0,
            start_angle: 0.0,
            add_height: 8.0,
        };
        let solids = arch_solids(DVec3::ZERO, DVec3::new(256.0, 256.0, 64.0), &arch, "X", &mut ids());
        assert_eq!(solids.len(), 8);
        let (low, high) = bounds(&solids[7]);
        assert_eq!((low.z, high.z), (56.0, 120.0));
        let ring = Arch {
            wall_width: 200.0,
            ..Arch::default()
        };
        assert_eq!(
            arch_solids(DVec3::ZERO, DVec3::new(256.0, 256.0, 64.0), &ring, "X", &mut ids()).len(),
            8
        );
    }
}
