//! Studio models (`.mdl` with its `.vvd` and `.vtx`), as placed by prop entities.

use vmdl::{Mdl, Vtx, Vvd};

use crate::vpk::normalize;
use crate::GameFs;

/// One vertex of a model, in the model's own space (X forward, Y left, Z up).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

/// The triangles of a model that share one material.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelMesh {
    /// Material path under `materials/`, without extension, such as
    /// `models/props_c17/oildrum001`.
    pub material: String,
    /// Triangle list indices into [`Model::vertices`].
    pub indices: Vec<u32>,
}

/// A model's highest-detail geometry with its default skin.
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub vertices: Vec<ModelVertex>,
    pub meshes: Vec<ModelMesh>,
    /// Smallest corner of the model's box: the hull the model was compiled with, as
    /// Hammer and the game use, or the geometry's bounds when it has none.
    pub min: [f32; 3],
    /// Largest corner of the model's box.
    pub max: [f32; 3],
}

impl Model {
    /// Number of triangles in all meshes.
    pub fn triangle_count(&self) -> usize {
        self.meshes.iter().map(|mesh| mesh.indices.len() / 3).sum()
    }
}

/// Which look of a model to load.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ModelVariant {
    /// The skin family, as an entity's `skin` key numbers them. One the model lacks
    /// loads the first.
    pub skin: usize,
    /// The body groups to show, as an entity's `body` key packs them. `None` loads the
    /// sub-models of every group at once.
    pub body: Option<u32>,
}

/// One body group of a model: a part that can be swapped between sub-models.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyGroup {
    /// The sub-models the group chooses between, by the name they were compiled with. A
    /// sub-model without meshes shows nothing, as the `blank` choice of a group does.
    pub choices: Vec<String>,
}

/// What a model is, beyond its geometry, for the model browser.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelDetails {
    /// How many skin families the model has; at least one.
    pub skins: usize,
    pub body_groups: Vec<BodyGroup>,
    /// Compiled with `$staticprop`, as `prop_static` needs.
    pub static_prop: bool,
    /// The surface property the model was compiled with, such as `metal`.
    pub surface_prop: String,
    pub bones: usize,
    /// The names of the model's animation sequences.
    pub sequences: Vec<String>,
    pub vertices: usize,
}

impl ModelDetails {
    /// The `body` value that shows the given choice of each body group.
    pub fn body_value(&self, choices: &[usize]) -> u32 {
        let mut value = 0_u32;
        let mut base = 1_u32;
        for (group, choice) in self.body_groups.iter().zip(choices) {
            let count = group.choices.len().max(1) as u32;
            value = value.saturating_add(base.saturating_mul((*choice as u32).min(count - 1)));
            base = base.saturating_mul(count);
        }
        value
    }
}

/// The choice a `body` value makes in each body group with these many sub-models.
fn body_choices(counts: &[usize], body: u32) -> Vec<usize> {
    let mut base = 1_u32;
    counts
        .iter()
        .map(|count| {
            let count = (*count).max(1) as u32;
            let choice = (body / base.max(1)) % count;
            base = base.saturating_mul(count);
            choice as usize
        })
        .collect()
}

/// Where the flags of a compiled model lie in its `.mdl` file.
const FLAGS_OFFSET: u64 = 152;
/// The flag `$staticprop` sets.
const STATIC_PROP_FLAG: u32 = 0x10;

/// Whether a model was compiled with `$staticprop`, read from the start of its `.mdl`
/// alone. `None` when the file is missing or cut short.
pub fn is_static_prop(filesystem: &GameFs, path: &str) -> Option<bool> {
    let file = filesystem.open(&normalize(path))?;
    let bytes: [u8; 4] = file.read(FLAGS_OFFSET, 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes) & STATIC_PROP_FLAG != 0)
}

/// Loads a model such as `models/props_c17/oildrum001.mdl` from the game's files.
///
/// Uses the first level of detail, the default skin and the model's idle orientation.
/// Each mesh's material is looked up in the model's material folders in order, like the
/// engine does.
pub fn load_model(filesystem: &GameFs, path: &str) -> Result<Model, String> {
    load_model_variant(filesystem, path, ModelVariant::default()).map(|(model, _)| model)
}

/// Loads a model like [`load_model`], with the chosen skin and body groups, and says
/// what the model is.
pub fn load_model_variant(
    filesystem: &GameFs,
    path: &str,
    variant: ModelVariant,
) -> Result<(Model, ModelDetails), String> {
    // The parser panics on some model features it does not support yet, such as
    // animations stored in a separate block, so a panic counts as a failed load.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read_model(filesystem, path, variant)))
        .unwrap_or_else(|_| Err(format!("{path} uses features the model parser does not support")))
}

