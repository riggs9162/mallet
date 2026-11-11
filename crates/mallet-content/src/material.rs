//! Resolves material names to the texture Hammer shows for them and how it uses alpha.

use std::collections::HashMap;
use std::sync::Arc;

use mallet_kv::{Block, Item};

use crate::filesystem::GameFile;
use crate::vpk::normalize;
use crate::vtf::{self, Image, Mips};
use crate::GameFs;

/// Largest side, in pixels, of the textures viewports show.
const MAX_TEXTURE_SIDE: u32 = 512;
/// How many `patch` materials may include each other before giving up.
const MAX_PATCH_DEPTH: usize = 8;
/// Alpha below which `$alphatest` materials are cut out when they set no reference.
const DEFAULT_ALPHA_TEST_REFERENCE: f32 = 0.5;

/// A texture ready for display. Shared, because many faces use the same one.
pub type Texture = Arc<Image>;

/// How a material's texture alpha is used.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AlphaMode {
    /// Alpha is ignored.
    Opaque,
    /// Pixels with alpha below the reference are cut out (`$alphatest`).
    Test(f32),
    /// Blended over what is behind, with the texture's alpha times this opacity
    /// (`$translucent`, or `$alpha` below 1).
    Blend(f32),
    /// Added onto what is behind (`$additive`), so black is clear and bright parts glow,
    /// scaled by `opacity` and, when `texture_alpha` is set (`$translucent` as well), by
    /// the texture's alpha.
    Add { opacity: f32, texture_alpha: bool },
}

impl AlphaMode {
    /// Whether the material lets what is behind it show through, blended or added, so
    /// its faces are drawn after the rest, back to front, without hiding what follows.
    pub fn see_through(self) -> bool {
        matches!(self, Self::Blend(_) | Self::Add { .. })
    }
}

/// What a material's faces are to the compilers, from its `%compile` keys.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Surface {
    /// An ordinary surface.
    #[default]
    Normal,
    /// Sky (`%compilesky` or `%compile2Dsky`): the skybox shows through it and the sun
    /// shines in through it.
    Sky,
    /// Never drawn but solid, so it still blocks light: `%compilenodraw` and
    /// `%compileblocklight`.
    Nodraw,
    /// A tool face the compilers remove, invisible and letting light through: clip, hint,
    /// skip, trigger, origin, areaportal, occluder, ladder and the like.
    Tool,
}

/// `%compile` keys of tool materials whose faces the compilers remove.
const TOOL_KEYS: [&str; 14] = [
    "%compileclip",
    "%compilehint",
    "%compileskip",
    "%compiletrigger",
    "%compileorigin",
    "%compileareaportal",
    "%compileoccluder",
    "%compilenpcclip",
    "%playerclip",
    "%compileplayerclip",
    "%compileladder",
    "%compileinvisible",
    "%compilefog",
    "%compileblocklos",
];

/// Which way a sprite faces, from `$spriteorientation`, numbered as the game numbers them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SpriteOrientation {
    /// `parallel_upright`: stands straight up and turns about its vertical axis to face
    /// the way the camera looks. The game's default.
    #[default]
    ParallelUpright,
    /// `facing_upright`: stands straight up and turns about its vertical axis.
    FacingUpright,
    /// `vp_parallel`: always parallel to the screen.
    Parallel,
    /// `oriented`: fixed in the world by the entity's angles.
    Oriented,
    /// `vp_parallel_oriented`: parallel to the screen, turned on it by the entity's roll.
    ParallelOriented,
}

impl SpriteOrientation {
    fn from_number(number: i64) -> Self {
        match number {
            1 => Self::FacingUpright,
            2 => Self::Parallel,
            3 => Self::Oriented,
            4 => Self::ParallelOriented,
            _ => Self::ParallelUpright,
        }
    }
}

