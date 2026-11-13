//! Wireframe geometry prepared once per map change and shared by all four viewports,
//! plus hit testing for selection.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use eframe::egui::Color32;
use glam::{DMat3, DMat4, DVec2, DVec3};
use mallet_content::{parallel_map, EntityClass, GameData, Model};
use mallet_vmf::displacement::Surface;
use mallet_vmf::geometry::{faces_bounds, solid_faces, Bounds, Plane};
use mallet_vmf::edit::Touched;
use mallet_vmf::light::MapLighting;
use mallet_vmf::overlay::{self, Piece};
use mallet_vmf::sound::MapSounds;
use mallet_vmf::{parse_vec3, Entity, Side, Solid, TexAxis, Vmf};

use crate::view2d::Axis2D;

/// Colour used when a brush or entity has no editor colour.
const DEFAULT_COLOR: Color32 = Color32::from_rgb(220, 220, 220);

/// Half-size of the box drawn for point entities until FGD sizes are loaded.
const POINT_ENTITY_HALF_SIZE: f64 = 8.0;

/// The model the 3D view shows for an entity: its own `model` key when that is a studio
/// model, or else the editor model its FGD class names with `studio("...")`, such as
/// the player figure of `info_player_start`.
pub fn entity_model<'a>(entity: &'a Entity, class: Option<&'a EntityClass>) -> Option<&'a str> {
    entity
        .get("model")
        .filter(|path| path.to_ascii_lowercase().ends_with(".mdl"))
        .or_else(|| class?.studio.as_deref())
}

/// The sprite the 3D view shows for a point entity. Entities the game draws as sprites,
/// such as env_sprite, show their own `model` the way the game draws it; others show the
/// editor icon their FGD class names with `iconsprite("...")`, such as the bulb of
/// `light` or the speaker of `ambient_generic`.
pub fn entity_sprite(entity: &Entity, class: Option<&EntityClass>) -> Option<Sprite> {
    let game = class.map_or_else(
        || {
            matches!(
                entity.classname().to_ascii_lowercase().as_str(),
                "env_sprite" | "env_sprite_oriented" | "env_glow" | "env_sprite_clientside"
            )
        },
        |class| class.sprite_from_model,
    );
    if game {
        let material = entity.get("model").and_then(sprite_material);
        if let Some(material) = material {
            return Some(Sprite {
                material,
                game: Some(GameSprite::read(entity)),
                tint: [255; 3],
            });
        }
    }
    Some(Sprite {
        material: sprite_material(class?.icon.as_deref()?)?,
        game: None,
        tint: icon_tint(entity),
    })
}

/// The colour Hammer tints an editor icon with: a light's `_light` colour, so the bulb of
/// a `light` shows the colour picked for it, and white for everything else.
fn icon_tint(entity: &Entity) -> [u8; 3] {
    let numbers: Vec<f64> = entity
        .get("_light")
        .map(|value| value.split_whitespace().filter_map(|number| number.parse().ok()).collect())
        .unwrap_or_default();
    match numbers.as_slice() {
        [red, green, blue, ..] => [*red, *green, *blue].map(|channel| channel.clamp(0.0, 255.0) as u8),
        _ => [255; 3],
    }
}

/// The material a sprite path names, as the game finds it: `sprites/glow01.spr` and
/// `materials/sprites/glow01.vmt` are both `sprites/glow01`.
fn sprite_material(path: &str) -> Option<String> {
    let path = path.trim().replace('\\', "/").to_ascii_lowercase();
    let path = path.strip_prefix("materials/").unwrap_or(&path);
    let path = path
        .strip_suffix(".vmt")
        .or_else(|| path.strip_suffix(".spr"))
        .unwrap_or(path);
    (!path.is_empty()).then(|| path.to_owned())
}

/// A sprite drawn for a point entity in the 3D view.
#[derive(Clone, Debug, PartialEq)]
pub struct Sprite {
    /// The material, as a path such as `editor/light` or `sprites/glow01`.
    pub material: String,
    /// How the game draws it, for entities that are sprites in the game; `None` for an
    /// editor icon.
    pub game: Option<GameSprite>,
    /// The colour an editor icon is tinted with.
    pub tint: [u8; 3],
}

/// How the game draws an env_sprite, from the entity's keys, read as the game reads them
/// when the sprite spawns.
#[derive(Clone, Debug, PartialEq)]
pub struct GameSprite {
    /// `rendermode`: 0 Normal, 1 Color, 2 Texture, 3 Glow, 4 Solid, 5 Additive, 7 Additive
    /// Fractional Frame, 8 Alpha Add, 9 World Space Glow, 10 Don't Render.
    pub mode: u8,
    /// `renderfx`, such as 14 for Constant Glow or 1 to 4 for pulses.
    pub fx: u8,
    /// `rendercolor` and `renderamt`.
    pub color: [u8; 3],
    pub amount: u8,
    /// `scale`, from 0 to 64. The game draws 0 at full size.
    pub scale: f64,
    /// `framerate`, in frames a second; the sprite animates when it is not 0 and its
    /// texture has more than two frames.
    pub framerate: f64,
    /// Pitch, yaw and roll in degrees, after the game's spawn swaps them: an env_sprite
    /// turns a yaw without roll into roll, and an env_sprite_oriented turns half way
    /// round.
    pub angles: DVec3,
    /// `GlowProxySize`: the radius around the origin that must be in sight for a glow to
    /// show.
    pub proxy: f64,
    /// `HDRColorScale`, which brightens the sprite in HDR.
    pub hdr_scale: f64,
    /// Named, and without the Start On flag: it stays hidden until something shows it.
    pub starts_off: bool,
    /// The entity's id, which sets apart the timing of its pulses and flickers.
    pub seed: u32,
}

/// The largest `scale` and `GlowProxySize` the game sends to its clients.
const MAX_SPRITE_SCALE: f64 = 64.0;

impl GameSprite {
    fn read(entity: &Entity) -> Self {
        let number = |key: &str| {
            entity
                .get(key)
                .and_then(|value| value.trim().parse::<f64>().ok())
                .filter(|value| value.is_finite())
        };
        let byte = |key: &str, default: u8| number(key).map_or(default, |value| value.clamp(0.0, 255.0) as u8);
        let color = entity
            .get("rendercolor")
            .map(|text| {
                let mut parts = text.split_whitespace().map(|part| part.parse::<f64>().unwrap_or(0.0));
                std::array::from_fn(|_| parts.next().unwrap_or(255.0).clamp(0.0, 255.0) as u8)
            })
            .unwrap_or([255; 3]);
        let mut angles = entity
            .get("angles")
            .and_then(parse_vec3)
            .or_else(|| number("angle").map(|yaw| DVec3::new(0.0, yaw, 0.0)))
            .unwrap_or(DVec3::ZERO);
        if entity.classname().eq_ignore_ascii_case("env_sprite_oriented") {
            angles.y = (angles.y + 180.0).rem_euclid(360.0);
        } else if angles.y != 0.0 && angles.z == 0.0 {
            angles = DVec3::new(angles.x, 0.0, angles.y);
        }
        let named = entity.get("targetname").is_some_and(|name| !name.trim().is_empty());
        let flags = number("spawnflags").unwrap_or(0.0) as i64;
        Self {
            mode: byte("rendermode", 0),
            fx: byte("renderfx", 0),
            color,
            amount: byte("renderamt", 255),
            scale: number("scale").unwrap_or(0.0).clamp(0.0, MAX_SPRITE_SCALE),
            framerate: number("framerate").unwrap_or(0.0),
            angles,
            proxy: number("GlowProxySize").unwrap_or(2.0).clamp(0.0, MAX_SPRITE_SCALE),
            hdr_scale: number("HDRColorScale").unwrap_or(1.0),
            starts_off: named && flags & 1 == 0,
            seed: entity.id().unwrap_or(0),
        }
    }
}

/// The box of a point entity without an FGD size, relative to its origin, as the
/// Half-Life 2 FGD gives it:
/// the standing player hull for spawn points and NPCs, a small cube otherwise.
fn point_entity_box(classname: &str) -> (DVec3, DVec3) {
    let classname = classname.to_ascii_lowercase();
    if classname.starts_with("info_player_") || classname.starts_with("npc_") {
        (DVec3::new(-16.0, -16.0, 0.0), DVec3::new(16.0, 16.0, 72.0))
    } else {
        let half = DVec3::splat(POINT_ENTITY_HALF_SIZE);
        (-half, half)
    }
}

/// Edge coordinates are rounded to this many steps per unit when removing shared edges.
const EDGE_QUANTIZE: f64 = 100.0;

/// How far in front of and behind its faces a click still picks an overlay or decal.
const DECAL_PICK_DEPTH: f64 = 1.0;
/// The size of an `infodecal` whose texture is not known, in world units.
const DEFAULT_DECAL_SIZE: f64 = 64.0;
/// How many units nearer a face that squarely faces a point counts than one seen
/// edge-on, when looking for the face nearest to the point.
const FACING_BONUS: f64 = 0.01;
/// Faces must point this much the same way to carry one decal between them.
const DECAL_SAME_FACING: f64 = 0.99;

/// The size in world units of the picture each `infodecal` texture projects, by
/// [`decal_key`]; `None` for a material that has no texture.
pub type DecalSizes = HashMap<String, Option<DVec2>>;

/// The key of a material in [`DecalSizes`]: lower case with forward slashes.
pub fn decal_key(material: &str) -> String {
    material.trim().replace('\\', "/").to_ascii_lowercase()
}

/// What a mesh belongs to, and so what clicking it selects.
///
/// World brushes are selected one by one. Any part of an entity selects the whole entity,
/// like Hammer's default "groups" selection mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Owner {
    /// A world brush, by solid id.
    Solid(u32),
    /// A point or brush entity, by entity id.
    Entity(u32),
}

/// Half the side length, in pixels, of a selection handle.
pub const HANDLE_HALF_SIZE: f32 = 3.5;

/// An affine transform previewed on the selection while it is dragged: a move, a resize
/// from the handles, or a rotation, skew or gizmo drag.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Remap {
    pub matrix: DMat4,
}

impl Remap {
    /// Moves by `offset`.
    pub fn translate(offset: DVec3) -> Self {
        Self {
            matrix: DMat4::from_translation(offset),
        }
    }

    /// Stretches and shifts each axis so the box `from` lands on the box `to`. Along an
    /// axis where `from` is flat, points are only shifted.
    pub fn boxes(from: &Bounds, to: &Bounds) -> Self {
        let (from_size, to_size) = (from.size(), to.size());
        let scale = DVec3::select(
            from_size.cmpgt(DVec3::ZERO),
            to_size / from_size.max(DVec3::splat(f64::MIN_POSITIVE)),
            DVec3::ONE,
        );
        Self {
            matrix: DMat4::from_translation(to.min - from.min * scale) * DMat4::from_scale(scale),
        }
    }

    pub fn apply(&self, point: DVec3) -> DVec3 {
        self.matrix.transform_point3(point)
    }

