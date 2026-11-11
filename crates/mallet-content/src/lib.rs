//! Reads Source engine game content the way the engine and Hammer find it.
//!
//! [`GameFs`] mounts the search paths listed in a game's `gameinfo.txt`: loose folders and
//! VPK archives, in priority order. [`Materials`] resolves material names to the texture
//! Hammer shows for them and decodes it from VTF, [`load_model`] reads prop models, [`load_collision`] their
//! collision models and [`GameData`] reads the entity classes of FGD files.

mod error;
pub mod fgd;
pub mod filesystem;
pub mod material;
pub mod model;
pub mod physics;
pub mod vpk;
pub mod vtf;

pub use error::ContentError;
pub use fgd::{default_fgd, Choice, ClassKind, EntityClass, GameData, Port, Property};
pub use filesystem::{mounted_games, GameFile, GameFs, Source, SourceKind};
pub use material::{
    parallel_map, AlphaMode, Material, MaterialInfo, MaterialLoader, Materials, SpriteLook, SpriteOrientation, Surface,
    Texture, Water,
};
pub use model::{
    is_static_prop, load_model, load_model_variant, BodyGroup, Model, ModelDetails, ModelMesh, ModelVariant,
    ModelVertex,
};
pub use physics::{load_collision, parse_collision, CollisionModel, CollisionSolid};