/// How the game draws a material as the sprite of an env_sprite, from its VMT.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpriteLook {
    /// Drawn by the `Sprite` shader, which blends as the entity's render mode says. Other
    /// shaders, such as `UnlitGeneric`, blend as the material itself says.
    pub sprite_shader: bool,
    pub orientation: SpriteOrientation,
    /// `$spriteorigin`: where the entity's origin lies on the sprite, as a share of its
    /// width from the left and of its height from the top.
    pub origin: [f32; 2],
    /// `$nosrgb`, on unless a `Sprite` material turns it off: the texture is read, tinted
    /// and blended in gamma space, as stored, rather than in linear light.
    pub gamma: bool,
    /// `$ignorevertexcolors`: the Additive render modes leave out the entity's colour.
    pub ignore_vertex_colors: bool,
    /// `$color` and `$alpha`, which tint the Additive render modes.
    pub color: [f32; 3],
    pub alpha: f32,
    /// `$ignorez`: drawn in front of everything.
    pub ignore_z: bool,
    /// `$vertexcolor` and `$vertexalpha`, with which other shaders take the entity's
    /// colour and opacity.
    pub vertex_color: bool,
    pub vertex_alpha: bool,
}

impl Default for SpriteLook {
    fn default() -> Self {
        Self {
            sprite_shader: false,
            orientation: SpriteOrientation::ParallelUpright,
            origin: [0.5, 0.5],
            gamma: false,
            ignore_vertex_colors: true,
            color: [1.0; 3],
            alpha: 1.0,
            ignore_z: false,
            vertex_color: false,
            vertex_alpha: false,
        }
    }
}

/// What the editor needs to draw a material.
#[derive(Debug, Clone)]
pub struct Material {
    /// The texture's pixels, when they were loaded: [`MaterialLoader::load`] loads them,
    /// [`MaterialLoader::measure`] and the cache of [`Materials`] do not. `None` too when
    /// the material or its texture cannot be found or decoded.
    pub texture: Option<Texture>,
    /// Width and height of the full-size texture, used for texture coordinates; `None`
    /// when the material or its texture cannot be found.
    pub size: Option<[u32; 2]>,
    pub alpha: AlphaMode,
    pub surface: Surface,
    /// For `$selfillum` materials, the glow's tint (`$selfillumtint`); the texture's
    /// alpha says where it glows.
    pub self_illum: Option<[f32; 3]>,
    /// Drawn without lighting, as `UnlitGeneric` and sky materials are.
    pub unlit: bool,
    /// World units per texel of an `infodecal` that uses the material (`$decalscale`).
    pub decal_scale: f32,
    /// How the material looks as a sprite.
    pub sprite: SpriteLook,
}

impl Material {
    fn missing() -> Self {
        Self {
            texture: None,
            size: None,
            alpha: AlphaMode::Opaque,
            surface: Surface::Normal,
            self_illum: None,
            unlit: false,
            decal_scale: 1.0,
            sprite: SpriteLook::default(),
        }
    }
}

/// Loads materials from a game's files, and caches what each one is and how large its
/// texture is. Pixels are not kept: a map can use a thousand materials, and the viewports
/// keep their own copies on the graphics card.
#[derive(Debug)]
pub struct Materials {
    loader: MaterialLoader,
    cache: HashMap<String, Material>,
    rectangles: HashMap<String, Option<Arc<str>>>,
}

/// Loads materials without caching. Cheap to clone and safe to send to worker threads,
/// so textures can stream in the background.
#[derive(Debug, Clone)]
pub struct MaterialLoader {
    filesystem: Arc<GameFs>,
}

/// Material keys that matter to the editor, collected from a VMT and the materials it
/// patches. `None` means the key was not set.
#[derive(Debug, Default, Clone)]
struct Properties {
    texture: Option<String>,
    translucent: Option<bool>,
    additive: Option<bool>,
    alpha_test: Option<bool>,
    alpha_test_reference: Option<f32>,
    alpha: Option<f32>,
    decal_scale: Option<f32>,
    self_illum: Option<bool>,
    self_illum_tint: Option<[f32; 3]>,
    envmap: Option<bool>,
    keywords: Option<String>,
    surface: Option<Surface>,
    /// The shader the material uses, lower case, such as `lightmappedgeneric`.
    shader: Option<String>,
    /// The hotspot `.rect` file (`%rectanglemap`), relative to `materials/` and without
    /// its extension.
    rectangle_map: Option<String>,
    sprite: SpriteKeys,
}

/// The VMT keys that say how a material looks as a sprite. `None` means the key was not
/// set.
#[derive(Debug, Default, Clone)]
struct SpriteKeys {
    orientation: Option<String>,
    /// `$spriteorigin` as written, which counts only when it is a `[x y]` vector.
    origin: Option<String>,
    no_srgb: Option<bool>,
    ignore_vertex_colors: Option<bool>,
    color: Option<[f32; 3]>,
    ignore_z: Option<bool>,
    vertex_color: Option<bool>,
    vertex_alpha: Option<bool>,
}

