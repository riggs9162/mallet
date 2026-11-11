//! Source's lighting entities, read the way VRAD and the engine read them.
//!
//! [`CATALOGUE`] lists every lighting class Mallet knows: the lights VRAD bakes into
//! lightmaps, the engine's dynamic lights, sprites and glows, the sky and sun, the
//! entities that set fog, tone mapping, colour correction and dynamic shadows, and the
//! helpers that tell the engine where to sample light. [`MapLighting::read`] turns a
//! map's entities into [`Light`]s with Source's colour, falloff, cone and direction maths,
//! so a preview lights surfaces the way a compile would, and [`parse_lights_rad`] reads
//! the texture lights a game lists in `lights.rad`.
//!
//! Brightness is measured so that 1 shows a texture at its full brightness: VRAD's
//! `255 255 255 200` light gives 200/255 at 100 units.

use std::collections::HashMap;

use glam::DVec3;

use crate::geometry::{faces_bounds, solid_faces, Bounds, Plane};
use crate::{parse_numbers, parse_vec3, Entity, Vmf};

/// Where a lighting class belongs in the catalogue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LightingGroup {
    /// Lights VRAD bakes into lightmaps.
    Baked,
    /// Lights the engine adds while the game runs.
    Dynamic,
    /// Sprites, glows, beams and the sky.
    Glow,
    /// Fog, tone mapping, colour correction and dynamic shadows.
    Atmosphere,
    /// Entities that tell the engine where to sample light and reflections.
    Helper,
}

impl LightingGroup {
    pub const ALL: [Self; 5] = [Self::Baked, Self::Dynamic, Self::Glow, Self::Atmosphere, Self::Helper];

    /// The group's heading in menus.
    pub fn label(self) -> &'static str {
        match self {
            Self::Baked => "Baked lights",
            Self::Dynamic => "Dynamic lights",
            Self::Glow => "Glows, beams and sky",
            Self::Atmosphere => "Fog, tone and shadows",
            Self::Helper => "Lighting helpers",
        }
    }
}

/// One lighting entity class of the catalogue.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightingClass {
    pub classname: &'static str,
    pub group: LightingGroup,
    /// What the entity does in the game.
    pub description: &'static str,
    /// What Mallet's lighting preview shows of it.
    pub preview: &'static str,
    /// Keyvalues a new entity of the class starts with when no FGD describes the class,
    /// the defaults of Source's own FGDs.
    pub defaults: &'static [(&'static str, &'static str)],
}

/// Keyvalues every baked light shares.
const LIGHT_DEFAULTS: [(&str, &str); 9] = [
    ("_light", "255 255 255 200"),
    ("_lightHDR", "-1 -1 -1 1"),
    ("_lightscaleHDR", "1"),
    ("style", "0"),
    ("_constant_attn", "0"),
    ("_linear_attn", "0"),
    ("_quadratic_attn", "1"),
    ("_fifty_percent_distance", "0"),
    ("_zero_percent_distance", "0"),
];