    /// The same mapping as a matrix, to apply on top of a model's transform.
    pub fn matrix(&self) -> DMat4 {
        self.matrix
    }

    /// The box around a transformed box's eight corners.
    pub fn map_bounds(&self, bounds: &Bounds) -> Bounds {
        let mut mapped = Bounds::EMPTY;
        for corner in 0..8 {
            let pick = |bit: usize, axis: usize| {
                if corner & bit == 0 {
                    bounds.min[axis]
                } else {
                    bounds.max[axis]
                }
            };
            mapped.add_point(self.apply(DVec3::new(pick(1, 0), pick(2, 1), pick(4, 2))));
        }
        mapped
    }

    /// How far the transform moves the origin, which is the whole of a plain move.
    pub fn offset(&self) -> DVec3 {
        self.matrix.w_axis.truncate()
    }
}

/// Temporary shapes drawn on top of the map while a tool is in use.
#[derive(Clone, Debug, Default)]
pub struct Overlay {
    /// Where selected meshes are drawn while being moved or resized.
    pub transform: Option<Remap>,
    /// Whether letting go of the drag will clone the selection rather than move it, so
    /// the originals stay drawn where they are.
    pub cloning: bool,
    /// The block tool's box, waiting for Enter.
    pub block: Option<Bounds>,
    /// A selection rectangle being dragged in one 2D view, as two world corners.
    pub marquee: Option<(Axis2D, DVec2, DVec2)>,
    /// The box that gets resize handles in the 2D views: the selection or the pending block.
    pub handles: Option<Bounds>,
    /// The entity tool's position for a new entity, waiting for Enter.
    pub entity: Option<DVec3>,
    /// Faces selected with the texture tool are drawn without their red tint.
    pub hide_face_mask: bool,
    /// What the selection's handles do in the 2D views.
    pub handle_mode: crate::transform::HandleMode,
    /// Where rotations turn around, when placed away from the selection's centre.
    pub pivot: Option<DVec3>,
    /// The clipping tool's points, plane and the pieces it would keep.
    pub clip: Option<crate::clipper::ClipOverlay>,
    /// Edges of the camera an MCP agent looks through, drawn in the window's views while
    /// the agent works in the same tab in its own view. Empty when there is none.
    pub agent_camera: Vec<[DVec3; 2]>,
    /// The 3D view's camera, drawn in the 2D views when Mallet Options > Viewports shows
    /// it there. `None` when it is hidden.
    pub view_camera: Option<CameraPreview>,
}

/// Where the 3D view's camera stands and what it sees, for drawing it in the 2D views.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraPreview {
    pub position: DVec3,
    /// The way the camera looks, of unit length.
    pub forward: DVec3,
    /// Horizontal field of view in degrees.
    pub fov: f64,
    /// Width over height of the 3D view's picture.
    pub aspect: f64,
    /// How far ahead of the camera its view pyramid reaches, in world units.
    pub length: f64,
}

impl CameraPreview {
    /// The edges of the camera's view pyramid.
    pub fn edges(&self) -> Vec<[DVec3; 2]> {
        camera_frustum_edges(self.position, self.forward, self.fov, self.aspect, self.length)
    }
}

impl Overlay {
    /// Whether the selection is drawn only where a drag is taking it, leaving nothing
    /// where it started. A clone drag keeps the originals in view.
    pub fn hides_selection(&self) -> bool {
        self.transform.is_some() && !self.cloning
    }
}

/// Half-size of the marker the entity tool draws where a new entity will go.
const ENTITY_MARKER_HALF_SIZE: f64 = 8.0;
/// Colour of the marker that shows where an MCP agent's camera is.
pub const AGENT_CAMERA_COLOR: Color32 = Color32::from_rgb(230, 110, 255);
/// How far in front of a camera marker its view pyramid reaches, in world units.
const CAMERA_MARKER_DEPTH: f64 = 64.0;
/// Half-size of the box drawn where a camera marker stands.
const CAMERA_MARKER_HALF_SIZE: f64 = 6.0;
/// Height of a camera marker's view pyramid as a share of its width.
const CAMERA_MARKER_ASPECT: f64 = 0.6;

/// The edges of a camera drawn into the views: a small box where it stands, a pyramid
/// as wide as its field of view towards where it looks, and a tick on the pyramid's top
/// edge that shows which way is up.
pub fn camera_marker_edges(position: DVec3, forward: DVec3, fov_degrees: f64) -> Vec<[DVec3; 2]> {
    let half = DVec3::splat(CAMERA_MARKER_HALF_SIZE);
    let mut edges = box_edges(&Bounds {
        min: position - half,
        max: position + half,
    });
    edges.extend(camera_frustum_edges(
        position,
        forward,
        fov_degrees,
        1.0 / CAMERA_MARKER_ASPECT,
        CAMERA_MARKER_DEPTH,
    ));
    edges
}

/// The edges of a camera's view pyramid: lines from `position` to the corners of a
/// picture `depth` units ahead, as wide as the horizontal field of view and `aspect`
/// times narrower than tall, with a tick on its top edge that shows which way is up.
pub fn camera_frustum_edges(
    position: DVec3,
    forward: DVec3,
    fov_degrees: f64,
    aspect: f64,
    depth: f64,
) -> Vec<[DVec3; 2]> {
    let mut edges = Vec::with_capacity(10);
    let forward = forward.normalize_or(DVec3::X);
    let right = forward.cross(DVec3::Z).normalize_or(DVec3::Y);
    let up = right.cross(forward);
    let half_width = depth * (fov_degrees.clamp(1.0, 170.0).to_radians() * 0.5).tan();
    let half_height = half_width / aspect.clamp(0.1, 10.0);
    let centre = position + forward * depth;
    let corners = [(-1.0, 1.0), (1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)]
        .map(|(x, y)| centre + right * (half_width * x) + up * (half_height * y));
    for (index, corner) in corners.iter().enumerate() {
        edges.push([position, *corner]);
        edges.push([*corner, corners[(index + 1) % 4]]);
    }
    let peak = centre + up * (half_height + half_width * 0.4);
    edges.push([corners[0], peak]);
    edges.push([peak, corners[1]]);
    edges
}

/// The edges of the entity tool's marker: a small box around the point with a cross
/// through it, so it is visible from any side.
pub fn entity_marker_edges(point: DVec3) -> Vec<[DVec3; 2]> {
    let half = DVec3::splat(ENTITY_MARKER_HALF_SIZE);
    let mut edges = box_edges(&Bounds {
        min: point - half,
        max: point + half,
    });
    for axis in [DVec3::X, DVec3::Y, DVec3::Z] {
        let arm = axis * ENTITY_MARKER_HALF_SIZE * 2.0;
        edges.push([point - arm, point + arm]);
    }
    edges
}

impl WireMesh {
    /// What [`Scene::patch`] leaves where a mesh was taken out: nothing to draw, pick
    /// or select, in a place that keeps the meshes after it where they are.
    fn hole() -> Self {
        Self {
            color: DEFAULT_COLOR,
            edges: Vec::new(),
            faces: Vec::new(),
            bounds: Bounds::EMPTY,
            geometry_bounds: Bounds::EMPTY,
            planes: Vec::new(),
            owner: None,
            solid: None,
            selected: false,
            model: None,
            sprite: None,
            origin: None,
            first_face: None,
            decal: false,
        }
    }

    /// Whether this is a [`WireMesh::hole`].
    fn is_hole(&self) -> bool {
        self.owner.is_none() && self.bounds.is_empty()
    }

    /// Whether selecting `owner` covers this mesh: it is the owner, or a solid it draws.
    pub fn selects(&self, owner: Owner) -> bool {
        self.owner == Some(owner) || matches!(owner, Owner::Solid(id) if self.solid == Some(id))
    }

    /// Colour to draw this mesh with and where to move it, given the current overlay.
    pub fn appearance(&self, overlay: &Overlay) -> (Color32, Option<Remap>) {
        if self.selected {
            (crate::style::current().selection, overlay.transform)
        } else {
            (self.color, None)
        }
    }

    /// The mesh bounds after an optional remap.
    pub fn mapped_bounds(&self, remap: Option<&Remap>) -> Bounds {
        match remap {
            Some(remap) => remap.map_bounds(&self.bounds),
            None => self.bounds,
        }
    }

    /// The geometry bounds after an optional remap.
    pub fn mapped_geometry_bounds(&self, remap: Option<&Remap>) -> Bounds {
        match remap {
            Some(remap) => remap.map_bounds(&self.geometry_bounds),
            None => self.geometry_bounds,
        }
    }
}

/// One polygon of a brush or point entity box, for the textured 3D view.
#[derive(Clone)]
pub struct SceneFace {
    /// The face's material, or `None` for point entity boxes drawn in a flat colour.
    pub material: Option<String>,
    /// Convex polygon in winding order.
    pub vertices: Vec<DVec3>,
    /// Outward unit normal.
    pub normal: DVec3,
    pub uaxis: TexAxis,
    pub vaxis: TexAxis,
    /// The brush side this face shows, by id, so the texture tool can pick and edit it.
    pub side: Option<u32>,
    /// The brush the side belongs to.
    pub solid: Option<u32>,
    /// Whether the texture tool has this face selected.
    pub selected: bool,
    /// Units per luxel of the face's lightmap (the side's `lightmapscale`).
    pub lightmap_scale: f64,
    /// A piece of an overlay or decal: its texture axes give coordinates from 0 to 1
    /// across the picture whatever the texture's size, and it is drawn over the brush
    /// face it lies on, blended by the picture's alpha, without a lightmap of its own.
    pub decal: bool,
    /// The surface of a displacement face, which is drawn and picked in place of the flat
    /// polygon in `vertices`. Its texture coordinates come from where each vertex sits on
    /// the flat face, as the game's do.
    pub displacement: Option<Arc<Surface>>,
}

impl SceneFace {
    /// The face's edges: its displacement's triangles, or its polygon's sides.
    pub fn edges(&self) -> Vec<[DVec3; 2]> {
        match &self.displacement {
            Some(surface) => surface.edges(),
            None => {
                let count = self.vertices.len();
                (0..count)
                    .map(|index| [self.vertices[index], self.vertices[(index + 1) % count]])
                    .collect()
            }
        }
    }
}

/// A face hit by a ray, for the texture tool.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceHit {
    pub side: u32,
    pub solid: u32,
    /// Where the ray met the face.
    pub point: DVec3,
}

/// Loaded prop models by [`model_key`]; `None` when a model could not be loaded.
pub type ModelCache = HashMap<String, Option<Arc<Model>>>;

/// The cache key for a model path: lower case with forward slashes.
pub fn model_key(path: &str) -> String {
    path.trim().replace('\\', "/").to_ascii_lowercase()
}

