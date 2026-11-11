//! Hammer's Carve: cutting one brush's shape out of the brushes it overlaps.

use glam::DVec3;

use crate::build::plane_points;
use crate::clip::{half, ClipError};
use crate::geometry::{faces_bounds, solid_faces, Bounds, Plane, EPSILON};
use crate::{IdAllocator, Solid};

/// Cuts the space `carver` fills out of `target`. See [`Carver::carve`]; to carve many
/// brushes with one carver, build the [`Carver`] once instead.
pub fn carve_solid(target: &Solid, carver: &Solid, ids: &mut IdAllocator) -> Result<Option<Vec<Solid>>, ClipError> {
    match Carver::new(carver) {
        Some(carver) => carver.carve(target, ids),
        None => Ok(None),
    }
}

/// A brush ready to carve with, its faces worked out once however many brushes it cuts.
#[derive(Clone, Debug)]
pub struct Carver {
    faces: Vec<CarverFace>,
    bounds: Bounds,
}

/// One face of a [`Carver`].
#[derive(Clone, Debug)]
struct CarverFace {
    plane: Plane,
    points: [DVec3; 3],
    material: String,
}

impl Carver {
    /// Prepares a brush for carving. `None` when it encloses no volume.
    pub fn new(solid: &Solid) -> Option<Self> {
        let faces = solid_faces(solid);
        if faces.len() < 4 {
            return None;
        }
        let bounds = faces_bounds(&faces);
        let sides: Vec<_> = solid.sides().collect();
        let mut prepared: Vec<CarverFace> = Vec::with_capacity(faces.len());
        for face in &faces {
            if prepared.iter().any(|earlier| same_plane(&earlier.plane, &face.plane)) {
                continue;
            }
            let side = sides[face.side];
            prepared.push(CarverFace {
                plane: face.plane,
                points: side.plane,
                material: side.material.clone(),
            });
        }
        Some(Self { faces: prepared, bounds })
    }

    /// Cuts the space the carver fills out of `target`, keeping the rest as a few
    /// well-shaped convex pieces instead of the long slivers Hammer's Carve leaves.
    ///
    /// The target is first cut along the sides of the carver's bounding box, so whatever
    /// lies beyond the box stays in up to six plain slabs. What is left inside the box is
    /// shared out between the carver's faces: each point goes to the face it lies furthest
    /// in front of. That gives every face at most one piece, bounded by the face itself and
    /// by the planes halfway between it and its neighbours, which run out from the carver's
    /// edges like the spokes of a wheel. A cylinder through a floor leaves one wedge per
    /// side around the hole.
    ///
    /// New faces lying on a carver face take its material. The cuts between pieces take the
    /// material most of the target's sides use.
    ///
    /// Returns `Ok(None)` when the two do not overlap, so the target stays as it is. The
    /// pieces keep the target's ids, apart from their new faces; give them fresh ones
    /// before adding them to a map.
    pub fn carve(&self, target: &Solid, ids: &mut IdAllocator) -> Result<Option<Vec<Solid>>, ClipError> {
        let target_faces = solid_faces(target);
        if target_faces.len() < 4 {
            return Ok(None);
        }
        let bounds = self.bounds;
        let target_bounds = faces_bounds(&target_faces);
        let apart = (0..3).any(|axis| {
            bounds.min[axis] >= target_bounds.max[axis] - EPSILON
                || target_bounds.min[axis] >= bounds.max[axis] - EPSILON
        });
        if apart {
            return Ok(None);
        }
        if target.sides().any(|side| side.dispinfo().is_some()) {
            return Err(ClipError::Displacement);
        }
        let fill = main_material(target);
        let center = bounds.center();
        let mut remaining = target.clone();
        let mut pieces = Vec::new();
        for axis in 0..3 {
            for (sign, edge) in [(-1.0, bounds.min[axis]), (1.0, bounds.max[axis])] {
                let mut normal = DVec3::ZERO;
                normal[axis] = sign;
                let plane = Plane { normal, dist: sign * edge };
                if !crosses(&remaining, &plane) {
                    continue;
                }
                let material = self
                    .faces
                    .iter()
                    .find(|face| same_plane(&face.plane, &plane))
                    .map_or(fill.as_str(), |face| face.material.as_str());
                let points = plane_points(&plane, center);
                let [a, b, c] = points;
                let outside = half(&remaining, [c, b, a], material, ids);
                let inside = half(&remaining, points, &fill, ids);
                if let (Some(outside), Some(inside)) = (outside, inside) {
                    pieces.push(outside);
                    remaining = inside;
                }
            }
        }
        for face in &self.faces {
            pieces.extend(self.region(face, &remaining, &fill, ids));
        }
        let kept: f64 = pieces.iter().map(volume).sum();
        if volume(target) - kept < MIN_CARVED_VOLUME {
            return Ok(None);
        }
        Ok(Some(pieces))
    }

