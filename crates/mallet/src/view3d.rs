//! The 3D camera: a position and two Euler angles, the matrices built from them, keyboard
//! flight, mouselook and the CPU wireframe fallback drawing.

use std::collections::HashSet;

use eframe::egui::{self, Color32, Pos2, Rect, ViewportCommand};
use glam::{DMat3, DMat4, DVec3, DVec4};
use mallet_vmf::geometry::Bounds;

use crate::canvas::Canvas;
use crate::keymap::{Action, KeyMap};
use crate::scene::{box_edges, Overlay, Remap, Scene, AGENT_CAMERA_COLOR};
use crate::style;

/// The fastest the movement keys fly, in world units per second, matching Hammer's
/// default forward speed.
pub const DEFAULT_MOVE_SPEED: f64 = 1000.0;
/// Hammer's default horizontal field of view in degrees.
pub const DEFAULT_FOV: f64 = 90.0;
/// Width over height assumed for a 3D view until one is shown.
pub const DEFAULT_ASPECT: f64 = 16.0 / 9.0;
/// Default degrees per second the look keys turn the camera.
pub const DEFAULT_TURN_SPEED: f64 = 90.0;
/// Degrees per pixel of mouse movement in mouselook.
pub const DEFAULT_LOOK_SENSITIVITY: f64 = 0.2;
/// Units of wheel delta in one mouse wheel notch, as Windows reports them.
pub const WHEEL_DELTA_PER_NOTCH: f64 = 120.0;
/// Default world units one mouse wheel notch moves the camera: half a unit per unit of
/// wheel delta, 60 units a notch.
pub const DEFAULT_WHEEL_STEP: f64 = 0.5 * WHEEL_DELTA_PER_NOTCH;
/// Points egui scrolls for one mouse wheel notch.
const POINTS_PER_NOTCH: f64 = 50.0;
/// World units the Camera tool's right drag slides the camera per point of mouse movement.
const PAN_UNITS_PER_POINT: f64 = 1.0;
/// World units the Space and right button strafe drag moves the camera per point of mouse
/// movement.
const STRAFE_UNITS_PER_POINT: f64 = 1.0;
/// Default acceleration of the movement keys in world units per second squared: full
/// speed after half a second, as in Hammer.
pub const DEFAULT_ACCELERATION: f64 = 2.0 * DEFAULT_MOVE_SPEED;
/// The default share of the camera speed flown while crouching (Alt in mouselook), like
/// Source's crouch speed.
pub const DEFAULT_CROUCH_SPEED: f64 = 1.0 / 3.0;
/// The longest frame time used for movement, so a hitch never flings the camera.
pub const MAX_FRAME_SECONDS: f64 = 0.3;
/// Default draw distance in world units: the far plane of the projection.
pub const DEFAULT_DRAW_DISTANCE: f64 = 5000.0;
/// Distance of the near clipping plane.
pub const NEAR: f64 = 1.0;
/// The steepest the camera looks up or down, in degrees.
pub const MAX_PITCH: f64 = 90.0;

/// Maps the camera's own axes (right, forward, up) to view space, where the camera looks
/// down -Z with +Y up, as wgpu's right-handed projection expects.
const CAMERA_TO_VIEW: DMat4 = DMat4::from_cols(
    DVec4::new(1.0, 0.0, 0.0, 0.0),
    DVec4::new(0.0, 0.0, -1.0, 0.0),
    DVec4::new(0.0, 1.0, 0.0, 0.0),
    DVec4::new(0.0, 0.0, 0.0, 1.0),
);

/// The three movement axes of keyboard flight, each with its own signed velocity.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Velocity {
    /// Along the view direction, positive forward.
    forward: f64,
    /// Along the camera's right, positive right.
    right: f64,
    /// Along the world's Z axis, positive up.
    vertical: f64,
}

impl Velocity {
    fn is_zero(&self) -> bool {
        *self == Self::default()
    }
}

/// One frame of one velocity axis: accelerates towards `input` (-1, 0 or 1) up to
/// `max_speed`, starts from rest when the input reverses, and stops dead without input.
fn accelerate(velocity: f64, input: f64, acceleration: f64, max_speed: f64, dt: f64) -> f64 {
    if input == 0.0 {
        return 0.0;
    }
    let start = if velocity != 0.0 && velocity.signum() != input.signum() {
        0.0
    } else {
        velocity
    };
    (start + acceleration * input * dt).clamp(-max_speed, max_speed)
}