/// A prop entity's model placed in the world.
#[derive(Clone, Debug)]
pub struct ModelInstance {
    /// Key into the [`ModelCache`].
    pub key: String,
    /// Model space to world: origin, then angles, then `modelscale`.
    pub transform: DMat4,
    /// Whether the model blocks light in the lighting preview: a prop's own model does
    /// unless `disableshadows` is set, while the editor models FGD classes show, such as
    /// a projected texture's lamp, are not in the game and do not.
    pub casts_shadows: bool,
    /// An editor model from the FGD class, such as an AI node's cube, rather than the
    /// entity's own model.
    pub helper: bool,
}

/// The geometry of one brush or point entity.
#[derive(Clone)]
pub struct WireMesh {
    pub color: Color32,
    pub edges: Vec<[DVec3; 2]>,
    /// Polygons for the textured 3D view.
    pub faces: Vec<SceneFace>,
    /// Everything the mesh draws, displaced surfaces included, which culling and the
    /// first test of a 2D outline click use.
    pub bounds: Bounds,
    /// The box the mesh is selected, moved and snapped by, as in Hammer: for a brush, the
    /// box of its own faces, leaving out the displacements sculpted on them, which can
    /// reach outside it; for everything else, the same as `bounds`.
    pub geometry_bounds: Bounds,
    /// Planes enclosing the mesh, used for 3D picking.
    pub planes: Vec<Plane>,
    /// `None` when the brush or entity has no id and cannot be selected.
    pub owner: Option<Owner>,
    /// The id of the brush this mesh draws, world or entity alike. Solids mode selects by
    /// it, where `owner` would select the whole entity.
    pub solid: Option<u32>,
    pub selected: bool,
    /// The prop model drawn for this entity in the 3D view, instead of `faces`.
    pub model: Option<ModelInstance>,
    /// The sprite drawn for this point entity in the 3D view. Once its texture loads, the
    /// sprite replaces the entity's flat box, the way Hammer shows lights, sounds and cube
    /// maps.
    pub sprite: Option<Sprite>,
    /// The origin of a point entity, which moves snap to the grid instead of the
    /// corner of its box, as in Hammer.
    pub origin: Option<DVec3>,
    /// The number of the first of `faces` among all the scene's faces, counting mesh by
    /// mesh, so a renderer can keep data for each face; `None` for meshes made outside
    /// [`Scene::build`].
    pub first_face: Option<u32>,
    /// The picture of an overlay or decal on brush faces. It is picked like any mesh,
    /// but the texture tool and tools that look for a surface see through it.
    pub decal: bool,
}

/// Everything the viewports draw.
#[derive(Default)]
pub struct Scene {
    pub meshes: Vec<WireMesh>,
    /// Changes whenever the geometry changes, so renderers know when to rebuild cached
    /// buffers.
    pub geometry_generation: u64,
    /// Changes whenever the selection changes.
    pub selection_generation: u64,
    pub bounds: Bounds,
    pub solid_count: usize,
    pub entity_count: usize,
    /// Recent changes that only touched some meshes, oldest first, so a renderer that
    /// has drawn generation `from` of one can follow the chain to the current
    /// generation and update just those meshes.
    pub retextures: Vec<Retexture>,
    /// The map's lights, sky, fog and glows, for the lighting preview.
    pub lighting: MapLighting,
    /// The number the next new face gets: one more than the highest
    /// [`WireMesh::first_face`] numbering in use.
    next_face: u32,
    /// The map's soundscape entities and ambient sounds, for the sound preview.
    pub sounds: MapSounds,
}

/// One step of [`Scene::retextures`]: these meshes changed from `from` to `to`.
#[derive(Debug, Clone)]
pub struct Retexture {
    pub from: u64,
    pub to: u64,
    pub meshes: Vec<usize>,
    /// Each mesh's bounds before the change, in the order of `meshes`, so a renderer
    /// also rebuilds where a reshaped mesh was; empty for a mesh that is new.
    pub previous: Vec<Bounds>,
}

/// How many retexture steps a scene remembers; a renderer further behind rebuilds all.
const MAX_RETEXTURES: usize = 64;

impl Scene {
    /// Builds wireframes for every visible brush and point entity in the map.
    ///
    /// World brushes use their own editor colour; brushes of brush entities use the
    /// entity's colour, matching Hammer's 2D views. Entities with a loaded `model` get the
    /// model, outlined by its rotated bounding box; others get the box their FGD class
    /// gives, or a small box. Point entities without their own colour use the class's.
    /// Overlays and decals also get their picture, cut to the brush faces it lies on;
    /// `decals` gives the size of each decal texture.
    pub fn build(vmf: &Vmf, models: &ModelCache, game_data: Option<&GameData>, decals: &DecalSizes) -> Self {
        let mut scene = Self {
            geometry_generation: next_generation(),
            selection_generation: next_generation(),
            ..Self::default()
        };
        std::thread::scope(|scope| {
            let lighting = scope.spawn(|| MapLighting::read(vmf));
            let sounds = scope.spawn(|| MapSounds::read(vmf));
            scene.add_meshes(vmf, models, game_data, decals);
            scene.lighting = lighting.join().expect("reading the map's lights panicked");
            scene.sounds = sounds.join().expect("reading the map's sounds panicked");
        });
        scene
    }

    /// Fills an empty scene with the map's meshes, as [`Scene::build`] describes them.
    fn add_meshes(&mut self, vmf: &Vmf, models: &ModelCache, game_data: Option<&GameData>, decals: &DecalSizes) {
        let scene = self;
        let mut parts = Vec::new();
        for entity in vmf.all_entities() {
            let is_world = is_worldspawn(entity);
            if !is_world {
                scene.entity_count += 1;
            }
            let class = game_data.and_then(|data| data.class(entity.classname()));
            let before = parts.len();
            parts.extend(entity.solids().map(|solid| Part::Brush(entity, solid, class)));
            scene.solid_count += parts.len() - before;
            if parts.len() == before && !is_world {
                parts.push(Part::Point(entity, class));
            }
        }
        let meshes = map_many(&parts, |part| match *part {
            Part::Brush(entity, solid, class) => owned_brush_mesh(entity, solid, class),
            Part::Point(entity, class) => point_mesh(entity, class, models),
        });
        scene.meshes = meshes.into_iter().flatten().collect();
        scene.add_decals(vmf, game_data, decals);
        let mut faces = 0u32;
        for mesh in &mut scene.meshes {
            scene.bounds.add_bounds(&mesh.bounds);
            mesh.first_face = Some(faces);
            faces = faces.saturating_add(mesh.faces.len() as u32);
        }
        scene.next_face = faces;
    }

    /// Adds the pictures of the map's overlays and decals, each cut to the brush faces
    /// it lies on. An overlay lies on the faces its `sides` key lists; a decal on the
    /// face nearest to it and the faces that continue that face.
    fn add_decals(&mut self, vmf: &Vmf, game_data: Option<&GameData>, sizes: &DecalSizes) {
        let entities: Vec<&Entity> = vmf
            .entities()
            .filter(|entity| is_overlay(entity) || is_decal(entity))
            .collect();
        if entities.is_empty() {
            return;
        }
        let by_side = entities
            .iter()
            .any(|entity| is_overlay(entity))
            .then(|| faces_by_side(&self.meshes));
        let brushes = self.meshes.as_slice();
        let added = map_many(&entities, |entity| {
            picture_mesh(entity, brushes, by_side.as_ref(), game_data, sizes)
        });
        self.meshes.extend(added.into_iter().flatten());
    }

    /// Whether an edit touched few enough brushes and entities for [`Scene::patch`] to
    /// be quicker than building the whole scene.
    pub fn patches(&self, touched: &Touched) -> bool {
        (touched.solids.len() + touched.entities.len()) * PATCH_SHARE <= self.meshes.len()
    }

    /// Brings the meshes of the brushes and entities an edit touched up to the map,
    /// with the pictures of the overlays and decals lying on or near them, keeping
    /// everything else, and notes them in [`Scene::retextures`]. A brush or entity that
    /// is new to the scene gets its meshes after all others, and one that left the map
    /// leaves a hole where its meshes were, so every other mesh keeps its place.
    ///
    /// Returns `false` when the caller should build the whole scene instead: so much
    /// changed that building anew is as quick, or enough holes have gathered to be
    /// worth closing.
    pub fn patch(
        &mut self,
        vmf: &Vmf,
        models: &ModelCache,
        game_data: Option<&GameData>,
        sizes: &DecalSizes,
        touched: &Touched,
    ) -> bool {
        if !self.patches(touched) {
            return false;
        }
        let mut by_solid: HashMap<u32, usize> = HashMap::new();
        let mut bodies: HashMap<u32, Vec<usize>> = HashMap::new();
        let mut pictures: HashMap<u32, usize> = HashMap::new();
        let (mut holes, mut faces) = (0, 0);
        for (at, mesh) in self.meshes.iter().enumerate() {
            if mesh.is_hole() {
                holes += 1;
                continue;
            }
            faces += mesh.faces.len();
            match (mesh.owner, mesh.decal) {
                (Some(Owner::Entity(id)), true) => {
                    pictures.insert(id, at);
                    continue;
                }
                (Some(Owner::Entity(id)), false) if touched.entities.contains(&id) => {
                    bodies.entry(id).or_default().push(at);
                }
                _ => {}
            }
            if let Some(id) = mesh.solid.filter(|_| !mesh.decal) {
                by_solid.insert(id, at);
            }
        }
        if holes * HOLE_SHARE > self.meshes.len() || self.next_face as usize > faces * 2 + SPARE_FACES {
            return false;
        }
        let class_of = |entity: &Entity| game_data.and_then(|data| data.class(entity.classname()));
        let mut parts: Vec<(Option<usize>, Part)> = Vec::new();
        let mut gone: Vec<usize> = Vec::new();
        let mut planned: HashSet<u32> = HashSet::new();
        let mut claimed: HashSet<usize> = HashSet::new();
        let mut reread = false;
        for id in &touched.solids {
            let at = by_solid.get(id).copied();
            match vmf.solid_with_owner(*id) {
                Some((entity, solid)) => {
                    reread |= !is_worldspawn(entity);
                    planned.insert(*id);
                    claimed.extend(at);
                    parts.push((at, Part::Brush(entity, solid, class_of(entity))));
                }
                None => {
                    reread |= at.is_some_and(|at| matches!(self.meshes[at].owner, Some(Owner::Entity(_))));
                    gone.extend(at);
                }
            }
        }
        let mut leftovers: Vec<usize> = Vec::new();
        for id in &touched.entities {
            reread = true;
            let body = bodies.get(id).map_or(&[][..], Vec::as_slice);
            let Some(entity) = vmf.entity_by_id(*id) else {
                leftovers.extend(body);
                continue;
            };
            if is_worldspawn(entity) {
                continue;
            }
            let class = class_of(entity);
            let mut kept: HashSet<usize> = HashSet::new();
            let mut brushes = false;
            for solid in entity.solids() {
                let Some(solid_id) = solid.id() else {
                    return false;
                };
                brushes = true;
                let at = by_solid.get(&solid_id).copied();
                kept.extend(at);
                if planned.insert(solid_id) {
                    claimed.extend(at);
                    parts.push((at, Part::Brush(entity, solid, class)));
                }
            }
            if !brushes {
                let at = body.iter().copied().find(|at| self.meshes[*at].solid.is_none());
                kept.extend(at);
                claimed.extend(at);
                parts.push((at, Part::Point(entity, class)));
            }
            leftovers.extend(body.iter().filter(|at| !kept.contains(at)));
        }
        gone.extend(leftovers.into_iter().filter(|at| !claimed.contains(at)));
        let made = map_many(&parts, |(_, part)| match *part {
            Part::Brush(entity, solid, class) => owned_brush_mesh(entity, solid, class),
            Part::Point(entity, class) => point_mesh(entity, class, models),
        });
        let mut step = PatchStep::default();
        for ((at, _), mesh) in parts.iter().zip(made) {
            self.put(*at, mesh, &mut step);
        }
        for at in gone {
            self.put(Some(at), None, &mut step);
        }
        let mut stale: Vec<(Option<usize>, Option<&Entity>)> = Vec::new();
        for (id, at) in &pictures {
            let entity = vmf.entity_by_id(*id);
            let lies_on_changed = entity.is_some_and(|entity| {
                if is_overlay(entity) {
                    overlay_sides(entity).any(|side| step.sides.contains(&side))
                } else {
                    decal_reach(entity, sizes).is_some_and(|(origin, reach)| {
                        step.moved.iter().any(|bounds| within(bounds, origin, reach))
                    })
                }
            });
            if lies_on_changed || entity.is_none() || touched.entities.contains(id) {
                stale.push((Some(*at), entity));
            }
        }
        for id in touched.entities.iter().filter(|id| !pictures.contains_key(id)) {
            if let Some(entity) = vmf.entity_by_id(*id) {
                stale.push((None, Some(entity)));
            }
        }
        let pictured = |entity: &&Entity| is_overlay(entity) || is_decal(entity);
        let by_side = stale
            .iter()
            .any(|(_, entity)| entity.is_some_and(is_overlay))
            .then(|| faces_by_side(&self.meshes));
        for (at, entity) in stale {
            let picture = entity
                .filter(pictured)
                .and_then(|entity| picture_mesh(entity, &self.meshes, by_side.as_ref(), game_data, sizes));
            self.put(at, picture, &mut step);
        }
        self.entity_count = vmf.entities().count();
        self.solid_count = vmf.all_entities().map(|entity| entity.solids().count()).sum();
        if reread {
            let (lighting, sounds) = std::thread::scope(|scope| {
                let lighting = scope.spawn(|| MapLighting::read(vmf));
                let sounds = MapSounds::read(vmf);
                (lighting.join().expect("reading the map's lights panicked"), sounds)
            });
            self.lighting = lighting;
            self.sounds = sounds;
        }
        self.note_changed(step.changed, step.previous);
        true
    }

