//! Texture alignment on brush faces, as Hammer's Face Edit sheet does it: world and face
//! alignment, rotation about the face, justifying or fitting a texture to a face, and
//! Hammer++'s hotspot texturing, which fits each face into the best matching rectangle of
//! a trim sheet.
//!
//! Source maps texture coordinates as `u = point · uaxis / scale + offset` (in texels), and
//! the same for `v`. Rotations turn both axes about the face normal; justify and fit
//! change the scale and offset so the face's edges land where they should on the texture.

use glam::DVec3;

use crate::{Side, TexAxis};

/// Where to line a texture up on a face.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Justify {
    Left,
    Right,
    Top,
    Bottom,
    Center,
    /// Stretch the texture once across the face.
    Fit,
    /// Fit the texture once across the face's width, keeping its proportions.
    FitWidth,
    /// Fit the texture once down the face's height, keeping its proportions.
    FitHeight,
}

/// World units per texel that hotspot rectangles are matched at, Hammer's default scale.
pub const HOTSPOT_SCALE: f64 = 0.25;

/// How much two rectangles' fit to a face may differ, as the sum of the logarithms of
/// their width and height ratios, and still count as equally good, so one is picked at
/// random.
const HOTSPOT_TIE: f64 = 0.05;

/// One rectangle of a hotspot texture, in texels from the texture's top left corner, as
/// a `.rect` file lists them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hotspot {
    pub min: [f64; 2],
    pub max: [f64; 2],
    /// The rectangle may be turned a quarter turn to suit a face.
    pub rotate: bool,
    /// The rectangle may be flipped horizontally at random.
    pub reflect: bool,
    /// An alternate, used only when alternates are asked for.
    pub alt: bool,
}

impl Hotspot {
    /// The rectangle's width and height in texels.
    pub fn size(&self) -> [f64; 2] {
        [(self.max[0] - self.min[0]).abs(), (self.max[1] - self.min[1]).abs()]
    }

    /// Reads the rectangles of a `.rect` file:
    ///
    /// ```text
    /// Rectangles
    /// {
    ///     rectangle { min "0 0" max "512 32" }
    ///     rectangle { min "512 0" max "768 32" reflect 1 }
    /// }
    /// ```
    ///
    /// Rectangles with no area or unreadable corners are skipped.
    pub fn parse_rect_file(text: &str) -> Result<Vec<Hotspot>, String> {
        let items = mallet_kv::parse(text).map_err(|error| error.to_string())?;
        let rectangles = items
            .iter()
            .filter_map(|item| match item {
                mallet_kv::Item::Block(block) => Some(block),
                mallet_kv::Item::Pair(_) => None,
            })
            .flat_map(|root| root.blocks_named("rectangle"));
        let corner = |value: Option<&str>| -> Option<[f64; 2]> {
            let mut numbers = value?.split_whitespace().map(|number| number.parse::<f64>().ok());
            Some([numbers.next()??, numbers.next()??])
        };
        let flag = |block: &mallet_kv::Block, key: &str| {
            block
                .get(key)
                .and_then(|value| value.trim().parse::<f64>().ok())
                .is_some_and(|value| value != 0.0)
        };
        Ok(rectangles
            .filter_map(|block| {
                let (Some(a), Some(b)) = (corner(block.get("min")), corner(block.get("max"))) else {
                    return None;
                };
                let hotspot = Hotspot {
                    min: [a[0].min(b[0]), a[1].min(b[1])],
                    max: [a[0].max(b[0]), a[1].max(b[1])],
                    rotate: flag(block, "rotate"),
                    reflect: flag(block, "reflect"),
                    alt: flag(block, "alt"),
                };
                let [width, height] = hotspot.size();
                (width > 0.0 && height > 0.0).then_some(hotspot)
            })
            .collect())
    }
}