/// The frame time movement uses: egui's measured time since the last frame, or its
/// predicted frame time on the first frame after the camera was still, when the measured
/// time covers the idle wait. `None` when the time is not positive, which skips the frame.
pub fn frame_seconds(measured: f64, predicted: f64, was_moving: bool) -> Option<f64> {
    let dt = if was_moving { measured } else { predicted };
    (dt > 0.0 && dt.is_finite()).then(|| dt.min(MAX_FRAME_SECONDS))
}

/// A free-flying first-person camera in Source's Z-up coordinates: a position and two
/// Euler angles. Roll is always zero.
pub struct Camera3D {
    pub position: DVec3,
    /// Degrees around Z; 0 looks along +X. Not bounded.
    pub yaw: f64,
    /// Degrees above the horizon, from -90 to 90.
    pub pitch: f64,
    /// Horizontal field of view in degrees.
    pub fov: f64,
    /// Width over height of the 3D view the camera was last shown in, for the projection
    /// and for the 2D views' drawing of its view pyramid.
    pub aspect: f64,
    /// Draw distance: the projection's far plane, like Hammer's back clipping plane.
    pub far: f64,
    /// The fastest the movement keys fly, in units per second.
    pub move_speed: f64,
    /// Degrees per point of mouse movement in mouselook.
    pub look_sensitivity: f64,
    /// Degrees per second the look keys turn.
    pub turn_speed: f64,
    /// How fast the movement keys speed up, in units per second squared.
    pub acceleration: f64,
    /// Units one mouse wheel notch moves the camera.
    pub wheel_step: f64,
    /// The share of [`Self::move_speed`] flown while crouching (Alt in mouselook), from 0.05 to 1.
    pub crouch_speed: f64,
    mouse_look: bool,
    /// Whether mouselook lasts only while the right mouse button is held.
    held_look: bool,
    velocity: Velocity,
    /// Whether the camera moved or turned last frame, so the next frame's time is the
    /// measured one rather than the idle wait before it.
    was_moving: bool,
    /// Keys pressed as part of a shortcut, which do not move the camera until released.
    shortcut_keys: HashSet<egui::Key>,
}

