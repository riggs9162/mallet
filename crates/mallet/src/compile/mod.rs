//! Compiling maps the way CompilePal does: a preset is a list of steps (VBSP, VVIS, VRAD,
//! SourceCubemap, copying to the game, launching it, or any program), each with options
//! picked from a catalog and free extra arguments. Steps can use variables such as
//! `{bsp}` and `{gamedir}`, so custom tools fit into the same chain.

pub mod runner;
mod session;

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{self, ToolKind};

pub use runner::{CompileJob, Outcome, Severity, StepStatus};
pub use session::COMPILE_FOLDER;

/// What a step does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Vbsp,
    Vvis,
    Vrad,
    Vbspinfo,
    SourceCubemap,
    /// `bspzip -repack -compress`: makes the BSP smaller.
    Repack,
    /// Copies the BSP into the game's `maps` folder.
    CopyToGame,
    /// Starts the game on the map without waiting for it.
    RunGame,
    /// Any program, with arguments that may use the variables.
    Custom,
}

impl StepKind {
    /// Every kind, in the order the "Add step" menu lists them.
    pub const ALL: [StepKind; 9] = [
        StepKind::Vbsp,
        StepKind::Vvis,
        StepKind::Vrad,
        StepKind::SourceCubemap,
        StepKind::Repack,
        StepKind::Vbspinfo,
        StepKind::CopyToGame,
        StepKind::RunGame,
        StepKind::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            StepKind::Vbsp => "VBSP",
            StepKind::Vvis => "VVIS",
            StepKind::Vrad => "VRAD",
            StepKind::Vbspinfo => "VBSPInfo",
            StepKind::SourceCubemap => "SourceCubemap",
            StepKind::Repack => "Repack (BSPZip)",
            StepKind::CopyToGame => "Copy to game",
            StepKind::RunGame => "Run game",
            StepKind::Custom => "Custom program",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            StepKind::Vbsp => "Builds the BSP from the VMF: brushes, entities, props and the leak check.",
            StepKind::Vvis => "Computes visibility, so the engine only draws what can be seen.",
            StepKind::Vrad => "Bakes lightmaps and prop lighting.",
            StepKind::Vbspinfo => "Lists what the BSP contains and how close it is to the engine's limits.",
            StepKind::SourceCubemap => {
                "Renders every env_cubemap on the graphics card and builds them into the BSP, without starting the game."
            }
            StepKind::Repack => "Compresses the BSP's lumps and pakfile, making the download smaller.",
            StepKind::CopyToGame => "Copies the BSP into the game's maps folder.",
            StepKind::RunGame => "Starts the game on the map. Mallet does not wait for it.",
            StepKind::Custom => "Runs any program. Arguments and the working folder may use the variables below.",
        }
    }

    /// The program setting this step runs, if it runs a known tool.
    pub fn tool(self) -> Option<ToolKind> {
        match self {
            StepKind::Vbsp => Some(ToolKind::Vbsp),
            StepKind::Vvis => Some(ToolKind::Vvis),
            StepKind::Vrad => Some(ToolKind::Vrad),
            StepKind::Vbspinfo => Some(ToolKind::Vbspinfo),
            StepKind::SourceCubemap => Some(ToolKind::SourceCubemap),
            StepKind::Repack => Some(ToolKind::Bspzip),
            StepKind::RunGame => Some(ToolKind::Game),
            StepKind::CopyToGame | StepKind::Custom => None,
        }
    }

    /// The options the step offers as checkboxes.
    pub fn options(self) -> &'static [OptionSpec] {
        match self {
            StepKind::Vbsp => VBSP_OPTIONS,
            StepKind::Vvis => VVIS_OPTIONS,
            StepKind::Vrad => VRAD_OPTIONS,
            StepKind::Vbspinfo => VBSPINFO_OPTIONS,
            StepKind::SourceCubemap => SOURCECUBEMAP_OPTIONS,
            StepKind::Repack => REPACK_OPTIONS,
            StepKind::RunGame => GAME_OPTIONS,
            StepKind::CopyToGame | StepKind::Custom => &[],
        }
    }
}

/// Which builds of a compile tool understand an option. The Hammer++ tools
/// (`vbspplusplus.exe` and friends) add many options and drop a few of the game's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Build {
    /// Every build.
    Any,
    /// Only the Hammer++ builds.
    PlusPlus,
    /// Only the game's own builds, not Hammer++.
    Stock,
}

impl Build {
    /// The build a program is, judged by its file name.
    pub fn of(program: &Path) -> Build {
        let is_plusplus = program
            .file_stem()
            .is_some_and(|stem| stem.to_string_lossy().to_ascii_lowercase().ends_with("plusplus"));
        if is_plusplus {
            Build::PlusPlus
        } else {
            Build::Stock
        }
    }

    /// Whether a program of this build understands an option made for `option`.
    pub fn supports(self, option: Build) -> bool {
        option == Build::Any || option == self
    }

    /// A short tag for options that only some builds understand.
    pub fn tag(self) -> &'static str {
        match self {
            Build::Any => "",
            Build::PlusPlus => "Hammer++",
            Build::Stock => "Game tools",
        }
    }

    /// Why an option is missing from a build.
    pub fn requirement(self) -> &'static str {
        match self {
            Build::Any => "",
            Build::PlusPlus => "Only the Hammer++ build of this tool understands this option.",
            Build::Stock => "Only the game's own build of this tool understands this option, not Hammer++.",
        }
    }
}

/// A known command-line option of a tool.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OptionSpec {
    pub flag: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// The default value when the option takes one.
    pub value: Option<&'static str>,
    /// Shown in amber when the option is risky or slow.
    pub warning: &'static str,
    /// Which builds of the tool understand the option.
    pub build: Build,
    /// The value is several arguments separated by spaces, such as `0 0 0`.
    pub split: bool,
}

const fn option(flag: &'static str, name: &'static str, description: &'static str) -> OptionSpec {
    OptionSpec {
        flag,
        name,
        description,
        value: None,
        warning: "",
        build: Build::Any,
        split: false,
    }
}

const fn valued(flag: &'static str, name: &'static str, value: &'static str, description: &'static str) -> OptionSpec {
    OptionSpec {
        value: Some(value),
        ..option(flag, name, description)
    }
}

const fn warned(spec: OptionSpec, warning: &'static str) -> OptionSpec {
    OptionSpec { warning, ..spec }
}

/// Marks an option that only the Hammer++ build understands.
const fn pp(spec: OptionSpec) -> OptionSpec {
    OptionSpec {
        build: Build::PlusPlus,
        ..spec
    }
}

/// Marks an option that only the game's own build understands.
const fn stock(spec: OptionSpec) -> OptionSpec {
    OptionSpec {
        build: Build::Stock,
        ..spec
    }
}

/// Marks an option whose value is several arguments.
const fn split(spec: OptionSpec) -> OptionSpec {
    OptionSpec { split: true, ..spec }
}

