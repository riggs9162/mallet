//! The virtual plane every move drag slides the selection along, in the 3D views and the
//! 2D views alike, so snapping and axis constraints behave the same everywhere.
//!
//! A drag fixes the plane when it starts: through the point the drag started on, spanned
//! by two world axes. Each frame the mouse ray meets the plane, and the difference from
//! the start point, snapped and constrained, is the move.

use glam::DVec3;

use crate::view2d::Axis2D;

const AXES: [DVec3; 3] = [DVec3::X, DVec3::Y, DVec3::Z];

/// Which of a plane's two axes a drag may move along.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Constraint {
    /// Both.
    #[default]
    Free,
    /// Only the plane's horizontal axis.
    Horizontal,
    /// Only the plane's vertical axis.
    Vertical,
}

/// Rounds a distance to a multiple of `step`; a step that is not positive leaves it be.
pub fn snap(value: f64, step: f64) -> f64 {
    if step > 0.0 && step.is_finite() {
        (value / step).round() * step
    } else {
        value
    }
}

/// A plane through `origin`, facing `normal`, with two world axes lying in it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TranslationPlane {
    pub origin: DVec3,
    pub normal: DVec3,
    pub horizontal: DVec3,
    pub vertical: DVec3,
}

/// The index of the world axis a direction runs most along.
fn dominant_axis(direction: DVec3) -> usize {
    let size = direction.abs();
    if size.x >= size.y && size.x >= size.z {
        0
    } else if size.y >= size.z {
        1
    } else {
        2
    }
}

impl TranslationPlane {
    /// The plane through `origin` spanned by world axes `horizontal` and `vertical`.
    pub fn spanned(origin: DVec3, horizontal: usize, vertical: usize) -> Self {
        let (horizontal, vertical) = (AXES[horizontal], AXES[vertical]);
        Self {
            origin,
            normal: horizontal.cross(vertical),
            horizontal,
            vertical,
        }
    }

    /// The plane a free drag in a 3D view moves along: through `origin`, spanned by the two
    /// world axes most square to the view direction, so the mouse ray always meets it at a
    /// steep angle. Its normal is the world axis the view looks most along; looking down,
    /// that is the floor's X and Y, and looking level it is Z with X or Y.
    pub fn facing(origin: DVec3, view_direction: DVec3) -> Self {
        let normal = dominant_axis(view_direction);
        let (first, second) = ((normal + 1) % 3, (normal + 2) % 3);
        let (horizontal, vertical) = if normal == 2 {
            (0, 1)
        } else if first == 2 {
            (second, first)
        } else {
            (first, second)
        };
        Self::spanned(origin, horizontal, vertical)
    }

    /// The plane a drag along one world axis moves on, with the constraint that keeps it to
    /// that axis: the axis together with whichever other world axis is most square to the
    /// view direction.
    pub fn along_axis(origin: DVec3, axis: usize, view_direction: DVec3) -> (Self, Constraint) {
        let (a, b) = ((axis + 1) % 3, (axis + 2) % 3);
        let other = if view_direction[a].abs() <= view_direction[b].abs() { a } else { b };
        (Self::spanned(origin, axis, other), Constraint::Horizontal)
    }

    /// The plane through `origin` square to world axis `axis`, as the gizmo's plane
    /// squares move on.
    pub fn across_axis(origin: DVec3, axis: usize) -> Self {
        Self::spanned(origin, (axis + 1) % 3, (axis + 2) % 3)
    }

    /// The plane of a 2D view through `origin`: the view's horizontal and vertical axes.
    pub fn for_view(origin: DVec3, axis: Axis2D) -> Self {
        let [horizontal, vertical, _] = axis.axes();
        Self::spanned(origin, horizontal, vertical)
    }

    /// The same plane through the point where a ray meets it, so a drag that starts there
    /// starts with no move. `None` when the ray misses the plane.
    pub fn anchored(self, origin: DVec3, direction: DVec3) -> Option<Self> {
        Some(Self {
            origin: self.intersect(origin, direction)?,
            ..self
        })
    }

    /// The ray a 2D view casts through a world point: along the view's depth axis, from
    /// behind the plane.
    pub fn view_ray(&self, point: DVec3) -> (DVec3, DVec3) {
        let along = (point - self.origin).dot(self.normal);
        let on_plane = point - self.normal * along;
        (on_plane - self.normal, self.normal)
    }