/// Every lighting entity class Source games use, in menu order.
pub const CATALOGUE: &[LightingClass] = &[
    LightingClass {
        classname: "light",
        group: LightingGroup::Baked,
        description: "An invisible point light that VRAD bakes into the lightmaps.",
        preview: "Lights the map with VRAD's colour, falloff and fifty and zero percent distances; casts shadows.",
        defaults: &[
            LIGHT_DEFAULTS[0],
            LIGHT_DEFAULTS[1],
            LIGHT_DEFAULTS[2],
            LIGHT_DEFAULTS[3],
            LIGHT_DEFAULTS[4],
            LIGHT_DEFAULTS[5],
            LIGHT_DEFAULTS[6],
            LIGHT_DEFAULTS[7],
            LIGHT_DEFAULTS[8],
            ("_hardfalloff", "0"),
            ("spawnflags", "0"),
            ("_distance", "0"),
        ],
    },
    LightingClass {
        classname: "light_spot",
        group: LightingGroup::Baked,
        description: "An invisible spotlight that VRAD bakes, with a bright inner cone that fades to an outer one.",
        preview: "Lights its cone with VRAD's falloff and focus; casts shadows.",
        defaults: &[
            LIGHT_DEFAULTS[0],
            LIGHT_DEFAULTS[1],
            LIGHT_DEFAULTS[2],
            LIGHT_DEFAULTS[3],
            LIGHT_DEFAULTS[4],
            LIGHT_DEFAULTS[5],
            LIGHT_DEFAULTS[6],
            LIGHT_DEFAULTS[7],
            LIGHT_DEFAULTS[8],
            ("_hardfalloff", "0"),
            ("_inner_cone", "30"),
            ("_cone", "45"),
            ("_exponent", "1"),
            ("_distance", "0"),
            ("pitch", "-90"),
            ("angles", "0 0 0"),
            ("spawnflags", "0"),
        ],
    },
    LightingClass {
        classname: "light_environment",
        group: LightingGroup::Baked,
        description: "Sunlight and sky light: the sun shines through sky faces, and the sky lights what can see it.",
        preview: "Sunlight with shadows, and ambient sky light traced from many directions.",
        defaults: &[
            ("pitch", "-45"),
            ("angles", "0 0 0"),
            ("_light", "255 255 255 200"),
            ("_lightHDR", "-1 -1 -1 1"),
            ("_lightscaleHDR", "1"),
            ("_ambient", "255 255 255 20"),
            ("_ambientHDR", "-1 -1 -1 1"),
            ("_AmbientScaleHDR", "1"),
            ("SunSpreadAngle", "0"),
        ],
    },
    LightingClass {
        classname: "light_directional",
        group: LightingGroup::Baked,
        description: "A directional light without falloff, like the sun of light_environment but without sky light.",
        preview: "Lights the map from its direction, with shadows.",
        defaults: &[
            ("pitch", "-45"),
            ("angles", "0 0 0"),
            ("_light", "255 255 255 200"),
            ("_lightHDR", "-1 -1 -1 1"),
            ("_lightscaleHDR", "0.7"),
            ("SunSpreadAngle", "0"),
        ],
    },
    LightingClass {
        classname: "light_dynamic",
        group: LightingGroup::Dynamic,
        description: "A dynamic light the engine draws as the game runs; it can change, move and be switched.",
        preview: "Without a cone, lights everything out to its maximum distance. With one, lights models in the                   cone and brushes in a round spot where its middle lands, as the game does. No shadows.",
        defaults: &[
            ("_light", "255 255 255 200"),
            ("brightness", "0"),
            ("_inner_cone", "30"),
            ("_cone", "45"),
            ("pitch", "-90"),
            ("distance", "120"),
            ("spotlight_radius", "80"),
            ("style", "0"),
            ("angles", "0 0 0"),
            ("spawnflags", "0"),
        ],
    },
    LightingClass {
        classname: "env_projectedtexture",
        group: LightingGroup::Dynamic,
        description: "A flashlight-style light that projects a texture and casts real-time shadows.",
        preview: "Projects its texture through its field of view out to its far distance, with shadows.",
        defaults: &[
            ("texturename", "effects/flashlight001"),
            ("textureframe", "0"),
            ("lightcolor", "255 255 255 200"),
            ("lightstrength", "1"),
            ("lightfov", "90"),
            ("nearz", "4"),
            ("farz", "750"),
            ("enableshadows", "1"),
            ("shadowquality", "1"),
            ("lightonlytarget", "0"),
            ("lightworld", "1"),
            ("cameraspace", "0"),
            ("angles", "0 0 0"),
            ("spawnflags", "1"),
        ],
    },
    LightingClass {
        classname: "point_spotlight",
        group: LightingGroup::Glow,
        description: "A visible spotlight beam with a halo; its dynamic light only reaches models.",
        preview: "Draws its beam and halo.",
        defaults: &[
            ("spotlightlength", "500"),
            ("spotlightwidth", "50"),
            ("rendercolor", "255 255 255"),
            ("renderamt", "255"),
            ("HDRColorScale", "1.0"),
            ("angles", "0 0 0"),
            ("spawnflags", "3"),
        ],
    },
    LightingClass {
        classname: "beam_spotlight",
        group: LightingGroup::Glow,
        description: "A client-side spotlight beam that can rotate, as on searchlights.",
        preview: "Draws its beam and halo.",
        defaults: &[
            ("spotlightlength", "500"),
            ("spotlightwidth", "50"),
            ("rendercolor", "255 255 255"),
            ("renderamt", "255"),
            ("HDRColorScale", "0.7"),
            ("maxspeed", "100"),
            ("angles", "0 0 0"),
            ("spawnflags", "1"),
        ],
    },
    LightingClass {
        classname: "env_sprite",
        group: LightingGroup::Glow,
        description: "A sprite, most often a glow around a lamp.",
        preview: "Drawn with its sprite, render mode, colour and scale, as the game draws it.",
        defaults: &[
            ("model", "sprites/glow01.spr"),
            ("scale", "0.25"),
            ("rendermode", "9"),
            ("rendercolor", "255 255 255"),
            ("renderamt", "255"),
            ("GlowProxySize", "2.0"),
            ("HDRColorScale", "1.0"),
            ("spawnflags", "1"),
        ],
    },
    LightingClass {
        classname: "env_lightglow",
        group: LightingGroup::Glow,
        description: "A glow that brightens as the viewer comes closer and fades at an angle.",
        preview: "Drawn as a glow that fades in between its minimum and maximum distances.",
        defaults: &[
            ("rendercolor", "255 255 255"),
            ("VerticalGlowSize", "30"),
            ("HorizontalGlowSize", "30"),
            ("MinDist", "500"),
            ("MaxDist", "2000"),
            ("OuterMaxDist", "0"),
            ("GlowProxySize", "2.0"),
            ("HDRColorScale", "1.0"),
            ("angles", "0 0 0"),
        ],
    },
    LightingClass {
        classname: "env_sun",
        group: LightingGroup::Glow,
        description: "Draws the sun's glow in the sky; it does not light anything.",
        preview: "Draws the sun in the sky, where its angles or viewer entity put it.",
        defaults: &[
            ("use_angles", "1"),
            ("pitch", "-45"),
            ("angles", "0 0 0"),
            ("rendercolor", "100 80 80"),
            ("overlaycolor", "0 0 0"),
            ("size", "16"),
            ("overlaysize", "-1"),
            ("material", "sprites/light_glow02_add_noz"),
            ("overlaymaterial", "sprites/light_glow02_add_noz"),
            ("HDRColorScale", "1.0"),
        ],
    },
    LightingClass {
        classname: "env_skypaint",
        group: LightingGroup::Glow,
        description: "Garry's Mod: paints the sky with colours, a sun glow and dusk when the sky is set to painted.",
        preview: "Paints sky faces with its colours when the map's sky is painted.",
        defaults: &[
            ("topcolor", "0.2 0.5 1.0"),
            ("bottomcolor", "0.8 1.0 1.0"),
            ("fadebias", "1.0"),
            ("sunsize", "2.0"),
            ("sunnormal", "0.4 0.0 0.01"),
            ("suncolor", "0.2 0.1 0.0"),
            ("duskscale", "1.0"),
            ("duskintensity", "1.0"),
            ("duskcolor", "1.0 0.2 0.0"),
            ("drawstars", "0"),
            ("hdrscale", "0.66"),
        ],
    },
    LightingClass {
        classname: "env_fog_controller",
        group: LightingGroup::Atmosphere,
        description: "Fog: its colours, where it starts and ends, and how dense it gets.",
        preview: "The first one with fog enabled fogs the view.",
        defaults: &[
            ("fogenable", "1"),
            ("fogblend", "0"),
            ("use_angles", "0"),
            ("fogcolor", "255 255 255"),
            ("fogcolor2", "255 255 255"),
            ("fogdir", "1 0 0"),
            ("fogstart", "500.0"),
            ("fogend", "2000.0"),
            ("fogmaxdensity", "1"),
            ("foglerptime", "0"),
            ("farz", "-1"),
            ("angles", "0 0 0"),
        ],
    },
    LightingClass {
        classname: "env_tonemap_controller",
        group: LightingGroup::Atmosphere,
        description: "Controls HDR tone mapping: auto exposure and bloom, set through its inputs.",
        preview: "Not drawn; use the preview's exposure and tone mapping settings.",
        defaults: &[],
    },
    LightingClass {
        classname: "color_correction",
        group: LightingGroup::Atmosphere,
        description: "Applies a colour correction lookup table near it or everywhere.",
        preview: "Not drawn.",
        defaults: &[
            ("minfalloff", "0.0"),
            ("maxfalloff", "200.0"),
            ("maxweight", "1.0"),
            ("fadeInDuration", "0.0"),
            ("fadeOutDuration", "0.0"),
            ("exclusive", "0"),
        ],
    },
    LightingClass {
        classname: "color_correction_volume",
        group: LightingGroup::Atmosphere,
        description: "A brush volume that applies a colour correction lookup table inside it.",
        preview: "Not drawn.",
        defaults: &[],
    },
    LightingClass {
        classname: "shadow_control",
        group: LightingGroup::Atmosphere,
        description: "Sets the direction, colour and length of the dynamic shadows props and characters cast.",
        preview: "Not drawn; baked shadows come from the lights themselves.",
        defaults: &[
            ("angles", "80 30 0"),
            ("color", "128 128 128"),
            ("distance", "75"),
            ("disableallshadows", "0"),
            ("enableshadowsfromlocallights", "0"),
        ],
    },
    LightingClass {
        classname: "info_lighting",
        group: LightingGroup::Helper,
        description: "Where props that name it in their lighting origin sample their light from.",
        preview: "Not drawn.",
        defaults: &[],
    },
    LightingClass {
        classname: "info_lighting_relative",
        group: LightingGroup::Helper,
        description: "A lighting origin that moves with an entity, for props in moving rooms.",
        preview: "Not drawn.",
        defaults: &[],
    },
    LightingClass {
        classname: "env_cubemap",
        group: LightingGroup::Helper,
        description: "A point where reflections are captured for nearby shiny materials.",
        preview: "Not drawn.",
        defaults: &[("cubemapsize", "0")],
    },
];

/// The catalogue entry for a class, ignoring case.
pub fn lighting_class(classname: &str) -> Option<&'static LightingClass> {
    CATALOGUE
        .iter()
        .find(|class| class.classname.eq_ignore_ascii_case(classname))
}

/// What shape of light a [`Light`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LightKind {
    /// Shines the same in every direction.
    Point,
    /// Shines into a cone.
    Spot,
    /// Shines along one direction everywhere, like the sun.
    Directional,
    /// Projects a texture through a field of view, like a flashlight.
    Projected,
    /// A light-giving surface, shining out of its face.
    Surface,
}