const VBSP_OPTIONS: &[OptionSpec] = &[
    option(
        "-onlyents",
        "Only entities",
        "Only updates the entities of an existing BSP. Fast, but brushes, props and lighting stay as they were.",
    ),
    option(
        "-onlyprops",
        "Only static props",
        "Only updates the static and detail props of an existing BSP.",
    ),
    option(
        "-leaktest",
        "Stop on leaks",
        "Stops the compile when the map leaks. Hammer++ does this already.",
    ),
    pp(option(
        "-noleaktest",
        "Keep going on leaks",
        "Carries on compiling when the map leaks.",
    )),
    option(
        "-nodetail",
        "No detail brushes",
        "Leaves out func_detail brushes, to see how they affect visibility.",
    ),
    option(
        "-fulldetail",
        "Detail blocks visibility",
        "Makes func_detail brushes take part in visibility.",
    ),
    option("-nowater", "No water", "Leaves out water brushes."),
    option(
        "-nodrawtriggers",
        "Hide triggers",
        "Compiles every trigger as if it used the nodraw texture.",
    ),
    option(
        "-allowdynamicpropsasstatic",
        "Dynamic props as static",
        "Allows prop_static with models that are not marked as allowed to be static.",
    ),
    valued(
        "-embed",
        "Embed folder",
        "",
        "Searches this folder for content and packs everything in it into the BSP.",
    ),
    pp(valued(
        "-missingmaterial",
        "Missing material",
        "",
        "Replaces missing brush materials with this material.",
    )),
    option(
        "-replacematerials",
        "Replace materials",
        "Swaps materials as the game's material substitution file says.",
    ),
    pp(option(
        "-warnmissinginstances",
        "Warn on missing instances",
        "Warns about missing func_instance files instead of stopping.",
    )),
    pp(option(
        "-nonamefixup",
        "No instance name prefixes",
        "Does not prefix entity names when collapsing instances.",
    )),
    option(
        "-keepstalezip",
        "Keep packed files",
        "Keeps the files packed into the BSP and rebuilds everything else.",
    ),
    valued(
        "-micro",
        "Smallest brush",
        "1.0",
        "Warns about brushes smaller than this many cubic units.",
    ),
    option(
        "-noweld",
        "No welding",
        "Does not join face vertices together.",
    ),
    option("-nocsg", "No CSG", "Does not cut overlapping brushes apart."),
    option(
        "-noshare",
        "No shared edges",
        "Gives every face its own edges instead of sharing them.",
    ),
    option(
        "-notjunc",
        "No T-junction fixes",
        "Does not fix T-junctions, which can leave sparkles between faces.",
    ),
    stock(option(
        "-noTJuncsDetail",
        "No detail T-junction fixes",
        "Does not fix T-junctions on func_detail faces. Helps maps that fail the T-junction pass.",
    )),
    pp(option(
        "-allowdetailcracks",
        "No detail T-junction fixes",
        "Does not fix T-junctions on func_detail faces.",
    )),
    pp(option(
        "-nodetailclip",
        "No detail clipping",
        "Does not clip detail brush faces against each other.",
    )),
    option(
        "-noopt",
        "Keep outer faces",
        "Keeps the faces on the outside of the map that can never be seen.",
    ),
    option(
        "-noprune",
        "No pruning",
        "Does not prune neighbouring solid nodes.",
    ),
    option(
        "-nomerge",
        "No face merging",
        "Does not merge chopped faces back together.",
    ),
    option(
        "-nomergewater",
        "No water merging",
        "Does not merge chopped water faces back together.",
    ),
    option(
        "-nosubdiv",
        "No subdividing",
        "Does not split faces up for lightmaps.",
    ),
    option("-bumpall", "Bump map everything", "Makes every surface bump mapped."),
    pp(option("-nobump", "No bump maps", "Turns off bump mapped lightmaps on every surface.")),
    option(
        "-snapaxial",
        "Snap axial planes",
        "Snaps planes that face along an axis to whole units.",
    ),
    option(
        "-forceskyvis",
        "Visibility in 3D skybox",
        "Computes visibility in the 3D skybox's leaves too.",
    ),
    valued(
        "-luxelscale",
        "Luxel scale",
        "1.0",
        "Scales the lightmaps of every face by this amount.",
    ),
    valued(
        "-minluxelscale",
        "Smallest luxel scale",
        "1.0",
        "No face gets a lightmap scale below this.",
    ),
    pp(valued(
        "-maxluxelscale",
        "Largest luxel scale",
        "999999",
        "No face gets a lightmap scale above this.",
    )),
    valued(
        "-defaultluxelsize",
        "Default luxel size",
        "16",
        "The lightmap scale of faces that have none set.",
    ),
    valued(
        "-maxlightmapdim",
        "Largest lightmap",
        "32",
        "Faces with a bigger lightmap than this are split.",
    ),
    option(
        "-lightifmissing",
        "Lightmaps everywhere",
        "Gives every surface a lightmap, even those that do not need one.",
    ),
    option(
        "-novirtualmesh",
        "No displacement compression",
        "Does not compress the collision of displacements.",
    ),
    pp(option(
        "-nodisp4virtualmesh",
        "Compress power 4 displacements",
        "Compresses the collision of power 4 displacements too.",
    )),
    pp(option("-nophysics", "No collision", "Does not build collision for the map.")),
    pp(option("-nobevel", "No bevels", "Does not bevel brushes for collision.")),
    pp(option(
        "-nodefaultcubemap",
        "No default cubemaps",
        "Does not build the default LDR and HDR cubemaps.",
    )),
    pp(option(
        "-noldrcubemap",
        "No default LDR cubemap",
        "Does not build the default LDR cubemap, for maps that are only HDR.",
    )),
    pp(option(
        "-nohdrcubemap",
        "No default HDR cubemap",
        "Does not build the default HDR cubemap, for maps that are only LDR.",
    )),
    pp(option(
        "-nocull",
        "No culling",
        "Does not reduce brushes, brush sides, vertices or planes.",
    )),
    pp(option("-cullall", "Cull everything", "Always uses every kind of culling.")),
    pp(option(
        "-cullbrushes",
        "Cull brushes",
        "Always culls brushes, even below the limit.",
    )),
    pp(option(
        "-cullbrushsides",
        "Cull brush sides",
        "Always culls brush sides, even below the limit.",
    )),
    pp(option(
        "-cullplanes",
        "Cull planes",
        "Always culls planes, even below the limit.",
    )),
    pp(option(
        "-cullplanesaggressive",
        "Cull planes harder",
        "Culls planes aggressively.",
    )),
    pp(option(
        "-cullverts",
        "Cull vertices",
        "Always culls vertices, even below the limit.",
    )),
    pp(warned(
        valued("-maxbrushes", "Brush limit", "8192", "The most brushes the map may have."),
        "Raising limits makes a map the game may not load.",
    )),
    pp(warned(
        valued(
            "-maxbrushsides",
            "Brush side limit",
            "65536",
            "The most brush sides the map may have.",
        ),
        "Raising limits makes a map the game may not load.",
    )),
    pp(warned(
        valued(
            "-maxdispinfo",
            "Displacement limit",
            "2048",
            "The most displacements the map may have.",
        ),
        "Raising limits makes a map the game may not load.",
    )),
    pp(warned(
        valued(
            "-maxtexinfo",
            "Texture info limit",
            "12288",
            "The most texture infos the map may have.",
        ),
        "Raising limits makes a map the game may not load.",
    )),
    valued(
        "-blocksize",
        "Block size",
        "1024",
        "The size of the grid squares the map is chopped into.",
    ),
    split(valued(
        "-block",
        "Block",
        "0 0",
        "Only compiles this one grid square: X and Y.",
    )),
    split(valued(
        "-blocks",
        "Blocks",
        "0 0 0 0",
        "Only compiles these grid squares: smallest X and Y, then largest X and Y.",
    )),
    pp(valued(
        "-bspformat",
        "BSP version",
        "20",
        "19 for HL2, 20 for TF2 and Garry's Mod, 21 for Alien Swarm, L4D2, Portal 2 and CS:GO.",
    )),
    pp(valued(
        "-staticpropformat",
        "Static prop version",
        "10_TF2",
        "6 for HL2, 7 or 8 for L4D, 9 for L4D2, 10_TF2 for TF2, 10 for Insurgency, 11 or 12 for CS:GO, 13 for Strata.",
    )),
    pp(valued(
        "-lightformat",
        "World light version",
        "0",
        "0 for HL2 and TF2, 1 for Alien Swarm, L4D2, Portal 2 and CS:GO.",
    )),
    pp(option(
        "-singleplayer",
        "SDK 2013 singleplayer",
        "Writes a BSP for Source SDK 2013 singleplayer games.",
    )),
    pp(option("-csgo", "CS:GO format", "Writes a BSP for CS:GO.")),
    pp(option(
        "-csgoclipcontents",
        "CS:GO clip brushes",
        "Compiles CS:GO grenade clip and drone clip brushes.",
    )),
    pp(option("-l4d2", "L4D2 format", "Writes a BSP for Left 4 Dead 2.")),
    pp(option("-portal2", "Portal 2 format", "Writes a BSP for Portal 2.")),
    pp(option("-asw", "Alien Swarm format", "Writes a BSP for Alien Swarm.")),
    pp(option("-insurgency", "Insurgency format", "Writes a BSP for Insurgency.")),
    pp(option(
        "-simpleladders",
        "Simple ladders",
        "Writes ladders as func_simpleladder, like L4D.",
    )),
    pp(option(
        "-matsyscompat",
        "Material compatibility",
        "Loads materials in compatibility mode. On by default outside Garry's Mod and TF2.",
    )),
    pp(valued(
        "-dxlevel",
        "DirectX level",
        "0",
        "Leaves out entities whose DirectX levels rule out this level.",
    )),
    pp(option(
        "-noineligiblevertexlitprops",
        "Keep vertex lighting on bumped props",
        "Does not mark bump mapped or phong props as unable to take vertex lighting.",
    )),
    pp(option(
        "-showineligiblevertexlitprops",
        "List props without vertex lighting",
        "Lists the props marked as unable to take vertex lighting.",
    )),
    option(
        "-glview",
        "Write GL view files",
        "Writes .gl files that glview can show.",
    ),
    stock(option(
        "-dumpstaticprops",
        "Dump static props",
        "Writes the static props to staticprop text files.",
    )),
    pp(option(
        "-dumpstaticprop",
        "Dump static props",
        "Writes the static props to staticprop text files.",
    )),
    option(
        "-dumpcollide",
        "Dump collision",
        "Writes files with collision information.",
    ),
    pp(option(
        "-materialdebug",
        "Material debugging",
        "Prints debugging information about materials.",
    )),
    pp(option(
        "-report_search_paths",
        "List search paths",
        "Prints the folders searched for content.",
    )),
    stock(option(
        "-tmpout",
        "Write to tmp",
        "Writes the output into the \\tmp folder.",
    )),
    stock(option(
        "-FullMinidumps",
        "Full crash dumps",
        "Writes large crash dumps.",
    )),
    option("-low", "Low priority", "Runs VBSP below normal priority."),
    option("-verbose", "Verbose", "Prints more information."),
    option(
        "-verboseentities",
        "Quiet brush entities",
        "With Verbose on, leaves out the details of brush entities.",
    ),
    valued("-threads", "Threads", "8", "How many threads to use."),
];

