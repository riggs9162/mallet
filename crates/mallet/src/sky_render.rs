//! Draws a map's 2D sky as seen from inside it, to a picture on the CPU, for the sky
//! browser's live preview: the six images of a skybox, or Garry's Mod's painted sky, with
//! the sun's glow. It matches how the 3D views draw the sky behind the map, so a sky looks
//! the same in the browser as it does once it is the map's.

use std::sync::OnceLock;

use glam::{DVec3, Vec3};
use mallet_content::{MaterialLoader, Texture};
use mallet_vmf::light::SkyPaint;

/// The suffixes of a skybox's six images, in the order [`SkyImages::faces`] keeps them.
pub const SUFFIXES: [&str; 6] = ["rt", "lf", "bk", "ft", "up", "dn"];
/// The sky name that shows Garry's Mod's painted sky rather than skybox images.
pub const PAINTED: &str = "painted";
/// The colour of a side whose image is missing, in linear light.
const MISSING: Vec3 = Vec3::new(0.02, 0.02, 0.025);

/// Where the preview camera looks from the middle of the sky.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    /// Degrees around the up axis; 0 looks along +X, the skybox's `rt` side.
    pub yaw: f32,
    /// Degrees above the horizon.
    pub pitch: f32,
    /// The vertical field of view in degrees.
    pub fov: f32,
}

impl Default for Look {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            pitch: 10.0,
            fov: 75.0,
        }
    }
}

impl Look {
    /// The look kept within what the camera can show: any yaw, never over the poles,
    /// and a field of view between a telephoto and a fisheye.
    pub fn clamped(self) -> Self {
        Self {
            yaw: self.yaw.rem_euclid(360.0),
            pitch: self.pitch.clamp(-89.0, 89.0),
            fov: self.fov.clamp(20.0, 140.0),
        }
    }
}

/// One side of a skybox: its image, and whether only its upper half holds the picture,
/// as with skyboxes whose side images are twice as wide as they are tall.
#[derive(Clone)]
pub struct Side {
    pub image: Texture,
    pub upper_half: bool,
}

/// The six images of a skybox, as [`SUFFIXES`] orders them.
#[derive(Clone, Default)]
pub struct SkyImages {
    pub faces: [Option<Side>; 6],
}

impl SkyImages {
    /// Loads skybox `name`'s six images, each at most `max_side` pixels across. A side
    /// that cannot be found is left out and drawn dark.
    pub fn load(loader: &MaterialLoader, name: &str, max_side: u32) -> Self {
        let faces = SUFFIXES.map(|suffix| {
            let image = loader.load_small(&format!("skybox/{name}{suffix}"), max_side).texture?;
            let upper_half = image.pixel_width >= image.pixel_height * 2;
            Some(Side { image, upper_half })
        });
        Self { faces }
    }

    /// The suffixes of the sides with no image.
    pub fn missing(&self) -> Vec<&'static str> {
        SUFFIXES
            .iter()
            .zip(&self.faces)
            .filter(|(_, face)| face.is_none())
            .map(|(suffix, _)| *suffix)
            .collect()
    }

    /// The width and height of the largest side's image.
    pub fn resolution(&self) -> Option<[u32; 2]> {
        self.faces
            .iter()
            .flatten()
            .map(|side| [side.image.pixel_width, side.image.pixel_height])
            .max_by_key(|[width, height]| width * height)
    }

    /// The sky's colour in `direction`, in linear light.
    fn sample(&self, direction: Vec3) -> Vec3 {
        let size = direction.abs();
        let (layer, s, t) = if size.x >= size.y && size.x >= size.z {
            let t = direction.z / size.x;
            if direction.x > 0.0 {
                (0, -direction.y / size.x, t)
            } else {
                (1, direction.y / size.x, t)
            }
        } else if size.y >= size.z {
            let t = direction.z / size.y;
            if direction.y > 0.0 {
                (2, direction.x / size.y, t)
            } else {
                (3, -direction.x / size.y, t)
            }
        } else {
            let s = -direction.y / size.z;
            if direction.z > 0.0 {
                (4, s, -direction.x / size.z)
            } else {
                (5, s, direction.x / size.z)
            }
        };
        let Some(side) = &self.faces[layer] else {
            return MISSING;
        };
        let u = (s + 1.0) * 0.5;
        let mut v = 1.0 - (t + 1.0) * 0.5;
        if side.upper_half {
            v = (v * 2.0).min(1.0);
        }
        bilinear(&side.image, u, v)
    }
}

/// What the preview shows: skybox images or a painted sky, with or without a sun.
pub struct SkyView<'a> {
    pub images: Option<&'a SkyImages>,
    /// The painted sky, drawn when there are no images.
    pub paint: Option<&'a SkyPaint>,
    /// The sun's glow: the direction from the ground towards it, its linear colour and
    /// its size in degrees.
    pub sun: Option<(Vec3, Vec3, f32)>,
}

/// Garry's Mod's painted sky as `env_skypaint` sets it up when none of its keys are set.
pub fn default_paint() -> SkyPaint {
    SkyPaint {
        top: DVec3::new(0.2, 0.5, 1.0),
        bottom: DVec3::new(0.8, 1.0, 1.0),
        fade_bias: 1.0,
        sun_normal: DVec3::new(0.4, 0.0, 0.01).normalize(),
        sun_color: DVec3::new(0.2, 0.1, 0.0),
        sun_size: 2.0,
        dusk_color: DVec3::new(1.0, 0.2, 0.0),
        dusk_scale: 1.0,
        dusk_intensity: 1.0,
    }
}

