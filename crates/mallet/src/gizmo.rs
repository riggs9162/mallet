//! The 3D transform gizmo Hammer++ draws on the selection: arrows and squares that move
//! it, rings that rotate it and cubes that scale it, depending on its mode.
//!
//! The gizmo keeps the same size on screen wherever the selection is. It is drawn with
//! egui on top of the 3D view's image. Moves are dragged live along a translation plane by
//! the window; a rotate or scale drag previews its transform through the overlay, like the
//! 2D handles, before it is applied as one undoable edit.

use std::f64::consts::{PI, TAU};

use eframe::egui::{self, Color32, Pos2, Rect, Stroke};
use glam::{DMat3, DMat4, DVec3};
use mallet_vmf::geometry::Bounds;
use mallet_vmf::transform::around;

use crate::transform::{snap_angle, GizmoMode};
use crate::view3d::Camera3D;

/// Length of an arm on screen, in pixels.
const ARM_PIXELS: f64 = 90.0;
/// How far beyond a drawn part, in pixels, the mouse still grabs it.
const GRAB_PIXELS: f32 = 6.0;
/// How close, in pixels, the mouse has to be to an arm or ring line to grab it: half the
/// line's width and the grab margin.
const REACH_PIXELS: f32 = 1.0 + GRAB_PIXELS;
/// Radius of the rotation rings, as a share of the arm.
const RING_RADIUS: f64 = 0.85;
/// Where the plane squares start and end along their two axes, as a share of the arm.
const PLANE_SPAN: [f64; 2] = [0.22, 0.42];
/// Half the size of the scale cubes, in pixels.
const CUBE_PIXELS: f32 = 5.0;
/// Radius of the centre marker, in pixels.
const CENTER_PIXELS: f32 = 6.0;
/// Segments drawn per ring.
const RING_SEGMENTS: usize = 64;
/// Arms shorter than this on screen point almost at the camera and cannot be dragged.
const MIN_ARM_PIXELS: f32 = 12.0;
/// Pixels of vertical mouse movement that double or halve the size in an even scale.
const UNIFORM_PIXELS: f64 = 150.0;

const AXIS_COLORS: [Color32; 3] = [
    Color32::from_rgb(230, 70, 70),
    Color32::from_rgb(90, 205, 90),
    Color32::from_rgb(80, 135, 250),
];
const HOT_COLOR: Color32 = Color32::from_rgb(255, 220, 60);
const AXES: [DVec3; 3] = [DVec3::X, DVec3::Y, DVec3::Z];

/// A part of the gizmo that can be hovered and dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// The centre: clicking it switches mode; dragging it moves in the view's plane or
    /// scales evenly.
    Center,
    /// The arrow, or scale cube, along a world axis.
    Axis(usize),
    /// The square for moving in the plane across a world axis.
    Plane(usize),
    /// The ring for rotating around a world axis.
    Ring(usize),
}

/// The gizmo as placed for one frame of one 3D view.
#[derive(Clone, Copy, Debug)]
pub struct Gizmo {
    pub center: DVec3,
    pub mode: GizmoMode,
    /// Length of an arm in world units at the centre's distance.
    arm: f64,
}

/// The two world axes across the one given.
fn others(axis: usize) -> (usize, usize) {
    ((axis + 1) % 3, (axis + 2) % 3)
}

