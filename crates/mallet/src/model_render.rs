//! Draws a studio model to a picture on the CPU, for the model browser's thumbnails and
//! its preview. Background threads call it, so browsing never waits on the graphics card
//! and a thumbnail needs nothing the 3D views own.

use glam::{Vec2, Vec3};
use mallet_content::{AlphaMode, MaterialLoader, Model, ModelDetails, ModelVariant, Texture};

/// The camera's vertical field of view, in degrees.
const FIELD_OF_VIEW: f32 = 36.0;
/// How much room a model that fits the picture leaves around itself.
const FIT_MARGIN: f32 = 1.08;
/// The colour of a mesh whose material or texture is missing.
const UNTEXTURED: [f32; 3] = [0.72, 0.72, 0.76];
/// Texels a blended material must cover before they draw at all.
const FAINTEST_ALPHA: f32 = 0.02;

/// Where the camera orbits a model from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Orbit {
    /// Degrees around the model's up axis; 0 looks at its front.
    pub yaw: f32,
    /// Degrees above the horizon.
    pub pitch: f32,
    /// Distance in multiples of the one that fits the whole model in the picture.
    pub distance: f32,
}

impl Default for Orbit {
    fn default() -> Self {
        Self {
            yaw: 35.0,
            pitch: 18.0,
            distance: 1.0,
        }
    }
}

impl Orbit {
    /// The orbit kept within what the camera can show: any yaw, never over the poles,
    /// and no closer than a tenth of the fitting distance.
    pub fn clamped(self) -> Self {
        Self {
            yaw: self.yaw.rem_euclid(360.0),
            pitch: self.pitch.clamp(-89.0, 89.0),
            distance: self.distance.clamp(0.1, 6.0),
        }
    }
}

/// A finished picture: RGBA pixels with straight alpha, row by row from the top. Where
/// nothing was drawn it is clear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// How one mesh of a model is painted.
#[derive(Debug, Clone)]
pub struct MeshPaint {
    /// The material's name, as the model names it.
    pub material: String,
    pub texture: Option<Texture>,
    pub alpha: AlphaMode,
    /// The material or its texture could not be found.
    pub missing: bool,
}

/// A model with everything drawing it needs.
#[derive(Debug, Clone)]
pub struct LoadedModel {
    pub model: Model,
    pub details: ModelDetails,
    /// One entry per mesh of `model`.
    pub paints: Vec<MeshPaint>,
    /// The middle of the drawn geometry.
    center: Vec3,
    /// How far the drawn geometry reaches from `center`.
    radius: f32,
}

impl LoadedModel {
    /// Loads a model and the textures of its meshes, each at most `texture_side` pixels
    /// across.
    pub fn load(loader: &MaterialLoader, path: &str, variant: ModelVariant, texture_side: u32) -> Result<Self, String> {
        let (model, details) = loader.model_variant(path, variant)?;
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        let drawn = || {
            model
                .meshes
                .iter()
                .flat_map(|mesh| &mesh.indices)
                .filter_map(|index| model.vertices.get(*index as usize))
                .map(|vertex| Vec3::from_array(vertex.position))
        };
        for position in drawn() {
            min = min.min(position);
            max = max.max(position);
        }
        if !min.is_finite() || !max.is_finite() {
            return Err(format!("{path} has nothing to draw"));
        }
        let center = (min + max) * 0.5;
        let radius = drawn()
            .map(|position| position.distance(center))
            .fold(0.0, f32::max)
            .max(0.5);
        let paints = model
            .meshes
            .iter()
            .map(|mesh| {
                let material = loader.load_small(&mesh.material, texture_side);
                MeshPaint {
                    material: mesh.material.clone(),
                    missing: material.texture.is_none(),
                    texture: material.texture,
                    alpha: material.alpha,
                }
            })
            .collect();
        Ok(Self {
            model,
            details,
            paints,
            center,
            radius,
        })
    }

    /// The size of the drawn geometry's box: along the model's forward, left and up axes.
    pub fn size(&self) -> [f32; 3] {
        let model = &self.model;
        [0, 1, 2].map(|axis| model.max[axis] - model.min[axis])
    }
}

/// A corner of a triangle after the camera has looked at it.
#[derive(Debug, Clone, Copy)]
struct Corner {
    /// Sideways, upwards and into the picture, in world units from the camera.
    view: Vec3,
    normal: Vec3,
    uv: Vec2,
}

impl Corner {
    fn mix(self, other: Self, amount: f32) -> Self {
        Self {
            view: self.view.lerp(other.view, amount),
            normal: self.normal.lerp(other.normal, amount),
            uv: self.uv.lerp(other.uv, amount),
        }
    }
}

/// The picture while it is drawn, at the larger size that is averaged down afterwards.
struct Canvas {
    width: usize,
    height: usize,
    /// Colour times alpha, and alpha.
    color: Vec<[f32; 4]>,
    /// One over the distance of what was drawn last; 0 where nothing was.
    depth: Vec<f32>,
    /// Half the picture's size over the tangent of half the field of view.
    focal: f32,
    near: f32,
    light: Vec3,
}