/// The axes Hammer and the compilers pick for a face aligned to the world: the major axis
/// closest to the normal decides which world plane the texture is projected from.
pub fn world_axes(normal: DVec3) -> (DVec3, DVec3) {
    // Quake's base axes, in the order Hammer tests them: floor, ceiling, west, east,
    // south, north.
    const BASES: [(DVec3, DVec3, DVec3); 6] = [
        (DVec3::Z, DVec3::X, DVec3::NEG_Y),
        (DVec3::NEG_Z, DVec3::X, DVec3::NEG_Y),
        (DVec3::X, DVec3::Y, DVec3::NEG_Z),
        (DVec3::NEG_X, DVec3::Y, DVec3::NEG_Z),
        (DVec3::Y, DVec3::X, DVec3::NEG_Z),
        (DVec3::NEG_Y, DVec3::X, DVec3::NEG_Z),
    ];
    let mut best = (f64::MIN, DVec3::X, DVec3::NEG_Y);
    for (base, u, v) in BASES {
        let dot = normal.dot(base);
        if dot > best.0 + 1e-9 {
            best = (dot, u, v);
        }
    }
    (best.1, best.2)
}

/// Axes lying in the face itself, so the texture is not stretched on slopes: `v` points
/// down the face, and floors and ceilings fall back to world alignment.
pub fn face_axes(normal: DVec3) -> (DVec3, DVec3) {
    let normal = normal.normalize_or_zero();
    let down = DVec3::NEG_Z - normal * normal.dot(DVec3::NEG_Z);
    if down.length() < 1e-6 {
        return world_axes(normal);
    }
    let v = down.normalize();
    let u = normal.cross(v).normalize();
    (u, v)
}

/// The least area a unit square of texture may keep when its axes are flattened onto a
/// face, for [`Side::copy_alignment`] to keep them: half, so a texture is stretched to at
/// most twice its size. World alignment never goes below about 0.58.
const AXES_FIT_MIN: f64 = 0.5;

/// Whether texture axes show a texture on a face with this normal without stretching it
/// more than [`AXES_FIT_MIN`] allows.
fn axes_fit(u: DVec3, v: DVec3, normal: DVec3) -> bool {
    let normal = normal.normalize_or_zero();
    let flat = |axis: DVec3| {
        let axis = axis.normalize_or_zero();
        axis - normal * axis.dot(normal)
    };
    flat(u).cross(flat(v)).dot(normal).abs() >= AXES_FIT_MIN
}

/// Turns a vector about `axis` by `degrees`, counter-clockwise looking against the axis.
fn rotate(vector: DVec3, axis: DVec3, degrees: f64) -> DVec3 {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let axis = axis.normalize_or_zero();
    vector * cos + axis.cross(vector) * sin + axis * axis.dot(vector) * (1.0 - cos)
}

/// The face's normal from its plane points, pointing out of the brush.
pub fn side_normal(side: &Side) -> DVec3 {
    let [a, b, c] = side.plane;
    (c - a).cross(b - a).normalize_or_zero()
}

/// The range of `point · axis` over the face, in world units.
fn extent(axis: DVec3, points: &[DVec3]) -> (f64, f64) {
    points
        .iter()
        .map(|point| point.dot(axis))
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), value| {
            (min.min(value), max.max(value))
        })
}

/// Keeps an offset within one texture repeat, as Hammer shows it.
fn wrap(offset: f64, size: f64) -> f64 {
    if size <= 0.0 {
        return offset;
    }
    let wrapped = offset.rem_euclid(size);
    if (wrapped - size).abs() < 1e-6 {
        0.0
    } else {
        (wrapped * 1000.0).round() / 1000.0
    }
}

impl Side {
    /// Aligns the texture to the world or to the face, keeping scale and shift and
    /// applying the side's rotation again.
    pub fn align(&mut self, to_face: bool) {
        let normal = side_normal(self);
        let (u, v) = if to_face { face_axes(normal) } else { world_axes(normal) };
        self.uaxis.axis = u;
        self.vaxis.axis = v;
        let rotation = self.rotation.unwrap_or(0.0);
        if rotation != 0.0 {
            self.uaxis.axis = rotate(u, normal, rotation);
            self.vaxis.axis = rotate(v, normal, rotation);
        }
    }

    /// Takes over the texture alignment of `from`: its axes, scale, shift, rotation, luxel
    /// scale and smoothing groups, as Hammer's Apply (texture + values) does. Axes that
    /// would stretch the texture badly on this face, as a floor's axes do on a wall, are
    /// replaced by this face's world alignment, turned by the same rotation.
    pub fn copy_alignment(&mut self, from: &Side) {
        self.uaxis = from.uaxis;
        self.vaxis = from.vaxis;
        self.rotation = from.rotation;
        self.lightmap_scale = from.lightmap_scale;
        self.smoothing_groups = from.smoothing_groups;
        if !axes_fit(self.uaxis.axis, self.vaxis.axis, side_normal(self)) {
            self.align(false);
        }
    }