fn distance_to_segment(point: Pos2, a: Pos2, b: Pos2) -> f32 {
    let along = b - a;
    let length = along.length_sq();
    let t = if length > 0.0 {
        ((point - a).dot(along) / length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    point.distance(a + along * t)
}

fn inside_polygon(point: Pos2, corners: &[Pos2]) -> bool {
    let mut sign = 0.0_f32;
    for (index, &a) in corners.iter().enumerate() {
        let b = corners[(index + 1) % corners.len()];
        let cross = (b - a).x * (point - a).y - (b - a).y * (point - a).x;
        if cross != 0.0 {
            if sign != 0.0 && cross.signum() != sign {
                return false;
            }
            sign = cross.signum();
        }
    }
    true
}

impl Gizmo {
    /// Places the gizmo at `center`, sized for the view. `None` when the centre is behind
    /// the camera.
    pub fn new(camera: &Camera3D, rect: Rect, center: DVec3, mode: GizmoMode) -> Option<Self> {
        let depth = camera.depth(center);
        (depth > 4.0).then(|| Self {
            center,
            mode,
            arm: depth * ARM_PIXELS / camera.focal(rect),
        })
    }

    fn tip(&self, axis: usize) -> DVec3 {
        self.center + AXES[axis] * self.arm
    }

    /// The screen ends of an arm, when it is long enough on screen to grab.
    fn arm_on_screen(&self, camera: &Camera3D, rect: Rect, axis: usize) -> Option<(Pos2, Pos2)> {
        let start = camera.project(rect, self.center)?;
        let end = camera.project(rect, self.tip(axis))?;
        (start.distance(end) >= MIN_ARM_PIXELS).then_some((start, end))
    }

    fn plane_corners(&self, camera: &Camera3D, rect: Rect, axis: usize) -> Option<[Pos2; 4]> {
        let (u, v) = others(axis);
        let [near, far] = PLANE_SPAN.map(|share| share * self.arm);
        let corner = |a: f64, b: f64| camera.project(rect, self.center + AXES[u] * a + AXES[v] * b);
        let corners = [
            corner(near, near)?,
            corner(far, near)?,
            corner(far, far)?,
            corner(near, far)?,
        ];
        let area = (corners[2] - corners[0]).length() * (corners[3] - corners[1]).length();
        (area > 40.0).then_some(corners)
    }

    /// Ring points in world space, closing back on the first.
    fn ring(&self, axis: usize) -> Vec<DVec3> {
        let (u, v) = others(axis);
        (0..=RING_SEGMENTS)
            .map(|step| {
                let angle = step as f64 / RING_SEGMENTS as f64 * TAU;
                self.center + (AXES[u] * angle.cos() + AXES[v] * angle.sin()) * self.arm * RING_RADIUS
            })
            .collect()
    }

    /// The part under a screen point, if any.
    pub fn hit(&self, camera: &Camera3D, rect: Rect, pointer: Pos2) -> Option<Part> {
        let center = camera.project(rect, self.center)?;
        if center.distance(pointer) <= CENTER_PIXELS * 0.8 + GRAB_PIXELS {
            return Some(Part::Center);
        }
        match self.mode {
            GizmoMode::Move => {
                let plane = (0..3).find(|axis| {
                    self.plane_corners(camera, rect, *axis)
                        .is_some_and(|corners| inside_polygon(pointer, &corners))
                });
                if let Some(axis) = plane {
                    return Some(Part::Plane(axis));
                }
                self.nearest_arm(camera, rect, pointer)
            }
            GizmoMode::Scale => {
                let cube = (0..3).find(|axis| {
                    self.arm_on_screen(camera, rect, *axis)
                        .is_some_and(|(_, end)| (end - pointer).abs().max_elem() <= CUBE_PIXELS + GRAB_PIXELS)
                });
                cube.map(Part::Axis).or_else(|| self.nearest_arm(camera, rect, pointer))
            }
            GizmoMode::Rotate => {
                let mut best: Option<(f32, usize)> = None;
                for axis in 0..3 {
                    let points: Vec<Option<Pos2>> = self
                        .ring(axis)
                        .into_iter()
                        .map(|point| camera.project(rect, point))
                        .collect();
                    for pair in points.windows(2) {
                        if let [Some(a), Some(b)] = pair {
                            let distance = distance_to_segment(pointer, *a, *b);
                            if distance <= REACH_PIXELS && best.is_none_or(|(closest, _)| distance < closest) {
                                best = Some((distance, axis));
                            }
                        }
                    }
                }
                best.map(|(_, axis)| Part::Ring(axis))
            }
        }
    }

    fn nearest_arm(&self, camera: &Camera3D, rect: Rect, pointer: Pos2) -> Option<Part> {
        (0..3)
            .filter_map(|axis| {
                let (start, end) = self.arm_on_screen(camera, rect, axis)?;
                let distance = distance_to_segment(pointer, start, end);
                (distance <= REACH_PIXELS).then_some((distance, axis))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, axis)| Part::Axis(axis))
    }

    /// Draws the gizmo, with the hovered or dragged part highlighted.
    pub fn draw(&self, painter: &egui::Painter, camera: &Camera3D, rect: Rect, hot: Option<Part>) {
        let Some(center) = camera.project(rect, self.center) else {
            return;
        };
        let color = |part: Part, axis: usize| {
            if hot == Some(part) {
                HOT_COLOR
            } else {
                AXIS_COLORS[axis]
            }
        };
        let width = |part: Part| if hot == Some(part) { 3.5_f32 } else { 2.0_f32 };
        let shadow = Stroke::new(4.5_f32, Color32::from_black_alpha(120));
        match self.mode {
            GizmoMode::Move | GizmoMode::Scale => {
                if self.mode == GizmoMode::Move {
                    for axis in 0..3 {
                        if let Some(corners) = self.plane_corners(camera, rect, axis) {
                            let part = Part::Plane(axis);
                            let base = color(part, axis);
                            let fill = Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 70);
                            painter.add(egui::Shape::convex_polygon(
                                corners.to_vec(),
                                fill,
                                Stroke::new(width(part) * 0.6, base),
                            ));
                        }
                    }
                }
                for axis in 0..3 {
                    let Some((start, end)) = self.arm_on_screen(camera, rect, axis) else {
                        continue;
                    };
                    let part = Part::Axis(axis);
                    let stroke = Stroke::new(width(part), color(part, axis));
                    painter.line_segment([start, end], shadow);
                    painter.line_segment([start, end], stroke);
                    let direction = (end - start).normalized();
                    if self.mode == GizmoMode::Move {
                        let side = egui::vec2(-direction.y, direction.x) * 6.0;
                        let base = end - direction * 14.0;
                        painter.add(egui::Shape::convex_polygon(
                            vec![end + direction * 4.0, base + side, base - side],
                            stroke.color,
                            Stroke::new(1.0_f32, Color32::from_black_alpha(160)),
                        ));
                    } else {
                        painter.rect(
                            Rect::from_center_size(end, egui::Vec2::splat(CUBE_PIXELS * 2.0)),
                            1.0,
                            stroke.color,
                            Stroke::new(1.0_f32, Color32::from_black_alpha(160)),
                            egui::StrokeKind::Outside,
                        );
                    }
                }
            }
            GizmoMode::Rotate => {
                let toward_camera = camera.position - self.center;
                for axis in 0..3 {
                    let part = Part::Ring(axis);
                    let base = color(part, axis);
                    let behind = Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), 70);
                    let points = self.ring(axis);
                    for pair in points.windows(2) {
                        let (Some(a), Some(b)) = (camera.project(rect, pair[0]), camera.project(rect, pair[1])) else {
                            continue;
                        };
                        let front = ((pair[0] + pair[1]) * 0.5 - self.center).dot(toward_camera) >= 0.0;
                        let stroke = if front {
                            Stroke::new(width(part), base)
                        } else {
                            Stroke::new(1.5_f32, behind)
                        };
                        if front {
                            painter.line_segment([a, b], shadow);
                        }
                        painter.line_segment([a, b], stroke);
                    }
                }
            }
        }
        let center_color = if hot == Some(Part::Center) {
            HOT_COLOR
        } else {
            Color32::WHITE
        };
        if self.mode == GizmoMode::Scale {
            painter.rect(
                Rect::from_center_size(center, egui::Vec2::splat(CENTER_PIXELS * 1.6)),
                1.0,
                center_color,
                Stroke::new(1.0_f32, Color32::from_black_alpha(160)),
                egui::StrokeKind::Outside,
            );
        } else {
            painter.circle(
                center,
                CENTER_PIXELS * 0.7,
                center_color,
                Stroke::new(1.0_f32, Color32::from_black_alpha(160)),
            );
        }
    }
}