    /// The part of `remaining` lying further in front of `face` than of any other face,
    /// with `fill` on the cuts between it and its neighbours' parts.
    fn region(&self, face: &CarverFace, remaining: &Solid, fill: &str, ids: &mut IdAllocator) -> Option<Solid> {
        let [a, b, c] = face.points;
        let mut piece = keep_behind(remaining, [c, b, a], &face.material, ids)?;
        let mut piece_corners = corners(&piece);
        let mut others: Vec<&CarverFace> =
            self.faces.iter().filter(|other| !std::ptr::eq(*other, face)).collect();
        others.sort_by(|a, b| facing(face, b).total_cmp(&facing(face, a)));
        for other in others {
            let Some(between) = bisector(&face.plane, &other.plane) else {
                continue;
            };
            let (lowest, highest) = range(&piece_corners, &between);
            if highest <= EPSILON {
                continue;
            }
            if lowest >= -EPSILON {
                return None;
            }
            piece = half(&piece, plane_points(&between, self.bounds.center()), fill, ids)?;
            piece_corners = corners(&piece);
        }
        Some(piece)
    }
}

/// Volume in cubic units below which a carve counts as removing nothing, so brushes that
/// only touch the carver stay whole.
const MIN_CARVED_VOLUME: f64 = 0.5;

/// How closely two faces point the same way, so the faces beside one come first.
fn facing(face: &CarverFace, other: &CarverFace) -> f64 {
    face.plane.normal.dot(other.plane.normal)
}

/// The volume of a brush in cubic units.
fn volume(solid: &Solid) -> f64 {
    let faces = solid_faces(solid);
    let six_times: f64 = faces
        .iter()
        .map(|face| {
            let first = face.vertices[0];
            face.vertices
                .windows(2)
                .skip(1)
                .map(|pair| first.dot((pair[0] - first).cross(pair[1] - first)))
                .sum::<f64>()
        })
        .sum();
    six_times.abs() / 6.0
}

/// The part of a brush behind the plane through `points`, with any new face using
/// `material`. A brush wholly behind the plane comes back whole; one wholly in front, or
/// only touching it, gives `None`.
fn keep_behind(solid: &Solid, points: [DVec3; 3], material: &str, ids: &mut IdAllocator) -> Option<Solid> {
    let Some(plane) = Plane::from_points(points) else {
        return Some(solid.clone());
    };
    let (lowest, highest) = extent(solid, &plane);
    if highest <= EPSILON {
        return Some(solid.clone());
    }
    if lowest >= -EPSILON {
        return None;
    }
    half(solid, points, material, ids)
}

/// Whether a plane passes through a brush rather than missing it or only touching it.
fn crosses(solid: &Solid, plane: &Plane) -> bool {
    let (lowest, highest) = extent(solid, plane);
    lowest < -EPSILON && highest > EPSILON
}

/// The lowest and highest signed distances of a brush's corners from a plane.
fn extent(solid: &Solid, plane: &Plane) -> (f64, f64) {
    range(&corners(solid), plane)
}

/// Every corner of a brush, once per face it belongs to.
fn corners(solid: &Solid) -> Vec<DVec3> {
    solid_faces(solid).into_iter().flat_map(|face| face.vertices).collect()
}

/// The lowest and highest signed distances of `points` from a plane.
fn range(points: &[DVec3], plane: &Plane) -> (f64, f64) {
    points
        .iter()
        .map(|point| plane.distance(*point))
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), distance| {
            (low.min(distance), high.max(distance))
        })
}

/// The plane where a point lies as far in front of `own` as of `other`, facing the side
/// where `other` is the nearer of the two. `None` for two parallel planes facing the same
/// way.
fn bisector(own: &Plane, other: &Plane) -> Option<Plane> {
    let normal = other.normal - own.normal;
    let length = normal.length();
    (length > 1e-6).then(|| Plane {
        normal: normal / length,
        dist: (other.dist - own.dist) / length,
    })
}

/// Whether two planes are the same plane facing the same way.
fn same_plane(a: &Plane, b: &Plane) -> bool {
    a.normal.dot(b.normal) > 1.0 - 1e-9 && (a.dist - b.dist).abs() < EPSILON
}

/// The material most of a brush's sides use, the first of them on a tie.
fn main_material(solid: &Solid) -> String {
    let materials: Vec<&str> = solid.sides().map(|side| side.material.as_str()).collect();
    let count = |material: &str| materials.iter().filter(|other| **other == material).count();
    let mut best = materials.first().copied().unwrap_or_default();
    for material in &materials {
        if count(material) > count(best) {
            best = material;
        }
    }
    best.to_owned()
}

#[cfg(test)]
mod tests {
    use glam::DVec3;

    use super::*;
    use crate::build::box_solid;
    use crate::primitive::{primitive_solid, Shape};
    use crate::Vmf;

    fn uses(solid: &Solid, material: &str) -> usize {
        solid.sides().filter(|side| side.material == material).count()
    }