    /// Sets the texture rotation in degrees, turning both axes about the face normal by
    /// the difference from the current rotation.
    pub fn set_rotation(&mut self, degrees: f64) {
        let delta = degrees - self.rotation.unwrap_or(0.0);
        if delta != 0.0 {
            let normal = side_normal(self);
            self.uaxis.axis = rotate(self.uaxis.axis, normal, delta);
            self.vaxis.axis = rotate(self.vaxis.axis, normal, delta);
        }
        self.rotation = Some(degrees);
    }

    /// Lines the texture up on `points` (the face polygon, or several faces' points for
    /// "treat as one"). `size` is the texture size in texels.
    pub fn justify(&mut self, how: Justify, points: &[DVec3], size: [f64; 2]) {
        if points.is_empty() {
            return;
        }
        let [width, height] = size.map(|side| side.max(1.0));
        let (u_min, u_max) = extent(self.uaxis.axis, points);
        let (v_min, v_max) = extent(self.vaxis.axis, points);
        match how {
            Justify::Fit => {
                self.uaxis.scale = ((u_max - u_min) / width).max(1e-4);
                self.vaxis.scale = ((v_max - v_min) / height).max(1e-4);
            }
            // One axis fits; the other gets the same world units per texel, so the
            // texture is not stretched.
            Justify::FitWidth => {
                self.uaxis.scale = ((u_max - u_min) / width).max(1e-4);
                self.vaxis.scale = self.uaxis.scale;
            }
            Justify::FitHeight => {
                self.vaxis.scale = ((v_max - v_min) / height).max(1e-4);
                self.uaxis.scale = self.vaxis.scale;
            }
            _ => {}
        }
        let (u, v) = (&mut self.uaxis, &mut self.vaxis);
        // The texel a world position lands on is `position / scale + offset`.
        let start = |axis: &TexAxis, min: f64| -min / axis.scale;
        let end = |axis: &TexAxis, max: f64, size: f64| size - max / axis.scale;
        let middle = |axis: &TexAxis, min: f64, max: f64, size: f64| size / 2.0 - (min + max) / 2.0 / axis.scale;
        match how {
            Justify::Left => u.offset = start(u, u_min),
            Justify::Right => u.offset = end(u, u_max, width),
            Justify::Top => v.offset = start(v, v_min),
            Justify::Bottom => v.offset = end(v, v_max, height),
            Justify::Center => {
                u.offset = middle(u, u_min, u_max, width);
                v.offset = middle(v, v_min, v_max, height);
            }
            Justify::Fit | Justify::FitWidth | Justify::FitHeight => {
                u.offset = start(u, u_min);
                v.offset = start(v, v_min);
            }
        }
        u.offset = wrap(u.offset, width);
        v.offset = wrap(v.offset, height);
    }