impl SpriteKeys {
    fn read(block: &Block) -> Self {
        let flag = |key: &str| {
            block
                .get(key)
                .and_then(|value| value.trim().parse::<f32>().ok())
                .map(|value| value != 0.0)
        };
        Self {
            orientation: block
                .get("$spriteorientation")
                .map(|value| value.trim().to_ascii_lowercase()),
            origin: block.get("$spriteorigin").map(str::to_owned),
            no_srgb: flag("$nosrgb"),
            ignore_vertex_colors: flag("$ignorevertexcolors"),
            color: block.get("$color").and_then(material_color),
            ignore_z: flag("$ignorez"),
            vertex_color: flag("$vertexcolor"),
            vertex_alpha: flag("$vertexalpha"),
        }
    }

    fn overridden_by(self, other: Self) -> Self {
        Self {
            orientation: other.orientation.or(self.orientation),
            origin: other.origin.or(self.origin),
            no_srgb: other.no_srgb.or(self.no_srgb),
            ignore_vertex_colors: other.ignore_vertex_colors.or(self.ignore_vertex_colors),
            color: other.color.or(self.color),
            ignore_z: other.ignore_z.or(self.ignore_z),
            vertex_color: other.vertex_color.or(self.vertex_color),
            vertex_alpha: other.vertex_alpha.or(self.vertex_alpha),
        }
    }

    /// The look the game gives these keys. The `Sprite` shader turns the orientation's
    /// name into its number and defaults `$spriteorigin` to the top left corner, as its
    /// parameter's default is `[0 0 0]`; under other shaders the orientation is read as a
    /// number, so a name counts as 0, and the origin defaults to the centre.
    fn look(&self, sprite_shader: bool, alpha: Option<f32>) -> SpriteLook {
        let orientation = match self.orientation.as_deref() {
            None => SpriteOrientation::ParallelUpright,
            Some(name) if sprite_shader => match name {
                "facing_upright" => SpriteOrientation::FacingUpright,
                "vp_parallel" => SpriteOrientation::Parallel,
                "oriented" => SpriteOrientation::Oriented,
                "vp_parallel_oriented" => SpriteOrientation::ParallelOriented,
                _ => SpriteOrientation::ParallelUpright,
            },
            Some(number) => SpriteOrientation::from_number(leading_integer(number)),
        };
        let vector = self.origin.as_deref().and_then(|text| {
            let text = text.trim();
            if !text.starts_with('[') && !text.starts_with('{') {
                return None;
            }
            let values: Vec<f32> = text
                .split(|c: char| c.is_whitespace() || matches!(c, '[' | ']' | '{' | '}'))
                .filter(|part| !part.is_empty())
                .map(|part| part.parse::<f32>().unwrap_or(0.0))
                .collect();
            Some([
                values.first().copied().unwrap_or(0.0),
                values.get(1).copied().unwrap_or(0.0),
            ])
        });
        let origin = match (vector, self.origin.is_some()) {
            (Some(origin), _) => origin,
            (None, false) if sprite_shader => [0.0, 0.0],
            (None, _) => [0.5, 0.5],
        };
        SpriteLook {
            sprite_shader,
            orientation,
            origin,
            gamma: sprite_shader && self.no_srgb.unwrap_or(true),
            ignore_vertex_colors: self.ignore_vertex_colors.unwrap_or(true),
            color: self.color.unwrap_or([1.0; 3]),
            alpha: alpha.unwrap_or(1.0).clamp(0.0, 1.0),
            ignore_z: self.ignore_z.unwrap_or(false),
            vertex_color: self.vertex_color.unwrap_or(false),
            vertex_alpha: self.vertex_alpha.unwrap_or(false),
        }
    }
}

/// The whole number a material value starts with, as the game reads a number from text:
/// 0 when it starts with none.
fn leading_integer(text: &str) -> i64 {
    let text = text.trim_start();
    let sign = usize::from(text.starts_with(['-', '+']));
    let digits = text[sign..]
        .find(|c: char| !c.is_ascii_digit())
        .map_or(text.len(), |end| end + sign);
    text[..digits].parse().unwrap_or(0)
}

