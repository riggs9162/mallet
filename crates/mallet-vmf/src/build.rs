//! Builders for new brushes, matching what Hammer's tools create.

use glam::DVec3;
use mallet_kv::{Block, Pair};

use crate::geometry::{solid_faces, Plane};
use crate::texture::face_axes;
use crate::{IdAllocator, Side, Solid, SolidItem, TexAxis};

/// Texture scale Hammer applies to new faces.
const DEFAULT_TEXTURE_SCALE: f64 = 0.25;
/// Luxels per world unit Hammer applies to new faces.
const DEFAULT_LIGHTMAP_SCALE: u32 = 16;

/// Builds an axis-aligned box brush between two corners, with every face using `material`
/// and world-aligned texture axes like Hammer's block tool.
///
/// Returns `None` when the box has no volume.
pub fn box_solid(min: DVec3, max: DVec3, material: &str, ids: &mut IdAllocator) -> Option<Solid> {
    let (min, max) = (min.min(max), min.max(max));
    if (max - min).min_element() <= 0.0 {
        return None;
    }
    let [x0, y0, z0] = min.to_array();
    let [x1, y1, z1] = max.to_array();
    let p = DVec3::new;
    let faces = [
        ([p(x0, y1, z1), p(x1, y1, z1), p(x1, y0, z1)], DVec3::X, DVec3::NEG_Y),
        ([p(x0, y0, z0), p(x1, y0, z0), p(x1, y1, z0)], DVec3::X, DVec3::NEG_Y),
        ([p(x0, y1, z1), p(x0, y0, z1), p(x0, y0, z0)], DVec3::Y, DVec3::NEG_Z),
        ([p(x1, y1, z0), p(x1, y0, z0), p(x1, y0, z1)], DVec3::Y, DVec3::NEG_Z),
        ([p(x1, y1, z1), p(x0, y1, z1), p(x0, y1, z0)], DVec3::X, DVec3::NEG_Z),
        ([p(x1, y0, z0), p(x0, y0, z0), p(x0, y0, z1)], DVec3::X, DVec3::NEG_Z),
    ];

    Some(solid_from_sides(&faces, material, ids))
}

/// Distance between the three points written for each side of a convex brush. Points
/// spread this far apart keep the plane accurate once they are rounded.
const PLANE_POINT_SPREAD: f64 = 128.0;

/// Decimal places kept in the plane points of a convex brush.
const PLANE_POINT_DECIMALS: i32 = 3;

/// Builds a brush bounded by outward-facing `planes`, such as the planes of a convex hull,
/// with every face using `material` and textures aligned to each face.
///
/// `inside` is any point inside the shape; each side's points are placed near it. Planes
/// that end up with no face are dropped. Returns `None` when the planes do not enclose a
/// volume.
pub fn convex_solid(planes: &[Plane], inside: DVec3, material: &str, ids: &mut IdAllocator) -> Option<Solid> {
    let faces: Vec<([DVec3; 3], DVec3, DVec3)> = planes
        .iter()
        .filter(|plane| plane.normal.is_finite() && plane.normal.length() > 0.5)
        .map(|plane| {
            let normal = plane.normal.normalize();
            let center = inside - normal * (normal.dot(inside) - plane.dist);
            let along = normal.any_orthonormal_vector();
            let across = normal.cross(along);
            let points = [
                center + along * PLANE_POINT_SPREAD,
                center,
                center + across * PLANE_POINT_SPREAD,
            ]
            .map(round_point);
            let (u, v) = face_axes(normal);
            (points, u, v)
        })
        .collect();
    let mut scratch = ids.clone();
    let draft = solid_from_sides(&faces, material, &mut scratch);
    let built = solid_faces(&draft);
    if built.len() < 4 {
        return None;
    }
    let kept: Vec<([DVec3; 3], DVec3, DVec3)> = built.iter().map(|face| faces[face.side]).collect();
    Some(solid_from_sides(&kept, material, ids))
}

fn round_point(point: DVec3) -> DVec3 {
    let scale = 10f64.powi(PLANE_POINT_DECIMALS);
    (point * scale).round() / scale + DVec3::ZERO
}

/// A brush with one side per plane and texture axes, using Hammer's defaults for the rest.
pub(crate) fn solid_from_sides(faces: &[([DVec3; 3], DVec3, DVec3)], material: &str, ids: &mut IdAllocator) -> Solid {
    let id = ids.object();
    let mut items = vec![SolidItem::Key(Pair::new("id", id.to_string()))];
    for &(plane, u, v) in faces {
        items.push(SolidItem::Side(Box::new(new_side(ids.side(), plane, material, u, v))));
    }
    let mut editor = Block::new("editor");
    editor.push_pair("color", editor_color(id));
    editor.push_pair("visgroupshown", "1");
    editor.push_pair("visgroupautoshown", "1");
    items.push(SolidItem::Other(editor));
    Solid { items }
}

/// A new face with Hammer's default texture scale, lightmap scale and no rotation.
pub(crate) fn new_side(id: u32, plane: [DVec3; 3], material: &str, u: DVec3, v: DVec3) -> Side {
    let axis = |axis| TexAxis {
        axis,
        offset: 0.0,
        scale: DEFAULT_TEXTURE_SCALE,
    };
    Side {
        id,
        plane,
        material: material.to_owned(),
        uaxis: axis(u),
        vaxis: axis(v),
        rotation: Some(0.0),
        lightmap_scale: Some(DEFAULT_LIGHTMAP_SCALE),
        smoothing_groups: Some(0),
        extra: Vec::new(),
        extra_slots: Vec::new(),
    }
}

/// A pseudo-random green-blue colour per brush, like the colours Hammer picks.
fn editor_color(id: u32) -> String {
    let green = 100 + id.wrapping_mul(37) % 156;
    let blue = 100 + id.wrapping_mul(91) % 156;
    format!("0 {green} {blue}")
}