/// Which surfaces a light reaches. The engine lights brushes and models separately, so a
/// dynamic light can reach just one of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Receivers {
    /// Brushes, displacements and models.
    #[default]
    All,
    /// Brushes and displacements only: a dlight, which lights every surface facing it at
    /// the strength its distance gives, whatever angle it is seen at.
    World,
    /// Models only: an elight, or a projected texture that does not light the world.
    Models,
}

/// How a light dims with distance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Falloff {
    /// VRAD's `1 / (constant + linear·d + quadratic·d²)`. The distance stops growing at
    /// `cap`, where a curve fitted to fifty and zero percent distances would turn bright
    /// again, and the light fades to nothing between `fade_start` and `fade_end` when
    /// `fade_end` is past `fade_start`. A positive `max_distance` cuts it off there.
    Inverse {
        constant: f64,
        linear: f64,
        quadratic: f64,
        cap: f64,
        fade_start: f64,
        fade_end: f64,
        max_distance: f64,
    },
    /// A projected texture: full within 100 units, then `100 / d`, fading out between 60%
    /// of its far distance and the far distance.
    Projected { far: f64 },
    /// A dynamic light: full at the light, gone at the radius.
    Radius { radius: f64 },
    /// Directional lights do not dim.
    None,
}

/// The light curve VRAD uses when a light sets no falloff of its own.
pub const QUADRATIC: Falloff = Falloff::Inverse {
    constant: 0.0,
    linear: 0.0,
    quadratic: 1.0,
    cap: f64::INFINITY,
    fade_start: 0.0,
    fade_end: -1.0,
    max_distance: 0.0,
};

/// The farthest any light is taken to reach.
pub const MAX_REACH: f64 = 65536.0;

/// How the engine's shaders scale projected textures: a quarter in HDR and twice in LDR,
/// both 2.5 times brighter on graphics cards that blend in linear colour, as all
/// current ones do.
pub const FLASHLIGHT_SCALE_HDR: f64 = 0.25 * 2.5;
pub const FLASHLIGHT_SCALE_LDR: f64 = 2.0 * 2.5;

impl Falloff {
    /// The share of a light's brightness that is left at a distance.
    pub fn at(&self, distance: f64) -> f64 {
        let distance = distance.max(1.0);
        match *self {
            Self::Inverse {
                constant,
                linear,
                quadratic,
                cap,
                fade_start,
                fade_end,
                max_distance,
            } => {
                if max_distance > 0.0 && distance > max_distance {
                    return 0.0;
                }
                let capped = distance.min(cap);
                let denominator = constant + linear * capped + quadratic * capped * capped;
                let mut share = if denominator > 0.0 { 1.0 / denominator } else { 0.0 };
                if fade_end > fade_start {
                    share *= ((fade_end - distance) / (fade_end - fade_start)).clamp(0.0, 1.0);
                }
                share
            }
            Self::Projected { far } => {
                let fade = ((far - distance) / (far * 0.4).max(1.0)).clamp(0.0, 1.0);
                (100.0 / distance).min(1.0) * fade
            }
            Self::Radius { radius } => {
                let left = (1.0 - distance / radius.max(1.0)).clamp(0.0, 1.0);
                left * left
            }
            Self::None => 1.0,
        }
    }

    /// How far a light of this brightness reaches before it is dimmer than `threshold`,
    /// never more than [`MAX_REACH`].
    pub fn reach(&self, brightness: f64, threshold: f64) -> f64 {
        if brightness <= 0.0 {
            return 0.0;
        }
        let threshold = threshold.max(1e-6);
        let reach = match *self {
            Self::Inverse {
                constant,
                linear,
                quadratic,
                cap,
                fade_start,
                fade_end,
                max_distance,
            } => {
                let target = brightness / threshold - constant;
                let solved = if quadratic > 0.0 {
                    (-linear + (linear * linear + 4.0 * quadratic * target.max(0.0)).sqrt()) / (2.0 * quadratic)
                } else if linear > 0.0 {
                    target.max(0.0) / linear
                } else if constant * threshold < brightness {
                    MAX_REACH
                } else {
                    0.0
                };
                let mut reach = if solved >= cap {
                    let at_cap = Self::Inverse {
                        constant,
                        linear,
                        quadratic,
                        cap,
                        fade_start: 0.0,
                        fade_end: -1.0,
                        max_distance: 0.0,
                    }
                    .at(cap);
                    if brightness * at_cap >= threshold {
                        MAX_REACH
                    } else {
                        solved
                    }
                } else {
                    solved
                };
                if fade_end > fade_start {
                    reach = reach.min(fade_end);
                }
                if max_distance > 0.0 {
                    reach = reach.min(max_distance);
                }
                reach
            }
            Self::Projected { far } => far.min(100.0 * brightness / threshold),
            Self::Radius { radius } => radius * (1.0 - (threshold / brightness).min(1.0).sqrt()),
            Self::None => MAX_REACH,
        };
        reach.clamp(0.0, MAX_REACH)
    }
}

/// A spotlight's cone, as cosines of the angles from its direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cone {
    /// Inside this the light is at full strength.
    pub inner: f64,
    /// Outside this there is no light.
    pub outer: f64,
    /// VRAD's focus: how the light fades between the cones; 1 fades evenly.
    pub exponent: f64,
}

impl Cone {
    /// From inner and outer angles in degrees, as VRAD reads `_inner_cone` and `_cone`:
    /// an inner angle of 0 means 10, a missing outer angle takes the inner one, and the
    /// outer cone is never narrower than the inner one.
    pub fn from_degrees(inner: f64, outer: f64, exponent: f64) -> Self {
        let inner = if inner == 0.0 { 10.0 } else { inner };
        let outer = if outer == 0.0 { inner } else { outer.max(inner) };
        Self {
            inner: inner.to_radians().cos(),
            outer: outer.to_radians().cos(),
            exponent,
        }
    }

    /// How much light reaches a point `cos` away from the cone's middle: all of it inside
    /// the inner cone, none outside the outer one, and in between VRAD's fade raised to
    /// the focus.
    pub fn at(&self, cos: f64) -> f64 {
        if cos <= self.outer {
            return 0.0;
        }
        if cos >= self.inner || self.inner <= self.outer {
            return 1.0;
        }
        let share = (cos - self.outer) / (self.inner - self.outer);
        if self.exponent == 0.0 || self.exponent == 1.0 {
            share
        } else {
            share.powf(self.exponent)
        }
    }

    /// The outer cone's half angle, in degrees.
    pub fn outer_degrees(&self) -> f64 {
        self.outer.clamp(-1.0, 1.0).acos().to_degrees()
    }

    /// The inner cone's half angle, in degrees.
    pub fn inner_degrees(&self) -> f64 {
        self.inner.clamp(-1.0, 1.0).acos().to_degrees()
    }
}

/// What a projected texture projects.
#[derive(Clone, Debug, PartialEq)]
pub struct Projection {
    /// The texture, such as `effects/flashlight001`.
    pub texture: String,
    /// The field of view, in degrees.
    pub fov: f64,
    /// Nothing nearer than this casts a shadow.
    pub near: f64,
}