/// What the texture browser's filters need to know about a material.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MaterialInfo {
    /// Blended or alpha-tested.
    pub translucent: bool,
    pub self_illum: bool,
    /// Has an `$envmap`, so it reflects.
    pub envmap: bool,
    /// The VMT's `%keywords`, for searching.
    pub keywords: String,
    /// The VMT cannot be read or its texture is missing.
    pub error: bool,
}

impl Properties {
    fn read(block: &Block) -> Self {
        let number = |key: &str| block.get(key).and_then(|value| value.trim().parse::<f32>().ok());
        let flag = |key: &str| number(key).map(|value| value != 0.0);
        Self {
            texture: block
                .get("%tooltexture")
                .or_else(|| block.get("$basetexture"))
                .map(|name| normalize(name).trim_end_matches(".vtf").to_owned()),
            translucent: flag("$translucent"),
            additive: flag("$additive"),
            alpha_test: flag("$alphatest"),
            alpha_test_reference: number("$alphatestreference"),
            alpha: number("$alpha"),
            decal_scale: number("$decalscale").filter(|scale| *scale > 0.0),
            self_illum: flag("$selfillum"),
            self_illum_tint: block.get("$selfillumtint").and_then(material_color),
            envmap: block.get("$envmap").map(|value| !value.trim().is_empty()),
            keywords: block.get("%keywords").map(str::to_owned),
            surface: Self::surface(block),
            shader: (!block.name.eq_ignore_ascii_case("patch")).then(|| block.name.to_ascii_lowercase()),
            rectangle_map: block
                .get("%rectanglemap")
                .map(|name| normalize(name).trim_end_matches(".rect").to_owned())
                .filter(|name| !name.is_empty()),
            sprite: SpriteKeys::read(block),
        }
    }

    /// What the `%compile` keys make of the material's faces, if any is set.
    fn surface(block: &Block) -> Option<Surface> {
        let set = |key: &str| {
            block
                .get(key)
                .and_then(|value| value.trim().parse::<f32>().ok())
                .is_some_and(|value| value != 0.0)
        };
        if set("%compilesky") || set("%compile2dsky") {
            Some(Surface::Sky)
        } else if set("%compilenodraw") || set("%compileblocklight") {
            Some(Surface::Nodraw)
        } else if TOOL_KEYS.iter().any(|key| set(key)) {
            Some(Surface::Tool)
        } else {
            None
        }
    }

    /// Takes every key `other` sets, keeping the rest.
    fn overridden_by(self, other: Self) -> Self {
        Self {
            texture: other.texture.or(self.texture),
            translucent: other.translucent.or(self.translucent),
            additive: other.additive.or(self.additive),
            alpha_test: other.alpha_test.or(self.alpha_test),
            alpha_test_reference: other.alpha_test_reference.or(self.alpha_test_reference),
            alpha: other.alpha.or(self.alpha),
            decal_scale: other.decal_scale.or(self.decal_scale),
            self_illum: other.self_illum.or(self.self_illum),
            self_illum_tint: other.self_illum_tint.or(self.self_illum_tint),
            envmap: other.envmap.or(self.envmap),
            keywords: other.keywords.or(self.keywords),
            surface: other.surface.or(self.surface),
            shader: other.shader.or(self.shader),
            rectangle_map: other.rectangle_map.or(self.rectangle_map),
            sprite: self.sprite.overridden_by(other.sprite),
        }
    }

    /// Whether the material darkens and lightens what is under it instead of covering
    /// it, as the `DecalModulate` shader of stains, cracks and scorch marks does.
    fn modulates(&self) -> bool {
        self.shader.as_deref() == Some("decalmodulate")
    }

    fn alpha_mode(&self) -> AlphaMode {
        let opacity = self.alpha.unwrap_or(1.0).clamp(0.0, 1.0);
        if self.additive == Some(true) && !self.modulates() {
            AlphaMode::Add {
                opacity,
                texture_alpha: self.translucent == Some(true),
            }
        } else if self.translucent == Some(true) || opacity < 1.0 || self.modulates() {
            AlphaMode::Blend(opacity)
        } else if self.alpha_test == Some(true) {
            AlphaMode::Test(self.alpha_test_reference.unwrap_or(DEFAULT_ALPHA_TEST_REFERENCE))
        } else {
            AlphaMode::Opaque
        }
    }

