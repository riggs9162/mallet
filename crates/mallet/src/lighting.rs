//! Choices of the lighting preview that Mallet Options and the viewport's settings
//! offer, and the texture lights a game and map give off, read from `lights.rad`.

use std::collections::HashMap;
use std::path::Path;

use mallet_content::GameFs;
use mallet_vmf::light::{parse_lights_rad, TextureLight};
use serde::{Deserialize, Serialize};

/// Texture lights by lower-case material name.
pub type TextureLights = HashMap<String, TextureLight>;

/// How light brighter than full white looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tonemap {
    /// Cut off at white, as Source draws maps compiled without HDR.
    #[default]
    Clamp,
    /// A filmic curve that rolls bright light off gently, closer to HDR.
    Filmic,
}

impl Tonemap {
    pub const ALL: [Self; 2] = [Self::Clamp, Self::Filmic];

    pub fn label(self) -> &'static str {
        match self {
            Self::Clamp => "Clamp, like LDR",
            Self::Filmic => "Filmic",
        }
    }
}

/// How finely brush faces are lit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightingDetail {
    /// Once per luxel at each face's lightmap scale, baked in the background as VRAD
    /// bakes lightmaps, with every light casting shadows.
    #[default]
    Luxels,
    /// Every pixel, live, with shadows from the lights around the camera.
    Pixels,
}

impl LightingDetail {
    pub const ALL: [Self; 2] = [Self::Luxels, Self::Pixels];

    pub fn label(self) -> &'static str {
        match self {
            Self::Luxels => "Luxels, like the game",
            Self::Pixels => "Every pixel",
        }
    }
}

/// Which lights show their range, cone or direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperMode {
    Off,
    /// Only selected lights, as Hammer draws its light helpers.
    #[default]
    Selected,
    All,
}

impl HelperMode {
    pub const ALL: [Self; 3] = [Self::Off, Self::Selected, Self::All];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Selected => "Selected lights",
            Self::All => "All lights",
        }
    }
}

/// The texture lights VRAD would use for a map: the game's `lights.rad`, then the map's
/// own `<map>.rad` beside it, whose lines win.
pub fn load_texture_lights(filesystem: Option<&GameFs>, map: Option<&Path>) -> TextureLights {
    let mut lights = TextureLights::new();
    if let Some(data) = filesystem.and_then(|filesystem| filesystem.read("lights.rad")) {
        lights.extend(parse_lights_rad(&String::from_utf8_lossy(&data)));
    }
    if let Some(text) = map.and_then(|map| std::fs::read(map.with_extension("rad")).ok()) {
        lights.extend(parse_lights_rad(&String::from_utf8_lossy(&text)));
    }
    lights
}