/// One light source of a map, as the lighting preview uses it.
#[derive(Clone, Debug, PartialEq)]
pub struct Light {
    /// The id of the entity it comes from.
    pub entity: Option<u32>,
    pub classname: String,
    pub kind: LightKind,
    pub origin: DVec3,
    /// The way the light shines, as a unit vector. For directional lights it is the way
    /// the light travels, from the sky to the ground.
    pub direction: DVec3,
    /// Colour and brightness when compiled without HDR.
    pub ldr: DVec3,
    /// Colour and brightness when compiled with HDR, including the HDR scale.
    pub hdr: DVec3,
    pub falloff: Falloff,
    pub cone: Option<Cone>,
    /// Brightness letters the light plays at ten a second, `a` dark and `m` normal; `None`
    /// for a steady light.
    pub pattern: Option<String>,
    /// Off until something turns it on.
    pub starts_dark: bool,
    /// Whether it casts shadows: baked lights always do, dynamic lights do not, and
    /// projected textures when they say so.
    pub shadows: bool,
    /// The engine adds it as the game runs, rather than VRAD baking it.
    pub dynamic: bool,
    pub projection: Option<Projection>,
    /// The sun's size in degrees, which softens its shadows (`SunSpreadAngle`).
    pub spread: f64,
    pub receivers: Receivers,
}

impl Light {
    /// The colour and brightness for an HDR or an LDR compile.
    pub fn color(&self, hdr: bool) -> DVec3 {
        if hdr {
            self.hdr
        } else {
            self.ldr
        }
    }

    /// How brightly the light reaches a point, before surfaces turn towards or away from
    /// it: falloff, cone and, for surfaces, the angle the point is seen at from the face.
    pub fn strength_at(&self, point: DVec3, hdr: bool) -> DVec3 {
        let color = self.color(hdr);
        if self.kind == LightKind::Directional {
            return color;
        }
        let offset = point - self.origin;
        let distance = offset.length();
        let toward = offset / distance.max(1e-9);
        let along = toward.dot(self.direction);
        let shape = match (self.kind, self.cone) {
            (LightKind::Surface, _) => along.max(0.0),
            (_, Some(cone)) => cone.at(along),
            _ => 1.0,
        };
        color * self.falloff.at(distance) * shape
    }

    /// How far the light reaches before it is dimmer than `threshold`.
    pub fn reach(&self, hdr: bool, threshold: f64) -> f64 {
        let brightest = self.color(hdr).max_element();
        self.falloff.reach(brightest, threshold)
    }
}

/// light_environment's ambient sky light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ambient {
    pub ldr: DVec3,
    pub hdr: DVec3,
}

impl Ambient {
    pub fn color(&self, hdr: bool) -> DVec3 {
        if hdr {
            self.hdr
        } else {
            self.ldr
        }
    }
}

/// The fog of an env_fog_controller.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fog {
    /// Linear colour.
    pub color: DVec3,
    /// The colour seen when looking away from `direction`, when the fog blends two.
    pub blend: Option<(DVec3, DVec3)>,
    pub start: f64,
    pub end: f64,
    /// The most the fog covers, from 0 to 1.
    pub max_density: f64,
}

/// The glow of an env_lightglow, drawn without lighting anything.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glow {
    pub entity: Option<u32>,
    pub origin: DVec3,
    /// Linear colour times its opacity.
    pub color: DVec3,
    /// Half its size, in units.
    pub radius: f64,
    /// Distances at which it starts to show and is at full strength, for env_lightglow.
    pub fade: Option<(f64, f64)>,
}

/// A visible spotlight beam.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Beam {
    pub entity: Option<u32>,
    pub origin: DVec3,
    pub direction: DVec3,
    pub length: f64,
    pub width: f64,
    /// Linear colour for LDR and HDR.
    pub ldr: DVec3,
    pub hdr: DVec3,
    /// The beam is off until something turns it on.
    pub starts_dark: bool,
}

/// Garry's Mod's painted sky.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkyPaint {
    pub top: DVec3,
    pub bottom: DVec3,
    pub fade_bias: f64,
    pub sun_normal: DVec3,
    pub sun_color: DVec3,
    pub sun_size: f64,
    pub dusk_color: DVec3,
    pub dusk_scale: f64,
    pub dusk_intensity: f64,
}

/// The sky of a map: the skybox it names, the sun drawn in it and a painted sky.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sky {
    /// The world's `skyname`, such as `sky_day01_01`.
    pub name: String,
    /// Where env_sun draws the sun, as the direction from the ground towards it, its
    /// linear colour and its size in degrees.
    pub sun: Option<(DVec3, DVec3, f64)>,
    pub paint: Option<SkyPaint>,
    /// Where the first sky_camera stands: inside the 3D skybox, which the game draws
    /// behind the sky and lights on its own.
    pub camera: Option<DVec3>,
}

/// Everything about a map's lighting that a preview needs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapLighting {
    pub lights: Vec<Light>,
    pub ambient: Option<Ambient>,
    pub fog: Option<Fog>,
    pub sky: Sky,
    pub glows: Vec<Glow>,
    pub beams: Vec<Beam>,
}