const VVIS_OPTIONS: &[OptionSpec] = &[
    warned(
        option(
            "-fast",
            "Fast",
            "A quick, rough visibility pass for testing. The game draws more than it needs to.",
        ),
        "Not for release: the map will run slower.",
    ),
    valued(
        "-radius_override",
        "Visibility radius",
        "2500",
        "Treats anything farther away than this many units as never visible, overriding the fog.",
    ),
    option("-nosort", "No sorting", "Does not sort portals."),
    split(valued(
        "-trace",
        "Trace clusters",
        "0 1",
        "Writes a line file that traces visibility from one cluster to another.",
    )),
    stock(option(
        "-tmpin",
        "Read from tmp",
        "Reads the portals from the \\tmp folder.",
    )),
    stock(option(
        "-FullMinidumps",
        "Full crash dumps",
        "Writes large crash dumps.",
    )),
    option("-low", "Low priority", "Runs VVIS below normal priority."),
    option("-verbose", "Verbose", "Prints more information."),
    valued("-threads", "Threads", "8", "How many threads to use."),
];

const VRAD_OPTIONS: &[OptionSpec] = &[
    option("-both", "LDR and HDR", "Bakes lighting for both LDR and HDR."),
    option("-hdr", "HDR only", "Bakes HDR lighting only."),
    option("-ldr", "LDR only", "Bakes LDR lighting only."),
    warned(
        option("-fast", "Fast", "Quick, low quality lighting for testing."),
        "Not for release: lighting will look wrong.",
    ),
    warned(
        option(
            "-final",
            "Final quality",
            "Takes more samples of the sky, area lights and props for smoother shadows.",
        ),
        "Much slower.",
    ),
    option(
        "-fastambient",
        "Fast ambient",
        "Takes fewer ambient lighting samples per leaf, to save time.",
    ),
    option(
        "-StaticPropLighting",
        "Static prop lighting",
        "Lights static props per vertex instead of from one sample.",
    ),
    option(
        "-StaticPropPolys",
        "Props cast shadows",
        "Static props block light using their real shape.",
    ),
    option(
        "-TextureShadows",
        "Texture shadows",
        "Translucent textures on props cast matching shadows. Needs the models listed in lights.rad.",
    ),
    pp(option(
        "-forcetextureshadows",
        "Texture shadows on every prop",
        "Lets every prop cast texture shadows.",
    )),
    pp(option(
        "-worldtextureshadows",
        "World texture shadows",
        "Translucent textures on brushes cast matching shadows.",
    )),
    pp(option(
        "-translucentshadows",
        "Translucent shadows",
        "Texture shadows also come from $translucent surfaces.",
    )),
    valued(
        "-StaticPropSampleScale",
        "Prop sample scale",
        "4",
        "Scales how many lighting samples props take. -final uses 4 in Hammer++ and 16 in the game's VRAD.",
    ),
    pp(valued(
        "-StaticPropIndirectMode",
        "Prop bounce lighting",
        "1",
        "How props get bounced light: 0 default, 1 TF2, 2 Orange Box.",
    )),
    stock(option(
        "-StaticPropBounce",
        "Props bounce light",
        "Static props bounce light onto their surroundings. Experimental.",
    )),
    pp(option(
        "-propambient",
        "Prop ambient tracing",
        "Traces props for ambient lighting.",
    )),
    option(
        "-nossprops",
        "No prop self-shadowing",
        "Turns off self-shadowing on every static prop.",
    ),
    option(
        "-noskyboxrecurse",
        "Ignore 3D skybox",
        "Does not let the 3D skybox cast shadows on the world.",
    ),
    valued("-bounce", "Light bounces", "100", "How many times light bounces. 0 turns bouncing off."),
    valued(
        "-smooth",
        "Smoothing angle",
        "45",
        "Faces meeting at less than this many degrees get smoothed lighting.",
    ),
    valued(
        "-luxeldensity",
        "Luxel density",
        "1.0",
        "Scales the lightmap resolution of every face. The game's VRAD only takes values below 1.",
    ),
    valued(
        "-extrasky",
        "Extra sky samples",
        "16",
        "Traces this many times more rays for sky and bounced light.",
    ),
    pp(valued(
        "-extrasoft",
        "Extra soft light samples",
        "4",
        "Traces this many times more rays for soft lights.",
    )),
    valued(
        "-softsun",
        "Soft sun",
        "2",
        "Treats the sun as an area light this many degrees wide, for soft shadows.",
    ),
    stock(valued(
        "-SunSamplesAreaLight",
        "Sun area samples",
        "5",
        "How many samples to take of the light_environment. 0 to 5 is recommended.",
    )),
    valued("-chop", "Chop", "4", "Smallest patch size for bounced light, in luxels."),
    valued("-maxchop", "Max chop", "4", "Largest patch size for bounced light, in luxels."),
    pp(valued(
        "-patchlightmapsize",
        "Patch lightmap size",
        "16",
        "The smallest lightmap scale used for chopping patches.",
    )),
    option(
        "-noextra",
        "No supersampling",
        "Turns off supersampling: faster, with blockier shadows.",
    ),
    option("-centersamples", "Centre samples", "Moves the sample centres."),
    option(
        "-dlightmap",
        "Separate direct lightmap",
        "Puts direct light in a different lightmap from bounced light.",
    ),
    pp(option(
        "-indirectonly",
        "Only bounced light",
        "Bakes only bounced light into the map.",
    )),
    option(
        "-ambientocclusion",
        "Ambient occlusion",
        "Bakes ambient occlusion into the lightmaps.",
    ),
    valued(
        "-aoscale",
        "Occlusion strength",
        "1.0",
        "How dark ambient occlusion is. The Hammer++ default is 0.5.",
    ),
    pp(valued(
        "-aoradius",
        "Occlusion radius",
        "40",
        "How far ambient occlusion reaches, in units.",
    )),
    valued(
        "-aofacesamples",
        "Occlusion samples on brushes",
        "32",
        "Ambient occlusion samples per brush face. -final uses 128.",
    ),
    valued(
        "-aopropsamples",
        "Occlusion samples on props",
        "16",
        "Ambient occlusion samples per prop. -final uses 32.",
    ),
    stock(option(
        "-aoNoSquare",
        "Softer occlusion",
        "Does not square ambient occlusion, so it looks less harsh.",
    )),
    pp(option(
        "-aodebug",
        "Show only occlusion",
        "Bakes only ambient occlusion, with every light off.",
    )),
    stock(option(
        "-softenCosine",
        "Softer lighting",
        "Makes lighting falloff softer.",
    )),
    stock(option(
        "-bounceFromNamed",
        "Bounce named lights",
        "Named lights also give bounced light.",
    )),
    stock(option(
        "-AmbientFromLeafCenters",
        "Ambient from leaf centres",
        "Samples ambient lighting from the centre of each leaf instead of random points.",
    )),
    stock(valued(
        "-LeafAmbientSampleReduction",
        "Fewer ambient samples",
        "0.5",
        "Reduces the ambient samples in each leaf by this fraction, from 0 to 1.",
    )),
    pp(split(valued(
        "-ambient",
        "Minimum ambient",
        "0 0 0",
        "The least light any part of the map gets: red, green and blue.",
    ))),
    pp(valued(
        "-scale",
        "Light scale",
        "1",
        "Multiplies the brightness of every light.",
    )),
    pp(valued(
        "-reflectivityscale",
        "Reflectivity scale",
        "1",
        "Multiplies the $reflectivity of every texture.",
    )),
    pp(valued(
        "-coring",
        "Coring",
        "1",
        "Light below this is made black, which shrinks the lightmaps.",
    )),
    pp(valued(
        "-dlight",
        "Texture light threshold",
        "0.1",
        "The least light a texture must give to become a texture light.",
    )),
    pp(option(
        "-choptexlights",
        "Chop texture lights",
        "Chops up texture lights that come from a lights file.",
    )),
    pp(option(
        "-notexscale",
        "No texture light scaling",
        "Does not scale texture lights by the texture's scale.",
    )),
    pp(option(
        "-nolightcull",
        "No light culling",
        "Does not cut lights off by distance.",
    )),
    pp(valued(
        "-minlightcullintensity",
        "Light cut-off",
        "1",
        "How faint a light gets before it is cut off.",
    )),
    pp(option("-skipSunLights", "Skip sun", "Leaves out sun lights.")),
    pp(option(
        "-skipAmbientLights",
        "Skip ambient lights",
        "Leaves out ambient lights.",
    )),
    pp(option(
        "-skipLocalLights",
        "Skip point and spot lights",
        "Leaves out point lights and spotlights.",
    )),
    pp(option(
        "-skipPVS",
        "Skip PVS optimisation",
        "Does not use visibility to speed up lights.",
    )),
    pp(option(
        "-fastradial",
        "Fast radial accumulation",
        "Uses a faster way to gather light onto faces.",
    )),
    pp(option(
        "-nopairedges",
        "No edge pairing",
        "Does not pair brush edges when gathering light.",
    )),
    pp(option(
        "-nopairgeometricedges",
        "No geometric edge pairing",
        "Does not pair edges by shape, which prevents seams against brush entities.",
    )),
    option(
        "-nodetaillight",
        "No detail prop lighting",
        "Does not light detail props.",
    ),
    option(
        "-onlydetail",
        "Only detail props",
        "Only lights detail props and the per-leaf lighting.",
    ),
    option(
        "-OnlyStaticProps",
        "Only static props",
        "Only does direct lighting of static props. For debugging.",
    ),
    option(
        "-StaticPropNormals",
        "Show prop normals",
        "Lights static props with their normals instead. For debugging.",
    ),
    valued(
        "-maxdispsamplesize",
        "Displacement sample size",
        "512",
        "The largest lighting sample size on displacements.",
    ),
    option(
        "-LargeDispSampleRadius",
        "Wide displacement sampling",
        "Gathers light over a wider area on displacements, which fixes splotches on terrain.",
    ),
    pp(option(
        "-DisplacementLightingOld",
        "Old displacement lighting",
        "Lights displacements the old way.",
    )),
    pp(valued(
        "-dispchop",
        "Displacement chop",
        "8",
        "Smallest patch size on displacements, in luxels.",
    )),
    pp(valued(
        "-disppatchradius",
        "Displacement patch radius",
        "1500",
        "The largest radius of a displacement patch.",
    )),
    pp(valued(
        "-dispsamplefactor",
        "Displacement light radius",
        "1",
        "Multiplies how far light reaches on displacements.",
    )),
    pp(valued(
        "-disppatchsamplefactor",
        "Displacement bounce radius",
        "1",
        "Multiplies how far bounced light reaches on displacements.",
    )),
    valued(
        "-lights",
        "Extra lights file",
        "",
        "Loads this lights file as well as lights.rad and the map's own.",
    ),
    pp(option(
        "-supportslightdirectional",
        "Keep light_directional",
        "Does not rename light_directional entities.",
    )),
    pp(option(
        "-supportslightprojected",
        "Keep light_projected",
        "Does not rename light_projected entities.",
    )),
    pp(warned(
        option(
            "-extendedsoftlimits",
            "No soft limit warnings",
            "Does not warn about going over soft limits.",
        ),
        "The game may not load a map over its limits.",
    )),
    pp(valued("-maxbrushes", "Brush limit", "8192", "The most brushes the map may have.")),
    pp(valued(
        "-maxbrushsides",
        "Brush side limit",
        "65536",
        "The most brush sides the map may have.",
    )),
    pp(valued(
        "-maxdispinfo",
        "Displacement limit",
        "2048",
        "The most displacements the map may have.",
    )),
    pp(valued(
        "-maxtexinfo",
        "Texture info limit",
        "12288",
        "The most texture infos the map may have.",
    )),
    option("-dump", "Dump patches", "Writes patch data for debugging."),
    option("-dumpnormals", "Dump normals", "Writes normals for debugging."),
    option(
        "-dumptrace",
        "Dump ray tracing",
        "Writes the ray-tracing scene for debugging.",
    ),
    pp(option(
        "-dumppropmaps",
        "Dump prop lightmaps",
        "Writes prop lightmaps to the game's sp_lightmaps folder.",
    )),
    option(
        "-debugextra",
        "Show supersampling",
        "Marks supersampling in the lightmaps, for debugging.",
    ),
    option(
        "-loghash",
        "Log sample hash",
        "Writes the sample hash table to samplehash.txt.",
    ),
    pp(option(
        "-propdebug",
        "List props",
        "Lists the props being lit, for debugging.",
    )),
    pp(option(
        "-rederrors",
        "Black errors",
        "Shows lighting errors as black instead of red.",
    )),
    pp(option(
        "-report_search_paths",
        "List search paths",
        "Prints the folders searched for content.",
    )),
    stock(option(
        "-FullMinidumps",
        "Full crash dumps",
        "Writes large crash dumps.",
    )),
    option("-low", "Low priority", "Runs VRAD below normal priority."),
    option("-verbose", "Verbose", "Prints more information."),
    valued("-threads", "Threads", "8", "How many threads to use."),
];