fn read_model(filesystem: &GameFs, path: &str, variant: ModelVariant) -> Result<(Model, ModelDetails), String> {
    let path = normalize(path);
    let base = path.strip_suffix(".mdl").unwrap_or(&path);
    let read = |suffix: &str| {
        filesystem
            .read(&format!("{base}{suffix}"))
            .ok_or_else(|| format!("{base}{suffix} not found"))
    };
    let mdl = Mdl::read(&read(".mdl")?).map_err(|error| error.to_string())?;
    let vtx_data = read(".dx90.vtx").or_else(|_| read(".vtx"))?;
    let vtx = Vtx::read(&vtx_data).map_err(|error| error.to_string())?;
    let vvd = Vvd::read(&read(".vvd")?).map_err(|error| error.to_string())?;
    let body_groups: Vec<BodyGroup> = mdl
        .body_parts
        .iter()
        .map(|part| BodyGroup {
            choices: part.models.iter().map(|model| model.name.as_str().to_owned()).collect(),
        })
        .collect();
    let mesh_owners: Vec<(usize, usize)> = mdl
        .body_parts
        .iter()
        .enumerate()
        .flat_map(|(group, part)| {
            part.models
                .iter()
                .enumerate()
                .flat_map(move |(choice, model)| model.meshes.iter().map(move |_| (group, choice)))
        })
        .collect();
    let group_sizes: Vec<usize> = body_groups.iter().map(|group| group.choices.len()).collect();
    let shown = variant.body.map(|body| body_choices(&group_sizes, body));
    let mut details = ModelDetails {
        skins: 1,
        body_groups,
        static_prop: mdl.header.flags.contains(vmdl::mdl::ModelFlags::STATIC_PROP),
        surface_prop: mdl.surface_prop.clone(),
        bones: mdl.bones.len(),
        sequences: mdl
            .animation_sequences
            .iter()
            .map(|sequence| sequence.label.clone())
            .collect(),
        vertices: 0,
    };
    let model = vmdl::Model::from_parts(mdl, vtx, vvd);
    details.skins = model.skin_tables().count().max(1);
    let (hull_min, hull_max) = model.bounding_box();

    let vertices: Vec<ModelVertex> = model
        .vertices()
        .iter()
        .map(|vertex| {
            let position = model.apply_root_transform(vertex.position);
            let normal = model.apply_root_transform(vertex.normal);
            ModelVertex {
                position: [position.x, position.y, position.z],
                normal: [normal.x, normal.y, normal.z],
                uv: vertex.texture_coordinates,
            }
        })
        .collect();

    let directories: Vec<String> = model
        .texture_directories()
        .iter()
        .map(|directory| normalize(directory).trim_end_matches('/').to_owned())
        .collect();
    let skin = model
        .skin_tables()
        .nth(variant.skin)
        .or_else(|| model.skin_tables().next());
    let mut meshes = Vec::new();
    for (number, mesh) in model.meshes().enumerate() {
        let hidden = shown.as_ref().is_some_and(|shown| {
            mesh_owners
                .get(number)
                .is_some_and(|(group, choice)| shown.get(*group) != Some(choice))
        });
        if hidden {
            continue;
        }
        let index = mesh.material_index();
        let texture = skin
            .as_ref()
            .and_then(|skin| skin.texture(index))
            .or_else(|| model.textures().get(index as usize).map(|info| info.name.as_str()))
            .unwrap_or_default();
        let indices: Vec<u32> = mesh
            .vertex_strip_indices()
            .flatten()
            .filter(|index| *index < vertices.len())
            .map(|index| index as u32)
            .collect();
        if indices.len() < 3 {
            continue;
        }
        meshes.push(ModelMesh {
            material: resolve_material(filesystem, &directories, texture),
            indices,
        });
    }

    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in &vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex.position[axis]);
            max[axis] = max[axis].max(vertex.position[axis]);
        }
    }
    if vertices.is_empty() {
        return Err(format!("{base}.mdl has no vertices"));
    }
    let (hull_min, hull_max) = (
        [hull_min.x, hull_min.y, hull_min.z],
        [hull_max.x, hull_max.y, hull_max.z],
    );
    if (0..3).all(|axis| hull_max[axis] > hull_min[axis]) {
        (min, max) = (hull_min, hull_max);
    }
    details.vertices = vertices.len();
    Ok((
        Model {
            vertices,
            meshes,
            min,
            max,
        },
        details,
    ))
}

/// The first `directory/texture` that has a material file, or the first candidate when
/// none does, so the renderer shows its missing-texture pattern.
fn resolve_material(filesystem: &GameFs, directories: &[String], texture: &str) -> String {
    let texture = normalize(texture);
    let candidates: Vec<String> = if directories.is_empty() {
        vec![texture.clone()]
    } else {
        directories
            .iter()
            .map(|directory| {
                if directory.is_empty() {
                    texture.clone()
                } else {
                    format!("{directory}/{texture}")
                }
            })
            .collect()
    };
    candidates
        .iter()
        .find(|candidate| filesystem.exists(&format!("materials/{candidate}.vmt")))
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}