impl MapLighting {
    /// Reads every lighting entity of a map. Entities with an `origin` that does not parse
    /// are skipped, as the compilers skip them.
    pub fn read(vmf: &Vmf) -> Self {
        let mut lighting = Self::default();
        let targets: HashMap<String, DVec3> = vmf
            .entities()
            .filter_map(|entity| Some((entity.get("targetname")?.to_ascii_lowercase(), entity.origin()?)))
            .collect();
        let aim = |entity: &Entity, origin: DVec3| -> Option<DVec3> {
            let target = entity.get("target")?.trim().to_ascii_lowercase();
            let toward = *targets.get(&target)? - origin;
            (toward.length_squared() > 1e-9).then(|| toward.normalize())
        };
        if let Some(world) = vmf.world() {
            lighting.sky.name = world.get("skyname").unwrap_or_default().trim().to_owned();
        }
        let mut sun_glow = None;
        let mut world_brushes: Option<WorldBrushes> = None;
        for entity in vmf.entities() {
            let classname = entity.classname().to_ascii_lowercase();
            let Some(origin) = entity.origin() else {
                continue;
            };
            let id = entity.id();
            let base = |kind: LightKind, direction: DVec3| Light {
                entity: id,
                classname: classname.clone(),
                kind,
                origin,
                direction,
                ldr: DVec3::ZERO,
                hdr: DVec3::ZERO,
                falloff: QUADRATIC,
                cone: None,
                pattern: pattern(entity),
                starts_dark: false,
                shadows: true,
                dynamic: false,
                projection: None,
                spread: 0.0,
                receivers: Receivers::All,
            };
            match classname.as_str() {
                "light" | "light_spot" => {
                    let (ldr, hdr) = light_colors(entity, "_light", "_lightHDR", "_lightscaleHDR", 1.0);
                    let (falloff, scale) = vrad_falloff(entity);
                    let direction = aim(entity, origin).unwrap_or_else(|| vrad_direction(entity));
                    let cone = Cone::from_degrees(
                        number(entity, "_inner_cone").unwrap_or(0.0),
                        number(entity, "_cone").unwrap_or(0.0),
                        number(entity, "_exponent").unwrap_or(1.0),
                    );
                    let cone = (classname == "light_spot" && cone.outer_degrees() < 179.99).then_some(cone);
                    lighting.lights.push(Light {
                        ldr: ldr * scale,
                        hdr: hdr * scale,
                        falloff,
                        cone,
                        starts_dark: named(entity) && flags(entity) & 1 != 0,
                        ..base(
                            if cone.is_some() {
                                LightKind::Spot
                            } else {
                                LightKind::Point
                            },
                            direction,
                        )
                    });
                }
                "light_environment" | "light_directional" => {
                    let default_scale = if classname == "light_directional" { 0.7 } else { 1.0 };
                    let (ldr, hdr) = light_colors(entity, "_light", "_lightHDR", "_lightscaleHDR", default_scale);
                    lighting.lights.push(Light {
                        ldr,
                        hdr,
                        falloff: Falloff::None,
                        spread: number(entity, "SunSpreadAngle").unwrap_or(0.0).max(0.0),
                        pattern: None,
                        ..base(LightKind::Directional, vrad_direction(entity))
                    });
                    if classname == "light_environment" && lighting.ambient.is_none() {
                        let (ldr, hdr) = light_colors(entity, "_ambient", "_ambientHDR", "_AmbientScaleHDR", 1.0);
                        lighting.ambient = Some(Ambient { ldr, hdr });
                    }
                }
                "light_dynamic" => {
                    let rgb = entity.get("_light").and_then(byte_color).unwrap_or(DVec3::ONE);
                    let exponent = number(entity, "brightness").unwrap_or(0.0).trunc().clamp(-127.0, 127.0);
                    let color = rgb * 2f64.powf(exponent);
                    let radius = number(entity, "distance").unwrap_or(120.0).max(1.0);
                    let outer = number(entity, "_cone").unwrap_or(0.0);
                    let direction = aim(entity, origin).unwrap_or_else(|| dynamic_forward(entity));
                    let spawnflags = flags(entity);
                    let (lights_world, lights_models) = (spawnflags & 1 == 0, spawnflags & 2 == 0);
                    let dynamic = |kind, at, radius, color, cone, receivers| Light {
                        origin: at,
                        ldr: color,
                        hdr: color,
                        falloff: Falloff::Radius { radius },
                        cone,
                        pattern: style_pattern(entity),
                        shadows: false,
                        dynamic: true,
                        receivers,
                        ..base(kind, direction)
                    };
                    if outer <= 0.0 {
                        let receivers = match (lights_world, lights_models) {
                            (true, true) => Receivers::All,
                            (true, false) => Receivers::World,
                            (false, true) => Receivers::Models,
                            (false, false) => continue,
                        };
                        lighting
                            .lights
                            .push(dynamic(LightKind::Point, origin, radius, color, None, receivers));
                        continue;
                    }
                    if lights_models {
                        let cone = (outer < 180.0)
                            .then(|| Cone::from_degrees(number(entity, "_inner_cone").unwrap_or(0.0), outer, 1.0));
                        let kind = if cone.is_some() {
                            LightKind::Spot
                        } else {
                            LightKind::Point
                        };
                        lighting
                            .lights
                            .push(dynamic(kind, origin, radius, color, cone, Receivers::Models));
                    }
                    if !lights_world {
                        continue;
                    }
                    let brushes = world_brushes.get_or_insert_with(|| WorldBrushes::new(vmf));
                    if let Some(fraction) = brushes.trace(origin, direction * radius) {
                        let left = (1.0 - fraction) * (1.0 - fraction);
                        let end = origin + direction * (radius * fraction - 1.0).max(0.0);
                        let spot = number(entity, "spotlight_radius").unwrap_or(80.0).max(1.0);
                        lighting.lights.push(dynamic(
                            LightKind::Point,
                            end,
                            spot,
                            color * left,
                            None,
                            Receivers::World,
                        ));
                    }
                }
                "env_projectedtexture" => {
                    let numbers = entity.get("lightcolor").and_then(parse_numbers).unwrap_or_default();
                    let rgb = match numbers.as_slice() {
                        [r, g, b, ..] => DVec3::new(linear(*r), linear(*g), linear(*b)),
                        _ => DVec3::ONE,
                    };
                    let alpha = numbers
                        .get(3)
                        .copied()
                        .filter(|alpha| *alpha > 0.0)
                        .map_or(1.0, |alpha| alpha / 255.0);
                    let intensity = alpha * number(entity, "lightstrength").unwrap_or(1.0).max(0.0);
                    let direction = aim(entity, origin).unwrap_or_else(|| engine_forward(entity));
                    let fov = number(entity, "lightfov").unwrap_or(45.0).clamp(1.0, 179.0);
                    let far = number(entity, "farz").unwrap_or(750.0).max(1.0);
                    let lights_world = number(entity, "lightworld").unwrap_or(1.0) != 0.0
                        && number(entity, "lightonlytarget").unwrap_or(0.0) == 0.0;
                    lighting.lights.push(Light {
                        ldr: rgb * intensity * FLASHLIGHT_SCALE_LDR,
                        hdr: rgb * intensity * FLASHLIGHT_SCALE_HDR,
                        falloff: Falloff::Projected { far },
                        cone: Some(Cone {
                            inner: (fov * 0.5).to_radians().cos(),
                            outer: (fov * 0.5).to_radians().cos(),
                            exponent: 1.0,
                        }),
                        starts_dark: flags(entity) & 4 != 0,
                        shadows: number(entity, "enableshadows").unwrap_or(0.0) != 0.0,
                        dynamic: true,
                        receivers: if lights_world {
                            Receivers::All
                        } else {
                            Receivers::Models
                        },
                        projection: Some(Projection {
                            texture: entity
                                .get("texturename")
                                .unwrap_or("effects/flashlight001")
                                .trim()
                                .to_owned(),
                            fov,
                            near: number(entity, "nearz").unwrap_or(4.0).max(1.0),
                        }),
                        ..base(LightKind::Projected, direction)
                    });
                }
                "point_spotlight" | "beam_spotlight" => {
                    let rgb = entity.get("rendercolor").and_then(rgb_color).unwrap_or(DVec3::ONE);
                    let alpha = number(entity, "renderamt").unwrap_or(255.0).clamp(0.0, 255.0) / 255.0;
                    let default_scale = if classname == "beam_spotlight" { 0.7 } else { 1.0 };
                    let hdr_scale = number(entity, "HDRColorScale").unwrap_or(default_scale);
                    lighting.beams.push(Beam {
                        entity: id,
                        origin,
                        direction: engine_forward(entity),
                        length: number(entity, "spotlightlength").unwrap_or(500.0).max(1.0),
                        width: number(entity, "spotlightwidth").unwrap_or(50.0).max(1.0),
                        ldr: rgb * alpha,
                        hdr: rgb * alpha * hdr_scale,
                        starts_dark: flags(entity) & 1 == 0,
                    });
                }
                "env_lightglow" => {
                    let rgb = entity.get("rendercolor").and_then(rgb_color).unwrap_or(DVec3::ONE);
                    let size = number(entity, "HorizontalGlowSize")
                        .unwrap_or(30.0)
                        .max(number(entity, "VerticalGlowSize").unwrap_or(30.0));
                    let near = number(entity, "MinDist").unwrap_or(500.0).max(0.0);
                    let far = number(entity, "MaxDist").unwrap_or(2000.0).max(near + 1.0);
                    lighting.glows.push(Glow {
                        entity: id,
                        origin,
                        color: rgb,
                        radius: size.clamp(1.0, 4096.0),
                        fade: Some((near, far)),
                    });
                }
                "sky_camera" if lighting.sky.camera.is_none() => {
                    lighting.sky.camera = Some(origin);
                }
                "env_sun" => {
                    let rgb = entity.get("rendercolor").and_then(rgb_color).unwrap_or(DVec3::ONE);
                    let toward = if number(entity, "use_angles").unwrap_or(0.0) != 0.0 {
                        Some(-vrad_direction(entity))
                    } else {
                        aim(entity, origin).map(|toward| -toward)
                    };
                    let size = number(entity, "size").unwrap_or(16.0).max(1.0) * 0.25;
                    sun_glow = sun_glow.or(toward.map(|toward| (toward, rgb, size)));
                }
                "env_skypaint" => {
                    let vector = |key: &str, fallback: DVec3| entity.get(key).and_then(parse_vec3).unwrap_or(fallback);
                    lighting.sky.paint.get_or_insert(SkyPaint {
                        top: vector("topcolor", DVec3::new(0.2, 0.5, 1.0)),
                        bottom: vector("bottomcolor", DVec3::new(0.8, 1.0, 1.0)),
                        fade_bias: number(entity, "fadebias").unwrap_or(1.0).max(0.01),
                        sun_normal: vector("sunnormal", DVec3::new(0.4, 0.0, 0.01)).normalize_or(DVec3::Z),
                        sun_color: vector("suncolor", DVec3::new(0.2, 0.1, 0.0)),
                        sun_size: number(entity, "sunsize").unwrap_or(2.0).max(0.0),
                        dusk_color: vector("duskcolor", DVec3::new(1.0, 0.2, 0.0)),
                        dusk_scale: number(entity, "duskscale").unwrap_or(1.0).max(0.0),
                        dusk_intensity: number(entity, "duskintensity").unwrap_or(1.0).max(0.0),
                    });
                }
                "env_fog_controller" if lighting.fog.is_none() => {
                    if number(entity, "fogenable").unwrap_or(0.0) == 0.0 {
                        continue;
                    }
                    let color = entity
                        .get("fogcolor_hdr")
                        .and_then(rgb_color)
                        .filter(|hdr| *hdr != DVec3::ZERO)
                        .or_else(|| entity.get("fogcolor").and_then(rgb_color))
                        .unwrap_or(DVec3::ONE);
                    let blend = (number(entity, "fogblend").unwrap_or(0.0) != 0.0).then(|| {
                        let second = entity.get("fogcolor2").and_then(rgb_color).unwrap_or(color);
                        let direction = if number(entity, "use_angles").unwrap_or(0.0) != 0.0 {
                            engine_forward(entity)
                        } else {
                            entity
                                .get("fogdir")
                                .and_then(parse_vec3)
                                .unwrap_or(DVec3::X)
                                .normalize_or(DVec3::X)
                        };
                        (second, direction)
                    });
                    let start = number(entity, "fogstart").unwrap_or(500.0);
                    lighting.fog = Some(Fog {
                        color,
                        blend,
                        start,
                        end: number(entity, "fogend").unwrap_or(2000.0).max(start + 1.0),
                        max_density: number(entity, "fogmaxdensity").unwrap_or(1.0).clamp(0.0, 1.0),
                    });
                }
                _ => {}
            }
        }
        lighting.sky.sun = sun_glow;
        lighting
    }