    /// What the editor needs to draw the material, without its texture.
    fn material(&self) -> Material {
        let unlit = self
            .shader
            .as_deref()
            .is_some_and(|shader| matches!(shader, "unlitgeneric" | "unlittwotexture" | "sky" | "sprite"));
        Material {
            texture: None,
            size: None,
            alpha: self.alpha_mode(),
            surface: self.surface.unwrap_or_default(),
            self_illum: self
                .self_illum
                .unwrap_or(false)
                .then(|| self.self_illum_tint.unwrap_or([1.0; 3])),
            unlit,
            decal_scale: self.decal_scale.unwrap_or(1.0),
            sprite: self.sprite.look(self.shader.as_deref() == Some("sprite"), self.alpha),
        }
    }

    /// Whether the material is drawn as a sprite of the game's own: by the `Sprite`
    /// shader, or kept with the sprites under `materials/sprites/`. Such materials have
    /// their animation frames loaded.
    fn animated_sprite(&self, key: &str) -> bool {
        self.shader.as_deref() == Some("sprite") || key.starts_with("sprites/")
    }
}

/// Rewrites the pixels of a modulating decal so that blending them by alpha looks like
/// the game multiplying the surface by twice their colour: mid grey becomes clear, darker
/// pixels become a dark tint as opaque as they are dark, and lighter pixels a white
/// haze. A blend cannot multiply, so the tint is the one that is right over a mid grey
/// surface.
fn modulate_to_blend(rgba: &mut [u8]) {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let color = [pixel[0], pixel[1], pixel[2]].map(|channel| f32::from(channel) / 255.0);
        let darkest = color.into_iter().fold(1.0, f32::min);
        let lightest = color.into_iter().fold(0.0, f32::max);
        let (tint, alpha) = if darkest < 0.5 {
            let alpha = 1.0 - 2.0 * darkest;
            (
                color.map(|channel| ((channel - darkest) / alpha).clamp(0.0, 1.0)),
                alpha,
            )
        } else {
            ([1.0; 3], (2.0 * lightest - 1.0).clamp(0.0, 1.0))
        };
        for (channel, value) in pixel.iter_mut().zip(tint) {
            *channel = (value * 255.0).round() as u8;
        }
        pixel[3] = (alpha * 255.0).round() as u8;
    }
}

impl Materials {
    pub fn new(filesystem: GameFs) -> Self {
        Self {
            loader: MaterialLoader {
                filesystem: Arc::new(filesystem),
            },
            cache: HashMap::new(),
            rectangles: HashMap::new(),
        }
    }

    pub fn filesystem(&self) -> &GameFs {
        &self.loader.filesystem
    }

    /// A loader for worker threads, sharing this game's mounted files.
    pub fn loader(&self) -> MaterialLoader {
        self.loader.clone()
    }

    /// The texture Hammer shows for a material such as `TOOLS/TOOLSNODRAW`, or `None`
    /// when the material or its texture cannot be found or decoded. It is read from the
    /// game's files every time.
    pub fn texture(&self, material: &str) -> Option<Texture> {
        self.loader.load(material).texture
    }

    /// Width and height of a material's full-size texture, or `None` when the material
    /// or its texture cannot be found. Results are cached.
    pub fn size(&mut self, material: &str) -> Option<[u32; 2]> {
        self.materials(&[material]).pop().and_then(|material| material.size)
    }

    /// The text of a hotspot material's `.rect` file, named by its `%rectanglemap`, or
    /// `None` when the material has none or the file is missing. Results are cached.
    pub fn rectangle_file(&mut self, material: &str) -> Option<Arc<str>> {
        let key = material_key(material);
        if let Some(cached) = self.rectangles.get(&key) {
            return cached.clone();
        }
        let text = self.loader.rectangle_file(&key).map(Arc::from);
        self.rectangles.insert(key, text.clone());
        text
    }

    /// Looks several materials up at once, spread over every CPU core, and returns them
    /// in the same order: what each is and its texture's size, without its pixels. Results
    /// are cached.
    pub fn materials(&mut self, materials: &[&str]) -> Vec<Material> {
        let keys: Vec<String> = materials.iter().map(|material| material_key(material)).collect();
        let mut missing: Vec<&String> = keys.iter().filter(|key| !self.cache.contains_key(*key)).collect();
        missing.sort();
        missing.dedup();
        let loaded = parallel_map(&missing, |key| self.loader.measure(key));
        for (key, material) in missing.into_iter().zip(loaded) {
            self.cache.insert(key.clone(), material);
        }
        keys.iter()
            .map(|key| self.cache.get(key).cloned().unwrap_or_else(Material::missing))
            .collect()
    }
}