/// What a finished rotate or scale gizmo drag does to the selection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Change {
    /// A rotation: entity angles turn with it.
    Rotate(DMat4),
    /// A scale: entity angles stay.
    Scale(DMat4),
}

impl Change {
    /// The transform to preview while dragging.
    pub fn matrix(&self) -> DMat4 {
        match *self {
            Self::Rotate(matrix) | Self::Scale(matrix) => matrix,
        }
    }
}

/// A gizmo drag in progress: the part, and where it started.
#[derive(Clone, Copy, Debug)]
pub struct GizmoDrag {
    pub slot: usize,
    pub part: Part,
    pub gizmo: Gizmo,
    /// The selection's bounds when the drag started.
    bounds: Bounds,
    start_pointer: Pos2,
    /// Where the press landed on the dragged axis, in units from the centre.
    start_along: f64,
}

/// Where along the line `center + t * axis` the mouse ray passes closest. `None` when
/// the line points along the ray.
fn along_axis(center: DVec3, axis: DVec3, origin: DVec3, direction: DVec3) -> Option<f64> {
    let between = center - origin;
    let facing = axis.dot(direction);
    let denominator = 1.0 - facing * facing;
    (denominator > 1e-6).then(|| (facing * direction.dot(between) - axis.dot(between)) / denominator)
}