    /// Makes the mesh at `at` what `mesh` is and notes the change in `step`: a mesh is
    /// replaced, keeping what the old one had selected, or left as a hole when there is
    /// no new one, and a mesh without a place is added after all others. Faces keep
    /// their numbers where the new mesh has no more of them than the old one.
    fn put(&mut self, at: Option<usize>, mesh: Option<WireMesh>, step: &mut PatchStep) {
        let Some(at) = at else {
            let Some(mut mesh) = mesh else {
                return;
            };
            mesh.first_face = Some(self.number_faces(mesh.faces.len()));
            step.note(&mesh);
            step.changed.push(self.meshes.len());
            step.previous.push(Bounds::EMPTY);
            self.bounds.add_bounds(&mesh.bounds);
            self.meshes.push(mesh);
            return;
        };
        step.note(&self.meshes[at]);
        step.changed.push(at);
        step.previous.push(self.meshes[at].bounds);
        let Some(mut mesh) = mesh else {
            self.meshes[at] = WireMesh::hole();
            return;
        };
        let old = &self.meshes[at];
        mesh.selected = old.selected;
        if mesh.faces.len() == old.faces.len() {
            for (face, old_face) in mesh.faces.iter_mut().zip(&old.faces) {
                face.selected = old_face.selected;
            }
        }
        let numbered = old.first_face.filter(|_| mesh.faces.len() <= old.faces.len());
        mesh.first_face = Some(numbered.unwrap_or_else(|| self.number_faces(mesh.faces.len())));
        step.note(&mesh);
        self.bounds.add_bounds(&mesh.bounds);
        self.meshes[at] = mesh;
    }

    /// Takes the next `count` face numbers and returns the first.
    fn number_faces(&mut self, count: usize) -> u32 {
        let first = self.next_face;
        self.next_face = first.saturating_add(count as u32);
        first
    }

    /// Moves the scene on to a new geometry generation in which only the meshes at
    /// `changed` differ, and remembers the step for renderers; `previous` holds their
    /// bounds before the change.
    fn note_changed(&mut self, changed: Vec<usize>, previous: Vec<Bounds>) {
        let from = self.geometry_generation;
        self.geometry_generation = next_generation();
        self.retextures.push(Retexture {
            from,
            to: self.geometry_generation,
            meshes: changed,
            previous,
        });
        if self.retextures.len() > MAX_RETEXTURES {
            self.retextures.remove(0);
        }
    }

    /// Whether any mesh belongs to `owner`.
    pub fn contains(&self, owner: Owner) -> bool {
        self.meshes.iter().any(|mesh| mesh.selects(owner))
    }

    /// Everything a selection may hold: each mesh's owner, and each brush's own solid,
    /// which Solids mode selects.
    pub fn selectable(&self) -> HashSet<Owner> {
        let mut owners = HashSet::with_capacity(self.meshes.len());
        for mesh in &self.meshes {
            owners.extend(mesh.owner);
            owners.extend(mesh.solid.map(Owner::Solid));
        }
        owners
    }

    /// Flags the meshes the selection covers: those whose owner is selected, and brushes
    /// whose own solid is selected, as in Solids mode.
    pub fn mark_selected(&mut self, selection: &BTreeSet<Owner>) {
        for mesh in &mut self.meshes {
            mesh.selected = mesh.owner.is_some_and(|owner| selection.contains(&owner))
                || mesh.solid.is_some_and(|id| selection.contains(&Owner::Solid(id)));
        }
        self.selection_generation = next_generation();
    }

    /// Bounds of every selected mesh; empty when nothing is selected.
    pub fn selection_bounds(&self) -> Bounds {
        let mut bounds = Bounds::EMPTY;
        for mesh in self.meshes.iter().filter(|mesh| mesh.selected) {
            bounds.add_bounds(&mesh.geometry_bounds);
        }
        bounds
    }

    /// The point that moves of the selection snap to the grid: the origin of the first
    /// selected point entity when only point entities are selected, as Hammer snaps them
    /// by their origin, or else the minimum corner of the selection's bounds.
    pub fn selection_anchor(&self) -> DVec3 {
        let mut selected = self.meshes.iter().filter(|mesh| mesh.selected).peekable();
        let first = selected.peek().and_then(|mesh| mesh.origin);
        match first {
            Some(origin) if selected.all(|mesh| mesh.origin.is_some()) => origin,
            _ => self.selection_bounds().min,
        }
    }

    /// Every selectable owner, without duplicates.
    pub fn owners(&self) -> BTreeSet<Owner> {
        self.meshes.iter().filter_map(|mesh| mesh.owner).collect()
    }