impl MaterialLoader {
    /// Loads a studio model such as `models/props_c17/oildrum001.mdl`.
    pub fn model(&self, path: &str) -> Result<crate::model::Model, String> {
        crate::model::load_model(&self.filesystem, path)
    }

    /// Loads a model with the chosen skin and body groups, and says what the model is.
    pub fn model_variant(
        &self,
        path: &str,
        variant: crate::model::ModelVariant,
    ) -> Result<(crate::model::Model, crate::model::ModelDetails), String> {
        crate::model::load_model_variant(&self.filesystem, path, variant)
    }

    /// Every model in the game, such as `models/props_c17/oildrum001.mdl`, sorted, with
    /// every source that has it, as [`GameFs::sources`] lists them; the first is the one it
    /// loads from.
    pub fn list_models(&self) -> Vec<(String, Vec<usize>)> {
        self.filesystem.list_sourced("models/", ".mdl")
    }

    /// The models as [`MaterialLoader::list_models`] lists them, and those among them
    /// that have a collision model, as physics props need: a `.phy` file beside the
    /// `.mdl`. Both come from one pass over the game's files.
    pub fn list_models_and_collision(&self) -> (Vec<(String, Vec<usize>)>, std::collections::HashSet<String>) {
        let mut models = Vec::new();
        let mut collision = std::collections::HashSet::new();
        for (path, sources) in self.filesystem.list_sourced("models/", "") {
            if path.ends_with(".mdl") {
                models.push((path, sources));
            } else if let Some(stem) = path.strip_suffix(".phy") {
                collision.insert(format!("{stem}.mdl"));
            }
        }
        (models, collision)
    }

    /// Whether a model was compiled with `$staticprop`, as `prop_static` needs. `None`
    /// when its file cannot be read.
    pub fn model_is_static(&self, path: &str) -> Option<bool> {
        crate::model::is_static_prop(&self.filesystem, path)
    }

    /// Where a model's `.mdl` lies on disk, when it is a loose file.
    pub fn locate_model(&self, path: &str) -> Option<std::path::PathBuf> {
        self.filesystem.locate(path)
    }

    /// Loads a material like [`MaterialLoader::load`], with its texture at most
    /// `max_side` pixels across, for small pictures such as model thumbnails.
    pub fn load_small(&self, material: &str, max_side: u32) -> Material {
        let key = material_key(material);
        let Some(properties) = self.properties(&key, 0) else {
            return Material::missing();
        };
        let image = properties
            .texture
            .as_ref()
            .and_then(|name| self.texture_file(name))
            .and_then(|file| vtf::load(|start, length| file.read(start, length), max_side).ok());
        Material {
            size: image.as_ref().map(|image| [image.width, image.height]),
            texture: image.map(Arc::new),
            ..properties.material()
        }
    }

    /// Loads a material such as `TOOLS/TOOLSNODRAW`: its texture and alpha mode.
    pub fn load(&self, material: &str) -> Material {
        let key = material_key(material);
        let Some(properties) = self.properties(&key, 0) else {
            return Material::missing();
        };
        let image = self.image(&properties);
        Material {
            size: image.as_ref().map(|image| [image.width, image.height]),
            texture: image.map(Arc::new),
            ..properties.material()
        }
    }

    /// Looks a material up without loading its texture: what it is, and the texture's
    /// size from the header of its file.
    pub fn measure(&self, material: &str) -> Material {
        let key = material_key(material);
        let Some(properties) = self.properties(&key, 0) else {
            return Material::missing();
        };
        let size = properties
            .texture
            .as_ref()
            .and_then(|name| self.texture_file(name))
            .and_then(|file| vtf::size(|start, length| file.read(start, length)).ok());
        Material {
            size,
            ..properties.material()
        }
    }

    /// Loads a material for a graphics card: what it is, with no texture of its own, and
    /// its texture at every size a viewport shows. With `blocks`, DXT textures stay
    /// compressed as their files store them, for cards that sample them that way.
    pub fn load_mips(&self, material: &str, blocks: bool) -> (Material, Option<Mips>) {
        let key = material_key(material);
        let Some(properties) = self.properties(&key, 0) else {
            return (Material::missing(), None);
        };
        let mips = if properties.modulates() {
            self.image(&properties).map(Mips::from_image)
        } else {
            properties
                .texture
                .as_ref()
                .and_then(|name| self.texture_file(name))
                .and_then(|file| {
                    vtf::load_mips(|start, length| file.read(start, length), MAX_TEXTURE_SIDE, blocks).ok()
                })
        };
        let material = Material {
            size: mips.as_ref().map(|mips| [mips.width, mips.height]),
            ..properties.material()
        };
        (material, mips)
    }