const VBSPINFO_OPTIONS: &[OptionSpec] = &[
    option(
        "-treeinfo",
        "Tree information",
        "Prints how the BSP tree is laid out.",
    ),
    option(
        "-worldtexturestats",
        "World texture statistics",
        "Lists the textures the world uses.",
    ),
    option(
        "-modelstats",
        "Model statistics",
        "Prints statistics for each brush model.",
    ),
    option(
        "-liststaticprops",
        "List static props",
        "Lists every static prop in the map.",
    ),
    option("-size", "World bounds", "Prints the bounds of the world."),
];

const REPACK_OPTIONS: &[OptionSpec] = &[pp(valued(
    "-threads",
    "Threads",
    "8",
    "How many threads to use. Uses every thread when this is off.",
))];

const SOURCECUBEMAP_OPTIONS: &[OptionSpec] = &[
    valued(
        "--quality",
        "Quality",
        "standard",
        "fast, standard or high. High renders at double resolution.",
    ),
    valued(
        "--hdr",
        "Lighting",
        "auto",
        "auto, ldr, hdr or both. Auto builds whatever lighting the map has.",
    ),
    valued(
        "--profile",
        "Game rules",
        "gmod",
        "gmod or hl2. Mallet picks one from the game folder when this is off.",
    ),
    warned(
        option(
            "--experimental",
            "Experimental",
            "Uses the closest match for anything that cannot be reproduced exactly, and lists it.",
        ),
        "Reflections are close to the game's own, not identical.",
    ),
    valued(
        "--water",
        "Water",
        "planar",
        "strict or planar. Planar renders water reflections and refraction, and needs Experimental.",
    ),
    valued(
        "--capture-time",
        "Capture time",
        "0",
        "The game time, in seconds, at which animated textures and the sky are captured.",
    ),
    option(
        "--incremental",
        "Incremental",
        "Reuses cubemaps that have not changed since the last build.",
    ),
    valued(
        "--mount",
        "Extra content",
        "",
        "Adds a content folder or archive the game does not load by itself.",
    ),
    valued(
        "--device",
        "Graphics card",
        "",
        "Uses the graphics card whose name contains this text, such as 3050.",
    ),
    valued(
        "--cache",
        "Cache folder",
        "",
        "Keeps SourceCubemap's cache in this folder.",
    ),
    valued(
        "--cache-limit-mb",
        "Cache limit",
        "2048",
        "The most the cache may grow to, in megabytes. 0 stops cache writes.",
    ),
    option(
        "--no-cache",
        "No cache",
        "Does not read or write SourceCubemap's cache.",
    ),
    valued(
        "--previews",
        "Previews folder",
        "{mapdir}/{map}_cubemaps",
        "Writes six PNG previews of every cubemap into this folder.",
    ),
    valued(
        "--json",
        "Report",
        "{mapdir}/{map}_cubemaps.json",
        "Saves the full build report as JSON.",
    ),
    option("--verbose", "Verbose", "Prints more information."),
];