    /// The first light_environment's sun.
    pub fn sun(&self) -> Option<&Light> {
        self.lights.iter().find(|light| light.classname == "light_environment")
    }
}

/// `_light`-style colour and brightness as VRAD reads it: each of the red, green and blue
/// values (0 to 255) is made linear with a 2.2 gamma and scaled by the brightness over
/// 255. One number is grey; three are a colour at brightness 255. Eight numbers hold LDR
/// then HDR values, and `hdr` picks which. Negative values mean "not set", as `-1 -1 -1 1`
/// does in `_lightHDR`.
pub fn light_intensity(text: &str, hdr: bool) -> Option<DVec3> {
    let numbers = parse_numbers(text)?;
    let values = match numbers.len() {
        8 if hdr => &numbers[4..],
        8 => &numbers[..4],
        1 | 3 | 4 => &numbers[..],
        _ => return None,
    };
    if values.iter().any(|value| *value < 0.0) {
        return None;
    }
    let color = match values {
        [grey] => DVec3::splat(linear(*grey)),
        [r, g, b, ..] => DVec3::new(linear(*r), linear(*g), linear(*b)),
        _ => return None,
    };
    let brightness = values.get(3).map_or(1.0, |brightness| brightness / 255.0);
    Some(color * brightness)
}

/// A light's LDR and HDR colours from a colour key, its HDR override and HDR scale, as
/// VRAD's ParseLightGeneric reads `_light`, `_lightHDR` and `_lightscaleHDR`.
fn light_colors(entity: &Entity, key: &str, hdr_key: &str, scale_key: &str, default_scale: f64) -> (DVec3, DVec3) {
    let ldr = entity
        .get(key)
        .and_then(|text| light_intensity(text, false))
        .unwrap_or(DVec3::ZERO);
    let hdr = entity
        .get(hdr_key)
        .and_then(|text| light_intensity(text, true))
        .or_else(|| entity.get(key).and_then(|text| light_intensity(text, true)))
        .unwrap_or(DVec3::ZERO);
    (ldr, hdr * number(entity, scale_key).unwrap_or(default_scale))
}

/// VRAD's falloff for a light and how much to scale its brightness, as
/// SetLightFalloffParams sets them up.
///
/// With `_fifty_percent_distance`, a quadratic is fitted through full brightness at the
/// light, half at the fifty percent distance and 1/256 at the zero percent distance
/// (twice the fifty when it is too short), bent towards a straight line until it no
/// longer turns bright again. `_hardfalloff` then fades it to exactly nothing by the zero
/// percent distance; otherwise a curve that would brighten again is capped and faded
/// out by ten times that distance. Without it, `_constant_attn`, `_linear_attn` and
/// `_quadratic_attn` are used as given (constant 1 when all are 0) and the brightness is
/// scaled so the light has its stated brightness at 100 units; `_distance` cuts it off.
pub fn vrad_falloff(entity: &Entity) -> (Falloff, f64) {
    let fifty = number(entity, "_fifty_percent_distance").unwrap_or(0.0);
    if fifty > 0.0 {
        let mut zero = number(entity, "_zero_percent_distance").unwrap_or(0.0);
        if zero < fifty {
            zero = 2.0 * fifty;
        }
        let (mut a, mut b, mut c) =
            solve_quadratic_monotonic([(0.0, 1.0), (fifty, 2.0), (zero, 256.0)]).unwrap_or((0.0, 1.0, 0.0));
        let at_fifty = c + fifty * (b + fifty * a);
        if at_fifty > 0.0 {
            let scale = 2.0 / at_fifty;
            a *= scale;
            b *= scale;
            c *= scale;
        }
        let (mut cap, mut fade_start, mut fade_end) = (f64::INFINITY, 0.0, -1.0);
        if number(entity, "_hardfalloff").unwrap_or(0.0) != 0.0 {
            fade_end = zero;
            fade_start = 0.75 * zero + 0.25 * fifty;
        } else if a.abs() > 0.0 {
            let turn = b / (-2.0 * a);
            if turn > 0.0 {
                cap = turn;
                fade_start = turn;
                fade_end = 10.0 * turn;
            }
        }
        let falloff = Falloff::Inverse {
            constant: c,
            linear: b,
            quadratic: a,
            cap,
            fade_start,
            fade_end,
            max_distance: 0.0,
        };
        return (falloff, 1.0);
    }
    let clean = |key: &str| number(entity, key).filter(|value| *value >= 1e-6).unwrap_or(0.0);
    let (mut constant, linear, quadratic) = (clean("_constant_attn"), clean("_linear_attn"), clean("_quadratic_attn"));
    if constant == 0.0 && linear == 0.0 && quadratic == 0.0 {
        constant = 1.0;
    }
    let ratio = constant + 100.0 * linear + 100.0 * 100.0 * quadratic;
    let falloff = Falloff::Inverse {
        constant,
        linear,
        quadratic,
        cap: f64::INFINITY,
        fade_start: 0.0,
        fade_end: -1.0,
        max_distance: number(entity, "_distance").unwrap_or(0.0).max(0.0),
    };
    (falloff, if ratio > 0.0 { ratio } else { 1.0 })
}