    /// Hammer++'s hotspot fit: picks the rectangle whose size at [`HOTSPOT_SCALE`] best
    /// matches the face `points`, turned a quarter turn if the rectangle allows it, and
    /// stretches the texture so the face covers exactly that rectangle. Equally good
    /// rectangles are chosen between with `random`, which also decides whether a
    /// reflectable rectangle is flipped. Rectangles marked `alt` are used only with
    /// `alternates`, and then they are the only ones used, unless the texture has none.
    /// Returns whether a rectangle was applied; faces with no area are left alone.
    pub fn hotspot(&mut self, points: &[DVec3], hotspots: &[Hotspot], alternates: bool, random: u64) -> bool {
        let normal = side_normal(self);
        let (mut u, mut v) = (self.uaxis.axis.normalize_or_zero(), self.vaxis.axis.normalize_or_zero());
        if u == DVec3::ZERO || v == DVec3::ZERO || u.cross(v).length() < 1e-6 {
            (u, v) = face_axes(normal);
        }
        let size = |axis: DVec3| {
            let (min, max) = extent(axis, points);
            max - min
        };
        if points.is_empty() {
            return false;
        }
        let (width, height) = (size(u), size(v));
        if width < 1e-3 || height < 1e-3 {
            return false;
        }
        let wanted_alt = alternates && hotspots.iter().any(|hotspot| hotspot.alt);
        let mut candidates: Vec<(f64, Hotspot, bool)> = Vec::new();
        for hotspot in hotspots.iter().filter(|hotspot| hotspot.alt == wanted_alt) {
            let [rect_width, rect_height] = hotspot.size().map(|texels| texels * HOTSPOT_SCALE);
            let error = |across: f64, down: f64| (across / rect_width).ln().abs() + (down / rect_height).ln().abs();
            candidates.push((error(width, height), *hotspot, false));
            if hotspot.rotate {
                candidates.push((error(height, width), *hotspot, true));
            }
        }
        let Some(best) = candidates.iter().map(|(error, ..)| *error).min_by(f64::total_cmp) else {
            return false;
        };
        let ties: Vec<&(f64, Hotspot, bool)> = candidates
            .iter()
            .filter(|(error, ..)| *error <= best + HOTSPOT_TIE)
            .collect();
        let (_, hotspot, turned) = *ties[(random % ties.len() as u64) as usize];
        if turned {
            (u, v) = (v, -u);
            self.rotation = Some((self.rotation.unwrap_or(0.0) + 90.0).rem_euclid(360.0));
        }
        if hotspot.reflect && (random >> 32) & 1 == 1 {
            u = -u;
        }
        let [rect_width, rect_height] = hotspot.size();
        let (u_min, u_max) = extent(u, points);
        let (v_min, v_max) = extent(v, points);
        self.uaxis.axis = u;
        self.vaxis.axis = v;
        self.uaxis.scale = ((u_max - u_min) / rect_width).max(1e-4);
        self.vaxis.scale = ((v_max - v_min) / rect_height).max(1e-4);
        self.uaxis.offset = hotspot.min[0] - u_min / self.uaxis.scale;
        self.vaxis.offset = hotspot.min[1] - v_min / self.vaxis.scale;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vmf;

    /// A 128 x 64 x 32 box's side whose normal points along `normal`.
    fn side_facing(normal: DVec3) -> Side {
        let text = r#"world { "id" "1" "classname" "worldspawn"
solid { "id" "2"
side { "id" "1" "plane" "(0 64 32) (128 64 32) (128 0 32)" "material" "A" "uaxis" "[1 0 0 0] 0.25" "vaxis" "[0 -1 0 0] 0.25" }
side { "id" "2" "plane" "(0 0 0) (128 0 0) (128 64 0)" "material" "A" "uaxis" "[1 0 0 0] 0.25" "vaxis" "[0 -1 0 0] 0.25" }
side { "id" "3" "plane" "(0 64 32) (0 0 32) (0 0 0)" "material" "A" "uaxis" "[0 1 0 0] 0.25" "vaxis" "[0 0 -1 0] 0.25" }
side { "id" "4" "plane" "(128 64 0) (128 0 0) (128 0 32)" "material" "A" "uaxis" "[0 1 0 0] 0.25" "vaxis" "[0 0 -1 0] 0.25" }
side { "id" "5" "plane" "(128 64 32) (0 64 32) (0 64 0)" "material" "A" "uaxis" "[1 0 0 0] 0.25" "vaxis" "[0 0 -1 0] 0.25" }
side { "id" "6" "plane" "(128 0 0) (0 0 0) (0 0 32)" "material" "A" "uaxis" "[1 0 0 0] 0.25" "vaxis" "[0 0 -1 0] 0.25" }
} }"#;
        let vmf = Vmf::parse(text).unwrap();
        let world = vmf.world().unwrap();
        let solid = world.solids().next().unwrap();
        let side = solid
            .sides()
            .find(|side| side_normal(side).dot(normal) > 0.99)
            .cloned()
            .unwrap();
        side
    }

    #[test]
    fn plane_points_give_outward_normals() {
        assert!(side_normal(&side_facing(DVec3::Z)).abs_diff_eq(DVec3::Z, 1e-9));
        assert!(side_normal(&side_facing(DVec3::NEG_X)).abs_diff_eq(DVec3::NEG_X, 1e-9));
    }

    #[test]
    fn world_alignment_matches_hammer() {
        assert_eq!(world_axes(DVec3::Z), (DVec3::X, DVec3::NEG_Y));
        assert_eq!(world_axes(DVec3::X), (DVec3::Y, DVec3::NEG_Z));
        assert_eq!(world_axes(DVec3::NEG_Y), (DVec3::X, DVec3::NEG_Z));
        // A 45 degree slope takes the floor's axes, as Hammer breaks ties.
        assert_eq!(world_axes(DVec3::new(1.0, 0.0, 1.0).normalize()).0, DVec3::X);
    }

