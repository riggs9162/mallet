//! Orthographic 2D viewports (top, front and side), drawn the way Hammer draws them.

use eframe::egui::{self, Color32, PointerButton, Pos2, Rect, Response};
use glam::{DMat4, DVec2, DVec3};
use mallet_vmf::geometry::Bounds;

use crate::canvas::Canvas;
use crate::clipper::{ClipOverlay, CLIP_PLANE_COLOR};
use crate::scene::{box_edges, entity_marker_edges, Overlay, Remap, Scene, AGENT_CAMERA_COLOR, HANDLE_HALF_SIZE};
use crate::style;
use crate::transform::HandleMode;

/// Radius, in pixels, of the pivot marker shown with the rotation handles.
const PIVOT_RADIUS: f32 = 4.0;
const PIVOT_COLOR: Color32 = Color32::from_rgb(255, 210, 60);
const MIN_SCALE: f64 = 1.0 / 256.0;
const MAX_SCALE: f64 = 32.0;
const MAX_GRID_LINES: f64 = 4000.0;
/// Half the size, in pixels, of the cross marking each brush's centre.
const CROSS_PIXELS: f64 = 3.0;
/// Brushes smaller than this on screen, in pixels, get no centre cross.
const CROSS_MIN_PIXELS: f64 = 10.0;
/// Scroll distance egui reports for one mouse wheel notch.
const POINTS_PER_NOTCH: f64 = 50.0;
/// Default zoom factor of one mouse wheel notch.
pub const DEFAULT_ZOOM_STEP: f64 = 1.2;

/// The zoom factor per step, kept to a sensible range; anything not finite is the default.
fn clamp_zoom_step(zoom_step: f64) -> f64 {
    if zoom_step.is_finite() {
        zoom_step.clamp(1.01, 4.0)
    } else {
        DEFAULT_ZOOM_STEP
    }
}

/// Which pair of world axes a 2D viewport shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis2D {
    /// Looking down: X right, Y up.
    Top,
    /// Y right, Z up.
    Front,
    /// X right, Z up.
    Side,
}

impl Axis2D {
    /// The label Hammer prints in the viewport corner.
    pub fn label(self) -> &'static str {
        match self {
            Self::Top => "top (x/y)",
            Self::Front => "front (y/z)",
            Self::Side => "side (x/z)",
        }
    }

    /// World axis indices: horizontal, vertical, then the depth axis the view looks along.
    pub fn axes(self) -> [usize; 3] {
        match self {
            Self::Top => [0, 1, 2],
            Self::Front => [1, 2, 0],
            Self::Side => [0, 2, 1],
        }
    }

    /// Drops the axis this view looks along.
    pub fn project(self, point: DVec3) -> DVec2 {
        let [horizontal, vertical, _] = self.axes();
        DVec2::new(point[horizontal], point[vertical])
    }

    /// Turns a 2D view offset into a world offset with no movement along the depth axis.
    pub fn lift(self, offset: DVec2) -> DVec3 {
        let [horizontal, vertical, _] = self.axes();
        let mut world = DVec3::ZERO;
        world[horizontal] = offset.x;
        world[vertical] = offset.y;
        world
    }
}

/// One of the eight resize handles on a box in a 2D view: -1, 0 or 1 along the view's
/// horizontal and vertical axes, where -1 is the minimum edge and 1 the maximum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle {
    pub x: i8,
    pub y: i8,
}

impl Handle {
    /// The four corners and four edge midpoints.
    pub const ALL: [Handle; 8] = [
        Handle { x: -1, y: -1 },
        Handle { x: 0, y: -1 },
        Handle { x: 1, y: -1 },
        Handle { x: -1, y: 0 },
        Handle { x: 1, y: 0 },
        Handle { x: -1, y: 1 },
        Handle { x: 0, y: 1 },
        Handle { x: 1, y: 1 },
    ];

    /// Where the handle sits on a box, in the view's 2D world coordinates.
    pub fn world_position(self, axis: Axis2D, bounds: &Bounds) -> DVec2 {
        let (min, max) = (axis.project(bounds.min), axis.project(bounds.max));
        let pick = |side: i8, low: f64, high: f64| match side {
            -1 => low,
            1 => high,
            _ => (low + high) * 0.5,
        };
        DVec2::new(pick(self.x, min.x, max.x), pick(self.y, min.y, max.y))
    }