const GAME_OPTIONS: &[OptionSpec] = &[
    option("-novid", "Skip intro video", "Starts without the intro video."),
    option("-console", "Console", "Opens the console on start."),
    option("-dev", "Developer mode", "Turns on developer messages."),
    option("-condebug", "Log the console", "Writes the console to console.log."),
    option("-windowed", "Windowed", "Runs in a window."),
    option("-noborder", "No border", "Runs in a window without a border."),
    option("-fullscreen", "Fullscreen", "Runs fullscreen."),
    valued("-w", "Width", "1600", "Window width in pixels."),
    valued("-h", "Height", "900", "Window height in pixels."),
    valued(
        "-dxlevel",
        "DirectX level",
        "95",
        "Starts with this DirectX level, such as 90 or 95.",
    ),
    option("-high", "High priority", "Runs the game above normal priority."),
    option("-nojoy", "No joystick", "Skips joystick support for a faster start."),
    option(
        "-multirun",
        "Allow more than one",
        "Lets this game run beside another copy of itself. Garry's Mod only.",
    ),
    valued(
        "+maxplayers",
        "Player slots",
        "2",
        "Starts a listen server with this many slots, to test multiplayer.",
    ),
    option("-tools", "Tools mode", "Starts the engine's tools mode."),
    option("+sv_cheats 1", "Cheats", "Turns cheats on."),
    option("-insecure", "Insecure", "Disables Valve Anti-Cheat so local mods load."),
];

