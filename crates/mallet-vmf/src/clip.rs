//! Splitting brushes along a plane, as Hammer's clipping tool does.

use std::collections::HashMap;

use glam::DVec3;

use crate::build::new_side;
use crate::geometry::{solid_faces, Plane, EPSILON};
use crate::texture::world_axes;
use crate::{IdAllocator, Solid, SolidItem};

/// Which part of a clipped brush the clipping tool keeps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Keep {
    /// Both halves, as two brushes.
    #[default]
    Both,
    /// The half the clip plane's normal points into.
    Front,
    /// The half behind the clip plane.
    Back,
}

impl Keep {
    /// The next mode in the order Shift+X cycles through, as in Hammer.
    pub fn next(self) -> Self {
        match self {
            Self::Both => Self::Front,
            Self::Front => Self::Back,
            Self::Back => Self::Both,
        }
    }

    /// The mode's name for the status bar.
    pub fn label(self) -> &'static str {
        match self {
            Self::Both => "both halves",
            Self::Front => "the front half",
            Self::Back => "the back half",
        }
    }

    /// Whether the front half survives.
    pub fn front(self) -> bool {
        self != Self::Back
    }

    /// Whether the back half survives.
    pub fn back(self) -> bool {
        self != Self::Front
    }
}

/// The two halves of a brush cut by a plane. Both keep the brush's id and side ids;
/// only the new face on the cut has a fresh id.
#[derive(Clone, Debug, PartialEq)]
pub struct Split {
    /// The part in front of the plane.
    pub front: Solid,
    /// The part behind the plane.
    pub back: Solid,
}

/// Why a brush could not be clipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipError {
    /// The three points do not span a plane.
    NoPlane,
    /// The plane misses the brush, or only touches it.
    Uncut,
    /// The brush has displacements, which a cut would destroy.
    Displacement,
}

/// Cuts a brush in two along the plane through `points`, wound like a side's plane so
/// that its normal points to the front.
///
/// Each half gets a new face on the cut with `material`, aligned to the world like a new
/// face in Hammer. Sides that end up with no area in a half are dropped from it.
pub fn split_solid(
    solid: &Solid,
    points: [DVec3; 3],
    material: &str,
    ids: &mut IdAllocator,
) -> Result<Split, ClipError> {
    let plane = Plane::from_points(points).ok_or(ClipError::NoPlane)?;
    if solid.sides().any(|side| side.dispinfo().is_some()) {
        return Err(ClipError::Displacement);
    }
    let faces = solid_faces(solid);
    let distances = faces.iter().flat_map(|face| &face.vertices).map(|vertex| plane.distance(*vertex));
    let (lowest, highest) = distances.fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), distance| {
        (low.min(distance), high.max(distance))
    });
    if lowest >= -EPSILON || highest <= EPSILON {
        return Err(ClipError::Uncut);
    }
    let [a, b, c] = points;
    let back = half(solid, points, material, ids).ok_or(ClipError::Uncut)?;
    let front = half(solid, [c, b, a], material, ids).ok_or(ClipError::Uncut)?;
    Ok(Split { front, back })
}

/// The part of `solid` behind the plane through `points`, or `None` when nothing is left.
pub(crate) fn half(solid: &Solid, points: [DVec3; 3], material: &str, ids: &mut IdAllocator) -> Option<Solid> {
    let normal = Plane::from_points(points)?.normal;
    let (u, v) = world_axes(normal);
    let mut piece = solid.clone();
    let after_last_side = piece
        .items
        .iter()
        .rposition(|item| matches!(item, SolidItem::Side(_)))
        .map_or(piece.items.len(), |index| index + 1);
    let cap = new_side(ids.side(), points, material, u, v);
    piece.items.insert(after_last_side, SolidItem::Side(Box::new(cap)));
    let faces = solid_faces(&piece);
    if faces.len() < 4 {
        return None;
    }
    let mut side_index = 0;
    piece.items.retain(|item| {
        if !matches!(item, SolidItem::Side(_)) {
            return true;
        }
        let keep = faces.iter().any(|face| face.side == side_index);
        side_index += 1;
        keep
    });
    Some(piece)
}

/// Gives a brush and its sides fresh ids, returning each old side id with its new one.
pub fn renumber_sides(solid: &mut Solid, ids: &mut IdAllocator) -> Vec<(u32, u32)> {
    let before: Vec<u32> = solid.sides().map(|side| side.id).collect();
    solid.renumber(ids);
    before.into_iter().zip(solid.sides().map(|side| side.id)).collect()
}

/// Rewrites a space-separated list of side ids, such as an overlay's `sides`, after
/// brushes were clipped. `faces` maps each side of a clipped brush to the sides it became;
/// ids not in it stay as they are.
///
/// Returns `None` when nothing changes, or when no face would be left, so an overlay is
/// never left on no face at all.
pub fn remap_side_list(list: &str, faces: &HashMap<u32, Vec<u32>>) -> Option<String> {
    let mut changed = false;
    let mut out: Vec<u32> = Vec::new();
    for word in list.split_whitespace() {
        let Ok(id) = word.parse::<u32>() else {
            return None;
        };
        let mapped = match faces.get(&id) {
            Some(mapped) => {
                changed |= mapped.as_slice() != [id];
                mapped.as_slice()
            }
            None => std::slice::from_ref(&id),
        };
        for id in mapped {
            if !out.contains(id) {
                out.push(*id);
            }
        }
    }
    (changed && !out.is_empty()).then(|| out.iter().map(u32::to_string).collect::<Vec<_>>().join(" "))
}