    /// The box after dragging this handle by `delta` in the view. Moved edges snap to the
    /// grid and stop one grid step short of the opposite edge.
    pub fn resize(self, axis: Axis2D, bounds: &Bounds, delta: DVec2, grid: f64) -> Bounds {
        let [horizontal, vertical, _] = axis.axes();
        let snap = |value: f64| (value / grid).round() * grid;
        let mut resized = *bounds;
        for (side, index, moved) in [(self.x, horizontal, delta.x), (self.y, vertical, delta.y)] {
            match side {
                -1 => resized.min[index] = snap(bounds.min[index] + moved).min(bounds.max[index] - grid),
                1 => resized.max[index] = snap(bounds.max[index] + moved).max(bounds.min[index] + grid),
                _ => {}
            }
        }
        resized
    }

    /// Whether the handle shows in a handle mode: every handle to scale, the corners to
    /// rotate and the edges to skew, as in Hammer.
    pub fn shown(self, mode: HandleMode) -> bool {
        let corner = self.x != 0 && self.y != 0;
        match mode {
            HandleMode::Scale => true,
            HandleMode::Rotate => corner,
            HandleMode::Skew => !corner,
        }
    }

    /// The skew of a box from dragging this edge handle by `delta`: the dragged edge
    /// slides along itself by the grid-snapped distance while the opposite edge stays.
    /// `None` for corners, flat boxes and drags that skew nothing.
    pub fn skew(self, axis: Axis2D, bounds: &Bounds, delta: DVec2, grid: f64) -> Option<DMat4> {
        let [horizontal, vertical, _] = axis.axes();
        let snap = |value: f64| (value / grid).round() * grid;
        let (along, by, side, moved) = match (self.x, self.y) {
            (0, side) if side != 0 => (horizontal, vertical, side, snap(delta.x)),
            (side, 0) if side != 0 => (vertical, horizontal, side, snap(delta.y)),
            _ => return None,
        };
        let length = bounds.size()[by];
        if length <= 0.0 || moved == 0.0 {
            return None;
        }
        let (fixed, amount) = if side > 0 {
            (bounds.min[by], moved / length)
        } else {
            (bounds.max[by], -moved / length)
        };
        Some(crate::transform::shear(along, by, amount, fixed))
    }

    /// The mouse cursor for skewing from this edge handle: along the edge.
    pub fn skew_cursor(self) -> egui::CursorIcon {
        if self.x == 0 {
            egui::CursorIcon::ResizeHorizontal
        } else {
            egui::CursorIcon::ResizeVertical
        }
    }

    /// The mouse cursor that shows which way this handle resizes.
    pub fn cursor(self) -> egui::CursorIcon {
        match (self.x, self.y) {
            (0, _) => egui::CursorIcon::ResizeVertical,
            (_, 0) => egui::CursorIcon::ResizeHorizontal,
            (x, y) if x == y => egui::CursorIcon::ResizeNeSw,
            _ => egui::CursorIcon::ResizeNwSe,
        }
    }
}

/// Pan and zoom state of one 2D viewport.
#[derive(Clone, Debug)]
pub struct View2D {
    pub axis: Axis2D,
    /// World position at the centre of the viewport.
    pub center: DVec2,
    /// Pixels per world unit.
    pub scale: f64,
}

impl View2D {
    pub fn new(axis: Axis2D) -> Self {
        Self {
            axis,
            center: DVec2::ZERO,
            scale: 0.25,
        }
    }

    /// Converts a 2D world position to a screen position inside `rect`.
    pub fn to_screen(&self, rect: Rect, world: DVec2) -> Pos2 {
        let center = rect.center();
        Pos2::new(
            center.x + ((world.x - self.center.x) * self.scale) as f32,
            center.y - ((world.y - self.center.y) * self.scale) as f32,
        )
    }

