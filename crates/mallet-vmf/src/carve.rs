//! Hammer's Carve: cutting one brush's shape out of the brushes it overlaps.

use crate::clip::{split_solid, ClipError};
use crate::geometry::{faces_bounds, solid_faces, Plane, EPSILON};
use crate::{IdAllocator, Solid};

/// Cuts the space `carver` fills out of `target`, as Hammer's Carve does: the target is
/// split along each of the carver's faces in turn, the pieces outside the carver are kept
/// and the piece inside is thrown away. Each new face takes the material of the carver's
/// face it lies on.
///
/// Returns `Ok(None)` when the two do not overlap, so the target stays as it is. The
/// pieces keep the target's ids, apart from their new faces; give them fresh ones before
/// adding them to a map.
pub fn carve_solid(target: &Solid, carver: &Solid, ids: &mut IdAllocator) -> Result<Option<Vec<Solid>>, ClipError> {
    let carver_faces = solid_faces(carver);
    let target_faces = solid_faces(target);
    if carver_faces.len() < 4 || target_faces.len() < 4 {
        return Ok(None);
    }
    let (a, b) = (faces_bounds(&carver_faces), faces_bounds(&target_faces));
    let apart = (0..3).any(|axis| a.min[axis] >= b.max[axis] - EPSILON || b.min[axis] >= a.max[axis] - EPSILON);
    if apart {
        return Ok(None);
    }
    if target.sides().any(|side| side.dispinfo().is_some()) {
        return Err(ClipError::Displacement);
    }
    let sides: Vec<_> = carver.sides().collect();
    let mut remaining = target.clone();
    let mut outside = Vec::new();
    for face in &carver_faces {
        let side = sides[face.side];
        match split_solid(&remaining, side.plane, &side.material, ids) {
            Ok(split) => {
                outside.push(split.front);
                remaining = split.back;
            }
            Err(ClipError::Uncut) => {
                if in_front(&remaining, &face.plane) {
                    if outside.is_empty() {
                        return Ok(None);
                    }
                    outside.push(remaining);
                    return Ok(Some(outside));
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(Some(outside))
}

/// Whether a brush lies in front of a plane, touching it at most.
fn in_front(solid: &Solid, plane: &Plane) -> bool {
    solid_faces(solid)
        .iter()
        .flat_map(|face| &face.vertices)
        .all(|vertex| plane.distance(*vertex) >= -EPSILON)
}

#[cfg(test)]
mod tests {
    use glam::DVec3;

    use super::*;
    use crate::build::box_solid;
    use crate::Vmf;

    #[test]
    fn a_door_hole_leaves_the_wall_around_it() {
        let mut ids = IdAllocator::for_map(&Vmf::new_map());
        let wall = box_solid(DVec3::ZERO, DVec3::new(256.0, 16.0, 128.0), "BRICK", &mut ids).unwrap();
        let door = box_solid(DVec3::new(64.0, -8.0, 0.0), DVec3::new(128.0, 24.0, 96.0), "TOOLS/TOOLSNODRAW", &mut ids)
            .unwrap();
        let pieces = carve_solid(&wall, &door, &mut ids).unwrap().expect("the door cuts the wall");
        assert_eq!(pieces.len(), 3);
        let volume: f64 = pieces
            .iter()
            .map(|piece| {
                let bounds = faces_bounds(&solid_faces(piece));
                let size = bounds.size();
                size.x * size.y * size.z
            })
            .sum();
        assert_eq!(volume, 256.0 * 16.0 * 128.0 - 64.0 * 16.0 * 96.0);
        assert!(pieces
            .iter()
            .flat_map(|piece| piece.sides())
            .any(|side| side.material == "TOOLS/TOOLSNODRAW"));
    }

    #[test]
    fn brushes_that_only_touch_are_left_alone() {
        let mut ids = IdAllocator::for_map(&Vmf::new_map());
        let floor = box_solid(DVec3::ZERO, DVec3::new(256.0, 256.0, 16.0), "X", &mut ids).unwrap();
        let crate_box = box_solid(DVec3::new(0.0, 0.0, 16.0), DVec3::splat(64.0), "Y", &mut ids).unwrap();
        assert_eq!(carve_solid(&floor, &crate_box, &mut ids), Ok(None));
    }
}
