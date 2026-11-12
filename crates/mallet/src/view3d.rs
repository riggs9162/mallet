//! The 3D camera: movement, mouselook and the CPU wireframe fallback drawing.

use std::collections::HashSet;

use eframe::egui::{self, Color32, Pos2, Rect, ViewportCommand};
use glam::DVec3;
use mallet_vmf::geometry::Bounds;

use crate::canvas::Canvas;
use crate::keymap::{Action, KeyMap};
use crate::scene::{box_edges, Overlay, Remap, Scene, AGENT_CAMERA_COLOR};
use crate::style;

/// World units per second, matching Hammer's default forward speed.
pub const DEFAULT_MOVE_SPEED: f64 = 1000.0;
/// Hammer's default horizontal field of view in degrees.
pub const DEFAULT_FOV: f64 = 90.0;
/// Width over height assumed for a 3D view until one is shown.
pub const DEFAULT_ASPECT: f64 = 16.0 / 9.0;
/// Default degrees per second for arrow-key turning.
pub const DEFAULT_TURN_SPEED: f64 = 90.0;
/// Degrees per pixel of mouse movement in mouselook.
pub const DEFAULT_LOOK_SENSITIVITY: f64 = 0.2;
/// Default world units the camera moves per mouse wheel notch.
pub const DEFAULT_WHEEL_STEP: f64 = 64.0;
const POINTS_PER_NOTCH: f64 = 50.0;
/// World units the Camera tool's right drag slides the camera per point of mouse movement.
const PAN_UNITS_PER_POINT: f64 = 1.0;
/// Default for how quickly the camera reaches its target speed, per second; higher is
/// snappier.
pub const DEFAULT_ACCELERATION: f64 = 14.0;
/// The default share of the camera speed flown while crouching (Alt in mouselook), like
/// Source's crouch speed.
pub const DEFAULT_CROUCH_SPEED: f64 = 1.0 / 3.0;
/// Below this speed, in units per second, a coasting camera stops.
const STOP_SPEED: f64 = 1.0;
/// The longest frame time used for movement, so a stalled frame cannot fling the camera.
const MAX_FRAME_SECONDS: f64 = 0.05;
/// How quickly the smoothed frame time follows the measured one, per frame.
const FRAME_TIME_SMOOTHING: f64 = 0.1;
/// Default number of frames of raw mouse movement mouselook averages over.
pub const DEFAULT_LOOK_FRAMES: usize = 4;
/// The most frames mouselook can average over.
pub const MAX_LOOK_FRAMES: usize = 16;
/// Default draw distance in world units.
pub const DEFAULT_DRAW_DISTANCE: f64 = 8192.0;
/// Distance of the near clipping plane.
const NEAR: f64 = 1.0;
const MAX_PITCH: f64 = 89.0;

/// A free-flying first-person camera in Source's Z-up coordinates.
pub struct Camera3D {
    pub position: DVec3,
    /// Degrees around Z; 0 looks along +X.
    pub yaw: f64,
    /// Degrees above the horizon.
    pub pitch: f64,
    /// Horizontal field of view in degrees.
    pub fov: f64,
    /// Width over height of the 3D view the camera was last shown in, so the 2D views
    /// can draw its view pyramid as tall as the picture.
    pub aspect: f64,
    /// Draw distance: nothing farther away is drawn, like Hammer's back clipping plane.
    pub far: f64,
    /// Units per second the movement keys fly at.
    pub move_speed: f64,
    /// Degrees per point of mouse movement in mouselook.
    pub look_sensitivity: f64,
    /// Degrees per second the turn keys turn.
    pub turn_speed: f64,
    /// How quickly the camera reaches its target speed, per second.
    pub acceleration: f64,
    /// Units one mouse wheel notch moves the camera.
    pub wheel_step: f64,
    /// The share of [`Self::move_speed`] flown while crouching (Alt in mouselook), from 0.05 to 1.
    pub crouch_speed: f64,
    /// Frames of mouse movement mouselook averages over, from 1 to [`MAX_LOOK_FRAMES`].
    pub look_frames: usize,
    mouse_look: bool,
    /// Current movement, eased towards what the movement keys ask for.
    velocity: DVec3,
    /// Raw mouse movement of the latest frames, newest last, for smoothing.
    recent_look: std::collections::VecDeque<egui::Vec2>,
    /// Frame time smoothed over recent frames; 0 until the first frame.
    frame_time: f64,
    /// Keys pressed as part of a shortcut, which do not move the camera until released.
    shortcut_keys: HashSet<egui::Key>,
}