    /// Converts a screen position inside `rect` to a 2D world position.
    pub fn to_world(&self, rect: Rect, screen: Pos2) -> DVec2 {
        let center = rect.center();
        DVec2::new(
            self.center.x + f64::from(screen.x - center.x) / self.scale,
            self.center.y - f64::from(screen.y - center.y) / self.scale,
        )
    }

    /// Sets the zoom in pixels per world unit, clamped to the supported range.
    pub fn set_scale(&mut self, scale: f64) {
        self.scale = scale.clamp(MIN_SCALE, MAX_SCALE);
    }

    /// Centres and zooms the view so the bounds fill most of it.
    pub fn fit(&mut self, bounds: &Bounds, rect: Rect) {
        if bounds.is_empty() || rect.width() < 1.0 || rect.height() < 1.0 {
            return;
        }
        let min = self.axis.project(bounds.min);
        let max = self.axis.project(bounds.max);
        let size = (max - min).max(DVec2::splat(64.0));
        self.center = (min + max) * 0.5;
        let fit = (f64::from(rect.width()) / size.x).min(f64::from(rect.height()) / size.y);
        self.scale = (fit * 0.9).clamp(MIN_SCALE, MAX_SCALE);
    }

    /// Pans with middle-drag or Space+left-drag and zooms around the cursor with the wheel,
    /// by `zoom_step` per notch.
    pub fn handle_input(&mut self, ui: &egui::Ui, response: &Response, rect: Rect, zoom_step: f64) {
        let space_held = ui.input(|input| input.key_down(egui::Key::Space));
        let panning =
            response.dragged_by(PointerButton::Middle) || (space_held && response.dragged_by(PointerButton::Primary));
        if panning {
            let delta = response.drag_delta();
            self.center.x -= f64::from(delta.x) / self.scale;
            self.center.y += f64::from(delta.y) / self.scale;
        }

        let scroll = ui.input(|input| input.raw_scroll_delta.y);
        let Some(pointer) = response.hover_pos().filter(|_| scroll != 0.0) else {
            return;
        };
        let factor = clamp_zoom_step(zoom_step).powf(f64::from(scroll) / POINTS_PER_NOTCH);
        self.zoom_at(rect, pointer, factor);
    }

    /// The Camera tool's zoom: a left click zooms in one step around the cursor, and a
    /// right click zooms out.
    pub fn handle_click_zoom(&mut self, response: &Response, rect: Rect, zoom_step: f64) {
        let Some(pointer) = response.hover_pos() else {
            return;
        };
        let step = clamp_zoom_step(zoom_step);
        if response.clicked_by(PointerButton::Primary) {
            self.zoom_at(rect, pointer, step);
        } else if response.clicked_by(PointerButton::Secondary) {
            self.zoom_at(rect, pointer, 1.0 / step);
        }
    }