    #[test]
    fn face_alignment_follows_slopes() {
        let normal = DVec3::new(1.0, 0.0, 1.0).normalize();
        let (u, v) = face_axes(normal);
        assert!(u.dot(normal).abs() < 1e-9 && v.dot(normal).abs() < 1e-9);
        assert!(v.z < 0.0, "v points down the slope");
        assert!(u.abs_diff_eq(DVec3::Y, 1e-9));
    }

    #[test]
    fn rotation_turns_both_axes_and_round_trips() {
        let mut side = side_facing(DVec3::Z);
        side.set_rotation(90.0);
        assert!(side.uaxis.axis.abs_diff_eq(DVec3::Y, 1e-9), "{:?}", side.uaxis.axis);
        assert!(side.vaxis.axis.abs_diff_eq(DVec3::X, 1e-9), "{:?}", side.vaxis.axis);
        side.set_rotation(0.0);
        assert!(side.uaxis.axis.abs_diff_eq(DVec3::X, 1e-9));
        side.align(false);
        assert!(side.uaxis.axis.abs_diff_eq(DVec3::X, 1e-9));
    }

    #[test]
    fn fit_and_justify_land_edges_on_texture_edges() {
        let mut side = side_facing(DVec3::Z);
        let points = [
            DVec3::new(0.0, 0.0, 32.0),
            DVec3::new(128.0, 0.0, 32.0),
            DVec3::new(128.0, 64.0, 32.0),
            DVec3::new(0.0, 64.0, 32.0),
        ];
        let texel = |side: &Side, point: DVec3| {
            [
                point.dot(side.uaxis.axis) / side.uaxis.scale + side.uaxis.offset,
                point.dot(side.vaxis.axis) / side.vaxis.scale + side.vaxis.offset,
            ]
        };
        side.justify(Justify::Fit, &points, [256.0, 256.0]);
        assert!((side.uaxis.scale - 0.5).abs() < 1e-9);
        assert!((side.vaxis.scale - 0.25).abs() < 1e-9);
        let corner = texel(&side, DVec3::new(0.0, 64.0, 32.0));
        assert!(
            corner[0].abs() < 1e-6 && corner[1].rem_euclid(256.0) < 1e-6,
            "{corner:?}"
        );

        let mut side = side_facing(DVec3::Z);
        side.justify(Justify::FitWidth, &points, [256.0, 256.0]);
        assert!((side.uaxis.scale - 0.5).abs() < 1e-9, "128 units across 256 texels");
        assert_eq!(side.vaxis.scale, side.uaxis.scale, "the height keeps the proportions");
        side.justify(Justify::FitHeight, &points, [256.0, 256.0]);
        assert!((side.vaxis.scale - 0.25).abs() < 1e-9, "64 units down 256 texels");
        assert_eq!(side.uaxis.scale, side.vaxis.scale);

        let mut side = side_facing(DVec3::Z);
        side.justify(Justify::Right, &points, [128.0, 128.0]);
        let right = texel(&side, DVec3::new(128.0, 0.0, 32.0))[0];
        assert!(
            (right.rem_euclid(128.0)).abs() < 1e-6,
            "the right edge is at a texture edge: {right}"
        );
        side.justify(Justify::Center, &points, [128.0, 128.0]);
        let middle = texel(&side, DVec3::new(64.0, 32.0, 32.0));
        assert!((middle[0].rem_euclid(128.0) - 64.0).abs() < 1e-6, "{middle:?}");
    }

    #[test]
    fn reads_rect_files() {
        let text = "Rectangles\n{\n\trectangle\n\t{\n\t\tmin\t\t\"0 0\"\n\t\tmax\t\t\"512 32\"\n\t}\n\
                    \trectangle\n\t{\n\t\tmin \"768 32\"\n\t\tmax \"512 0\"\n\t\treflect 1\n\t\trotate 1\n\t}\n\
                    \trectangle { min \"0 0\" max \"0 10\" }\n}\n";
        let hotspots = Hotspot::parse_rect_file(text).unwrap();
        assert_eq!(hotspots.len(), 2, "the empty rectangle is skipped");
        assert_eq!(hotspots[0].size(), [512.0, 32.0]);
        assert_eq!(hotspots[1].min, [512.0, 0.0]);
        assert!(hotspots[1].reflect && hotspots[1].rotate && !hotspots[1].alt);
    }