impl Default for Camera3D {
    fn default() -> Self {
        Self::new()
    }
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
            mouse_look: false,
            held_look: false,
            velocity: Velocity::default(),
            was_moving: false,
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
            ..Self::new()
        }
    }

    /// The rotation from the Euler angles: pitch about X first, then yaw about Z, applied
    /// to the camera's own axes (right along X, forward along Y, up along Z).
    pub fn orientation(&self) -> DMat3 {
        DMat3::from_rotation_z((self.yaw - 90.0).to_radians()) * DMat3::from_rotation_x(self.pitch.to_radians())
    }

    /// The view direction.
    pub fn forward(&self) -> DVec3 {
        self.orientation() * DVec3::Y
    }

    /// The camera's right, always level since roll is zero.
    pub fn right(&self) -> DVec3 {
        self.orientation() * DVec3::X
    }

    /// The camera's up, square to its forward and right.
    pub fn up(&self) -> DVec3 {
        self.orientation() * DVec3::Z
    }

    /// Moves the camera along its view direction.
    pub fn move_forward(&mut self, units: f64) {
        self.position += self.forward() * units;
    }

    /// Moves the camera along its right.
    pub fn move_right(&mut self, units: f64) {
        self.position += self.right() * units;
    }

    /// Moves the camera along its up.
    pub fn move_up(&mut self, units: f64) {
        self.position += self.up() * units;
    }

    /// Sets the pitch, clamped to straight up or down.
    pub fn set_pitch(&mut self, degrees: f64) {
        self.pitch = degrees.clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Sets the yaw, which is not bounded.
    pub fn set_yaw(&mut self, degrees: f64) {
        self.yaw = degrees;
    }

    /// The view matrix: the rotation from the Euler angles, then the translation by the
    /// negated camera position.
    pub fn build_view_matrix(&self) -> DMat4 {
        CAMERA_TO_VIEW
            * DMat4::from_mat3(self.orientation().transpose())
            * DMat4::from_translation(-self.position)
    }

    /// The perspective projection for a view `aspect` (width over height) wide, with the
    /// camera's horizontal field of view, the near plane at [`NEAR`] and the far plane at
    /// the draw distance.
    pub fn build_projection_matrix(&self, aspect: f64) -> DMat4 {
        let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { DEFAULT_ASPECT };
        let horizontal = self.fov.clamp(1.0, 179.0).to_radians();
        let vertical = 2.0 * ((horizontal * 0.5).tan() / aspect).atan();
        DMat4::perspective_rh(vertical, aspect, NEAR, self.far.max(NEAR * 2.0))
    }

    /// The projection times the view matrix, for a view `aspect` wide.
    pub fn view_projection(&self, aspect: f64) -> DMat4 {
        self.build_projection_matrix(aspect) * self.build_view_matrix()
    }

    /// The inverse of [`Self::view_projection`], which turns screen points back into the
    /// world.
    pub fn inverse_view_projection(&self, aspect: f64) -> DMat4 {
        self.view_projection(aspect).inverse()
    }

    /// The world-space ray from the camera through a point of the viewport: the point is
    /// unprojected onto the near plane through the inverse view-projection matrix, and the
    /// ray runs from the camera's position through it.
    pub fn build_ray(&self, rect: Rect, pos: Pos2) -> (DVec3, DVec3) {
        let aspect = rect_aspect(rect);
        let x = f64::from((pos.x - rect.left()) / rect.width().max(1.0)) * 2.0 - 1.0;
        let y = 1.0 - f64::from((pos.y - rect.top()) / rect.height().max(1.0)) * 2.0;
        let near = self.inverse_view_projection(aspect).project_point3(DVec3::new(x, y, 0.0));
        let direction = (near - self.position).normalize_or_zero();
        let direction = if direction == DVec3::ZERO { self.forward() } else { direction };
        (self.position, direction)
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
            if direction.x != 0.0 || direction.y != 0.0 {
                self.yaw = direction.y.atan2(direction.x).to_degrees().rem_euclid(360.0);
            }
            self.set_pitch(direction.z.clamp(-1.0, 1.0).asin().to_degrees());
        }
        self.velocity = Velocity::default();
    }

    /// Moves the camera back from the bounds' centre until all of it is in view.
    pub fn look_at(&mut self, bounds: &Bounds) {
        if bounds.is_empty() {
            return;
        }
        let radius = (bounds.size().length() * 0.5).max(128.0);
        self.position = bounds.center() - self.forward() * radius * 1.5;
    }

    /// Turns mouselook on or off: on hides the cursor and keeps it in the window, off
    /// gives the normal cursor back. `held` mouselook lasts only while the right mouse
    /// button is held.
    pub fn set_mouse_look(&mut self, ctx: &egui::Context, enabled: bool, held: bool) {
        self.mouse_look = enabled;
        self.held_look = enabled && held;
        let grab = if enabled {
            egui::CursorGrab::Confined
        } else {
            egui::CursorGrab::None
        };
        ctx.send_viewport_cmd(ViewportCommand::CursorGrab(grab));
        ctx.send_viewport_cmd(ViewportCommand::CursorVisible(!enabled));
    }

    /// Whether the current mouselook ends when the right mouse button is let go.
    pub fn held_look(&self) -> bool {
        self.mouse_look && self.held_look
    }

    /// Turns the camera by `delta` points of mouse movement at the mouselook sensitivity:
    /// right turns right and down looks down.
    pub fn look_by(&mut self, delta: egui::Vec2) {
        self.set_yaw(self.yaw - f64::from(delta.x) * self.look_sensitivity);
        self.set_pitch(self.pitch - f64::from(delta.y) * self.look_sensitivity);
    }

    /// Applies keyboard, wheel and mouselook input. Returns whether the camera needs
    /// another frame because it is moving or in mouselook.
    ///
    /// Input is read only while the viewport is hovered or mouselook is on. Z toggles
    /// mouselook and Escape leaves it.
    ///
    /// In mouselook the mouse's movement since the last frame turns the camera, which the
    /// window then puts back at the viewport's centre. The movement keys drive one signed
    /// velocity per axis: each frame it grows by the acceleration times the frame time
    /// towards the key's direction, up to the camera speed. It starts from rest when the
    /// direction reverses and stops dead as soon as the keys are let go. The look keys
    /// turn at [`Self::turn_speed`] degrees per second. Frame times are clamped to
    /// [`MAX_FRAME_SECONDS`], and a frame without a positive time moves nothing.
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
            self.velocity = Velocity::default();
            self.was_moving = false;
            return false;
        }
        let (toggle, escape) = ctx.input(|input| {
            (
                keymap.pressed(input, Action::ToggleMouseLook),
                input.key_pressed(egui::Key::Escape),
            )
        });
        if toggle || (escape && self.mouse_look) {
            self.set_mouse_look(ctx, toggle && !self.mouse_look, false);
        }

        let looking = self.mouse_look;
        let mut moving = false;
        ctx.input(|input| {
            if looking {
                let delta: egui::Vec2 = input
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::MouseMoved(delta) => Some(*delta),
                        _ => None,
                    })
                    .fold(egui::Vec2::ZERO, |total, delta| total + delta);
                self.look_by(delta);
            }

            let scroll = f64::from(input.raw_scroll_delta.y);
            if scroll != 0.0 {
                self.move_forward(self.wheel_step * scroll / POINTS_PER_NOTCH);
            }

            let Some(dt) = frame_seconds(
                f64::from(input.unstable_dt),
                f64::from(input.predicted_dt),
                self.was_moving,
            ) else {
                return;
            };
            let blocked = &self.shortcut_keys;
            let axis = |positive: Action, negative: Action| {
                let held = |action| f64::from(u8::from(keymap.held(input, action, blocked, looking)));
                held(positive) - held(negative)
            };
            let yaw_input = axis(Action::TurnLeft, Action::TurnRight);
            let pitch_input = axis(Action::LookUp, Action::LookDown);
            let forward_input = axis(Action::MoveForward, Action::MoveBack);
            let right_input = axis(Action::StrafeRight, Action::StrafeLeft);
            let vertical_input = axis(Action::StrafeUp, Action::StrafeDown);
            self.set_yaw(self.yaw + yaw_input * self.turn_speed * dt);
            self.set_pitch(self.pitch + pitch_input * self.turn_speed * dt);

            let max_speed = if looking && input.modifiers.alt {
                self.move_speed * self.crouch_speed.clamp(0.05, 1.0)
            } else {
                self.move_speed
            };
            let acceleration = self.acceleration.max(1.0);
            let step = |velocity, input| accelerate(velocity, input, acceleration, max_speed, dt);
            self.velocity = Velocity {
                forward: step(self.velocity.forward, forward_input),
                right: step(self.velocity.right, right_input),
                vertical: step(self.velocity.vertical, vertical_input),
            };
            self.move_forward(self.velocity.forward * dt);
            self.move_right(self.velocity.right * dt);
            self.position.z += self.velocity.vertical * dt;
            moving = !self.velocity.is_zero() || yaw_input != 0.0 || pitch_input != 0.0;
        });
        self.was_moving = moving;
        moving || self.mouse_look
    }

    /// Draws the small crosshair at the centre of a 3D view while mouselook is on; clicks
    /// then select what is under it.
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

    /// Slides the camera across its view by a mouse drag of `delta` points, so the scene
    /// moves with the mouse: dragging right moves the camera left. The Camera tool's right
    /// drag uses this.
    pub fn pan_by(&mut self, delta: egui::Vec2) {
        self.move_right(-f64::from(delta.x) * PAN_UNITS_PER_POINT);
        self.move_up(f64::from(delta.y) * PAN_UNITS_PER_POINT);
    }

    /// Strafes the camera in its view plane by a mouse drag of `delta` points, the way the
    /// Space and right button drag does: dragging right moves the camera right and
    /// dragging up moves it up.
    pub fn strafe_by(&mut self, delta: egui::Vec2) {
        self.move_right(f64::from(delta.x) * STRAFE_UNITS_PER_POINT);
        self.move_up(-f64::from(delta.y) * STRAFE_UNITS_PER_POINT);
    }

    /// Focal length in pixels for a viewport of this size.
    pub fn focal(&self, rect: Rect) -> f64 {
        f64::from(rect.width()) * 0.5 / (self.fov.to_radians() * 0.5).tan()
    }

    /// How far in front of the camera a point is, along the view direction.
    pub fn depth(&self, point: DVec3) -> f64 {
        (point - self.position).dot(self.forward())
    }

    /// Where a world point shows in a viewport, through the view-projection matrix, or
    /// `None` when it is behind the near plane.
    pub fn project(&self, rect: Rect, point: DVec3) -> Option<Pos2> {
        if self.depth(point) < NEAR {
            return None;
        }
        let clip = self.view_projection(rect_aspect(rect)) * point.extend(1.0);
        let (x, y) = (clip.x / clip.w, clip.y / clip.w);
        Some(Pos2::new(
            rect.left() + ((x + 1.0) * 0.5) as f32 * rect.width(),
            rect.top() + ((1.0 - y) * 0.5) as f32 * rect.height(),
        ))
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

/// Width over height of a viewport rectangle.
fn rect_aspect(rect: Rect) -> f64 {
    if rect.height() >= 1.0 {
        f64::from(rect.width() / rect.height())
    } else {
        DEFAULT_ASPECT
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

    #[test]
    fn the_view_matrix_puts_the_view_direction_down_negative_z() {
        let mut camera = Camera3D::new();
        for (yaw, pitch) in [(0.0, 0.0), (45.0, -30.0), (200.0, 60.0), (-725.0, 90.0), (10.0, -90.0)] {
            camera.yaw = yaw;
            camera.pitch = pitch;
            let view = camera.build_view_matrix();
            let ahead = view.transform_point3(camera.position + camera.forward());
            let right = view.transform_point3(camera.position + camera.right());
            let up = view.transform_point3(camera.position + camera.up());
            assert!((ahead - DVec3::NEG_Z).length() < 1e-9, "{yaw} {pitch}: {ahead:?}");
            assert!((right - DVec3::X).length() < 1e-9, "{yaw} {pitch}: {right:?}");
            assert!((up - DVec3::Y).length() < 1e-9, "{yaw} {pitch}: {up:?}");
        }
    }

    #[test]
    fn yaw_zero_looks_along_x_and_pitch_looks_up() {
        let mut camera = Camera3D::new();
        camera.yaw = 0.0;
        camera.pitch = 0.0;
        assert!((camera.forward() - DVec3::X).length() < 1e-12);
        assert!((camera.right() - DVec3::NEG_Y).length() < 1e-12);
        camera.pitch = 90.0;
        assert!((camera.forward() - DVec3::Z).length() < 1e-12);
    }

    #[test]
    fn a_ray_through_the_centre_follows_the_view_direction() {
        let mut camera = Camera3D::new();
        camera.pitch = 90.0;
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), egui::vec2(800.0, 450.0));
        let (origin, direction) = camera.build_ray(rect, rect.center());
        assert_eq!(origin, camera.position);
        assert!((direction - camera.forward()).length() < 1e-9, "{direction:?}");
    }

    #[test]
    fn rays_and_projection_agree() {
        let camera = Camera3D::new();
        let rect = Rect::from_min_size(Pos2::new(5.0, 7.0), egui::vec2(640.0, 360.0));
        for pixel in [Pos2::new(5.0, 7.0), Pos2::new(100.0, 300.0), Pos2::new(640.0, 360.0)] {
            let (origin, direction) = camera.build_ray(rect, pixel);
            let projected = camera.project(rect, origin + direction * 300.0).unwrap();
            assert!(projected.distance(pixel) < 0.01, "{pixel:?} came back as {projected:?}");
        }
    }

    #[test]
    fn the_horizontal_field_of_view_spans_the_viewport_width() {
        let mut camera = Camera3D::new();
        camera.yaw = 0.0;
        camera.pitch = 0.0;
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 400.0));
        let (_, edge) = camera.build_ray(rect, Pos2::new(800.0, 200.0));
        assert!((edge.angle_between(camera.forward()).to_degrees() - 45.0).abs() < 1e-6);
    }

    #[test]
    fn flight_speeds_up_reverses_from_rest_and_stops_dead() {
        let speed = accelerate(0.0, 1.0, 2000.0, 1000.0, 0.1);
        assert_eq!(speed, 200.0);
        assert_eq!(accelerate(speed, 1.0, 2000.0, 1000.0, 1.0), 1000.0);
        assert_eq!(accelerate(900.0, -1.0, 2000.0, 1000.0, 0.1), -200.0);
        assert_eq!(accelerate(900.0, 0.0, 2000.0, 1000.0, 0.1), 0.0);
    }

    #[test]
    fn frame_times_are_clamped_and_empty_frames_skipped() {
        assert_eq!(frame_seconds(2.0, 1.0 / 60.0, true), Some(MAX_FRAME_SECONDS));
        assert_eq!(frame_seconds(2.0, 1.0 / 60.0, false), Some(1.0 / 60.0));
        assert_eq!(frame_seconds(0.0, 1.0 / 60.0, true), None);
    }

    #[test]
    fn moves_follow_the_camera_basis() {
        let mut camera = Camera3D::new();
        let start = camera.position;
        camera.move_forward(10.0);
        camera.move_right(-3.0);
        camera.move_up(2.0);
        let moved = camera.position - start;
        assert!((moved.dot(camera.forward()) - 10.0).abs() < 1e-9);
        assert!((moved.dot(camera.right()) + 3.0).abs() < 1e-9);
        assert!((moved.dot(camera.up()) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn strafing_follows_the_drag() {
        let mut camera = Camera3D::new();
        let start = camera.position;
        camera.strafe_by(vec2(8.0, -4.0));
        let moved = camera.position - start;
        assert!(moved.dot(camera.right()) > 0.0);
        assert!(moved.dot(camera.up()) > 0.0);
    }
}