    /// Multiplies the zoom by `factor`, keeping the world point under `pointer` in place.
    pub fn zoom_at(&mut self, rect: Rect, pointer: Pos2, factor: f64) {
        let anchor = self.to_world(rect, pointer);
        self.scale = (self.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        let center = rect.center();
        self.center.x = anchor.x - f64::from(pointer.x - center.x) / self.scale;
        self.center.y = anchor.y + f64::from(pointer.y - center.y) / self.scale;
    }

    /// Draws the grid, every wireframe that overlaps the view, and the corner label.
    ///
    /// Selected meshes are drawn last, in red, where the overlay is moving or resizing them;
    /// then the block tool's box, the resize handles and this view's selection marquee.
    pub fn draw(&self, canvas: &mut dyn Canvas, rect: Rect, scene: &Scene, grid: f64, active: bool, overlay: &Overlay) {
        let style = style::current();
        canvas.fill(rect, style.background);
        self.draw_grid(canvas, rect, grid);
        if let Some(handles) = &overlay.handles {
            self.draw_handle_outline(canvas, rect, handles);
        }

        let view_min = self.to_world(rect, rect.left_bottom());
        let view_max = self.to_world(rect, rect.right_top());
        let unselected = scene.meshes.iter().filter(|mesh| !mesh.selected);
        let selected = scene.meshes.iter().filter(|mesh| mesh.selected);
        for mesh in unselected.chain(selected) {
            let (color, remap) = mesh.appearance(overlay);
            let bounds = mesh.mapped_bounds(remap.as_ref());
            let (min, max) = (self.axis.project(bounds.min), self.axis.project(bounds.max));
            if max.x < view_min.x || min.x > view_max.x || max.y < view_min.y || min.y > view_max.y {
                continue;
            }
            self.draw_edges(canvas, rect, &mesh.edges, remap.as_ref(), color);
        }

        for ([a, b], color) in self.centre_crosses(rect, scene, overlay) {
            canvas.lines(&[[self.to_screen(rect, a), self.to_screen(rect, b)]], color);
        }
        if let Some(block) = &overlay.block {
            self.draw_edges(canvas, rect, &box_edges(block), None, style.tool);
        }
        if let Some(point) = overlay.entity {
            self.draw_edges(canvas, rect, &entity_marker_edges(point), None, style.tool);
        }
        self.draw_edges(canvas, rect, &overlay.agent_camera, None, AGENT_CAMERA_COLOR);
        if let Some(clip) = &overlay.clip {
            for (edge, color) in clip.edges_2d() {
                self.draw_edges(canvas, rect, &[edge], None, color);
            }
        }
        self.draw_overlay(canvas, rect, overlay, active);
    }

    /// The small × Hammer draws at the centre of each brush and point entity in view,
    /// as 2D world-space segments with colours. Clicking a cross selects its brush.
    /// Brushes too small on screen to tell apart get none.
    pub fn centre_crosses(&self, rect: Rect, scene: &Scene, overlay: &Overlay) -> Vec<([DVec2; 2], Color32)> {
        let view_min = self.to_world(rect, rect.left_bottom());
        let view_max = self.to_world(rect, rect.right_top());
        let arm = CROSS_PIXELS / self.scale;
        let mut crosses = Vec::new();
        for mesh in scene.meshes.iter().filter(|mesh| mesh.owner.is_some()) {
            let (color, remap) = mesh.appearance(overlay);
            let bounds = mesh.mapped_bounds(remap.as_ref());
            let (min, max) = (self.axis.project(bounds.min), self.axis.project(bounds.max));
            if (max - min).max_element() * self.scale < CROSS_MIN_PIXELS {
                continue;
            }
            let centre = (min + max) * 0.5;
            if centre.cmplt(view_min).any() || centre.cmpgt(view_max).any() {
                continue;
            }
            crosses.push(([centre - DVec2::splat(arm), centre + DVec2::splat(arm)], color));
            crosses.push(([centre + DVec2::new(-arm, arm), centre + DVec2::new(arm, -arm)], color));
        }
        crosses
    }

    /// Draws what sits on top of the map: the resize handles, this view's selection
    /// marquee and the corner label. The GPU-drawn views use this on top of their image.
    pub fn draw_overlay(&self, canvas: &mut dyn Canvas, rect: Rect, overlay: &Overlay, active: bool) {
        let style = style::current();
        if let Some(handles) = &overlay.handles {
            let mode = overlay.handle_mode;
            for (handle, at) in self.handle_positions(rect, handles) {
                if !handle.shown(mode) {
                    continue;
                }
                match mode {
                    HandleMode::Rotate => canvas.circle(at, HANDLE_HALF_SIZE + 0.5, style.handles),
                    HandleMode::Scale | HandleMode::Skew => canvas.fill(
                        Rect::from_center_size(at, egui::Vec2::splat(HANDLE_HALF_SIZE * 2.0)),
                        style.handles,
                    ),
                }
            }
            if mode == HandleMode::Rotate {
                let pivot = overlay.pivot.unwrap_or_else(|| handles.center());
                let at = self.to_screen(rect, self.axis.project(pivot));
                canvas.circle(at, PIVOT_RADIUS, PIVOT_COLOR);
                canvas.lines(
                    &[
                        [
                            at - egui::vec2(PIVOT_RADIUS + 3.0, 0.0),
                            at + egui::vec2(PIVOT_RADIUS + 3.0, 0.0),
                        ],
                        [
                            at - egui::vec2(0.0, PIVOT_RADIUS + 3.0),
                            at + egui::vec2(0.0, PIVOT_RADIUS + 3.0),
                        ],
                    ],
                    PIVOT_COLOR,
                );
            }
        }
        if let Some((axis, a, b)) = overlay.marquee {
            if axis == self.axis {
                let corners = [a, DVec2::new(b.x, a.y), b, DVec2::new(a.x, b.y)];
                let segments: Vec<[Pos2; 2]> = (0..4)
                    .map(|i| {
                        [
                            self.to_screen(rect, corners[i]),
                            self.to_screen(rect, corners[(i + 1) % 4]),
                        ]
                    })
                    .collect();
                canvas.lines(&segments, style.tool);
            }
        }
        if let Some(clip) = &overlay.clip {
            self.draw_clip_points(canvas, rect, clip);
        }

        let label_color = if active { Color32::WHITE } else { Color32::GRAY };
        canvas.label(rect.left_top() + egui::vec2(6.0, 4.0), self.axis.label(), label_color);
    }

    /// Draws the clipping tool's points as white handles, joined by the cyan clip line in
    /// the view it was drawn in, as Hammer does. Three-point clipping shows its points in
    /// every view. With measurements on, every view also labels the kept pieces' sizes.
    fn draw_clip_points(&self, canvas: &mut dyn Canvas, rect: Rect, clip: &ClipOverlay) {
        let style = style::current();
        for bounds in &clip.measured {
            self.draw_measurements(canvas, rect, bounds, style.tool);
        }
        if clip.axis.is_some_and(|axis| axis != self.axis) {
            return;
        }
        let at: Vec<Pos2> = clip
            .points
            .iter()
            .map(|point| self.to_screen(rect, self.axis.project(*point)))
            .collect();
        if let (Some(_), [a, b]) = (clip.axis, at.as_slice()) {
            canvas.lines(&[[*a, *b]], CLIP_PLANE_COLOR);
        }
        for point in at {
            canvas.fill(
                Rect::from_center_size(point, egui::Vec2::splat(HANDLE_HALF_SIZE * 2.0)),
                style.handles,
            );
        }
    }

    /// Labels a box's width above it and its height to its left, as Hammer labels sizes.
    fn draw_measurements(&self, canvas: &mut dyn Canvas, rect: Rect, bounds: &Bounds, color: Color32) {
        /// Rough width of one label character, in pixels, for centring.
        const CHAR_PIXELS: f32 = 7.0;
        /// Gap between a box and its labels, in pixels.
        const GAP: f32 = 4.0;
        /// Height of a label line, in pixels.
        const LINE_PIXELS: f32 = 14.0;
        let size = self.axis.project(bounds.size());
        let screen = Rect::from_two_pos(
            self.to_screen(rect, self.axis.project(bounds.min)),
            self.to_screen(rect, self.axis.project(bounds.max)),
        );
        let width = format!("{:.1}", size.x);
        let height = format!("{:.1}", size.y);
        let width_at = egui::pos2(
            screen.center().x - width.len() as f32 * CHAR_PIXELS / 2.0,
            screen.top() - GAP - LINE_PIXELS,
        );
        let height_at = egui::pos2(
            screen.left() - GAP - height.len() as f32 * CHAR_PIXELS,
            screen.center().y - LINE_PIXELS / 2.0,
        );
        canvas.label(width_at, &width, color);
        canvas.label(height_at, &height, color);
    }

    /// Screen positions of the eight resize handles of a box in this view.
    pub fn handle_positions(&self, rect: Rect, bounds: &Bounds) -> [(Handle, Pos2); 8] {
        Handle::ALL.map(|handle| (handle, self.to_screen(rect, handle.world_position(self.axis, bounds))))
    }

    /// The handle of a box under a screen position, among those a handle mode shows.
    pub fn hit_handle(&self, rect: Rect, bounds: &Bounds, pointer: Pos2, mode: HandleMode) -> Option<Handle> {
        let reach = HANDLE_HALF_SIZE + 2.0;
        self.handle_positions(rect, bounds)
            .into_iter()
            .filter(|(handle, _)| handle.shown(mode))
            .find(|(_, at)| (at.x - pointer.x).abs() <= reach && (at.y - pointer.y).abs() <= reach)
            .map(|(handle, _)| handle)
    }

    /// Draws the box the handles sit on. It goes under the brushes so a selected brush's
    /// red outline stays visible where the two coincide.
    fn draw_handle_outline(&self, canvas: &mut dyn Canvas, rect: Rect, bounds: &Bounds) {
        let outline: Vec<[Pos2; 2]> = self
            .handle_outline(bounds)
            .iter()
            .map(|[a, b]| [self.to_screen(rect, *a), self.to_screen(rect, *b)])
            .collect();
        canvas.lines(&outline, style::current().handles);
    }

    /// The four sides of a box as projected into this view, in 2D world coordinates.
    pub fn handle_outline(&self, bounds: &Bounds) -> [[DVec2; 2]; 4] {
        let (min, max) = (self.axis.project(bounds.min), self.axis.project(bounds.max));
        let corners = [min, DVec2::new(max.x, min.y), max, DVec2::new(min.x, max.y)];
        [0, 1, 2, 3].map(|i| [corners[i], corners[(i + 1) % 4]])
    }

    fn draw_edges(
        &self,
        canvas: &mut dyn Canvas,
        rect: Rect,
        edges: &[[DVec3; 2]],
        remap: Option<&Remap>,
        color: Color32,
    ) {
        let map = |point: DVec3| remap.map_or(point, |remap| remap.apply(point));
        let segments: Vec<[Pos2; 2]> = edges
            .iter()
            .map(|[a, b]| (self.axis.project(map(*a)), self.axis.project(map(*b))))
            .filter(|(a, b)| a.distance_squared(*b) >= 1e-6)
            .map(|(a, b)| [self.to_screen(rect, a), self.to_screen(rect, b)])
            .collect();
        canvas.lines(&segments, color);
    }

    fn draw_grid(&self, canvas: &mut dyn Canvas, rect: Rect, grid: f64) {
        for ([a, b], color) in self.grid_lines(rect, grid) {
            canvas.lines(&[[self.to_screen(rect, a), self.to_screen(rect, b)]], color);
        }
    }

    /// The visible grid lines, in 2D world coordinates, with their colours. Lines closer
    /// than the style's minimum spacing are thinned out by doubling the spacing, and
    /// nothing is returned if even that would be too many lines, or for a grid of 0,
    /// which is how a hidden grid is passed.
    pub fn grid_lines(&self, rect: Rect, grid: f64) -> Vec<([DVec2; 2], Color32)> {
        if !(grid > 0.0 && grid.is_finite()) {
            return Vec::new();
        }
        let style = style::current();
        let mut spacing = grid.max(MIN_SCALE);
        while spacing * self.scale < f64::from(style.grid_min_pixels) {
            spacing *= 2.0;
        }
        let min = self.to_world(rect, rect.left_bottom());
        let max = self.to_world(rect, rect.right_top());
        if (max.x - min.x) / spacing + (max.y - min.y) / spacing > MAX_GRID_LINES {
            return Vec::new();
        }
        let mut lines = Vec::new();
        for vertical in [true, false] {
            let (low, high) = if vertical { (min.x, max.x) } else { (min.y, max.y) };
            let first = (low / spacing).floor() as i64;
            let last = (high / spacing).ceil() as i64;
            for step in first..=last {
                let value = step as f64 * spacing;
                let segment = if vertical {
                    [DVec2::new(value, min.y), DVec2::new(value, max.y)]
                } else {
                    [DVec2::new(min.x, value), DVec2::new(max.x, value)]
                };
                lines.push((segment, style.grid_color(value)));
            }
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{pos2, Rect};

    use super::*;

    #[test]
    fn zooming_keeps_the_point_under_the_cursor() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), eframe::egui::vec2(800.0, 600.0));
        let mut view = View2D::new(Axis2D::Top);
        view.set_scale(1.0);
        let pointer = pos2(600.0, 150.0);
        let before = view.to_world(rect, pointer);
        view.zoom_at(rect, pointer, DEFAULT_ZOOM_STEP);
        let after = view.to_world(rect, pointer);
        assert!((before - after).length() < 1e-6, "{before:?} != {after:?}");
        assert!((view.scale - DEFAULT_ZOOM_STEP).abs() < 1e-9);
    }
}