/// Draws a loaded model from an orbiting camera. `samples` is how many times larger the
/// picture is drawn before it is averaged down, which smooths its edges; 2 is plenty.
pub fn render(model: &LoadedModel, orbit: Orbit, width: u32, height: u32, samples: u32) -> Picture {
    let orbit = orbit.clamped();
    let samples = samples.clamp(1, 4) as usize;
    let (width, height) = (width.max(1) as usize, height.max(1) as usize);
    let (yaw, pitch) = (orbit.yaw.to_radians(), orbit.pitch.to_radians());
    let outward = Vec3::new(pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), pitch.sin());
    let half_height = (FIELD_OF_VIEW * 0.5).to_radians();
    let aspect = width as f32 / height as f32;
    let narrowest = (half_height.tan() * aspect.min(1.0)).atan();
    let fitting = model.radius * FIT_MARGIN / narrowest.sin();
    let eye = model.center + outward * fitting * orbit.distance;
    let forward = -outward;
    let right = forward.cross(Vec3::Z).normalize_or(Vec3::Y);
    let up = right.cross(forward);

    let mut canvas = Canvas {
        width: width * samples,
        height: height * samples,
        color: vec![[0.0; 4]; width * samples * height * samples],
        depth: vec![0.0; width * samples * height * samples],
        focal: (height * samples) as f32 * 0.5 / half_height.tan(),
        near: (model.radius * 0.01).max(0.05),
        light: Vec3::new(0.4, -0.6, 0.7).normalize(),
    };
    let corners: Vec<Corner> = model
        .model
        .vertices
        .iter()
        .map(|vertex| {
            let offset = Vec3::from_array(vertex.position) - eye;
            let normal = Vec3::from_array(vertex.normal);
            Corner {
                view: Vec3::new(offset.dot(right), offset.dot(up), offset.dot(forward)),
                normal: Vec3::new(normal.dot(right), normal.dot(up), normal.dot(forward)),
                uv: Vec2::from_array(vertex.uv),
            }
        })
        .collect();

    let blended = |paint: &MeshPaint| matches!(paint.alpha, AlphaMode::Blend(_));
    for see_through in [false, true] {
        for (mesh, paint) in model.model.meshes.iter().zip(&model.paints) {
            if blended(paint) != see_through {
                continue;
            }
            for triangle in mesh.indices.as_chunks::<3>().0 {
                let corner = |index: u32| corners.get(index as usize).copied();
                if let (Some(a), Some(b), Some(c)) = (corner(triangle[0]), corner(triangle[1]), corner(triangle[2])) {
                    canvas.triangle([a, b, c], paint);
                }
            }
        }
    }
    canvas.finish(samples)
}

impl Canvas {
    /// Draws a triangle, cut where it passes behind the camera.
    fn triangle(&mut self, corners: [Corner; 3], paint: &MeshPaint) {
        let mut shape: Vec<Corner> = Vec::with_capacity(4);
        for index in 0..3 {
            let (from, to) = (corners[index], corners[(index + 1) % 3]);
            let (inside, next_inside) = (from.view.z >= self.near, to.view.z >= self.near);
            if inside {
                shape.push(from);
            }
            if inside != next_inside {
                shape.push(from.mix(to, (self.near - from.view.z) / (to.view.z - from.view.z)));
            }
        }
        for index in 1..shape.len().saturating_sub(1) {
            self.fill([shape[0], shape[index], shape[index + 1]], paint);
        }
    }

    /// Where a corner lands in the picture, in pixels.
    fn project(&self, corner: &Corner) -> Vec2 {
        let scale = self.focal / corner.view.z;
        Vec2::new(
            self.width as f32 * 0.5 + corner.view.x * scale,
            self.height as f32 * 0.5 - corner.view.y * scale,
        )
    }