/// One option turned on in a step, with its value when it takes one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepOption {
    pub flag: String,
    #[serde(default)]
    pub value: String,
}

/// One step of a preset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Step {
    pub kind: StepKind,
    pub enabled: bool,
    /// Options from the catalog that are turned on.
    pub options: Vec<StepOption>,
    /// More arguments, appended as typed. May use variables.
    pub extra_args: String,
    /// For custom steps: the name shown in the list.
    pub name: String,
    /// For custom steps: the program; for tool steps, overrides Mallet Options.
    pub program: Option<PathBuf>,
    /// For custom steps: the arguments, which may use variables.
    pub args: String,
    /// For custom steps: the working folder, which may use variables. Empty for the
    /// map's folder.
    pub working_dir: String,
    /// Keep compiling when this step fails.
    pub continue_on_error: bool,
}

impl Default for Step {
    fn default() -> Self {
        Self::new(StepKind::Custom)
    }
}

impl Step {
    pub fn new(kind: StepKind) -> Self {
        let mut step = Self {
            kind,
            enabled: true,
            options: Vec::new(),
            extra_args: String::new(),
            name: String::new(),
            program: None,
            args: String::new(),
            working_dir: String::new(),
            continue_on_error: false,
        };
        // Sensible defaults, as a fresh CompilePal step has.
        match kind {
            StepKind::SourceCubemap => step.set_option("--experimental", true),
            StepKind::RunGame => {
                for flag in ["-novid", "-console", "-dev", "-windowed"] {
                    step.set_option(flag, true);
                }
            }
            _ => {}
        }
        // A report or a game that fails to start should not mark the compile as failed.
        step.continue_on_error = matches!(kind, StepKind::Vbspinfo | StepKind::RunGame);
        step
    }

    /// The name shown in the step list.
    pub fn title(&self) -> String {
        if self.kind == StepKind::Custom && !self.name.trim().is_empty() {
            self.name.clone()
        } else {
            self.kind.label().to_owned()
        }
    }

    pub fn option(&self, flag: &str) -> Option<&StepOption> {
        self.options.iter().find(|option| option.flag == flag)
    }

    /// Turns an option on with its catalog default value, or off.
    pub fn set_option(&mut self, flag: &str, on: bool) {
        let present = self.option(flag).is_some();
        if on && !present {
            let value = self
                .kind
                .options()
                .iter()
                .find(|spec| spec.flag == flag)
                .and_then(|spec| spec.value)
                .unwrap_or("")
                .to_owned();
            self.options.push(StepOption {
                flag: flag.to_owned(),
                value,
            });
        } else if !on {
            self.options.retain(|option| option.flag != flag);
        }
    }

    pub fn with(mut self, flags: &[&str]) -> Self {
        for flag in flags {
            self.set_option(flag, true);
        }
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

/// A named list of steps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub steps: Vec<Step>,
}

/// The presets a new install starts with, like CompilePal's.
pub fn default_presets() -> Vec<Preset> {
    let tail = || vec![Step::new(StepKind::CopyToGame), Step::new(StepKind::RunGame).disabled()];
    let preset = |name: &str, description: &str, mut steps: Vec<Step>| {
        steps.extend(tail());
        Preset {
            name: name.to_owned(),
            description: description.to_owned(),
            steps,
        }
    };
    vec![
        preset(
            "Fast",
            "Quick test compile: rough visibility and lighting.",
            vec![
                Step::new(StepKind::Vbsp),
                Step::new(StepKind::Vvis).with(&["-fast"]),
                Step::new(StepKind::Vrad).with(&["-fast", "-ldr"]),
            ],
        ),
        preset(
            "Normal",
            "Full visibility and lighting for LDR and HDR.",
            vec![
                Step::new(StepKind::Vbsp),
                Step::new(StepKind::Vvis),
                Step::new(StepKind::Vrad).with(&["-both"]),
                Step::new(StepKind::SourceCubemap).disabled(),
            ],
        ),
        preset(
            "Final",
            "Release quality: final lighting, prop lighting and shadows, cubemaps and a compressed BSP.",
            vec![
                Step::new(StepKind::Vbsp),
                Step::new(StepKind::Vvis),
                Step::new(StepKind::Vrad).with(&[
                    "-both",
                    "-final",
                    "-StaticPropLighting",
                    "-StaticPropPolys",
                    "-TextureShadows",
                ]),
                Step::new(StepKind::SourceCubemap).with(&["--quality"]),
                Step::new(StepKind::Repack),
                Step::new(StepKind::Vbspinfo),
            ],
        ),
        preset(
            "Entities only",
            "Updates only the entities of the last compile. Seconds instead of minutes.",
            vec![Step::new(StepKind::Vbsp).with(&["-onlyents"])],
        ),
        preset(
            "Cubemaps",
            "Rebuilds the cubemaps of the last compile with SourceCubemap.",
            vec![Step::new(StepKind::SourceCubemap)],
        ),
    ]
}

fn presets_path() -> Option<PathBuf> {
    config::data_dir().map(|dir| dir.join("compile_presets.json"))
}

/// The saved presets, or the defaults when none are saved.
pub fn load_presets() -> Vec<Preset> {
    presets_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<Vec<Preset>>(&text).ok())
        .filter(|presets| !presets.is_empty())
        .unwrap_or_else(default_presets)
}