fn snap(value: f64, grid: f64) -> f64 {
    (value / grid).round() * grid
}

impl GizmoDrag {
    /// Starts a rotate or scale drag of a part; moves are dragged along a
    /// [`crate::translation_plane::TranslationPlane`] instead. `None` when the part cannot
    /// be dragged from this angle.
    #[allow(clippy::too_many_arguments)]
    pub fn begin(
        slot: usize,
        gizmo: Gizmo,
        part: Part,
        camera: &Camera3D,
        rect: Rect,
        pointer: Pos2,
        bounds: Bounds,
    ) -> Option<Self> {
        let (origin, direction) = camera.build_ray(rect, pointer);
        let start_along = match (gizmo.mode, part) {
            (GizmoMode::Move, _) | (GizmoMode::Rotate, Part::Center) => return None,
            (_, Part::Axis(axis)) => along_axis(gizmo.center, AXES[axis], origin, direction)?,
            _ => 0.0,
        };
        Some(Self {
            slot,
            part,
            gizmo,
            bounds,
            start_pointer: pointer,
            start_along,
        })
    }

    /// The point a finished rotation or scale holds still: the gizmo's centre, except that
    /// stretching along an axis holds the selection's opposite face, the one at its
    /// minimum along that axis.
    pub fn pivot(&self) -> DVec3 {
        let mut pivot = self.gizmo.center;
        if let (GizmoMode::Scale, Part::Axis(axis)) = (self.gizmo.mode, self.part) {
            pivot[axis] = self.bounds.min[axis];
        }
        pivot
    }

    /// What the drag does with the mouse at `pointer`. Scales snap the selection's size to
    /// the grid, and rotations snap to `angle_step` degrees when it is given.
    pub fn change(
        &self,
        camera: &Camera3D,
        rect: Rect,
        pointer: Pos2,
        grid: f64,
        angle_step: Option<f64>,
    ) -> Option<Change> {
        let (origin, direction) = camera.build_ray(rect, pointer);
        let center = self.gizmo.center;
        match (self.gizmo.mode, self.part) {
            (GizmoMode::Rotate, Part::Ring(axis)) => {
                let middle = camera.project(rect, center)?;
                let angle = |at: Pos2| f64::from(-(at.y - middle.y)).atan2(f64::from(at.x - middle.x));
                let mut turned = angle(pointer) - angle(self.start_pointer);
                turned = (turned + PI).rem_euclid(TAU) - PI;
                if AXES[axis].dot(center - camera.position) > 0.0 {
                    turned = -turned;
                }
                let mut degrees = turned.to_degrees();
                if let Some(step) = angle_step {
                    degrees = snap_angle(degrees, step);
                }
                let rotation = DMat3::from_axis_angle(AXES[axis], degrees.to_radians());
                Some(Change::Rotate(around(center, rotation)))
            }
            (GizmoMode::Scale, Part::Axis(axis)) => {
                let along = along_axis(center, AXES[axis], origin, direction)?;
                let size = self.bounds.size()[axis];
                if size <= 0.0 {
                    return None;
                }
                let wanted = size + along - self.start_along;
                let mut factors = DVec3::ONE;
                factors[axis] = snap(wanted, grid).max(grid) / size;
                Some(Change::Scale(around(self.pivot(), DMat3::from_diagonal(factors))))
            }
            (GizmoMode::Scale, Part::Center) => {
                let size = self.bounds.size();
                let largest = size.max_element();
                if largest <= 0.0 {
                    return None;
                }
                let raised = f64::from(self.start_pointer.y - pointer.y);
                let wanted = largest * 2f64.powf(raised / UNIFORM_PIXELS);
                let factor = snap(wanted, grid).max(grid) / largest;
                Some(Change::Scale(around(
                    center,
                    DMat3::from_diagonal(DVec3::splat(factor)),
                )))
            }
            _ => None,
        }
    }
}