/// Fits `y = a·x² + b·x + c` through three points, as Source's SolveInverseQuadratic.
fn solve_quadratic(points: [(f64, f64); 3]) -> Option<(f64, f64, f64)> {
    let [(x1, y1), (x2, y2), (x3, y3)] = points;
    let det = (x1 - x2) * (x1 - x3) * (x2 - x3);
    if det == 0.0 {
        return None;
    }
    let a = (x3 * (-y1 + y2) + x2 * (y1 - y3) + x1 * (-y2 + y3)) / det;
    let b = (x3 * x3 * (y1 - y2) + x1 * x1 * (y2 - y3) + x2 * x2 * (-y1 + y3)) / det;
    let c = (x1 * x3 * (-x1 + x3) * y2 + x2 * x2 * (x3 * y1 - x1 * y3) + x2 * (-(x3 * x3 * y1) + x1 * x1 * y3)) / det;
    Some((a, b, c))
}

/// Source's SolveInverseQuadraticMonotonic: like [`solve_quadratic`], but when the curve
/// would not keep rising (or falling) near its start the middle point is moved towards
/// the straight line between the outer two in steps of 5% until it does, at most 95% of
/// the way, as Source's floating point loop stops there.
fn solve_quadratic_monotonic(mut points: [(f64, f64); 3]) -> Option<(f64, f64, f64)> {
    points.sort_by(|a, b| a.0.total_cmp(&b.0));
    let [(x1, y1), (x2, y2), (x3, y3)] = points;
    let straight = y1 + (y3 - y1) * (x2 - x1) / (x3 - x1);
    let mut last = None;
    for step in 0..20 {
        let blend = f64::from(step) * 0.05;
        let middle = (1.0 - blend) * y2 + blend * straight;
        let (a, b, c) = solve_quadratic([(x1, y1), (x2, middle), (x3, y3)])?;
        let slope = 2.0 * a + b;
        let rising = y1 < y2 && y2 < y3;
        let falling = y1 > y2 && y2 > y3;
        if (rising && slope >= 0.0) || (falling && slope <= 0.0) || (!rising && !falling) {
            return Some((a, b, c));
        }
        last = Some((a, b, c));
    }
    last
}

/// The way a light entity points, as VRAD's SetupLightNormalFromProps works it out from
/// `angles`, `angle` and `pitch`: the yaw comes from `angle` when it is set (-1 straight
/// up, -2 straight down) and from `angles` otherwise, and the pitch from `pitch` when it
/// is not 0 and from `angles` otherwise, where a negative pitch points down.
pub fn vrad_direction(entity: &Entity) -> DVec3 {
    let angles = entity.get("angles").and_then(parse_vec3).unwrap_or(DVec3::ZERO);
    let angle = number(entity, "angle").unwrap_or(0.0);
    let pitch = number(entity, "pitch").unwrap_or(0.0);
    let mut direction = if angle == -1.0 {
        DVec3::Z
    } else if angle == -2.0 {
        DVec3::NEG_Z
    } else {
        let yaw = if angle == 0.0 { angles.y } else { angle }.to_radians();
        DVec3::new(yaw.cos(), yaw.sin(), 0.0)
    };
    let pitch = if pitch == 0.0 { angles.x } else { pitch }.to_radians();
    direction.z = pitch.sin();
    direction.x *= pitch.cos();
    direction.y *= pitch.cos();
    direction.normalize_or(DVec3::NEG_Z)
}

/// The way an entity faces by its `angles`, as the engine's AngleVectors: positive pitch
/// looks down.
pub fn engine_forward(entity: &Entity) -> DVec3 {
    angle_vectors(entity.get("angles").and_then(parse_vec3).unwrap_or(DVec3::ZERO))
}

/// The way a light_dynamic points, as the game reads it: its `angles`, with the pitch
/// replaced by `pitch` turned around when that is set, so `-90` points straight down
/// whatever `angles` says.
pub fn dynamic_forward(entity: &Entity) -> DVec3 {
    let mut angles = entity.get("angles").and_then(parse_vec3).unwrap_or(DVec3::ZERO);
    if let Some(pitch) = number(entity, "pitch").filter(|pitch| *pitch != 0.0) {
        angles.x = -pitch;
    }
    angle_vectors(angles)
}

/// The engine's AngleVectors: the forward direction of pitch, yaw and roll in degrees.
fn angle_vectors(angles: DVec3) -> DVec3 {
    let (pitch, yaw) = (angles.x.to_radians(), angles.y.to_radians());
    DVec3::new(pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), -pitch.sin()).normalize_or(DVec3::X)
}

/// The brushes the engine's traces against the world hit, for where a light_dynamic's
/// spot lands: world and func_detail brushes, leaving out those made only of tool
/// textures traces pass through.
struct WorldBrushes {
    brushes: Vec<(Bounds, Vec<Plane>)>,
}

impl WorldBrushes {
    fn new(vmf: &Vmf) -> Self {
        let details = vmf
            .entities()
            .filter(|entity| entity.classname().eq_ignore_ascii_case("func_detail"));
        let brushes = vmf
            .world()
            .into_iter()
            .chain(details)
            .flat_map(|entity| entity.solids())
            .filter(|solid| solid.sides().any(|side| !passable(&side.material)))
            .filter_map(|solid| {
                let bounds = faces_bounds(&solid_faces(solid));
                let planes: Vec<Plane> = solid
                    .sides()
                    .filter_map(|side| Plane::from_points(side.plane))
                    .collect();
                (!bounds.is_empty()).then_some((bounds, planes))
            })
            .collect();
        Self { brushes }
    }

    /// Where a line from `from` along `delta` first meets a brush, as a share of `delta`
    /// from 0 to 1; 0 when it starts inside one.
    fn trace(&self, from: DVec3, delta: DVec3) -> Option<f64> {
        self.brushes
            .iter()
            .filter(|(bounds, _)| segment_meets_box(from, delta, bounds))
            .filter_map(|(_, planes)| enter_convex(from, delta, planes))
            .min_by(f64::total_cmp)
    }
}

/// Tool textures that traces pass through, as brushes made of them are not solid.
const PASSABLE_TOOLS: [&str; 11] = [
    "tools/toolstrigger",
    "tools/toolshint",
    "tools/toolsskip",
    "tools/toolsareaportal",
    "tools/toolsoccluder",
    "tools/toolsorigin",
    "tools/toolsfog",
    "tools/toolsplayerclip",
    "tools/toolsinvisibleladder",
    "tools/toolsblock_los",
    "tools/toolsblocklight",
];

fn passable(material: &str) -> bool {
    let name = material.trim().replace('\\', "/");
    PASSABLE_TOOLS.iter().any(|tool| name.eq_ignore_ascii_case(tool))
}

/// Whether a line from `from` along `delta` passes through a box, give or take a unit.
fn segment_meets_box(from: DVec3, delta: DVec3, bounds: &Bounds) -> bool {
    let (mut enter, mut exit) = (0.0f64, 1.0f64);
    for axis in 0..3 {
        let (low, high) = (bounds.min[axis] - 1.0, bounds.max[axis] + 1.0);
        if delta[axis].abs() < 1e-12 {
            if from[axis] < low || from[axis] > high {
                return false;
            }
            continue;
        }
        let (a, b) = ((low - from[axis]) / delta[axis], (high - from[axis]) / delta[axis]);
        enter = enter.max(a.min(b));
        exit = exit.min(a.max(b));
        if enter > exit {
            return false;
        }
    }
    true
}

