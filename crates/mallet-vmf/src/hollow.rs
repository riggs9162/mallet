//! Hammer's Make Hollow: turning a brush into walls of a set thickness around the space
//! it filled, the quick way the Valve tutorials make a room.

use std::fmt;

use crate::geometry::{faces_bounds, solid_faces, Plane};
use crate::{IdAllocator, Solid, SolidItem};

/// Why a brush could not be hollowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HollowError {
    /// A wall thickness of zero leaves nothing to build.
    NoThickness,
    /// The brush has displacements, which hollowing would destroy.
    Displacement,
    /// The brush's planes do not enclose a volume.
    Invalid,
    /// Walls this thick would fill the brush.
    TooThick,
}

impl fmt::Display for HollowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoThickness => "the wall width must not be 0",
            Self::Displacement => "brushes with displacements cannot be hollowed",
            Self::Invalid => "the brush is not a closed solid",
            Self::TooThick => "the walls would be thicker than the brush",
        })
    }
}

/// Turns a brush into one wall per face, `thickness` deep, as Hammer's Make Hollow does:
/// each wall is the brush cut off by its face's plane moved inwards, so the walls meet,
/// overlapping at the edges. The new inner face of each wall copies the face it lies
/// behind, texture and all. A negative thickness first grows the brush outwards by that
/// much, so the walls end up around the space the brush filled.
///
/// Every wall gets fresh ids; the brush itself is left alone.
pub fn hollow_solid(solid: &Solid, thickness: f64, ids: &mut IdAllocator) -> Result<Vec<Solid>, HollowError> {
    if thickness == 0.0 || !thickness.is_finite() {
        return Err(HollowError::NoThickness);
    }
    if solid.sides().any(|side| side.dispinfo().is_some()) {
        return Err(HollowError::Displacement);
    }
    let depth = thickness.abs();
    let base = if thickness < 0.0 { grown(solid, depth) } else { solid.clone() };
    let faces = solid_faces(&base);
    if faces.len() < 4 {
        return Err(HollowError::Invalid);
    }
    if faces_bounds(&faces).size().min_element() <= 2.0 * depth {
        return Err(HollowError::TooThick);
    }
    let sides: Vec<_> = base.sides().cloned().collect();
    let mut walls = Vec::with_capacity(faces.len());
    for face in &faces {
        let mut wall = base.clone();
        wall.renumber(ids);
        let mut cap = sides[face.side].clone();
        cap.id = ids.side();
        let [a, b, c] = cap.plane;
        let inwards = face.plane.normal * depth;
        cap.plane = [c - inwards, b - inwards, a - inwards];
        let after_last_side = wall
            .items
            .iter()
            .rposition(|item| matches!(item, SolidItem::Side(_)))
            .map_or(wall.items.len(), |index| index + 1);
        wall.items.insert(after_last_side, SolidItem::Side(Box::new(cap)));
        if let Some(wall) = trimmed(wall) {
            walls.push(wall);
        }
    }
    Ok(walls)
}

/// The brush with every face moved outwards by `by`.
fn grown(solid: &Solid, by: f64) -> Solid {
    let mut grown = solid.clone();
    for side in grown.sides_mut() {
        if let Some(plane) = Plane::from_points(side.plane) {
            let outwards = plane.normal * by;
            side.plane = side.plane.map(|point| point + outwards);
        }
    }
    grown
}

/// The brush without the sides that ended up with no face, or `None` when it has no
/// volume left.
fn trimmed(mut solid: Solid) -> Option<Solid> {
    let faces = solid_faces(&solid);
    if faces.len() < 4 {
        return None;
    }
    let mut side_index = 0;
    solid.items.retain(|item| {
        if !matches!(item, SolidItem::Side(_)) {
            return true;
        }
        let keep = faces.iter().any(|face| face.side == side_index);
        side_index += 1;
        keep
    });
    Some(solid)
}

#[cfg(test)]
mod tests {
    use glam::DVec3;

    use super::*;
    use crate::build::box_solid;

    fn bounds_of(solid: &Solid) -> crate::geometry::Bounds {
        faces_bounds(&solid_faces(solid))
    }

    #[test]
    fn a_box_becomes_six_walls_inside_its_bounds() {
        let mut ids = IdAllocator::for_map(&crate::Vmf::new_map());
        let room = box_solid(DVec3::ZERO, DVec3::new(1024.0, 1024.0, 512.0), "DEV/DEV_MEASUREWALL01A", &mut ids)
            .unwrap();
        let walls = hollow_solid(&room, 32.0, &mut ids).unwrap();
        assert_eq!(walls.len(), 6);
        let floor = walls
            .iter()
            .map(bounds_of)
            .find(|bounds| bounds.max.z == 32.0)
            .expect("a floor 32 units thick");
        assert_eq!(floor.min, DVec3::ZERO);
        assert_eq!(floor.max, DVec3::new(1024.0, 1024.0, 32.0));
        let mut solid_ids = std::collections::HashSet::new();
        let mut side_ids = std::collections::HashSet::new();
        for wall in &walls {
            assert!(solid_ids.insert(wall.id().unwrap()));
            for side in wall.sides() {
                assert!(side_ids.insert(side.id));
                assert_eq!(side.material, "DEV/DEV_MEASUREWALL01A");
            }
            assert_eq!(wall.sides().count(), 6);
        }
    }

    #[test]
    fn negative_thickness_builds_the_walls_outside() {
        let mut ids = IdAllocator::for_map(&crate::Vmf::new_map());
        let room = box_solid(DVec3::ZERO, DVec3::splat(256.0), "TOOLS/TOOLSNODRAW", &mut ids).unwrap();
        let walls = hollow_solid(&room, -16.0, &mut ids).unwrap();
        let mut outer = crate::geometry::Bounds::EMPTY;
        for wall in &walls {
            outer.add_bounds(&bounds_of(wall));
        }
        assert_eq!(outer.min, DVec3::splat(-16.0));
        assert_eq!(outer.max, DVec3::splat(272.0));
    }

    #[test]
    fn walls_thicker_than_the_brush_are_refused() {
        let mut ids = IdAllocator::for_map(&crate::Vmf::new_map());
        let slab = box_solid(DVec3::ZERO, DVec3::new(256.0, 256.0, 32.0), "TOOLS/TOOLSNODRAW", &mut ids).unwrap();
        assert_eq!(hollow_solid(&slab, 16.0, &mut ids), Err(HollowError::TooThick));
        assert_eq!(hollow_solid(&slab, 0.0, &mut ids), Err(HollowError::NoThickness));
    }
}
