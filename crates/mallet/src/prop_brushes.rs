//! Mesh to Geometry: rebuilding prop entities as brushes.
//!
//! A prop's shape comes either from its collision model, whose pieces are convex already
//! and map one to one onto brushes, or from its visual mesh, which is split into nearly
//! convex parts first. Either way each convex shape becomes one brush, placed where the
//! prop's origin, angles and scale put the model.

use glam::DVec3;
use mallet_content::load_collision;
use mallet_vmf::build::convex_solid;
use mallet_vmf::edit::Edit;
use mallet_vmf::hull::{convex_hull, convex_parts, Hull};
use mallet_vmf::{Entity, EntityItem, IdAllocator, Solid};

use crate::editor::{find_entity, Editor, SelectMode};
use crate::scene::{entity_transform, entity_variant, variant_key, Owner};

/// The most sides a generated brush gets; curved surfaces are flattened to fit.
const MAX_BRUSH_SIDES: usize = 32;

/// How far, in units, the hulls of a visual mesh's parts may stray from its surface
/// before a part is split further.
const MESH_TOLERANCE: f64 = 1.0;

/// How deep, in units, a single-sided panel of a visual mesh is made.
const PANEL_THICKNESS: f64 = 1.0;

/// The most brushes one prop's visual mesh is split into.
const MAX_MESH_BRUSHES: usize = 128;

/// Where Mesh to Geometry takes a prop's shape from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropShape {
    /// The collision model (`.phy`): few brushes, matching what players bump into.
    Collision,
    /// The visual mesh: follows the model's look more closely, with more brushes.
    Mesh,
}

impl PropShape {
    pub fn parse(name: &str) -> Result<Self, String> {
        match name.trim().to_ascii_lowercase().as_str() {
            "collision" => Ok(Self::Collision),
            "mesh" | "visual" => Ok(Self::Mesh),
            other => Err(format!("unknown shape \"{other}\"; use collision or mesh")),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Collision => "collision model",
            Self::Mesh => "visual mesh",
        }
    }
}

/// How Mesh to Geometry places its brushes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PropBrushOptions {
    /// Put the brushes in a `func_detail` per prop, rather than in the world.
    pub detail: bool,
    /// Leave the props in the map next to their new brushes.
    pub keep_props: bool,
}

impl Default for PropBrushOptions {
    fn default() -> Self {
        Self {
            detail: true,
            keep_props: false,
        }
    }
}

impl Editor {
    /// The selected entities that show their own studio model, which Mesh to Geometry
    /// can turn into brushes.
    pub fn selected_props(&self) -> Vec<u32> {
        let Some(document) = &self.document else {
            return Vec::new();
        };
        self.selection
            .iter()
            .filter_map(|owner| match owner {
                Owner::Entity(id) => Some(*id),
                Owner::Solid(_) => None,
            })
            .filter(|id| find_entity(&document.vmf, *id).is_ok_and(|entity| prop_model(entity).is_some()))
            .collect()
    }