/// Where a line from `from` along `delta` enters a convex brush, as a share of `delta`;
/// 0 when it starts inside, and `None` when it misses or stops short.
fn enter_convex(from: DVec3, delta: DVec3, planes: &[Plane]) -> Option<f64> {
    let (mut enter, mut exit) = (0.0f64, 1.0f64);
    for plane in planes {
        let start = plane.distance(from);
        let along = plane.normal.dot(delta);
        if along.abs() < 1e-12 {
            if start > 0.0 {
                return None;
            }
            continue;
        }
        let at = -start / along;
        if along < 0.0 {
            enter = enter.max(at);
        } else {
            exit = exit.min(at);
        }
        if enter > exit {
            return None;
        }
    }
    Some(enter)
}

/// The brightness patterns of Source's light appearances (`style`), by number.
pub const STYLE_PATTERNS: [&str; 13] = [
    "m",
    "mmnmmommommnonmmonqnmmo",
    "abcdefghijklmnopqrstuvwxyzyxwvutsrqponmlkjihgfedcba",
    "mmmmmaaaaammmmmaaaaaabcdefgabcdefg",
    "mamamamamama",
    "jklmnopqrstuvwxyzyxwvutsrqponmlkj",
    "nmonqnmomnmomomno",
    "mmmaaaabcdefgmmmmaaaammmaamm",
    "mmmaaammmaaammmabcdefaaaammmmabcdefmmmaaaa",
    "aaaaaaaazzzzzzzz",
    "mmamammmmammamamaaamammma",
    "abcdefghijklmnopqrrqponmlkjihgfedcba",
    "mmnnmmnnnmmnn",
];

/// How many letters of a pattern play each second.
pub const PATTERN_RATE: f64 = 10.0;

/// A light's appearance: its custom `pattern`, or the pattern of its `style`, unless it
/// is steady.
fn pattern(entity: &Entity) -> Option<String> {
    let custom = entity
        .get("pattern")
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty());
    match custom {
        Some(pattern) => {
            let pattern = pattern.to_ascii_lowercase();
            pattern.chars().any(|letter| letter != 'm').then_some(pattern)
        }
        None => style_pattern(entity),
    }
}

/// The pattern of a light's `style`, unless it is steady. The engine's dynamic lights
/// only play these, never a custom pattern.
fn style_pattern(entity: &Entity) -> Option<String> {
    let style = number(entity, "style").unwrap_or(0.0) as usize;
    let pattern = STYLE_PATTERNS.get(style)?;
    pattern.chars().any(|letter| letter != 'm').then(|| pattern.to_string())
}

/// The brightness a pattern gives at a time in seconds, where `m` is 1, `a` is dark and
/// `z` a little over twice as bright.
pub fn pattern_brightness(pattern: &str, seconds: f64) -> f64 {
    let letters = pattern.as_bytes();
    if letters.is_empty() {
        return 1.0;
    }
    let index = ((seconds.max(0.0) * PATTERN_RATE) as usize) % letters.len();
    let letter = letters[index].to_ascii_lowercase().clamp(b'a', b'z');
    f64::from(letter - b'a') / f64::from(b'm' - b'a')
}

/// A material's light from `lights.rad`, for LDR and HDR compiles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextureLight {
    pub ldr: DVec3,
    pub hdr: DVec3,
}

/// Reads a `lights.rad` file: lines of a material name and a light value, such as
/// `lights/white001 255 255 255 200`. Lines starting `ldr:` or `hdr:` apply only to that
/// kind of compile; `noshadow` and `forcetextureshadow` lines and `//` comments are
/// skipped. Material names come back lower case with forward slashes.
pub fn parse_lights_rad(text: &str) -> HashMap<String, TextureLight> {
    let mut lights: HashMap<String, (Option<DVec3>, Option<DVec3>)> = HashMap::new();
    for line in text.lines() {
        let line = line.split("//").next().unwrap_or_default().trim();
        let (line, only) = match line.get(..4).map(str::to_ascii_lowercase).as_deref() {
            Some("ldr:") => (line[4..].trim_start(), Some(false)),
            Some("hdr:") => (line[4..].trim_start(), Some(true)),
            _ => (line, None),
        };
        let mut words = line.splitn(2, char::is_whitespace);
        let (Some(name), Some(value)) = (words.next(), words.next()) else {
            continue;
        };
        let lowered = name.to_ascii_lowercase();
        if lowered == "noshadow" || lowered == "forcetextureshadow" {
            continue;
        }
        let key = lowered.replace('\\', "/");
        let entry = lights.entry(key).or_default();
        if only != Some(true) {
            if let Some(color) = light_intensity(value, false) {
                entry.0 = Some(color);
            }
        }
        if only != Some(false) {
            if let Some(color) = light_intensity(value, true) {
                entry.1 = Some(color);
            }
        }
    }
    lights
        .into_iter()
        .filter_map(|(name, (ldr, hdr))| {
            let ldr = ldr.or(hdr)?;
            Some((
                name,
                TextureLight {
                    ldr,
                    hdr: hdr.unwrap_or(ldr),
                },
            ))
        })
        .collect()
}

/// VRAD's brightness for a texture light: its value for each time the texture repeats
/// across the face, times 100², so one tile of a `200` light lights a surface 100 units in
/// front of it like a `200` point light. `tile_area` is the area one repeat of the texture
/// covers on the face.
pub fn texture_light_scale(face_area: f64, tile_area: f64) -> f64 {
    if tile_area <= 0.0 {
        return 0.0;
    }
    face_area / tile_area * 100.0 * 100.0
}

/// A colour key as the engine reads it into a dynamic light: red, green and blue bytes,
/// taken as 0 to 1 without turning them linear.
fn byte_color(text: &str) -> Option<DVec3> {
    let numbers = parse_numbers(text)?;
    let [r, g, b] = [*numbers.first()?, *numbers.get(1)?, *numbers.get(2)?];
    Some(DVec3::new(r, g, b).clamp(DVec3::ZERO, DVec3::splat(255.0)) / 255.0)
}

/// A colour key such as `rendercolor`, as linear red, green and blue from 0 to 1. Values
/// of 1 or less on every channel are read as 0 to 1 already, as `color1` keys are.
fn rgb_color(text: &str) -> Option<DVec3> {
    let numbers = parse_numbers(text)?;
    let [r, g, b] = [*numbers.first()?, *numbers.get(1)?, *numbers.get(2)?];
    if r <= 1.0 && g <= 1.0 && b <= 1.0 && text.contains('.') {
        return Some(DVec3::new(r, g, b).max(DVec3::ZERO));
    }
    Some(DVec3::new(linear(r), linear(g), linear(b)))
}

/// One 0 to 255 colour value made linear, as VRAD does with a 2.2 gamma.
fn linear(value: f64) -> f64 {
    (value.max(0.0) / 255.0).powf(2.2)
}

fn number(entity: &Entity, key: &str) -> Option<f64> {
    entity
        .get(key)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
        .filter(|value: &f64| value.is_finite())
}

fn flags(entity: &Entity) -> u32 {
    number(entity, "spawnflags").map_or(0, |flags| flags.max(0.0) as u32)
}

/// Lights only start dark when they have a name, since VRAD bakes unnamed lights in.
fn named(entity: &Entity) -> bool {
    entity.get("targetname").is_some_and(|name| !name.trim().is_empty())
}