pub fn save_presets(presets: &[Preset]) -> Result<(), String> {
    let path = presets_path().ok_or("Mallet's settings folder is unavailable, so presets cannot be saved")?;
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    }
    let text = serde_json::to_string_pretty(presets).map_err(|error| error.to_string())?;
    fs::write(path, text).map_err(|error| error.to_string())
}

/// The variables steps may use, with what they mean.
pub const VARIABLES: [(&str, &str); 7] = [
    ("{map}", "Map name without extension"),
    ("{vmf}", "Full path of the VMF"),
    ("{bsp}", "Full path of the compiled BSP"),
    ("{mapdir}", "Folder of the VMF and BSP"),
    ("{gamedir}", "Game folder holding gameinfo.txt"),
    ("{gamemaps}", "The game's maps folder"),
    ("{bindir}", "The game's bin folder"),
];

/// What a compile is about: the map and the game.
#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub vmf: PathBuf,
    pub game_dir: PathBuf,
}

impl Context {
    pub fn map_name(&self) -> String {
        self.vmf
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn map_dir(&self) -> PathBuf {
        self.vmf.parent().map(Path::to_owned).unwrap_or_default()
    }

    pub fn bsp(&self) -> PathBuf {
        self.vmf.with_extension("bsp")
    }

    pub fn game_maps(&self) -> PathBuf {
        self.game_dir.join("maps")
    }

    /// Replaces the variables in `text`.
    pub fn expand(&self, text: &str) -> String {
        let bin = self.game_dir.parent().map(|root| root.join("bin")).unwrap_or_default();
        text.replace("{map}", &self.map_name())
            .replace("{vmf}", &display(&self.vmf))
            .replace("{bsp}", &display(&self.bsp()))
            .replace("{mapdir}", &display(&self.map_dir()))
            .replace("{gamedir}", &display(&self.game_dir))
            .replace("{gamemaps}", &display(&self.game_maps()))
            .replace("{bindir}", &display(&bin))
    }

    /// Which SourceCubemap rules fit the game.
    fn cubemap_profile(&self) -> &'static str {
        let is_gmod = self
            .game_dir
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("garrysmod"));
        if is_gmod {
            "gmod"
        } else {
            "hl2"
        }
    }
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

/// A program to run, with its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLine {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
}

