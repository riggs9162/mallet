//! Typed, lossless model of Hammer's VMF map format, plus brush geometry helpers.
//!
//! [`Vmf`] keeps every block it does not understand verbatim, so loading and saving a
//! map changes nothing except number formatting that parses back to the same value.

pub mod build;
pub mod carve;
pub mod clip;
pub mod displacement;
pub mod edit;
pub mod geometry;
pub mod hollow;
pub mod hull;
pub mod light;
pub mod lightmap;
mod model;
pub mod overlay;
pub mod primitive;
pub mod sides;
pub mod texture;
pub mod transform;
pub mod vertex;
mod view;

pub use mallet_kv as kv;
pub use model::*;
pub use view::SavedCamera;