    /// Picks what is under a 2D view point the way Hammer does: a brush's centre cross
    /// first, then its outline. Clicking empty space inside a brush picks nothing, so a
    /// selection rectangle can start there. Among several hits the smallest brush wins.
    /// `tolerance` is how close, in world units, the point must be.
    /// With `solids`, a brush of an entity picks its own solid rather than the entity.
    pub fn pick_2d(&self, axis: Axis2D, point: DVec2, tolerance: f64, solids: bool) -> Option<Owner> {
        let smallest = |hits: &mut dyn Iterator<Item = &WireMesh>| {
            hits.filter_map(|mesh| Some((picked(mesh, solids)?, projected_area(axis, &mesh.geometry_bounds))))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(owner, _)| owner)
        };
        let on_centre =
            |mesh: &&WireMesh| (axis.project(mesh.geometry_bounds.center()) - point).abs().max_element() <= tolerance;
        let on_outline = |mesh: &&WireMesh| {
            let min = axis.project(mesh.bounds.min) - DVec2::splat(tolerance);
            let max = axis.project(mesh.bounds.max) + DVec2::splat(tolerance);
            point.cmpge(min).all()
                && point.cmple(max).all()
                && mesh
                    .edges
                    .iter()
                    .any(|[a, b]| segment_distance(point, axis.project(*a), axis.project(*b)) <= tolerance)
        };
        smallest(&mut self.meshes.iter().filter(on_centre))
            .or_else(|| smallest(&mut self.meshes.iter().filter(on_outline)))
    }

    /// Picks the nearest mesh hit by a ray, the way Hammer does: a prop counts where the
    /// ray meets its model's triangles, so clicking through a gap in a fence picks what is
    /// behind it. Only when the ray hits nothing at all is a prop picked by its bounding
    /// box, so thin or hollow models can still be clicked.
    /// With `solids`, a brush of an entity picks its own solid rather than the entity.
    pub fn pick_ray(&self, origin: DVec3, direction: DVec3, models: &ModelCache, solids: bool) -> Option<Owner> {
        let nearest = |hits: &mut dyn Iterator<Item = (Owner, f64)>| {
            hits.min_by(|a, b| a.1.total_cmp(&b.1)).map(|(owner, _)| owner)
        };
        nearest(
            &mut self
                .meshes
                .iter()
                .filter_map(|mesh| Some((picked(mesh, solids)?, mesh_hit(mesh, origin, direction, models)?))),
        )
        .or_else(|| {
            nearest(
                &mut self
                    .meshes
                    .iter()
                    .filter(|mesh| mesh.model.is_some())
                    .filter_map(|mesh| Some((picked(mesh, solids)?, ray_box(origin, direction, &mesh.bounds)?))),
            )
        })
    }

    /// Whether something drawn in the game stands between `from` and `to`, as for a glow
    /// sprite's visibility test: a brush whose face the line enters through is
    /// `drawn`, or a prop's triangles. Point entity boxes and decals do not count.
    pub fn line_blocked(
        &self,
        from: DVec3,
        to: DVec3,
        models: &ModelCache,
        drawn: impl Fn(&SceneFace) -> bool,
    ) -> bool {
        let offset = to - from;
        let length = offset.length();
        if length <= f64::EPSILON {
            return false;
        }
        let direction = offset / length;
        let line = Bounds {
            min: from.min(to),
            max: from.max(to),
        };
        self.meshes
            .iter()
            .filter(|mesh| {
                !mesh.decal
                    && mesh.owner.is_some()
                    && mesh.bounds.min.cmple(line.max).all()
                    && mesh.bounds.max.cmpge(line.min).all()
            })
            .any(|mesh| {
                if mesh.model.is_some() {
                    return mesh_hit(mesh, from, direction, models).is_some_and(|distance| distance < length);
                }
                if mesh.sprite.is_some() || mesh.faces.iter().all(|face| face.side.is_none()) {
                    return false;
                }
                if let Some((distance, face)) = displacement_hit(mesh, from, direction) {
                    return distance < length && drawn(&mesh.faces[face]);
                }
                ray_convex_face(from, direction, &mesh.planes).is_some_and(|(distance, plane)| {
                    distance < length && plane.and_then(|plane| mesh.faces.get(plane)).is_none_or(&drawn)
                })
            })
    }

    /// Whether a brush stands between `from` and `to`, as a brush-only trace in the game:
    /// props and point entities never count. `counts` says whether a brush stops the
    /// trace, given its mesh and the face the line enters through; a line that starts
    /// inside a brush is stopped by it when any of its faces counts. A brush with a
    /// displacement is only as solid as the displacement's surface.
    pub fn brush_line_blocked(&self, from: DVec3, to: DVec3, counts: impl Fn(&WireMesh, &SceneFace) -> bool) -> bool {
        let offset = to - from;
        let length = offset.length();
        if length <= f64::EPSILON {
            return false;
        }
        let direction = offset / length;
        let line = Bounds {
            min: from.min(to),
            max: from.max(to),
        };
        self.meshes
            .iter()
            .filter(|mesh| {
                !mesh.decal
                    && mesh.owner.is_some()
                    && mesh.model.is_none()
                    && mesh.sprite.is_none()
                    && mesh.bounds.min.cmple(line.max).all()
                    && mesh.bounds.max.cmpge(line.min).all()
                    && mesh.faces.iter().any(|face| face.side.is_some())
            })
            .any(|mesh| {
                if mesh.faces.iter().any(|face| face.displacement.is_some()) {
                    return displacement_hit(mesh, from, direction)
                        .is_some_and(|(distance, face)| distance < length && counts(mesh, &mesh.faces[face]));
                }
                if !mesh.planes.is_empty() && mesh.planes.iter().all(|plane| plane.distance(from) <= 0.0) {
                    return mesh.faces.iter().any(|face| counts(mesh, face));
                }
                ray_convex_face(from, direction, &mesh.planes).is_some_and(|(distance, plane)| {
                    distance < length
                        && plane
                            .and_then(|plane| mesh.faces.get(plane))
                            .is_none_or(|face| counts(mesh, face))
                })
            })
    }

    /// The brush face a ray hits first, for the texture tool. Point entities and props
    /// block the ray but are not faces; a prop blocks it only where its model is.
    pub fn pick_face(&self, origin: DVec3, direction: DVec3, models: &ModelCache) -> Option<FaceHit> {
        let (mesh, distance, plane) = self
            .meshes
            .iter()
            .filter(|mesh| mesh.owner.is_some() && !mesh.decal)
            .filter_map(|mesh| {
                if mesh.model.is_some() {
                    return Some((mesh, mesh_hit(mesh, origin, direction, models)?, None));
                }
                if let Some((distance, face)) = displacement_hit(mesh, origin, direction) {
                    return Some((mesh, distance, Some(face)));
                }
                let (distance, plane) = ray_convex_face(origin, direction, &mesh.planes)?;
                Some((mesh, distance, plane))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))?;
        let face = mesh.faces.get(plane?)?;
        Some(FaceHit {
            side: face.side?,
            solid: face.solid?,
            point: origin + direction * distance,
        })
    }

    /// Rebuilds the meshes of brushes whose materials, texture alignment or displacements
    /// changed, keeping everything else, and notes them in [`Scene::retextures`]. Returns
    /// `false` when a brush has no mesh, so the caller should rebuild the whole scene.
    pub fn retexture(&mut self, solids: &[Solid]) -> bool {
        let index: HashMap<u32, usize> = self
            .meshes
            .iter()
            .enumerate()
            .filter_map(|(at, mesh)| Some((mesh.faces.first()?.solid?, at)))
            .collect();
        let mut changed = Vec::with_capacity(solids.len());
        let mut previous = Vec::with_capacity(solids.len());
        for solid in solids {
            let Some(&at) = solid.id().and_then(|id| index.get(&id)) else {
                return false;
            };
            let old = &self.meshes[at];
            let Some(mut mesh) = brush_mesh(solid, old.color, old.owner) else {
                return false;
            };
            if mesh.faces.len() != old.faces.len() {
                return false;
            }
            mesh.selected = old.selected;
            mesh.first_face = old.first_face;
            for (face, old_face) in mesh.faces.iter_mut().zip(&old.faces) {
                face.selected = old_face.selected;
            }
            previous.push(old.bounds);
            self.bounds.add_bounds(&mesh.bounds);
            self.meshes[at] = mesh;
            changed.push(at);
        }
        self.note_changed(changed, previous);
        true
    }

    /// Marks the faces whose side ids are in `sides` as selected, for drawing.
    pub fn mark_faces(&mut self, sides: &BTreeSet<u32>) {
        for face in self.meshes.iter_mut().flat_map(|mesh| mesh.faces.iter_mut()) {
            face.selected = face.side.is_some_and(|side| sides.contains(&side));
        }
        self.selection_generation = next_generation();
    }

    /// Where a ray first hits a brush face, ignoring point entities and props. Used to
    /// place new entities on the surface under the cursor.
    pub fn surface_hit(&self, origin: DVec3, direction: DVec3) -> Option<DVec3> {
        self.meshes
            .iter()
            .filter(|mesh| mesh.model.is_none() && !mesh.decal && mesh.faces.iter().any(|face| face.material.is_some()))
            .filter_map(|mesh| {
                displacement_hit(mesh, origin, direction)
                    .map(|(distance, _)| distance)
                    .or_else(|| ray_convex(origin, direction, &mesh.planes))
            })
            .min_by(f64::total_cmp)
            .map(|distance| origin + direction * distance)
    }

    /// The brush face a ray hits first and where, seeing through point entities, props,
    /// overlays and decals. Used to place overlays and decals on the face under the cursor.
    pub fn surface_face(&self, origin: DVec3, direction: DVec3) -> Option<(FaceHit, DVec3)> {
        self.surface_face_among(origin, direction, false)
    }

    /// As [`Scene::surface_face`], seeing through the selected brushes too. Used while
    /// the selection is shown somewhere other than where its brushes are.
    pub fn surface_face_behind_selection(&self, origin: DVec3, direction: DVec3) -> Option<(FaceHit, DVec3)> {
        self.surface_face_among(origin, direction, true)
    }

    /// The brush face a ray hits first, its hit point and its outward normal, passing
    /// over selected brushes when `skip_selected` is set.
    fn surface_face_among(&self, origin: DVec3, direction: DVec3, skip_selected: bool) -> Option<(FaceHit, DVec3)> {
        let (mesh, distance, plane) = self
            .meshes
            .iter()
            .filter(|mesh| !(skip_selected && mesh.selected))
            .filter(|mesh| mesh.model.is_none() && !mesh.decal && mesh.faces.iter().any(|face| face.side.is_some()))
            .filter_map(|mesh| {
                if let Some((distance, face)) = displacement_hit(mesh, origin, direction) {
                    return Some((mesh, distance, face));
                }
                let (distance, plane) = ray_convex_face(origin, direction, &mesh.planes)?;
                Some((mesh, distance, plane?))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))?;
        let face = mesh.faces.get(plane)?;
        let point = origin + direction * distance;
        let hit = FaceHit {
            side: face.side?,
            solid: face.solid?,
            point,
        };
        let normal = face
            .displacement
            .as_ref()
            .and_then(|surface| Some(surface.normals[surface.nearest_vertex(point)?]))
            .unwrap_or(face.normal);
        Some((hit, normal))
    }

    /// The brush face nearest to a point and no further than `reach` from it, passing
    /// over tool faces: its side id, the face's point nearest to the given one, and its
    /// outward normal.
    pub fn nearest_face(&self, point: DVec3, reach: f64) -> Option<(u32, DVec3, DVec3)> {
        nearest_face(&self.meshes, point, reach)
    }

    /// The brush faces that lie in the plane of the face with side id `side` and touch
    /// the box `around`, that face included, so a picture placed there can run on over
    /// the neighbouring brushes of the same wall.
    pub fn faces_in_plane(&self, side: u32, around: &Bounds) -> Vec<u32> {
        let Some(base) = self
            .meshes
            .iter()
            .flat_map(|mesh| &mesh.faces)
            .find(|face| face.side == Some(side))
        else {
            return Vec::new();
        };
        let Some(&anchor) = base.vertices.first() else {
            return vec![side];
        };
        let touches = |bounds: &Bounds| bounds.min.cmple(around.max).all() && bounds.max.cmpge(around.min).all();
        self.meshes
            .iter()
            .filter(|mesh| !mesh.decal && touches(&mesh.bounds))
            .flat_map(|mesh| &mesh.faces)
            .filter(|face| {
                face.normal.dot(base.normal) > DECAL_SAME_FACING
                    && face
                        .vertices
                        .first()
                        .is_some_and(|vertex| (*vertex - anchor).dot(base.normal).abs() < mallet_vmf::geometry::EPSILON)
            })
            .filter_map(|face| face.side)
            .collect()
    }

    /// Owners with a centre cross inside a 2D rectangle, the way Hammer's selection
    /// rectangle picks brushes. For an entity made of several brushes, any one of their
    /// centres counts.
    /// With `solids`, brushes of entities count as their own solids rather than entities.
    pub fn owners_inside(&self, axis: Axis2D, corner_a: DVec2, corner_b: DVec2, solids: bool) -> BTreeSet<Owner> {
        let (min, max) = (corner_a.min(corner_b), corner_a.max(corner_b));
        self.meshes
            .iter()
            .filter(|mesh| {
                let centre = axis.project(mesh.geometry_bounds.center());
                centre.cmpge(min).all() && centre.cmple(max).all()
            })
            .filter_map(|mesh| picked(mesh, solids))
            .collect()
    }
}

#[cfg(test)]
impl Scene {
    /// Everything the scene shows, mesh by mesh in an order that does not depend on
    /// theirs, so two scenes can be compared whatever way they were put together.
    pub fn outline(&self) -> Vec<String> {
        let mut meshes: Vec<String> = self
            .meshes
            .iter()
            .filter(|mesh| !mesh.is_hole())
            .map(|mesh| {
                let faces: Vec<String> = mesh
                    .faces
                    .iter()
                    .map(|face| {
                        format!(
                            "{:?} {:?} {:?} {:?} {:?}",
                            face.side, face.solid, face.material, face.vertices, face.normal
                        )
                    })
                    .collect();
                let mut edges: Vec<String> = mesh.edges.iter().map(|edge| format!("{edge:?}")).collect();
                edges.sort();
                format!(
                    "{:?} {:?} decal={} {:?} {:?} {:?} {:?} {:?} {:?} {edges:?} {faces:?}",
                    mesh.owner,
                    mesh.solid,
                    mesh.decal,
                    mesh.color,
                    mesh.bounds,
                    mesh.geometry_bounds,
                    mesh.origin,
                    mesh.model.as_ref().map(|model| (&model.key, model.transform)),
                    mesh.sprite,
                )
            })
            .collect();
        meshes.sort();
        meshes
    }
}