    #[test]
    fn a_door_hole_leaves_the_wall_around_it() {
        let mut ids = IdAllocator::for_map(&Vmf::new_map());
        let wall = box_solid(DVec3::ZERO, DVec3::new(256.0, 16.0, 128.0), "BRICK", &mut ids).unwrap();
        let door = box_solid(
            DVec3::new(64.0, -8.0, 0.0),
            DVec3::new(128.0, 24.0, 96.0),
            "TOOLS/TOOLSNODRAW",
            &mut ids,
        )
        .unwrap();
        let pieces = carve_solid(&wall, &door, &mut ids)
            .unwrap()
            .expect("the door cuts the wall");
        assert_eq!(pieces.len(), 3);
        let total: f64 = pieces.iter().map(volume).sum();
        assert!((total - (256.0 * 16.0 * 128.0 - 64.0 * 16.0 * 96.0)).abs() < 1e-6);
        assert_eq!(
            pieces
                .iter()
                .map(|piece| uses(piece, "TOOLS/TOOLSNODRAW"))
                .sum::<usize>(),
            3
        );
    }

    #[test]
    fn a_cylinder_leaves_one_wedge_per_side_around_the_hole() {
        let mut ids = IdAllocator::for_map(&Vmf::new_map());
        let floor = box_solid(DVec3::ZERO, DVec3::new(512.0, 512.0, 32.0), "FLOOR", &mut ids).unwrap();
        let pillar = primitive_solid(
            Shape::Cylinder,
            DVec3::new(128.0, 128.0, -64.0),
            DVec3::new(384.0, 384.0, 96.0),
            16,
            "HOLE",
            &mut ids,
        )
        .unwrap();
        let pieces = carve_solid(&floor, &pillar, &mut ids)
            .unwrap()
            .expect("the pillar cuts the floor");
        assert_eq!(pieces.len(), 4 + 16);
        let wedges: Vec<_> = pieces.iter().filter(|piece| uses(piece, "HOLE") > 0).collect();
        assert_eq!(wedges.len(), 16);
        assert!(wedges.iter().all(|piece| uses(piece, "HOLE") == 1));
        let hole = volume(&pillar) * 32.0 / 160.0;
        let total: f64 = pieces.iter().map(volume).sum();
        assert!((total + hole - 512.0 * 512.0 * 32.0).abs() < 1.0, "{total} + {hole}");
        let smallest = pieces.iter().map(volume).fold(f64::INFINITY, f64::min);
        assert!(smallest > 1000.0, "a piece of {smallest} cubic units is a sliver");
    }

    #[test]
    fn a_shallow_pit_keeps_a_floor_under_it() {
        let mut ids = IdAllocator::for_map(&Vmf::new_map());
        let block = box_solid(DVec3::ZERO, DVec3::splat(256.0), "ROCK", &mut ids).unwrap();
        let pit = primitive_solid(
            Shape::Cylinder,
            DVec3::new(64.0, 64.0, 128.0),
            DVec3::new(192.0, 192.0, 320.0),
            8,
            "PIT",
            &mut ids,
        )
        .unwrap();
        let pieces = carve_solid(&block, &pit, &mut ids)
            .unwrap()
            .expect("the pit cuts the block");
        let total: f64 = pieces.iter().map(volume).sum();
        let pit_volume = volume(&pit) * 128.0 / 192.0;
        assert!(
            (total + pit_volume - 256.0f64.powi(3)).abs() < 1.0,
            "{total} + {pit_volume}"
        );
        assert!(pieces.iter().all(|piece| uses(piece, "PIT") <= 1));
    }

    #[test]
    fn a_sphere_leaves_at_most_one_of_its_faces_on_each_piece() {
        let mut ids = IdAllocator::for_map(&Vmf::new_map());
        let plate = box_solid(DVec3::new(-512.0, -512.0, 0.0), DVec3::new(512.0, 512.0, 64.0), "X", &mut ids).unwrap();
        let ball = primitive_solid(Shape::Sphere, DVec3::splat(-256.0), DVec3::splat(256.0), 100, "Y", &mut ids).unwrap();
        let pieces = carve_solid(&plate, &ball, &mut ids).unwrap().expect("the ball cuts the plate");
        let total: f64 = pieces.iter().map(volume).sum();
        assert!(total < 1024.0 * 1024.0 * 64.0);
        assert!(pieces.iter().all(|piece| uses(piece, "Y") <= 1));
    }

    #[test]
    fn brushes_that_only_touch_are_left_alone() {
        let mut ids = IdAllocator::for_map(&Vmf::new_map());
        let floor = box_solid(DVec3::ZERO, DVec3::new(256.0, 256.0, 16.0), "X", &mut ids).unwrap();
        let crate_box = box_solid(DVec3::new(0.0, 0.0, 16.0), DVec3::splat(64.0), "Y", &mut ids).unwrap();
        assert_eq!(carve_solid(&floor, &crate_box, &mut ids), Ok(None));
    }

    #[test]
    fn a_cylinder_beside_a_box_corner_leaves_it_whole() {
        let mut ids = IdAllocator::for_map(&Vmf::new_map());
        let block = box_solid(DVec3::ZERO, DVec3::splat(128.0), "X", &mut ids).unwrap();
        let pillar = primitive_solid(
            Shape::Cylinder,
            DVec3::new(120.0, 120.0, 0.0),
            DVec3::new(376.0, 376.0, 128.0),
            16,
            "Y",
            &mut ids,
        )
        .unwrap();
        assert_eq!(carve_solid(&block, &pillar, &mut ids), Ok(None));
    }
}