    /// Rebuilds the selected props as brushes with the current material, as one undo step,
    /// and selects the new brushes. Returns how many brushes were made.
    pub fn props_to_brushes(&mut self, shape: PropShape, options: PropBrushOptions) -> Result<usize, String> {
        let props = self.selected_props();
        if props.is_empty() {
            return Err("select a prop with a model first".to_owned());
        }
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let world_id = vmf.world().and_then(Entity::id).ok_or("the map has no world")?;
        let mut ids = IdAllocator::for_map(vmf);
        let mut edits = Vec::new();
        let mut created = Vec::new();
        let mut brushes = 0;
        let mut skipped = Vec::new();
        for id in props {
            let entity = find_entity(vmf, id)?;
            let hulls = match self.prop_hulls(entity, shape) {
                Ok(hulls) => hulls,
                Err(error) => {
                    skipped.push(error);
                    continue;
                }
            };
            let solids: Vec<Solid> = hulls
                .iter()
                .filter_map(|hull| {
                    let planes = hull.brush_planes(MAX_BRUSH_SIDES);
                    convex_solid(&planes, hull.center(), &self.material, &mut ids)
                })
                .collect();
            if solids.is_empty() {
                skipped.push(format!(
                    "{} gave no usable brushes",
                    prop_model(entity).unwrap_or_default()
                ));
                continue;
            }
            brushes += solids.len();
            if options.detail {
                let detail_id = ids.object();
                let mut detail = Entity::default();
                detail.set("id", detail_id.to_string());
                detail.set("classname", "func_detail");
                detail.items.extend(solids.into_iter().map(EntityItem::Solid));
                edits.push(Edit::AddEntity {
                    index: None,
                    entity: detail,
                });
                created.push(Owner::Entity(detail_id));
            } else {
                for solid in solids {
                    created.extend(solid.id().map(Owner::Solid));
                    edits.push(Edit::AddSolid {
                        entity: world_id,
                        index: None,
                        solid,
                    });
                }
            }
            if !options.keep_props {
                edits.push(Edit::RemoveEntity { entity: id });
            }
        }
        if edits.is_empty() {
            return Err(skipped.join("; "));
        }
        self.apply_edit(&Edit::Batch(edits))?;
        self.select(created, SelectMode::Replace);
        let noun = if brushes == 1 { "brush" } else { "brushes" };
        self.message = format!("Built {brushes} {noun} from the {}", shape.label());
        if !skipped.is_empty() {
            self.message = format!("{}; skipped: {}", self.message, skipped.join("; "));
        }
        Ok(brushes)
    }

    /// The convex shapes, in world space, that cover a prop.
    fn prop_hulls(&self, entity: &Entity, shape: PropShape) -> Result<Vec<Hull>, String> {
        let path = prop_model(entity).ok_or("the entity has no model")?;
        let origin = entity.origin().ok_or_else(|| format!("{path} has no origin"))?;
        let transform = entity_transform(origin, entity);
        let place = |point: [f32; 3]| transform.transform_point3(DVec3::from_array(point.map(f64::from)));
        match shape {
            PropShape::Collision => {
                let materials = self.materials.as_ref().ok_or("choose a game folder first")?;
                let collision = load_collision(materials.filesystem(), path)?;
                if collision.solids.len() > 1 {
                    return Err(format!(
                        "{path} is a ragdoll, whose collision parts move with its bones; use the visual mesh"
                    ));
                }
                let hulls: Vec<Hull> = collision
                    .solids
                    .iter()
                    .flat_map(|solid| &solid.pieces)
                    .filter_map(|piece| convex_hull(&piece.iter().map(|point| place(*point)).collect::<Vec<_>>()))
                    .collect();
                if hulls.is_empty() {
                    return Err(format!("{path} has an empty collision model"));
                }
                Ok(hulls)
            }
            PropShape::Mesh => {
                let model = self
                    .models
                    .get(&variant_key(path, entity_variant(entity)))
                    .cloned()
                    .flatten()
                    .ok_or_else(|| format!("{path} is not loaded"))?;
                let positions: Vec<DVec3> = model.vertices.iter().map(|vertex| place(vertex.position)).collect();
                let triangles: Vec<[u32; 3]> = model
                    .meshes
                    .iter()
                    .flat_map(|mesh| mesh.indices.as_chunks::<3>().0)
                    .copied()
                    .collect();
                Ok(convex_parts(
                    &positions,
                    &triangles,
                    MESH_TOLERANCE,
                    PANEL_THICKNESS,
                    MAX_MESH_BRUSHES,
                ))
            }
        }
    }
}

/// The studio model an entity shows itself, such as a `prop_static`'s `model` key; not the
/// editor helpers FGD classes draw.
fn prop_model(entity: &Entity) -> Option<&str> {
    entity
        .get("model")
        .map(str::trim)
        .filter(|path| path.to_ascii_lowercase().ends_with(".mdl"))
}