/// The owner a click on `mesh` selects: the mesh's own solid when `solids` is set and it
/// has one, otherwise its owner. A mesh without an owner cannot be picked.
fn picked(mesh: &WireMesh, solids: bool) -> Option<Owner> {
    let owner = mesh.owner?;
    match mesh.solid {
        Some(id) if solids => Some(Owner::Solid(id)),
        _ => Some(owner),
    }
}

fn projected_area(axis: Axis2D, bounds: &Bounds) -> f64 {
    let size = axis.project(bounds.size());
    size.x * size.y
}

/// Shortest distance from a point to a line segment.
fn segment_distance(point: DVec2, a: DVec2, b: DVec2) -> f64 {
    let along = b - a;
    let length = along.length_squared();
    let t = if length > 0.0 {
        ((point - a).dot(along) / length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    point.distance(a + along * t)
}

/// Distance along a ray to where it meets a mesh: the triangles of a prop's model, or
/// the planes of a brush or entity box. A prop whose model is not loaded counts as its box.
fn mesh_hit(mesh: &WireMesh, origin: DVec3, direction: DVec3, models: &ModelCache) -> Option<f64> {
    let loaded = mesh
        .model
        .as_ref()
        .and_then(|instance| Some((instance, models.get(&instance.key)?.as_deref()?)));
    match loaded {
        Some((instance, model)) => ray_model(origin, direction, &mesh.bounds, instance, model),
        None => displacement_hit(mesh, origin, direction)
            .map(|(distance, _)| distance)
            .or_else(|| ray_convex(origin, direction, &mesh.planes)),
    }
}

/// The nearest displacement surface of a mesh that a ray meets: how far along the ray,
/// and the face's index among the mesh's faces.
fn displacement_hit(mesh: &WireMesh, origin: DVec3, direction: DVec3) -> Option<(f64, usize)> {
    mesh.faces
        .iter()
        .enumerate()
        .filter_map(|(index, face)| Some((face.displacement.as_ref()?.ray_hit(origin, direction)?, index)))
        .min_by(|a, b| a.0.total_cmp(&b.0))
}

/// Distance along a ray to the nearest triangle of a placed model, from either side.
fn ray_model(origin: DVec3, direction: DVec3, bounds: &Bounds, instance: &ModelInstance, model: &Model) -> Option<f64> {
    ray_box(origin, direction, bounds)?;
    if instance.transform.determinant().abs() < 1e-12 {
        return None;
    }
    // An affine transform keeps distances along the ray in proportion, so the distance
    // found in model space is the distance in the world.
    let inverse = instance.transform.inverse();
    let local_origin = inverse.transform_point3(origin);
    let local_direction = inverse.transform_vector3(direction);
    let corner = |index: u32| {
        model
            .vertices
            .get(index as usize)
            .map(|vertex| DVec3::from_array(vertex.position.map(f64::from)))
    };
    model
        .meshes
        .iter()
        .flat_map(|mesh| mesh.indices.as_chunks::<3>().0)
        .filter_map(|&[a, b, c]| {
            let [a, b, c] = [corner(a)?, corner(b)?, corner(c)?];
            ray_triangle(local_origin, local_direction, a, b, c)
        })
        .min_by(f64::total_cmp)
}

/// Möller-Trumbore: distance along a ray to a triangle, hit from either side.
fn ray_triangle(origin: DVec3, direction: DVec3, a: DVec3, b: DVec3, c: DVec3) -> Option<f64> {
    let edge1 = b - a;
    let edge2 = c - a;
    let p = direction.cross(edge2);
    let determinant = edge1.dot(p);
    if determinant.abs() < 1e-12 {
        return None;
    }
    let inverse = 1.0 / determinant;
    let s = origin - a;
    let u = s.dot(p) * inverse;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(edge1);
    let v = direction.dot(q) * inverse;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = edge2.dot(q) * inverse;
    (t > 0.0).then_some(t)
}

/// Distance along a ray to an axis-aligned box, or 0 when it starts inside.
fn ray_box(origin: DVec3, direction: DVec3, bounds: &Bounds) -> Option<f64> {
    let mut near = f64::NEG_INFINITY;
    let mut far = f64::INFINITY;
    for axis in 0..3 {
        let (start, step) = (origin[axis], direction[axis]);
        let (low, high) = (bounds.min[axis], bounds.max[axis]);
        if step.abs() < 1e-12 {
            if start < low || start > high {
                return None;
            }
            continue;
        }
        let (t1, t2) = ((low - start) / step, (high - start) / step);
        near = near.max(t1.min(t2));
        far = far.min(t1.max(t2));
    }
    (near <= far && far >= 0.0).then_some(near.max(0.0))
}

/// Distance along the ray to where it enters a convex volume, or `None` on a miss.
/// A ray starting inside the volume misses it, so clicking from inside a brush picks
/// what is beyond it, as faces seen from inside are not drawn either.
fn ray_convex(origin: DVec3, direction: DVec3, planes: &[Plane]) -> Option<f64> {
    ray_convex_face(origin, direction, planes).map(|(distance, _)| distance)
}

/// Like [`ray_convex`], also giving the index of the plane the ray enters through.
fn ray_convex_face(origin: DVec3, direction: DVec3, planes: &[Plane]) -> Option<(f64, Option<usize>)> {
    if planes.is_empty() || planes.iter().all(|plane| plane.distance(origin) <= 0.0) {
        return None;
    }
    let mut enter: f64 = 0.0;
    let mut entered = None;
    let mut exit = f64::INFINITY;
    for (index, plane) in planes.iter().enumerate() {
        let facing = plane.normal.dot(direction);
        let distance = plane.distance(origin);
        if facing.abs() < 1e-12 {
            if distance > 0.0 {
                return None;
            }
            continue;
        }
        let t = -distance / facing;
        if facing < 0.0 {
            if t > enter || entered.is_none() {
                enter = enter.max(t);
                entered = Some(index);
            }
        } else {
            exit = exit.min(t);
        }
        if enter > exit {
            return None;
        }
    }
    Some((enter, entered))
}

fn to_color(color: Option<[u8; 3]>) -> Color32 {
    color.map_or(DEFAULT_COLOR, |[r, g, b]| Color32::from_rgb(r, g, b))
}

/// An edge by its two quantized ends, the smaller first, so both directions match.
type EdgeKey = ([i64; 3], [i64; 3]);

fn quantize(point: DVec3) -> [i64; 3] {
    (point * EDGE_QUANTIZE).round().as_i64vec3().to_array()
}

/// One mesh to build: a brush of the world or of an entity, or a point entity.
#[derive(Clone, Copy)]
enum Part<'a> {
    Brush(&'a Entity, &'a Solid, Option<&'a EntityClass>),
    Point(&'a Entity, Option<&'a EntityClass>),
}

/// What [`Scene::patch`] has changed so far.
#[derive(Default)]
struct PatchStep {
    /// The meshes replaced, taken out or added.
    changed: Vec<usize>,
    /// Their bounds before, in the same order; empty for an added mesh.
    previous: Vec<Bounds>,
    /// The side ids of changed brushes' faces, old and new, which overlays may list.
    sides: HashSet<u32>,
    /// The bounds of changed brushes and entities, old and new, which decals may lie on.
    moved: Vec<Bounds>,
}

impl PatchStep {
    /// Notes a mesh as it was before a change or as it is after it, for the overlays
    /// and decals that lie on it.
    fn note(&mut self, mesh: &WireMesh) {
        if !mesh.decal && !mesh.is_hole() {
            self.sides.extend(mesh.faces.iter().filter_map(|face| face.side));
            self.moved.push(mesh.bounds);
        }
    }
}

/// A patch that would rebuild more than one mesh in this many gives way to building
/// the whole scene, which is spread over every processor core.
const PATCH_SHARE: usize = 3;

/// Once more than one mesh in this many is a hole, the whole scene is built again.
const HOLE_SHARE: usize = 8;

/// Face numbers may run this far past twice the number of faces before the whole scene
/// is built again and its faces are numbered from 0.
const SPARE_FACES: usize = 4096;

/// Below this many items [`map_many`] works on the calling thread, as starting threads
/// would take longer than the work.
const MANY: usize = 64;

/// Maps `items` in order, on every processor core when there are many of them.
fn map_many<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    if items.len() < MANY {
        items.iter().map(work).collect()
    } else {
        parallel_map(items, work)
    }
}

fn is_worldspawn(entity: &Entity) -> bool {
    entity.classname().eq_ignore_ascii_case("worldspawn")
}

fn is_overlay(entity: &Entity) -> bool {
    entity.classname().eq_ignore_ascii_case(overlay::OVERLAY_CLASS)
}

fn is_decal(entity: &Entity) -> bool {
    entity.classname().eq_ignore_ascii_case(overlay::DECAL_CLASS)
}

/// The mesh of one brush of `entity`. A world brush has its own editor colour and is
/// selected by itself; a brush of an entity has the entity's colour and selects it.
fn owned_brush_mesh(entity: &Entity, solid: &Solid, class: Option<&EntityClass>) -> Option<WireMesh> {
    let (color, owner) = if is_worldspawn(entity) {
        (solid.editor_color(), solid.id().map(Owner::Solid))
    } else {
        let color = entity.editor_color().or_else(|| class.and_then(|class| class.color));
        (color.or_else(|| solid.editor_color()), entity.id().map(Owner::Entity))
    };
    brush_mesh(solid, to_color(color), owner)
}

/// The mesh of an entity without brushes: its loaded model, outlined by the model's
/// rotated bounding box, or else the box its FGD class gives, or a small box. `None`
/// for an entity without an origin.
fn point_mesh(entity: &Entity, class: Option<&EntityClass>, models: &ModelCache) -> Option<WireMesh> {
    let origin = entity.origin()?;
    let color = to_color(entity.editor_color().or_else(|| class.and_then(|class| class.color)));
    let owner = entity.id().map(Owner::Entity);
    let loaded = entity_model(entity, class)
        .map(model_key)
        .and_then(|key| Some((models.get(&key)?.clone()?, key)));
    let Some((model, key)) = loaded else {
        let (min, max) = match class.and_then(|class| class.size) {
            Some((min, max)) => (DVec3::from_array(min), DVec3::from_array(max)),
            None => point_entity_box(entity.classname()),
        };
        let bounds = Bounds {
            min: origin + min,
            max: origin + max,
        };
        return Some(WireMesh {
            color,
            edges: box_edges(&bounds),
            faces: box_faces(&bounds),
            bounds,
            geometry_bounds: bounds,
            planes: box_planes(&bounds),
            owner,
            solid: None,
            selected: false,
            model: None,
            sprite: entity_sprite(entity, class),
            origin: Some(origin),
            first_face: None,
            decal: false,
        });
    };
    let transform = if class.is_some_and(|class| class.light_prop) {
        light_prop_transform(origin, entity)
    } else {
        entity_transform(origin, entity)
    };
    let local = Bounds {
        min: DVec3::from_array(model.min.map(f64::from)),
        max: DVec3::from_array(model.max.map(f64::from)),
    };
    let edges: Vec<[DVec3; 2]> = box_edges(&local)
        .into_iter()
        .map(|[a, b]| [transform.transform_point3(a), transform.transform_point3(b)])
        .collect();
    let own_model = entity
        .get("model")
        .is_some_and(|path| path.trim().to_ascii_lowercase().ends_with(".mdl"));
    let hull = class.and_then(|class| class.size).filter(|_| !own_model);
    let (edges, bounds) = match hull {
        Some((min, max)) => {
            let bounds = Bounds {
                min: origin + DVec3::from_array(min),
                max: origin + DVec3::from_array(max),
            };
            (box_edges(&bounds), bounds)
        }
        None => {
            let mut bounds = Bounds::EMPTY;
            for point in edges.iter().flatten() {
                bounds.add_point(*point);
            }
            (edges, bounds)
        }
    };
    let shadows_off = entity.get("disableshadows").is_some_and(|value| value.trim() == "1");
    Some(WireMesh {
        color,
        edges,
        faces: Vec::new(),
        planes: box_planes(&bounds),
        bounds,
        geometry_bounds: bounds,
        owner,
        solid: None,
        selected: false,
        model: Some(ModelInstance {
            key,
            transform,
            casts_shadows: own_model && !shadows_off,
            helper: !own_model,
        }),
        sprite: None,
        origin: Some(origin),
        first_face: None,
        decal: false,
    })
}

/// Where each brush face is among `meshes`, by side id: the mesh's index and the face's
/// index in it.
fn faces_by_side(meshes: &[WireMesh]) -> HashMap<u32, (usize, usize)> {
    meshes
        .iter()
        .enumerate()
        .flat_map(|(mesh_index, mesh)| {
            mesh.faces
                .iter()
                .enumerate()
                .filter_map(move |(face_index, face)| Some((face.side?, (mesh_index, face_index))))
        })
        .collect()
}

/// The side ids an overlay's `sides` key lists.
fn overlay_sides(entity: &Entity) -> impl Iterator<Item = u32> + '_ {
    entity
        .get("sides")
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|word| word.parse::<u32>().ok())
}