    /// Animation frame `frame` of a sprite material's texture, as [`MaterialLoader::load_mips`]
    /// loads the first. `None` for a frame the texture does not have, and for materials
    /// that are not sprites, whose frames the editor never shows.
    pub fn load_frame_mips(&self, material: &str, frame: u32, blocks: bool) -> Option<Mips> {
        let key = material_key(material);
        let properties = self.properties(&key, 0)?;
        if !properties.animated_sprite(&key) {
            return None;
        }
        let file = self.texture_file(properties.texture.as_ref()?)?;
        vtf::load_frame_mips(
            |start, length| file.read(start, length),
            MAX_TEXTURE_SIDE,
            blocks,
            frame,
        )
        .ok()
    }

    /// A material's texture as the viewports show it, decoded.
    fn image(&self, properties: &Properties) -> Option<Image> {
        let file = self.texture_file(properties.texture.as_ref()?)?;
        let mut image = vtf::load(|start, length| file.read(start, length), MAX_TEXTURE_SIDE).ok()?;
        if properties.modulates() {
            modulate_to_blend(&mut image.rgba);
        }
        Some(image)
    }

    /// Opens a texture's VTF file, named without `materials/` or its extension.
    fn texture_file(&self, name: &str) -> Option<GameFile<'_>> {
        self.filesystem.open(&format!("materials/{name}.vtf"))
    }

    /// Loads a texture by its own name, such as `effects/flashlight001`, as projected
    /// textures and skyboxes name them, at most `max_side` pixels across.
    pub fn texture_image(&self, texture: &str, max_side: u32) -> Option<Image> {
        let name = normalize(texture);
        let name = name.strip_prefix("materials/").unwrap_or(&name);
        let name = name.strip_suffix(".vtf").unwrap_or(name);
        let file = self.texture_file(name)?;
        vtf::load(|start, length| file.read(start, length), max_side).ok()
    }

    /// Loads a material's texture at most `max_side` pixels across, for thumbnails.
    pub fn thumbnail(&self, material: &str, max_side: u32) -> Option<Image> {
        let key = material_key(material);
        let name = self.properties(&key, 0)?.texture?;
        let file = self.texture_file(&name)?;
        vtf::load(|start, length| file.read(start, length), max_side).ok()
    }

    /// Every material in the game, such as `brick/brickwall001a`: the `.vmt` files under
    /// `materials/`, sorted.
    pub fn list(&self) -> Vec<String> {
        self.list_sourced().into_iter().map(|(name, _)| name).collect()
    }

    /// Like [`MaterialLoader::list`], with every source that has each material, as
    /// [`GameFs::sources`] lists them; the first is the one it loads from.
    pub fn list_sourced(&self) -> Vec<(String, Vec<usize>)> {
        self.filesystem
            .list_sourced("materials/", ".vmt")
            .into_iter()
            .filter_map(|(path, sources)| {
                let name = path.strip_prefix("materials/")?.strip_suffix(".vmt")?;
                Some((name.to_owned(), sources))
            })
            .collect()
    }

    /// The mounted sources of the game's files.
    pub fn sources(&self) -> Vec<crate::Source> {
        self.filesystem.sources().to_vec()
    }

    /// Where a material's VMT lies on disk, when it is a loose file.
    pub fn locate(&self, material: &str) -> Option<std::path::PathBuf> {
        self.filesystem
            .locate(&format!("materials/{}.vmt", material_key(material)))
    }

    /// A material's VMT text, from a loose file or a VPK.
    pub fn source_text(&self, material: &str) -> Option<String> {
        let data = self
            .filesystem
            .read(&format!("materials/{}.vmt", material_key(material)))?;
        Some(String::from_utf8_lossy(&data).into_owned())
    }

    /// The text of a hotspot material's `.rect` file, as [`Materials::rectangle_file`]
    /// reads it, without caching.
    pub fn rectangle_file(&self, material: &str) -> Option<String> {
        let name = self.properties(&material_key(material), 0)?.rectangle_map?;
        let data = self.filesystem.read(&format!("materials/{name}.rect"))?;
        Some(String::from_utf8_lossy(&data).into_owned())
    }

    /// What the texture browser's filters need to know about a material.
    pub fn describe(&self, material: &str) -> MaterialInfo {
        let key = material_key(material);
        let Some(properties) = self.properties(&key, 0) else {
            return MaterialInfo {
                error: true,
                ..MaterialInfo::default()
            };
        };
        let texture_exists = properties
            .texture
            .as_ref()
            .is_some_and(|name| self.filesystem.exists(&format!("materials/{name}.vtf")));
        MaterialInfo {
            translucent: !matches!(properties.alpha_mode(), AlphaMode::Opaque),
            self_illum: properties.self_illum.unwrap_or(false),
            envmap: properties.envmap.unwrap_or(false),
            keywords: properties.keywords.unwrap_or_default(),
            error: !texture_exists,
        }
    }

    /// Reads a material's properties. The texture is `%tooltexture` if set, as Hammer
    /// shows for tool materials, otherwise `$basetexture`. A `patch` material takes the
    /// properties of the material it includes and applies its `insert` and `replace`
    /// blocks on top.
    fn properties(&self, key: &str, depth: usize) -> Option<Properties> {
        let data = self.filesystem.read(&format!("materials/{key}.vmt"))?;
        let text = String::from_utf8_lossy(&data);
        let items = mallet_kv::parse(&text).ok()?;
        let root = items.iter().find_map(|item| match item {
            Item::Block(block) => Some(block),
            Item::Pair(_) => None,
        })?;
        if !root.name.eq_ignore_ascii_case("patch") {
            return Some(Properties::read(root));
        }
        let base = match root.get("include") {
            Some(include) if depth < MAX_PATCH_DEPTH => {
                self.properties(&material_key(include), depth + 1).unwrap_or_default()
            }
            _ => Properties::default(),
        };
        Some(
            ["insert", "replace"]
                .iter()
                .filter_map(|name| root.blocks_named(name).next())
                .map(Properties::read)
                .fold(base, Properties::overridden_by),
        )
    }
}