    #[test]
    fn hotspot_fits_the_face_into_the_closest_rectangle() {
        let rect = |min: [f64; 2], max: [f64; 2], rotate: bool| Hotspot {
            min,
            max,
            rotate,
            reflect: false,
            alt: false,
        };
        let texel = |side: &Side, point: DVec3| {
            [
                point.dot(side.uaxis.axis) / side.uaxis.scale + side.uaxis.offset,
                point.dot(side.vaxis.axis) / side.vaxis.scale + side.vaxis.offset,
            ]
        };
        let points = [
            DVec3::new(0.0, 0.0, 32.0),
            DVec3::new(128.0, 0.0, 32.0),
            DVec3::new(128.0, 64.0, 32.0),
            DVec3::new(0.0, 64.0, 32.0),
        ];
        let hotspots = [
            rect([0.0, 0.0], [256.0, 256.0], false),
            rect([256.0, 0.0], [768.0, 256.0], false),
            rect([0.0, 256.0], [64.0, 512.0], false),
        ];
        let mut side = side_facing(DVec3::Z);
        assert!(side.hotspot(&points, &hotspots, false, 7));
        let corners = [DVec3::new(0.0, 64.0, 32.0), DVec3::new(128.0, 0.0, 32.0)].map(|point| texel(&side, point));
        assert!(
            (corners[0][0] - 256.0).abs() < 1e-6 && corners[0][1].abs() < 1e-6,
            "the top left corner lands on the wide rectangle's corner: {corners:?}"
        );
        assert!(
            (corners[1][0] - 768.0).abs() < 1e-6 && (corners[1][1] - 256.0).abs() < 1e-6,
            "the bottom right corner lands on the opposite corner: {corners:?}"
        );

        let tall = [rect([0.0, 0.0], [256.0, 512.0], true)];
        let mut side = side_facing(DVec3::Z);
        assert!(side.hotspot(&points, &tall, false, 0));
        assert_eq!(
            side.rotation,
            Some(90.0),
            "the tall rectangle is turned to suit the wide face"
        );
        let spans = [side.uaxis, side.vaxis].map(|axis| {
            let (min, max) = extent(axis.axis, &points);
            (max - min) / axis.scale
        });
        assert!(
            (spans[0] - 256.0).abs() < 1e-6 && (spans[1] - 512.0).abs() < 1e-6,
            "{spans:?}"
        );
    }

    #[test]
    fn hotspot_alternates_and_flat_faces() {
        let base = Hotspot {
            min: [0.0, 0.0],
            max: [512.0, 256.0],
            rotate: false,
            reflect: false,
            alt: false,
        };
        let alternate = Hotspot {
            min: [0.0, 256.0],
            max: [64.0, 320.0],
            alt: true,
            ..base
        };
        let points = [
            DVec3::new(0.0, 0.0, 32.0),
            DVec3::new(128.0, 0.0, 32.0),
            DVec3::new(128.0, 64.0, 32.0),
            DVec3::new(0.0, 64.0, 32.0),
        ];
        let top = DVec3::new(0.0, 64.0, 32.0);
        let corner = |side: &Side| top.dot(side.vaxis.axis) / side.vaxis.scale + side.vaxis.offset;
        let mut side = side_facing(DVec3::Z);
        assert!(side.hotspot(&points, &[base, alternate], true, 3));
        assert!((corner(&side) - 256.0).abs() < 1e-6, "only the alternate is used");
        let small = DVec3::new(16.0, 16.0, 0.0);
        let square = [
            points[0],
            points[0] + small * DVec3::X,
            points[0] + small,
            points[0] + small * DVec3::Y,
        ];
        let mut side = side_facing(DVec3::Z);
        assert!(side.hotspot(&square, &[base, alternate], false, 3));
        let top = square[3];
        let texel = top.dot(side.vaxis.axis) / side.vaxis.scale + side.vaxis.offset;
        assert!(
            texel.abs() < 1e-6,
            "without alternates the alternate is skipped, though it fits better"
        );
        let line = [DVec3::ZERO, DVec3::X * 64.0];
        assert!(
            !side.hotspot(&line, &[base], false, 0),
            "a face with no area is left alone"
        );
    }
}