/// The size in world units of the picture a decal entity projects.
fn decal_size(material: &str, sizes: &DecalSizes) -> DVec2 {
    sizes
        .get(&decal_key(material))
        .copied()
        .flatten()
        .unwrap_or(DVec2::splat(DEFAULT_DECAL_SIZE))
}

/// A decal entity's origin and how far from it a brush can be and still carry part of
/// its picture.
fn decal_reach(entity: &Entity, sizes: &DecalSizes) -> Option<(DVec3, f64)> {
    let size = decal_size(entity.get("texture")?, sizes);
    Some((entity.origin()?, size.length().max(overlay::DECAL_REACH)))
}

/// Whether `point` is inside `bounds` or no further than `reach` from it along each axis.
fn within(bounds: &Bounds, point: DVec3, reach: f64) -> bool {
    let reach = DVec3::splat(reach);
    point.cmpge(bounds.min - reach).all() && point.cmple(bounds.max + reach).all()
}

/// The picture of one overlay or decal entity, cut to the faces of `brushes` it lies
/// on. `by_side` is [`faces_by_side`] of the same brushes, which overlays need. `None`
/// for an entity without a material or a place.
fn picture_mesh(
    entity: &Entity,
    brushes: &[WireMesh],
    by_side: Option<&HashMap<u32, (usize, usize)>>,
    game_data: Option<&GameData>,
    sizes: &DecalSizes,
) -> Option<WireMesh> {
    let (material, placed, pieces) = if is_overlay(entity) {
        let pieces = by_side.map_or_else(Vec::new, |by_side| overlay_pieces(entity, brushes, by_side));
        (entity.get("material"), overlay::Overlay::of(entity), pieces)
    } else {
        let (placed, pieces) = decal_pieces(entity, brushes, sizes);
        (entity.get("texture"), placed, pieces)
    };
    let material = material.filter(|name| !name.trim().is_empty())?;
    let placed = placed?;
    let class = game_data.and_then(|data| data.class(entity.classname()));
    let color = entity.editor_color().or_else(|| class.and_then(|class| class.color));
    Some(decal_mesh(entity, material, &placed, pieces, to_color(color)))
}

/// The pieces of an overlay on the faces its `sides` key lists.
fn overlay_pieces(entity: &Entity, brushes: &[WireMesh], by_side: &HashMap<u32, (usize, usize)>) -> Vec<Piece> {
    let Some(placed) = overlay::Overlay::of(entity) else {
        return Vec::new();
    };
    let texture = overlay::texture_corners(entity);
    let mut pieces = Vec::new();
    for side in overlay_sides(entity) {
        if let Some(face) = by_side
            .get(&side)
            .and_then(|(mesh, face)| brushes.get(*mesh)?.faces.get(*face))
        {
            placed.pieces_on(&texture, &face.vertices, face.normal, &mut pieces);
        }
    }
    pieces
}

/// Where a decal lands and its pieces: on the brush face nearest to it within
/// [`overlay::DECAL_REACH`], and on the faces that lie in the same plane. Tool faces
/// are passed over, as the game's trace passes through most of them.
fn decal_pieces(entity: &Entity, brushes: &[WireMesh], sizes: &DecalSizes) -> (Option<overlay::Overlay>, Vec<Piece>) {
    let (Some(origin), Some(material)) = (entity.origin(), entity.get("texture")) else {
        return (None, Vec::new());
    };
    let size = decal_size(material, sizes);
    let Some((_, point, normal)) = nearest_face(brushes, origin, overlay::DECAL_REACH) else {
        return (Some(overlay::Overlay::on_surface(origin, DVec3::Z, size)), Vec::new());
    };
    let placed = overlay::Overlay::on_surface(point, normal, size);
    let texture = overlay::UPRIGHT_TEXTURE;
    let mut pieces = Vec::new();
    for face in brushes
        .iter()
        .filter(|mesh| reaches(mesh, origin, size.length()))
        .flat_map(decal_targets)
    {
        let same_plane = face.normal.dot(normal) > DECAL_SAME_FACING
            && face
                .vertices
                .first()
                .is_some_and(|vertex| (*vertex - point).dot(normal).abs() < DECAL_PICK_DEPTH);
        if same_plane {
            placed.pieces_on(&texture, &face.vertices, face.normal, &mut pieces);
        }
    }
    (Some(placed), pieces)
}

/// The mesh of a brush. A brush with displacements shows only them, as VBSP leaves its
/// other faces out of the game: their surfaces are its faces, their triangles its edges,
/// and rays are tested against the triangles instead of the brush's planes.
fn brush_mesh(solid: &Solid, color: Color32, owner: Option<Owner>) -> Option<WireMesh> {
    let faces = solid_faces(solid);
    if faces.is_empty() {
        return None;
    }
    let sides: Vec<&Side> = solid.sides().collect();
    let surfaces: Vec<Option<Arc<Surface>>> = faces
        .iter()
        .map(|face| {
            let displacement = sides.get(face.side)?.displacement()?;
            Surface::new(&displacement, &face.vertices, face.plane.normal).map(Arc::new)
        })
        .collect();
    let displaced = surfaces.iter().any(Option::is_some);
    let mut seen = HashSet::new();
    let mut edges = Vec::new();
    let mut add_edge = |[a, b]: [DVec3; 2]| {
        let (qa, qb) = (quantize(a), quantize(b));
        let key = if qa <= qb { (qa, qb) } else { (qb, qa) };
        if seen.insert(key) {
            edges.push([a, b]);
        }
    };
    if displaced {
        for surface in surfaces.iter().flatten() {
            surface.edges().into_iter().for_each(&mut add_edge);
        }
    } else {
        for face in &faces {
            let count = face.vertices.len();
            for index in 0..count {
                add_edge([face.vertices[index], face.vertices[(index + 1) % count]]);
            }
        }
    }
    let geometry_bounds = faces_bounds(&faces);
    let mut bounds = geometry_bounds;
    for surface in surfaces.iter().flatten() {
        bounds.add_bounds(&surface.bounds);
    }
    // Faces and planes stay in the same order, so the plane a ray enters through names
    // the face it hit.
    let (scene_faces, planes): (Vec<SceneFace>, Vec<Plane>) = faces
        .iter()
        .zip(surfaces)
        .filter(|(_, surface)| !displaced || surface.is_some())
        .filter_map(|(face, surface)| {
            let side = sides.get(face.side)?;
            let scene_face = SceneFace {
                material: Some(side.material.clone()),
                vertices: face.vertices.clone(),
                normal: face.plane.normal,
                uaxis: side.uaxis,
                vaxis: side.vaxis,
                side: Some(side.id),
                solid: solid.id(),
                selected: false,
                lightmap_scale: f64::from(side.lightmap_scale.unwrap_or(16).max(1)),
                decal: false,
                displacement: surface,
            };
            Some((scene_face, face.plane))
        })
        .unzip();
    Some(WireMesh {
        color,
        edges,
        faces: scene_faces,
        bounds,
        geometry_bounds,
        planes: if displaced { Vec::new() } else { planes },
        owner,
        solid: solid.id(),
        selected: false,
        model: None,
        sprite: None,
        origin: None,
        first_face: None,
        decal: false,
    })
}

/// Whether a point is inside a mesh's bounds grown by `reach` on every side.
fn reaches(mesh: &WireMesh, point: DVec3, reach: f64) -> bool {
    let reach = DVec3::splat(reach);
    point.cmpge(mesh.bounds.min - reach).all() && point.cmple(mesh.bounds.max + reach).all()
}

/// The face among `meshes` nearest to `point` and within `reach` of it, as
/// [`Scene::nearest_face`] gives it. Between faces just as near, as at the corner of a
/// brush, the one that faces the point most squarely wins.
fn nearest_face(meshes: &[WireMesh], point: DVec3, reach: f64) -> Option<(u32, DVec3, DVec3)> {
    meshes
        .iter()
        .filter(|mesh| !mesh.decal && reaches(mesh, point, reach))
        .flat_map(decal_targets)
        .filter_map(|face| {
            let (distance, nearest) = overlay::nearest_on_polygon(point, &face.vertices, face.normal)?;
            let facing = face.normal.dot((point - nearest).normalize_or_zero());
            (distance <= reach).then_some((distance - facing * FACING_BONUS, face.side?, nearest, face.normal))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, side, nearest, normal)| (side, nearest, normal))
}