/// Draws `view` as seen with `look`, `width` by `height` pixels: sRGB pixels, opaque,
/// row by row from the top. Bands of rows are drawn on every core at once, so the
/// preview keeps up while it turns.
pub fn render(view: &SkyView<'_>, look: Look, width: u32, height: u32) -> Vec<u8> {
    let look = look.clamped();
    let (yaw, pitch) = (look.yaw.to_radians(), look.pitch.to_radians());
    let forward = Vec3::new(pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), pitch.sin());
    let right = Vec3::new(yaw.sin(), -yaw.cos(), 0.0);
    let up = right.cross(forward);
    let half_height = (look.fov.to_radians() * 0.5).tan();
    let half_width = half_height * width as f32 / height.max(1) as f32;
    let sun = view.sun.map(|(toward, color, size)| {
        let toward = toward.try_normalize().unwrap_or(Vec3::Z);
        (toward, color, size.clamp(0.1, 30.0).to_radians().cos())
    });
    let row_bytes = width as usize * 4;
    let mut pixels = vec![0u8; row_bytes * height as usize];
    let draw_row = |y: usize, row: &mut [u8]| {
        let along_up = (1.0 - (y as f32 + 0.5) / height as f32 * 2.0) * half_height;
        for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let along_right = ((x as f32 + 0.5) / width as f32 * 2.0 - 1.0) * half_width;
            let direction = (forward + right * along_right + up * along_up).normalize();
            let mut color = match (view.images, view.paint) {
                (Some(images), _) => images.sample(direction),
                (None, Some(paint)) => painted(paint, direction),
                (None, None) => MISSING,
            };
            if let Some((toward, glow, size)) = sun {
                color += glow * sun_glow(direction.dot(toward), size);
            }
            pixel.copy_from_slice(&[encode(color.x), encode(color.y), encode(color.z), 255]);
        }
    };
    if row_bytes == 0 {
        return pixels;
    }
    let cores = std::thread::available_parallelism().map_or(1, usize::from).max(1);
    let band_rows = (height as usize).div_ceil(cores).max(1);
    std::thread::scope(|scope| {
        for (band, rows) in pixels.chunks_mut(band_rows * row_bytes).enumerate() {
            let draw_row = &draw_row;
            scope.spawn(move || {
                for (offset, row) in rows.chunks_exact_mut(row_bytes).enumerate() {
                    draw_row(band * band_rows + offset, row);
                }
            });
        }
    });
    pixels
}

/// How bright the sun's disc and halo are `along` (the cosine of the angle) from its
/// middle, for a sun whose edge is at cosine `size`.
fn sun_glow(along: f32, size: f32) -> f32 {
    let core = smoothstep(size, size + (1.0 - size) * 0.3, along);
    let halo = along.max(0.0).powi(256) * 0.6;
    core + halo
}

/// The painted sky in `direction`: the bottom colour fading to the top one above the
/// horizon, with the dusk colour along it.
fn painted(paint: &SkyPaint, direction: Vec3) -> Vec3 {
    let top = paint.top.as_vec3();
    let bottom = paint.bottom.as_vec3();
    let fade = direction.z.clamp(0.0, 1.0).powf((paint.fade_bias as f32).max(0.01));
    let mut color = bottom.lerp(top, fade);
    let sharpness = (4.0 / (paint.dusk_scale as f32).max(0.05)).max(0.5);
    let horizon = (1.0 - direction.z.abs()).powf(sharpness);
    color += paint.dusk_color.as_vec3() * paint.dusk_intensity as f32 * horizon * 0.5;
    color
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The image's linear colour at `u`, `v` (0 to 1 across and down), blended between the
/// four nearest pixels and clamped at the edges.
fn bilinear(image: &Texture, u: f32, v: f32) -> Vec3 {
    let (width, height) = (image.pixel_width.max(1), image.pixel_height.max(1));
    let x = (u * width as f32 - 0.5).clamp(0.0, (width - 1) as f32);
    let y = (v * height as f32 - 0.5).clamp(0.0, (height - 1) as f32);
    let (x0, y0) = (x as u32, y as u32);
    let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let texel = |x: u32, y: u32| {
        let at = ((y * width + x) * 4) as usize;
        match image.rgba.get(at..at + 3) {
            Some(&[r, g, b]) => Vec3::new(decode(r), decode(g), decode(b)),
            _ => Vec3::ONE,
        }
    };
    let top = texel(x0, y0).lerp(texel(x1, y0), fx);
    let bottom = texel(x0, y1).lerp(texel(x1, y1), fx);
    top.lerp(bottom, fy)
}

/// An sRGB channel in linear light.
fn decode(channel: u8) -> f32 {
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|index| {
            let value = index as f32 / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        })
    })[usize::from(channel)]
}

/// A linear channel as sRGB, clamped to what a pixel holds.
fn encode(linear: f32) -> u8 {
    const STEPS: usize = 4096;
    static TABLE: OnceLock<Vec<u8>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        (0..=STEPS)
            .map(|index| {
                let value = index as f32 / STEPS as f32;
                let encoded = if value <= 0.003_130_8 {
                    value * 12.92
                } else {
                    1.055 * value.powf(1.0 / 2.4) - 0.055
                };
                (encoded * 255.0).round().clamp(0.0, 255.0) as u8
            })
            .collect()
    });
    table[(linear.clamp(0.0, 1.0) * STEPS as f32) as usize]
}