impl Camera3D {
    pub fn new() -> Self {
        Self {
            position: DVec3::new(-512.0, -512.0, 512.0),
            yaw: 45.0,
            pitch: -30.0,
            fov: DEFAULT_FOV,
            aspect: DEFAULT_ASPECT,
            far: DEFAULT_DRAW_DISTANCE,
            move_speed: DEFAULT_MOVE_SPEED,
            look_sensitivity: DEFAULT_LOOK_SENSITIVITY,
            turn_speed: DEFAULT_TURN_SPEED,
            acceleration: DEFAULT_ACCELERATION,
            wheel_step: DEFAULT_WHEEL_STEP,
            crouch_speed: DEFAULT_CROUCH_SPEED,
            look_frames: DEFAULT_LOOK_FRAMES,
            mouse_look: false,
            velocity: DVec3::ZERO,
            recent_look: std::collections::VecDeque::with_capacity(MAX_LOOK_FRAMES),
            frame_time: 0.0,
            shortcut_keys: HashSet::new(),
        }
    }

    /// A still camera at the same place, aim, field of view, draw distance and speeds as
    /// `other`, without its movement in progress.
    pub fn looking_like(other: &Self) -> Self {
        Self {
            position: other.position,
            yaw: other.yaw,
            pitch: other.pitch,
            fov: other.fov,
            aspect: other.aspect,
            far: other.far,
            move_speed: other.move_speed,
            look_sensitivity: other.look_sensitivity,
            turn_speed: other.turn_speed,
            acceleration: other.acceleration,
            wheel_step: other.wheel_step,
            crouch_speed: other.crouch_speed,
            look_frames: other.look_frames,
            ..Self::new()
        }
    }