/// The faces of a brush a decal can land on: all but tool faces.
fn decal_targets(mesh: &WireMesh) -> impl Iterator<Item = &SceneFace> {
    mesh.faces.iter().filter(|face| {
        face.side.is_some()
            && !face
                .material
                .as_deref()
                .is_some_and(|material| decal_key(material).starts_with("tools/"))
    })
}

/// The mesh of an overlay's or decal's picture: its pieces as faces, their outline as
/// edges, and for picking the slab of its quad just in front of and behind the faces.
/// Without pieces, as when it lists no faces, hangs in the air or is wound so that the
/// game leaves it out, only the quad's outline is drawn, so it can still be found and
/// picked.
fn decal_mesh(
    entity: &Entity,
    material: &str,
    placed: &overlay::Overlay,
    pieces: Vec<Piece>,
    color: Color32,
) -> WireMesh {
    let corners = placed.world_corners();
    let mut counts: HashMap<EdgeKey, ([DVec3; 2], usize)> = HashMap::new();
    for piece in &pieces {
        let count = piece.vertices.len();
        for index in 0..count {
            let (a, b) = (piece.vertices[index], piece.vertices[(index + 1) % count]);
            let (qa, qb) = (quantize(a), quantize(b));
            let key = if qa <= qb { (qa, qb) } else { (qb, qa) };
            counts.entry(key).or_insert(([a, b], 0)).1 += 1;
        }
    }
    let mut edges: Vec<[DVec3; 2]> = counts
        .into_values()
        .filter_map(|(edge, count)| (count == 1).then_some(edge))
        .collect();
    let mut bounds = Bounds::EMPTY;
    for vertex in pieces.iter().flat_map(|piece| &piece.vertices) {
        bounds.add_point(*vertex);
    }
    if pieces.is_empty() {
        edges = (0..4).map(|index| [corners[index], corners[(index + 1) % 4]]).collect();
        for corner in corners {
            bounds.add_point(corner);
        }
    }
    let (normal, on_face) = pieces.first().map_or((placed.normal, placed.origin), |piece| {
        (piece.normal, piece.vertices[0])
    });
    let centre = corners.iter().sum::<DVec3>() * 0.25;
    let mut planes = vec![
        Plane {
            normal,
            dist: normal.dot(on_face) + DECAL_PICK_DEPTH,
        },
        Plane {
            normal: -normal,
            dist: -normal.dot(on_face) + DECAL_PICK_DEPTH,
        },
    ];
    for index in 0..4 {
        let (a, b) = (corners[index], corners[(index + 1) % 4]);
        if let Some(mut outward) = (b - a).cross(placed.normal).try_normalize() {
            if outward.dot(centre - a) > 0.0 {
                outward = -outward;
            }
            planes.push(Plane {
                normal: outward,
                dist: outward.dot(a),
            });
        }
    }
    let faces = pieces
        .into_iter()
        .map(|piece| SceneFace {
            material: Some(material.to_owned()),
            vertices: piece.vertices,
            normal: piece.normal,
            uaxis: piece.uaxis,
            vaxis: piece.vaxis,
            side: None,
            solid: None,
            selected: false,
            lightmap_scale: 16.0,
            decal: true,
            displacement: None,
        })
        .collect();
    WireMesh {
        color,
        edges,
        faces,
        bounds,
        geometry_bounds: bounds,
        planes,
        owner: entity.id().map(Owner::Entity),
        solid: None,
        selected: false,
        model: None,
        sprite: None,
        origin: entity.origin(),
        first_face: None,
        decal: true,
    }
}

/// The 12 edges of an axis-aligned box.
pub fn box_edges(bounds: &Bounds) -> Vec<[DVec3; 2]> {
    let (min, max) = (bounds.min, bounds.max);
    let corner = |x: bool, y: bool, z: bool| {
        DVec3::new(
            if x { max.x } else { min.x },
            if y { max.y } else { min.y },
            if z { max.z } else { min.z },
        )
    };
    let mut edges = Vec::with_capacity(12);
    for a in [false, true] {
        for b in [false, true] {
            edges.push([corner(false, a, b), corner(true, a, b)]);
            edges.push([corner(a, false, b), corner(a, true, b)]);
            edges.push([corner(a, b, false), corner(a, b, true)]);
        }
    }
    edges
}

/// The six outward-facing quads of an axis-aligned box, without a material.
fn box_faces(bounds: &Bounds) -> Vec<SceneFace> {
    let flat_axis = TexAxis {
        axis: DVec3::ZERO,
        offset: 0.0,
        scale: 1.0,
    };
    box_planes(bounds)
        .into_iter()
        .map(|plane| {
            let normal = plane.normal;
            let (u, v) = if normal.z.abs() > 0.5 {
                (DVec3::X, DVec3::Y)
            } else {
                (DVec3::Z, normal.cross(DVec3::Z))
            };
            let center = bounds.center() + normal * (bounds.size() * 0.5).dot(normal.abs());
            let half = bounds.size() * 0.5;
            let (du, dv) = (u * half.dot(u.abs()), v * half.dot(v.abs()));
            let mut vertices = vec![center - du - dv, center + du - dv, center + du + dv, center - du + dv];
            if (vertices[1] - vertices[0]).cross(vertices[2] - vertices[0]).dot(normal) < 0.0 {
                vertices.reverse();
            }
            SceneFace {
                material: None,
                vertices,
                normal,
                uaxis: flat_axis,
                vaxis: flat_axis,
                side: None,
                solid: None,
                selected: false,
                lightmap_scale: 16.0,
                decal: false,
                displacement: None,
            }
        })
        .collect()
}

/// Model space to world for a prop: `origin`, then `angles` (pitch, yaw, roll in degrees,
/// applied roll first, then pitch, then yaw, like the engine), then `modelscale`.
pub fn entity_transform(origin: DVec3, entity: &Entity) -> DMat4 {
    let [pitch, yaw, roll] = entity
        .get("angles")
        .and_then(parse_vec3)
        .map_or([0.0; 3], |angles| angles.to_array().map(f64::to_radians));
    let scale = entity
        .get("modelscale")
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|scale| *scale > 0.0)
        .unwrap_or(1.0);
    let rotation = DMat3::from_rotation_z(yaw) * DMat3::from_rotation_y(pitch) * DMat3::from_rotation_x(roll);
    DMat4::from_translation(origin) * DMat4::from_mat3(rotation) * DMat4::from_scale(DVec3::splat(scale))
}

/// Model space to world for a `lightprop()` model, such as light_spot's lamp: turned so
/// its front points the way the light shines, as the compiler reads `angles`, `angle`
/// and `pitch`, keeping the roll from `angles`.
fn light_prop_transform(origin: DVec3, entity: &Entity) -> DMat4 {
    let direction = mallet_vmf::light::vrad_direction(entity);
    let roll = entity
        .get("angles")
        .and_then(parse_vec3)
        .map_or(0.0, |angles| angles.z.to_radians());
    let yaw = direction.y.atan2(direction.x);
    let pitch = -direction.z.clamp(-1.0, 1.0).asin();
    let rotation = DMat3::from_rotation_z(yaw) * DMat3::from_rotation_y(pitch) * DMat3::from_rotation_x(roll);
    DMat4::from_translation(origin) * DMat4::from_mat3(rotation)
}

static GENERATION: AtomicU64 = AtomicU64::new(1);

pub fn next_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::Relaxed)
}

fn box_planes(bounds: &Bounds) -> Vec<Plane> {
    [DVec3::X, DVec3::Y, DVec3::Z]
        .into_iter()
        .flat_map(|axis| {
            [
                Plane {
                    normal: axis,
                    dist: axis.dot(bounds.max),
                },
                Plane {
                    normal: -axis,
                    dist: -axis.dot(bounds.min),
                },
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use mallet_content::{ModelMesh, ModelVertex};

    use super::*;

    fn box_mesh(owner: Owner, min: DVec3, max: DVec3) -> WireMesh {
        let bounds = Bounds { min, max };
        WireMesh {
            color: DEFAULT_COLOR,
            edges: box_edges(&bounds),
            faces: Vec::new(),
            planes: box_planes(&bounds),
            bounds,
            geometry_bounds: bounds,
            owner: Some(owner),
            solid: None,
            selected: false,
            model: None,
            sprite: None,
            origin: None,
            first_face: None,
            decal: false,
        }
    }

    /// A prop whose model is one upright triangle in the plane x = 0, filling only the
    /// lower corner of its 64-unit bounding box.
    fn fence() -> (WireMesh, ModelCache) {
        let vertex = |position: [f32; 3]| ModelVertex {
            position,
            normal: [1.0, 0.0, 0.0],
            uv: [0.0, 0.0],
        };
        let model = Model {
            vertices: vec![
                vertex([0.0, -32.0, 0.0]),
                vertex([0.0, 0.0, 0.0]),
                vertex([0.0, -32.0, 32.0]),
            ],
            meshes: vec![ModelMesh {
                material: "fence".to_owned(),
                indices: vec![0, 1, 2],
            }],
            min: [-32.0, -32.0, 0.0],
            max: [32.0, 32.0, 64.0],
        };
        let transform = DMat4::from_translation(DVec3::new(100.0, 0.0, 0.0));
        let mut prop = box_mesh(
            Owner::Entity(7),
            DVec3::new(68.0, -32.0, 0.0),
            DVec3::new(132.0, 32.0, 64.0),
        );
        prop.model = Some(ModelInstance {
            key: "fence.mdl".to_owned(),
            transform,
            casts_shadows: true,
            helper: false,
        });
        let models = ModelCache::from([("fence.mdl".to_owned(), Some(Arc::new(model)))]);
        (prop, models)
    }

    #[test]
    fn props_are_picked_by_their_triangles_before_their_box() {
        let (prop, models) = fence();
        let wall = box_mesh(
            Owner::Solid(1),
            DVec3::new(200.0, -64.0, 0.0),
            DVec3::new(216.0, 64.0, 64.0),
        );
        let scene = Scene {
            meshes: vec![prop.clone(), wall],
            ..Scene::default()
        };
        let along_x = DVec3::X;
        // Through the model's triangle: the prop, even though the wall is behind it.
        let on_model = scene.pick_ray(DVec3::new(0.0, -20.0, 8.0), along_x, &models, false);
        assert_eq!(on_model, Some(Owner::Entity(7)));
        // Through the empty part of its box: the wall behind it.
        let through_gap = scene.pick_ray(DVec3::new(0.0, 20.0, 40.0), along_x, &models, false);
        assert_eq!(through_gap, Some(Owner::Solid(1)));
        // Nothing behind it: the box still picks the prop.
        let alone = Scene {
            meshes: vec![prop],
            ..Scene::default()
        };
        assert_eq!(
            alone.pick_ray(DVec3::new(0.0, 20.0, 40.0), along_x, &models, false),
            Some(Owner::Entity(7))
        );
        assert_eq!(alone.pick_ray(DVec3::new(0.0, 200.0, 40.0), along_x, &models, false), None);
    }
}
