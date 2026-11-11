//! Turns a brush's planes into face polygons, the same way VBSP builds windings.

use glam::DVec3;

use crate::Solid;

/// Distance within which a point counts as lying on a plane.
pub const EPSILON: f64 = 0.01;

/// Half-size of the starting polygon each face is clipped from. Larger than any legal map.
const MAX_EXTENT: f64 = 131_072.0;

/// Vertices this close to a whole number are snapped onto it.
const SNAP_EPSILON: f64 = 0.001;

/// A plane in `normal · p = dist` form, with the normal pointing out of the brush.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plane {
    /// Unit normal.
    pub normal: DVec3,
    /// Distance from the origin along the normal.
    pub dist: f64,
}

impl Plane {
    /// Builds a plane from three points in Hammer's winding order.
    ///
    /// Returns `None` when the points are collinear or coincide.
    pub fn from_points([a, b, c]: [DVec3; 3]) -> Option<Self> {
        let normal = (a - b).cross(c - b);
        let length = normal.length();
        if length < 1e-9 {
            return None;
        }
        let normal = normal / length;
        Some(Self {
            normal,
            dist: normal.dot(a),
        })
    }

    /// Signed distance of a point from the plane; positive means outside the brush.
    pub fn distance(&self, point: DVec3) -> f64 {
        self.normal.dot(point) - self.dist
    }
}

/// An axis-aligned bounding box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    /// Smallest corner.
    pub min: DVec3,
    /// Largest corner.
    pub max: DVec3,
}

impl Default for Bounds {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Bounds {
    /// A box containing nothing; adding any point makes it valid.
    pub const EMPTY: Self = Self {
        min: DVec3::splat(f64::INFINITY),
        max: DVec3::splat(f64::NEG_INFINITY),
    };

    /// Whether no point has been added yet.
    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x
    }

    /// Grows the box to contain a point.
    pub fn add_point(&mut self, point: DVec3) {
        self.min = self.min.min(point);
        self.max = self.max.max(point);
    }

    /// Grows the box to contain another box.
    pub fn add_bounds(&mut self, other: &Bounds) {
        if !other.is_empty() {
            self.add_point(other.min);
            self.add_point(other.max);
        }
    }

    /// Centre of the box.
    pub fn center(&self) -> DVec3 {
        (self.min + self.max) * 0.5
    }

    /// Edge lengths of the box.
    pub fn size(&self) -> DVec3 {
        self.max - self.min
    }

    /// Maps a point from this box onto another, scaling each axis so the corners line up.
    /// Along an axis where this box is flat, the point is only shifted.
    pub fn remap(&self, to: &Bounds, point: DVec3) -> DVec3 {
        let from_size = self.size();
        let to_size = to.size();
        let mut mapped = DVec3::ZERO;
        for axis in 0..3 {
            mapped[axis] = if from_size[axis] > 0.0 {
                to.min[axis] + (point[axis] - self.min[axis]) * to_size[axis] / from_size[axis]
            } else {
                point[axis] + to.min[axis] - self.min[axis]
            };
        }
        mapped
    }
}

/// One polygonal face of a brush.
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    /// Index of the side this face came from, counting only the solid's sides.
    pub side: usize,
    /// The face plane.
    pub plane: Plane,
    /// Polygon vertices in winding order.
    pub vertices: Vec<DVec3>,
}

/// Builds every face polygon of a brush.
///
/// Each side's plane starts as a huge square and is clipped by every other plane, keeping
/// the part inside the brush. Sides with degenerate planes or no remaining area are skipped.
/// A brush whose planes do not enclose a volume would leave faces reaching the size of the
/// starting square; such a brush is invalid and gets no faces at all.
pub fn solid_faces(solid: &Solid) -> Vec<Face> {
    let faces = clipped_faces(solid);
    let open = faces
        .iter()
        .flat_map(|face| &face.vertices)
        .any(|vertex| vertex.abs().max_element() >= MAX_EXTENT * 0.5);
    if open {
        Vec::new()
    } else {
        faces
    }
}

fn clipped_faces(solid: &Solid) -> Vec<Face> {
    let planes: Vec<Option<Plane>> = solid.sides().map(|side| Plane::from_points(side.plane)).collect();
    planes
        .iter()
        .enumerate()
        .filter_map(|(index, plane)| {
            let plane = (*plane)?;
            let mut polygon = base_polygon(&plane);
            for (other_index, other) in planes.iter().enumerate() {
                if other_index == index {
                    continue;
                }
                if let Some(other) = other.filter(|other| reaches(&polygon, other)) {
                    polygon = clip_polygon(&polygon, &other);
                }
                if polygon.len() < 3 {
                    return None;
                }
            }
            Some(Face {
                side: index,
                plane,
                vertices: polygon.into_iter().map(snap).collect(),
            })
        })
        .collect()
}

/// Whether any point of a polygon lies in front of a plane, so clipping by it would change
/// the polygon.
fn reaches(points: &[DVec3], plane: &Plane) -> bool {
    points.iter().any(|point| plane.distance(*point) > EPSILON)
}

/// Returns the bounds of a set of faces.
pub fn faces_bounds(faces: &[Face]) -> Bounds {
    let mut bounds = Bounds::EMPTY;
    for vertex in faces.iter().flat_map(|face| &face.vertices) {
        bounds.add_point(*vertex);
    }
    bounds
}

/// A square on the plane, far larger than the map, wound to face along the normal.
pub fn base_polygon(plane: &Plane) -> Vec<DVec3> {
    let normal = plane.normal;
    let mostly_vertical = normal.z.abs() > normal.x.abs() && normal.z.abs() > normal.y.abs();
    let up = if mostly_vertical { DVec3::X } else { DVec3::Z };
    let up = (up - normal * up.dot(normal)).normalize() * MAX_EXTENT;
    let right = up.cross(normal);
    let origin = normal * plane.dist;
    vec![
        origin - right + up,
        origin + right + up,
        origin + right - up,
        origin - right - up,
    ]
}

/// Clips a convex polygon by a plane, keeping the part on or behind it.
pub fn clip_polygon(points: &[DVec3], plane: &Plane) -> Vec<DVec3> {
    let mut out = Vec::with_capacity(points.len() + 1);
    for (index, &current) in points.iter().enumerate() {
        let next = points[(index + 1) % points.len()];
        let current_distance = plane.distance(current);
        let next_distance = plane.distance(next);
        if current_distance <= EPSILON {
            out.push(current);
        }
        let crosses = (current_distance < -EPSILON && next_distance > EPSILON)
            || (current_distance > EPSILON && next_distance < -EPSILON);
        if crosses {
            let t = current_distance / (current_distance - next_distance);
            out.push(current + (next - current) * t);
        }
    }
    out
}

fn snap(point: DVec3) -> DVec3 {
    let rounded = point.round();
    DVec3::select(
        (point - rounded).abs().cmplt(DVec3::splat(SNAP_EPSILON)),
        rounded,
        point,
    )
}