    pub fn forward(&self) -> DVec3 {
        let (yaw, pitch) = (self.yaw.to_radians(), self.pitch.to_radians());
        DVec3::new(pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), pitch.sin())
    }

    pub fn right(&self) -> DVec3 {
        let yaw = self.yaw.to_radians();
        DVec3::new(yaw.sin(), -yaw.cos(), 0.0)
    }

    /// A point straight ahead of the camera, as Hammer saves a camera's `look`.
    pub fn look_point(&self) -> DVec3 {
        self.position + self.forward() * 64.0
    }

    /// Places the camera at `position`, looking towards `look`.
    pub fn set_view(&mut self, position: DVec3, look: DVec3) {
        self.position = position;
        let direction = (look - position).normalize_or_zero();
        if direction != DVec3::ZERO {
            self.yaw = direction.y.atan2(direction.x).to_degrees().rem_euclid(360.0);
            self.pitch = direction
                .z
                .clamp(-1.0, 1.0)
                .asin()
                .to_degrees()
                .clamp(-MAX_PITCH, MAX_PITCH);
        }
        self.velocity = DVec3::ZERO;
    }

    /// Moves the camera back from the bounds' centre until all of it is in view.
    pub fn look_at(&mut self, bounds: &Bounds) {
        if bounds.is_empty() {
            return;
        }
        let radius = (bounds.size().length() * 0.5).max(128.0);
        self.position = bounds.center() - self.forward() * radius * 1.5;
    }

    fn set_mouse_look(&mut self, ctx: &egui::Context, enabled: bool) {
        self.mouse_look = enabled;
        self.recent_look.clear();
        let grab = if enabled {
            egui::CursorGrab::Confined
        } else {
            egui::CursorGrab::None
        };
        ctx.send_viewport_cmd(ViewportCommand::CursorGrab(grab));
        ctx.send_viewport_cmd(ViewportCommand::CursorVisible(!enabled));
    }

    /// Applies keyboard, wheel and mouselook input. Returns whether the camera needs
    /// another frame because it is moving or in mouselook.
    ///
    /// Input is only read while the viewport is hovered or mouselook is on, like Hammer,
    /// where Z toggles mouselook and Escape leaves it.
    ///
    /// To avoid judder, movement uses a frame time smoothed over recent frames rather than
    /// each frame's own. The GPU queue makes frames start in uneven bursts, but they are
    /// still shown one per screen refresh, so equal steps per frame look smooth while
    /// steps sized by each burst frame's start time look like a low frame rate. Raw mouse
    /// movement arrives in the same bursts, so mouselook averages it over the last few
    /// frames, like Source's `m_filter`. The camera also eases towards the speed the keys
    /// ask for instead of jumping to it.
    ///
    /// In mouselook, holding Alt crouches: the camera flies at a third of its speed, and
    /// Alt with a movement key moves instead of starting a shortcut.
    pub fn update(&mut self, ctx: &egui::Context, hovered: bool, keymap: &KeyMap) -> bool {
        // Watch every key event, even while another view is hovered, so a shortcut's key
        // never turns into movement later.
        ctx.input(|input| {
            for event in &input.events {
                if let egui::Event::Key {
                    key,
                    pressed,
                    repeat: false,
                    modifiers,
                    ..
                } = event
                {
                    if !pressed {
                        self.shortcut_keys.remove(key);
                    } else if (modifiers.alt && !(self.mouse_look && keymap.is_movement_key(*key)))
                        || modifiers.ctrl
                        || modifiers.command
                    {
                        self.shortcut_keys.insert(*key);
                    }
                }
            }
        });
        if !hovered && !self.mouse_look {
            return false;
        }
        let (toggle, escape) = ctx.input(|input| {
            (
                keymap.pressed(input, Action::ToggleMouseLook),
                input.key_pressed(egui::Key::Escape),
            )
        });
        if toggle || (escape && self.mouse_look) {
            self.set_mouse_look(ctx, toggle && !self.mouse_look);
        }

        let mut active = self.mouse_look;
        let looking = self.mouse_look;
        ctx.input(|input| {
            let crouching = looking && input.modifiers.alt;
            let measured = f64::from(input.unstable_dt);
            if self.frame_time == 0.0 {
                self.frame_time = measured.clamp(0.001, MAX_FRAME_SECONDS);
            } else if measured < MAX_FRAME_SECONDS {
                self.frame_time += (measured - self.frame_time) * FRAME_TIME_SMOOTHING;
            }
            let dt = self.frame_time;
            if self.mouse_look {
                let raw: egui::Vec2 = input
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::MouseMoved(delta) => Some(*delta),
                        _ => None,
                    })
                    .fold(egui::Vec2::ZERO, |total, delta| total + delta);
                let frames = self.look_frames.clamp(1, MAX_LOOK_FRAMES);
                while self.recent_look.len() >= frames {
                    self.recent_look.pop_front();
                }
                self.recent_look.push_back(raw);
                let smoothed = self
                    .recent_look
                    .iter()
                    .fold(egui::Vec2::ZERO, |total, delta| total + *delta)
                    / frames as f32;
                self.yaw -= f64::from(smoothed.x) * self.look_sensitivity;
                self.pitch = (self.pitch - f64::from(smoothed.y) * self.look_sensitivity).clamp(-MAX_PITCH, MAX_PITCH);
            }

            let turn = |positive: Action, negative: Action| {
                let held = |action| f64::from(u8::from(keymap.held(input, action, &self.shortcut_keys, looking)));
                held(positive) - held(negative)
            };
            let yaw_input = turn(Action::TurnLeft, Action::TurnRight);
            let pitch_input = turn(Action::LookUp, Action::LookDown);
            self.yaw += yaw_input * self.turn_speed * dt;
            self.pitch = (self.pitch + pitch_input * self.turn_speed * dt).clamp(-MAX_PITCH, MAX_PITCH);
            self.yaw = self.yaw.rem_euclid(360.0);

            let forward = self.forward();
            let right = self.right();
            let mut wish = DVec3::ZERO;
            wish += forward * turn(Action::MoveForward, Action::MoveBack);
            wish += right * turn(Action::StrafeRight, Action::StrafeLeft);
            wish += right.cross(forward) * turn(Action::StrafeUp, Action::StrafeDown);
            let speed = if crouching {
                self.move_speed * self.crouch_speed.clamp(0.05, 1.0)
            } else {
                self.move_speed
            };
            let target = wish.normalize_or_zero() * speed;
            self.velocity += (target - self.velocity) * (self.acceleration.max(0.1) * dt).min(1.0);
            if target == DVec3::ZERO && self.velocity.length() < STOP_SPEED {
                self.velocity = DVec3::ZERO;
            }
            self.position += self.velocity * dt;

            let scroll = f64::from(input.raw_scroll_delta.y);
            if (hovered || self.mouse_look) && scroll != 0.0 {
                self.position += forward * self.wheel_step * scroll / POINTS_PER_NOTCH;
            }
            active |= self.velocity != DVec3::ZERO || yaw_input != 0.0 || pitch_input != 0.0;
        });
        active
    }

    /// Draws the small crosshair at the centre of a 3D view. In mouselook, clicks select
    /// what is under it.
    pub fn draw_crosshair(painter: &egui::Painter, rect: Rect) {
        let center = rect.center();
        let arm = 5.0;
        for (stroke, extra) in [
            (egui::Stroke::new(3.0_f32, Color32::from_black_alpha(160)), 1.0),
            (egui::Stroke::new(1.0_f32, Color32::WHITE), 0.0),
        ] {
            painter.line_segment(
                [
                    center - egui::vec2(arm + extra, 0.0),
                    center + egui::vec2(arm + extra, 0.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    center - egui::vec2(0.0, arm + extra),
                    center + egui::vec2(0.0, arm + extra),
                ],
                stroke,
            );
        }
    }

    /// Whether mouselook is on.
    pub fn mouse_look(&self) -> bool {
        self.mouse_look
    }

    /// Turns the camera by a mouse drag of `delta` points, at the mouselook sensitivity.
    /// The Camera tool's left drag uses this.
    pub fn look_by(&mut self, delta: egui::Vec2) {
        self.yaw = (self.yaw - f64::from(delta.x) * self.look_sensitivity).rem_euclid(360.0);
        self.pitch = (self.pitch - f64::from(delta.y) * self.look_sensitivity).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Slides the camera across its view by a mouse drag of `delta` points, so the scene
    /// moves with the mouse: dragging right moves the camera left. The Camera tool's right
    /// drag uses this.
    pub fn pan_by(&mut self, delta: egui::Vec2) {
        let up = self.right().cross(self.forward());
        let step = up * f64::from(delta.y) - self.right() * f64::from(delta.x);
        self.position += step * PAN_UNITS_PER_POINT;
    }

    /// Focal length in pixels for a viewport of this size.
    pub fn focal(&self, rect: Rect) -> f64 {
        f64::from(rect.width()) * 0.5 / (self.fov.to_radians() * 0.5).tan()
    }

    /// How far in front of the camera a point is, along the view direction.
    pub fn depth(&self, point: DVec3) -> f64 {
        (point - self.position).dot(self.forward())
    }

    /// Where a world point shows in a viewport, or `None` when it is behind the near plane.
    pub fn project(&self, rect: Rect, point: DVec3) -> Option<Pos2> {
        let forward = self.forward();
        let right = self.right();
        let up = right.cross(forward);
        let offset = point - self.position;
        let depth = offset.dot(forward);
        if depth < NEAR {
            return None;
        }
        let focal = self.focal(rect);
        let center = rect.center();
        Some(Pos2::new(
            center.x + (offset.dot(right) / depth * focal) as f32,
            center.y - (offset.dot(up) / depth * focal) as f32,
        ))
    }

    /// The world-space ray from the camera through a point of the viewport.
    pub fn ray(&self, rect: Rect, pos: Pos2) -> (DVec3, DVec3) {
        let forward = self.forward();
        let right = self.right();
        let up = right.cross(forward);
        let focal = self.focal(rect);
        let x = f64::from(pos.x - rect.center().x) / focal;
        let y = -f64::from(pos.y - rect.center().y) / focal;
        (self.position, (forward + right * x + up * y).normalize())
    }

    /// Draws every wireframe in perspective, clipping edges at the near plane. Selected
    /// meshes are drawn last in red at the overlay's move offset, then the block tool's box.
    pub fn draw(&self, canvas: &mut dyn Canvas, rect: Rect, scene: &Scene, active: bool, overlay: &Overlay) {
        let style = style::current();
        canvas.fill(rect, style.background);
        let forward = self.forward();
        let right = self.right();
        let up = right.cross(forward);
        let focal = self.focal(rect);
        let center = rect.center();
        let to_camera = |point: DVec3| {
            let offset = point - self.position;
            DVec3::new(offset.dot(right), offset.dot(up), offset.dot(forward))
        };
        let to_screen = |camera: DVec3| {
            Pos2::new(
                center.x + (camera.x / camera.z * focal) as f32,
                center.y - (camera.y / camera.z * focal) as f32,
            )
        };
        let draw_edges = |canvas: &mut dyn Canvas, edges: &[[DVec3; 2]], remap: Option<&Remap>, color: Color32| {
            let map = |point: DVec3| remap.map_or(point, |remap| remap.apply(point));
            let mut segments = Vec::with_capacity(edges.len());
            for [a, b] in edges {
                let (mut a, mut b) = (to_camera(map(*a)), to_camera(map(*b)));
                if a.z < NEAR && b.z < NEAR {
                    continue;
                }
                if a.z < NEAR {
                    a = a + (b - a) * ((NEAR - a.z) / (b.z - a.z));
                } else if b.z < NEAR {
                    b = b + (a - b) * ((NEAR - b.z) / (a.z - b.z));
                }
                segments.push([to_screen(a), to_screen(b)]);
            }
            canvas.lines(&segments, color);
        };

        let unselected = scene.meshes.iter().filter(|mesh| !mesh.selected);
        let selected = scene.meshes.iter().filter(|mesh| mesh.selected);
        for mesh in unselected.chain(selected) {
            let (color, remap) = mesh.appearance(overlay);
            let bounds = mesh.mapped_bounds(remap.as_ref());
            let radius = bounds.size().length() * 0.5;
            if to_camera(bounds.center()).z + radius < NEAR {
                continue;
            }
            draw_edges(canvas, &mesh.edges, remap.as_ref(), color);
        }
        if let Some(block) = &overlay.block {
            draw_edges(canvas, &box_edges(block), None, style.tool);
        }
        draw_edges(canvas, &overlay.agent_camera, None, AGENT_CAMERA_COLOR);
        if let Some(clip) = &overlay.clip {
            for (edge, color) in clip.edges_3d() {
                draw_edges(canvas, &[edge], None, color);
            }
        }

        let label_color = if active { Color32::WHITE } else { Color32::GRAY };
        canvas.label(
            rect.left_top() + egui::vec2(6.0, 4.0),
            "camera (wireframe)",
            label_color,
        );
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::vec2;

    use super::*;

    #[test]
    fn dragging_right_turns_the_camera_right() {
        let mut camera = Camera3D::new();
        let yaw = camera.yaw;
        camera.look_by(vec2(10.0, 0.0));
        assert!(camera.yaw < yaw, "a right turn lowers the yaw, as mouselook does");
    }

    #[test]
    fn looking_clamps_the_pitch() {
        let mut camera = Camera3D::new();
        camera.look_by(vec2(0.0, -100_000.0));
        assert_eq!(camera.pitch, MAX_PITCH);
    }

    #[test]
    fn dragging_right_slides_the_camera_left() {
        let mut camera = Camera3D::new();
        camera.yaw = 0.0;
        camera.pitch = 0.0;
        let before = camera.position;
        camera.pan_by(vec2(8.0, 0.0));
        let moved = camera.position - before;
        assert!((moved.dot(camera.right()) + 8.0 * PAN_UNITS_PER_POINT).abs() < 1e-9, "{moved:?}");
        assert!(moved.z.abs() < 1e-9, "a sideways drag keeps the height: {moved:?}");
    }

    #[test]
    fn dragging_down_slides_the_camera_up() {
        let mut camera = Camera3D::new();
        camera.yaw = 0.0;
        camera.pitch = 0.0;
        let before = camera.position;
        camera.pan_by(vec2(0.0, 8.0));
        assert!((camera.position - before).z > 0.0);
    }
}