/// Applies `work` to every item using one thread per CPU core, keeping the input order.
pub fn parallel_map<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism().map_or(4, usize::from);
    let chunk = items.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|part| scope.spawn(|| part.iter().map(&work).collect::<Vec<R>>()))
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("texture worker panicked"))
            .collect()
    })
}

/// A material colour such as `[1 0.5 0.5]` or `{255 128 128}`: square brackets hold 0 to 1
/// values, braces 0 to 255 ones.
fn material_color(text: &str) -> Option<[f32; 3]> {
    let bytes = text.trim().starts_with('{');
    let values: Vec<f32> = text
        .split(|c: char| c.is_whitespace() || matches!(c, '[' | ']' | '{' | '}'))
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<f32>().ok())
        .collect::<Option<_>>()?;
    let scale = if bytes { 1.0 / 255.0 } else { 1.0 };
    match values.as_slice() {
        [grey] => Some([grey * scale; 3]),
        [r, g, b, ..] => Some([r * scale, g * scale, b * scale]),
        _ => None,
    }
}

/// The cache key for a material: lower case, forward slashes, without the `materials/`
/// prefix or `.vmt` extension.
fn material_key(material: &str) -> String {
    let key = normalize(material);
    let key = key.strip_prefix("materials/").unwrap_or(&key);
    key.strip_suffix(".vmt").unwrap_or(key).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_game_materials_and_makes_thumbnails() {
        let Some(dir) = std::env::var_os("MALLET_GAME_DIR") else {
            return;
        };
        let materials = Materials::new(GameFs::from_gameinfo(std::path::Path::new(&dir)).unwrap());
        let loader = materials.loader();
        let started = std::time::Instant::now();
        let list = loader.list();
        eprintln!("{} materials listed in {:?}", list.len(), started.elapsed());
        assert!(list.iter().any(|name| name == "tools/toolsnodraw"));
        let started = std::time::Instant::now();
        let thumbnail = loader.thumbnail("brick/brickwall001a", 128).unwrap();
        eprintln!(
            "thumbnail {}x{} in {:?}",
            thumbnail.pixel_width,
            thumbnail.pixel_height,
            started.elapsed()
        );
        assert!(thumbnail.pixel_width <= 128 && thumbnail.pixel_height <= 128);
    }
}