impl CommandLine {
    /// The command as one line, quoted the way a shell would need it.
    pub fn display(&self) -> String {
        std::iter::once(display(&self.program))
            .chain(self.args.iter().cloned())
            .map(|part| {
                if part.contains(' ') || part.is_empty() {
                    format!("\"{part}\"")
                } else {
                    part
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// What one step of a compile will do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Run a program and wait for it.
    Run(CommandLine),
    /// Start a program and move on.
    Launch(CommandLine),
    /// Copy a file.
    Copy { from: PathBuf, to: PathBuf },
}

impl Action {
    pub fn display(&self) -> String {
        match self {
            Action::Run(command) | Action::Launch(command) => command.display(),
            Action::Copy { from, to } => format!("copy \"{}\" \"{}\"", from.display(), to.display()),
        }
    }
}

/// A step ready to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedStep {
    pub title: String,
    pub kind: StepKind,
    pub action: Action,
    pub continue_on_error: bool,
}

/// Splits arguments at spaces, keeping "quoted parts" together.
pub fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(current);
    }
    args
}

/// The arguments of a step's turned-on options, with variables expanded. Options such as
/// `+sv_cheats 1` that contain a space become two arguments, and so do the values of
/// options that take several numbers, such as `-ambient 0 0 0`.
fn option_args(step: &Step, context: &Context) -> Vec<String> {
    let mut args = Vec::new();
    for option in &step.options {
        args.extend(option.flag.split_whitespace().map(str::to_owned));
        let spec = step.kind.options().iter().find(|spec| spec.flag == option.flag);
        let Some(spec) = spec.filter(|spec| spec.value.is_some()) else {
            continue;
        };
        let value = context.expand(option.value.trim());
        if spec.split {
            args.extend(split_args(&value));
        } else if !value.is_empty() {
            args.push(value);
        }
    }
    args.extend(split_args(&context.expand(&step.extra_args)));
    args
}

/// Works out the command for one step. `tool` finds the program for a tool kind.
pub fn plan_step(
    step: &Step,
    context: &Context,
    tool: &dyn Fn(ToolKind) -> Option<PathBuf>,
) -> Result<PlannedStep, String> {
    let program = |kind: ToolKind| -> Result<PathBuf, String> {
        step.program
            .clone()
            .filter(|path| !path.as_os_str().is_empty())
            .or_else(|| tool(kind))
            .ok_or_else(|| {
                format!(
                    "{} was not found. Set it in Edit > Mallet Options > Compile tools.",
                    kind.label()
                )
            })
    };
    let game = display(&context.game_dir);
    let bsp = display(&context.bsp());
    let no_extension = |path: PathBuf| display(&path.with_extension(""));
    let map_dir = context.map_dir();
    let run = |kind: ToolKind, mut args: Vec<String>, tail: Vec<String>| -> Result<Action, String> {
        args.extend(option_args(step, context));
        args.extend(tail);
        Ok(Action::Run(CommandLine {
            program: program(kind)?,
            args,
            working_dir: map_dir.clone(),
        }))
    };
    let action = match step.kind {
        StepKind::Vbsp | StepKind::Vvis | StepKind::Vrad => {
            let tool = step.kind.tool().expect("compilers are tools");
            run(
                tool,
                vec!["-game".to_owned(), game],
                vec![no_extension(context.vmf.clone())],
            )?
        }
        StepKind::Vbspinfo => run(ToolKind::Vbspinfo, Vec::new(), vec![bsp])?,
        StepKind::Repack => run(
            ToolKind::Bspzip,
            vec!["-repack".to_owned(), "-compress".to_owned()],
            vec![bsp],
        )?,
        StepKind::SourceCubemap => {
            let mut args = vec![
                "build".to_owned(),
                bsp,
                "--game".to_owned(),
                game,
                // Keep the map's name: the cubemaps are stored under it.
                "--in-place".to_owned(),
                "--no-backup".to_owned(),
            ];
            if step.option("--profile").is_none() {
                args.extend(["--profile".to_owned(), context.cubemap_profile().to_owned()]);
            }
            run(ToolKind::SourceCubemap, args, Vec::new())?
        }
        StepKind::CopyToGame => Action::Copy {
            from: context.bsp(),
            to: context.game_maps().join(format!("{}.bsp", context.map_name())),
        },
        StepKind::RunGame => {
            let mut args = vec!["-game".to_owned(), game];
            args.extend(option_args(step, context));
            args.extend(["+map".to_owned(), context.map_name()]);
            let program = program(ToolKind::Game)?;
            let working_dir = program.parent().map(Path::to_owned).unwrap_or_default();
            Action::Launch(CommandLine {
                program,
                args,
                working_dir,
            })
        }
        StepKind::Custom => {
            let program = step
                .program
                .clone()
                .filter(|path| !path.as_os_str().is_empty())
                .ok_or_else(|| format!("{} has no program to run.", step.title()))?;
            let program = PathBuf::from(context.expand(&display(&program)));
            let working_dir = match context.expand(step.working_dir.trim()) {
                dir if dir.is_empty() => map_dir.clone(),
                dir => PathBuf::from(dir),
            };
            let mut args = split_args(&context.expand(&step.args));
            args.extend(split_args(&context.expand(&step.extra_args)));
            Action::Run(CommandLine {
                program,
                args,
                working_dir,
            })
        }
    };
    Ok(PlannedStep {
        title: step.title(),
        kind: step.kind,
        action,
        continue_on_error: step.continue_on_error,
    })
}

/// Plans every enabled step of a preset.
pub fn plan(
    preset: &Preset,
    context: &Context,
    tool: &dyn Fn(ToolKind) -> Option<PathBuf>,
) -> Result<Vec<PlannedStep>, String> {
    let steps: Vec<PlannedStep> = preset
        .steps
        .iter()
        .filter(|step| step.enabled)
        .map(|step| plan_step(step, context, tool))
        .collect::<Result<_, _>>()?;
    if steps.is_empty() {
        return Err(format!("the preset \"{}\" has no steps turned on", preset.name));
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> Context {
        Context {
            vmf: PathBuf::from("D:/maps/my map.vmf"),
            game_dir: PathBuf::from("C:/Steam/GarrysMod/garrysmod"),
        }
    }

    fn tools(kind: ToolKind) -> Option<PathBuf> {
        Some(PathBuf::from(format!("C:/bin/{}.exe", kind.key())))
    }

    #[test]
    fn splits_quoted_arguments() {
        assert_eq!(split_args(r#"-a "b c" d  "" e"#), ["-a", "b c", "d", "", "e"]);
    }

    #[test]
    fn plans_the_compilers_with_game_and_map() {
        let step = Step::new(StepKind::Vrad).with(&["-both", "-bounce"]);
        let planned = plan_step(&step, &context(), &tools).unwrap();
        let Action::Run(command) = planned.action else {
            panic!("VRAD runs")
        };
        assert_eq!(command.program, PathBuf::from("C:/bin/vrad.exe"));
        assert_eq!(
            command.args,
            [
                "-game",
                "C:/Steam/GarrysMod/garrysmod",
                "-both",
                "-bounce",
                "100",
                "D:/maps/my map"
            ]
        );
        assert!(command.display().contains("\"D:/maps/my map\""));
    }

    #[test]
    fn sourcecubemap_keeps_the_map_name_and_picks_the_profile() {
        let planned = plan_step(&Step::new(StepKind::SourceCubemap), &context(), &tools).unwrap();
        let Action::Run(command) = planned.action else {
            panic!("SourceCubemap runs")
        };
        assert_eq!(command.args[..2], ["build", "D:/maps/my map.bsp"]);
        assert!(command.args.contains(&"--in-place".to_owned()));
        let profile = command.args.iter().position(|arg| arg == "--profile").unwrap();
        assert_eq!(command.args[profile + 1], "gmod");
        assert!(command.args.contains(&"--experimental".to_owned()));
    }

    #[test]
    fn custom_steps_expand_variables() {
        let step = Step {
            program: Some(PathBuf::from("{bindir}/tool.exe")),
            args: "--in \"{bsp}\" --name {map}".to_owned(),
            working_dir: "{gamemaps}".to_owned(),
            ..Step::new(StepKind::Custom)
        };
        let planned = plan_step(&step, &context(), &tools).unwrap();
        let Action::Run(command) = planned.action else {
            panic!("custom steps run")
        };
        assert_eq!(command.program, PathBuf::from("C:/Steam/GarrysMod/bin/tool.exe"));
        assert_eq!(command.args, ["--in", "D:/maps/my map.bsp", "--name", "my", "map"]);
        assert_eq!(command.working_dir, PathBuf::from("C:/Steam/GarrysMod/garrysmod/maps"));
    }

    #[test]
    fn copy_and_run_game() {
        let copy = plan_step(&Step::new(StepKind::CopyToGame), &context(), &tools).unwrap();
        assert_eq!(
            copy.action,
            Action::Copy {
                from: PathBuf::from("D:/maps/my map.bsp"),
                to: PathBuf::from("C:/Steam/GarrysMod/garrysmod/maps/my map.bsp"),
            }
        );
        let game = plan_step(
            &Step::new(StepKind::RunGame).with(&["+sv_cheats 1"]),
            &context(),
            &tools,
        )
        .unwrap();
        let Action::Launch(command) = game.action else {
            panic!("the game is launched")
        };
        assert!(command.args.ends_with(&[
            "+sv_cheats".to_owned(),
            "1".to_owned(),
            "+map".to_owned(),
            "my map".to_owned()
        ]));
    }

    #[test]
    fn several_number_values_become_separate_arguments() {
        let mut step = Step::new(StepKind::Vrad).with(&["-ambient", "-lights"]);
        step.options[1].value = "{mapdir}/extra lights.rad".to_owned();
        let planned = plan_step(&step, &context(), &tools).unwrap();
        let Action::Run(command) = planned.action else {
            panic!("VRAD runs")
        };
        assert_eq!(
            command.args[2..8],
            ["-ambient", "0", "0", "0", "-lights", "D:/maps/extra lights.rad"]
        );
    }

    #[test]
    fn option_flags_are_unique_per_step() {
        for kind in StepKind::ALL {
            let mut seen = std::collections::HashSet::new();
            for spec in kind.options() {
                assert!(seen.insert(spec.flag), "{} lists {} twice", kind.label(), spec.flag);
            }
        }
    }

    #[test]
    fn hammer_plusplus_builds_are_recognised() {
        let plusplus = Build::of(Path::new("C:/GarrysMod/bin/win64/vradplusplus.exe"));
        let stock = Build::of(Path::new("C:/GarrysMod/bin/win64/vrad.exe"));
        assert_eq!(plusplus, Build::PlusPlus);
        assert_eq!(stock, Build::Stock);
        assert!(plusplus.supports(Build::Any) && plusplus.supports(Build::PlusPlus));
        assert!(!stock.supports(Build::PlusPlus));
    }

    #[test]
    fn missing_tools_are_reported() {
        let error = plan_step(&Step::new(StepKind::Vbsp), &context(), &|_| None).unwrap_err();
        assert!(error.contains("VBSP was not found"));
    }

    #[test]
    fn default_presets_plan_and_round_trip() {
        let presets = default_presets();
        for preset in &presets {
            plan(preset, &context(), &tools).unwrap();
        }
        let text = serde_json::to_string(&presets).unwrap();
        assert_eq!(serde_json::from_str::<Vec<Preset>>(&text).unwrap(), presets);
    }
}