    /// Where a ray meets the plane in front of its origin. `None` when the ray runs along
    /// the plane or points away from it.
    pub fn intersect(&self, origin: DVec3, direction: DVec3) -> Option<DVec3> {
        let facing = direction.dot(self.normal);
        if facing.abs() < 1e-9 {
            return None;
        }
        let distance = (self.origin - origin).dot(self.normal) / facing;
        (distance > 0.0 && distance.is_finite()).then(|| origin + direction * distance)
    }

    /// The move from the plane's origin to where a ray meets the plane: its two in-plane
    /// coordinates snapped to multiples of `step` (none when it is not positive), and the
    /// one the constraint rules out dropped.
    pub fn translation(&self, origin: DVec3, direction: DVec3, step: f64, constraint: Constraint) -> Option<DVec3> {
        let offset = self.intersect(origin, direction)? - self.origin;
        let along = |axis: DVec3| snap(offset.dot(axis), step);
        let horizontal = if constraint == Constraint::Vertical { 0.0 } else { along(self.horizontal) };
        let vertical = if constraint == Constraint::Horizontal { 0.0 } else { along(self.vertical) };
        Some(self.horizontal * horizontal + self.vertical * vertical + DVec3::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looking_down_drags_on_the_floor() {
        let plane = TranslationPlane::facing(DVec3::new(0.0, 0.0, 64.0), DVec3::new(0.2, 0.1, -0.9));
        assert_eq!((plane.horizontal, plane.vertical), (DVec3::X, DVec3::Y));
        assert_eq!(plane.normal.abs(), DVec3::Z);
    }

    #[test]
    fn looking_level_drags_upright() {
        let plane = TranslationPlane::facing(DVec3::ZERO, DVec3::new(0.9, 0.3, -0.1));
        assert_eq!((plane.horizontal, plane.vertical), (DVec3::Y, DVec3::Z));
        let plane = TranslationPlane::facing(DVec3::ZERO, DVec3::new(0.3, -0.9, 0.1));
        assert_eq!((plane.horizontal, plane.vertical), (DVec3::X, DVec3::Z));
    }

    #[test]
    fn moves_snap_to_the_grid_and_follow_constraints() {
        let plane = TranslationPlane::facing(DVec3::ZERO, DVec3::NEG_Z);
        let (origin, direction) = (DVec3::new(37.0, -70.0, 100.0), DVec3::NEG_Z);
        let snapped = plane.translation(origin, direction, 16.0, Constraint::Free);
        assert_eq!(snapped, Some(DVec3::new(32.0, -64.0, 0.0)));
        let free = plane.translation(origin, direction, 0.0, Constraint::Free);
        assert_eq!(free, Some(DVec3::new(37.0, -70.0, 0.0)));
        let across = plane.translation(origin, direction, 1.0, Constraint::Horizontal);
        assert_eq!(across, Some(DVec3::new(37.0, 0.0, 0.0)));
        let up = plane.translation(origin, direction, 1.0, Constraint::Vertical);
        assert_eq!(up, Some(DVec3::new(0.0, -70.0, 0.0)));
    }

    #[test]
    fn an_axis_drag_keeps_to_its_axis() {
        let (plane, constraint) = TranslationPlane::along_axis(DVec3::ZERO, 2, DVec3::new(1.0, 0.0, 0.0));
        assert_eq!(plane.horizontal, DVec3::Z);
        assert_eq!(plane.vertical, DVec3::Y);
        let moved = plane
            .translation(DVec3::new(-100.0, 5.0, 20.0), DVec3::X, 0.0, constraint)
            .unwrap();
        assert_eq!(moved, DVec3::new(0.0, 0.0, 20.0));
    }

    #[test]
    fn a_ray_behind_the_camera_misses() {
        let plane = TranslationPlane::across_axis(DVec3::ZERO, 2);
        assert_eq!(plane.intersect(DVec3::new(0.0, 0.0, 10.0), DVec3::Z), None);
        assert_eq!(plane.intersect(DVec3::new(0.0, 0.0, 10.0), DVec3::X), None);
    }

    #[test]
    fn view_rays_meet_the_plane_under_the_point() {
        let plane = TranslationPlane::for_view(DVec3::new(0.0, 0.0, 32.0), Axis2D::Top);
        let (origin, direction) = plane.view_ray(DVec3::new(48.0, -16.0, 0.0));
        assert_eq!(plane.intersect(origin, direction), Some(DVec3::new(48.0, -16.0, 32.0)));
    }
}