    /// Fills a triangle that lies wholly in front of the camera.
    fn fill(&mut self, corners: [Corner; 3], paint: &MeshPaint) {
        let points = [
            self.project(&corners[0]),
            self.project(&corners[1]),
            self.project(&corners[2]),
        ];
        let area = (points[1] - points[0]).perp_dot(points[2] - points[0]);
        if area.abs() < 1e-6 {
            return;
        }
        let low = points[0].min(points[1]).min(points[2]);
        let high = points[0].max(points[1]).max(points[2]);
        let first_column = low.x.floor().max(0.0) as usize;
        let first_row = low.y.floor().max(0.0) as usize;
        let last_column = (high.x.ceil() as isize).min(self.width as isize - 1);
        let last_row = (high.y.ceil() as isize).min(self.height as isize - 1);
        if last_column < first_column as isize || last_row < first_row as isize {
            return;
        }
        let nearness = [0, 1, 2].map(|index| 1.0 / corners[index].view.z);
        let (cutoff, opacity, writes_depth) = match paint.alpha {
            AlphaMode::Opaque => (None, 1.0, true),
            AlphaMode::Test(reference) => (Some(reference), 1.0, true),
            AlphaMode::Blend(opacity) => (Some(FAINTEST_ALPHA), opacity, false),
        };
        for row in first_row..=last_row as usize {
            for column in first_column..=last_column as usize {
                let pixel = Vec2::new(column as f32 + 0.5, row as f32 + 0.5);
                let weights = [
                    (points[1] - pixel).perp_dot(points[2] - pixel) / area,
                    (points[2] - pixel).perp_dot(points[0] - pixel) / area,
                    (points[0] - pixel).perp_dot(points[1] - pixel) / area,
                ];
                if weights.iter().any(|weight| *weight < 0.0) {
                    continue;
                }
                let near = weights[0] * nearness[0] + weights[1] * nearness[1] + weights[2] * nearness[2];
                let slot = row * self.width + column;
                if near <= self.depth[slot] {
                    continue;
                }
                // Texture coordinates run evenly over the surface, not over the picture.
                let shares = [0, 1, 2].map(|index| weights[index] * nearness[index] / near);
                let uv = corners[0].uv * shares[0] + corners[1].uv * shares[1] + corners[2].uv * shares[2];
                let [red, green, blue, alpha] = match &paint.texture {
                    Some(texture) => sample(texture, uv),
                    None => [UNTEXTURED[0], UNTEXTURED[1], UNTEXTURED[2], 1.0],
                };
                let alpha = match paint.alpha {
                    AlphaMode::Opaque => 1.0,
                    AlphaMode::Test(_) => alpha,
                    AlphaMode::Blend(_) => alpha * opacity,
                };
                if cutoff.is_some_and(|cutoff| alpha < cutoff) {
                    continue;
                }
                let normal =
                    (corners[0].normal * shares[0] + corners[1].normal * shares[1] + corners[2].normal * shares[2])
                        .normalize_or_zero();
                let facing = normal.dot(-self.light) * 0.5 + 0.5;
                let light = 0.28 + 0.86 * facing * facing;
                let lit = [red * light, green * light, blue * light].map(|channel| channel.min(1.0));
                if writes_depth {
                    self.color[slot] = [lit[0], lit[1], lit[2], 1.0];
                    self.depth[slot] = near;
                } else {
                    let under = self.color[slot];
                    let keep = 1.0 - alpha;
                    self.color[slot] = [
                        lit[0] * alpha + under[0] * keep,
                        lit[1] * alpha + under[1] * keep,
                        lit[2] * alpha + under[2] * keep,
                        alpha + under[3] * keep,
                    ];
                }
            }
        }
    }

    /// Averages the drawing down by `samples` each way.
    fn finish(self, samples: usize) -> Picture {
        let (width, height) = (self.width / samples, self.height / samples);
        let mut rgba = Vec::with_capacity(width * height * 4);
        let count = (samples * samples) as f32;
        for row in 0..height {
            for column in 0..width {
                let mut sum = [0.0_f32; 4];
                for y in 0..samples {
                    for x in 0..samples {
                        let pixel = self.color[(row * samples + y) * self.width + column * samples + x];
                        for (total, channel) in sum.iter_mut().zip(pixel) {
                            *total += channel;
                        }
                    }
                }
                let alpha = sum[3] / count;
                for channel in &sum[..3] {
                    let straight = if alpha > 0.0 { channel / count / alpha } else { 0.0 };
                    rgba.push((straight.clamp(0.0, 1.0) * 255.0).round() as u8);
                }
                rgba.push((alpha.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
        }
        Picture {
            width: width as u32,
            height: height as u32,
            rgba,
        }
    }
}

/// A texture's colour and alpha at a texture coordinate, blended from the four texels
/// around it; the texture repeats.
fn sample(texture: &Texture, uv: Vec2) -> [f32; 4] {
    let (width, height) = (texture.pixel_width as usize, texture.pixel_height as usize);
    if width == 0 || height == 0 || texture.rgba.len() < width * height * 4 || !uv.is_finite() {
        return [UNTEXTURED[0], UNTEXTURED[1], UNTEXTURED[2], 1.0];
    }
    let x = uv.x.rem_euclid(1.0) * width as f32 - 0.5;
    let y = uv.y.rem_euclid(1.0) * height as f32 - 0.5;
    let (left, top) = (x.floor(), y.floor());
    let (across, down) = (x - left, y - top);
    let texel = |column: f32, row: f32| {
        let column = (column as isize).rem_euclid(width as isize) as usize;
        let row = (row as isize).rem_euclid(height as isize) as usize;
        let start = (row * width + column) * 4;
        [0, 1, 2, 3].map(|channel| f32::from(texture.rgba[start + channel]) / 255.0)
    };
    let corners = [
        texel(left, top),
        texel(left + 1.0, top),
        texel(left, top + 1.0),
        texel(left + 1.0, top + 1.0),
    ];
    [0, 1, 2, 3].map(|channel| {
        let upper = corners[0][channel] * (1.0 - across) + corners[1][channel] * across;
        let lower = corners[2][channel] * (1.0 - across) + corners[3][channel] * across;
        upper * (1.0 - down) + lower * down
    })
}
