//! Source's ambient sound, written down the way the engine runs it, with no audio device
//! in it.
//!
//! [`MapSounds::read`] turns a map's `env_soundscape`, `env_soundscape_proxy`,
//! `env_soundscape_triggerable`, `trigger_soundscape` and `ambient_generic` entities into
//! what the game makes of them. [`SoundscapeChooser`] picks the soundscape a listener
//! hears the way the server does: the nearest enabled soundscape entity whose radius the
//! listener is in and whose origin a brush trace reaches, kept until another one takes
//! over, with `trigger_soundscape` volumes on top. [`SoundscapePlayer`] runs a soundscape
//! script the way the client does: looping sounds that cross-fade over
//! [`SOUNDSCAPE_FADE_TIME`] seconds, random sounds on their timers, sub-soundscapes with
//! their volume and position overrides. [`distance_gain`] and [`pan`] are the engine's
//! falloff with distance for a sound level and its two-speaker spread.
//!
//! Sounds are named and handed to a [`SoundSink`]; loading and mixing them belongs to
//! the caller.

use std::collections::{BTreeSet, HashMap};

use glam::DVec3;
use mallet_kv::{Block, Item};

use crate::geometry::{faces_bounds, solid_faces, Bounds, Plane};
use crate::{Entity, Vmf};

/// `SNDLVL_NONE`: the sound does not fade with distance.
pub const SOUNDLEVEL_NONE: i32 = 0;
/// `SNDLVL_NORM`, the sound level of `ATTN_NORM`.
pub const SOUNDLEVEL_NORM: i32 = 75;
/// `PITCH_NORM`: a sound played at its own speed.
pub const PITCH_NORM: i32 = 100;
/// The `soundscape_fadetime` console variable: seconds a looping sound takes to fade
/// from silence to full volume, or back.
pub const SOUNDSCAPE_FADE_TIME: f64 = 3.0;
/// How far from the listener a `position random` sound plays (`DEFAULT_SOUND_RADIUS`).
pub const RANDOM_POSITION_RADIUS: f64 = 36.0;
/// How deep `playsoundscape` may nest (`MAX_SOUNDSCAPE_RECURSION`).
pub const MAX_SOUNDSCAPE_RECURSION: u32 = 8;
/// The volume a new looping sound with a place starts fading in from; the engine drops
/// placed sounds that start silent. A looping sound without a place starts from silence.
const PLACED_LOOP_START_VOLUME: f64 = 0.05;
/// How far apart, along each axis, two places may be and still count as the same one.
const SAME_PLACE_TOLERANCE: f64 = 0.1;
/// `snd_refdb`: the sound level at which a sound has full volume at [`REFERENCE_DISTANCE`].
const REFERENCE_DB: f64 = 60.0;
/// `snd_refdist`: the distance sound levels are measured at, in units.
const REFERENCE_DISTANCE: f64 = 36.0;
/// `SND_GAIN_COMP_THRESH`: gains above this are compressed towards 1.
const GAIN_COMPRESSION_THRESHOLD: f64 = 0.5;
/// `SND_GAIN_COMP_EXP_MAX` and `SND_GAIN_COMP_EXP_MIN`: the compression curve's exponent
/// for quiet and for very loud sound levels.
const GAIN_COMPRESSION_EXPONENT_MAX: f64 = 2.5;
const GAIN_COMPRESSION_EXPONENT_MIN: f64 = 0.8;
/// `SND_DB_MED` and `SND_DB_MAX`: the sound levels between which the exponent changes.
const DB_MEDIUM: f64 = 90.0;
const DB_MAX: f64 = 140.0;
/// `SND_DB_MIN`: the sound level with the smallest source radius.
const DB_MIN: f64 = 60.0;
/// `snd_gain_min`: below this gain a sound fades out to nothing.
const GAIN_MIN: f64 = 0.01;
/// `SND_RADIUS_MIN` and `SND_RADIUS_MAX`: how large a sound source is taken to be when
/// testing whether walls hide it, for the quietest and loudest sound levels.
const SOURCE_RADIUS_MIN: f64 = 24.0;
const SOURCE_RADIUS_MAX: f64 = 240.0;
/// `snd_obscured_gain_dB`: decibels lost for each hidden part of a sound source.
pub const OBSCURED_LOSS_DB: f64 = -2.70;
/// `DVAR_DIST_MIN` and `DVAR_DIST_MAX`: the distances between which a `^` sound fades
/// from its near recording to its far one.
const DISTANT_VARIANT_NEAR: f64 = 240.0;
const DISTANT_VARIANT_FAR: f64 = 1320.0;
/// The standing player's hull around the eyes: 32 units wide, reaching 64 below the eyes
/// and 8 above.
const HULL_HALF_WIDTH: f64 = 16.0;
const HULL_BELOW_EYES: f64 = 64.0;
const HULL_ABOVE_EYES: f64 = 8.0;
/// Characters before a sound's file name that tell the engine how to play it.
const SOUND_CHARACTERS: &str = "*?!#><^@)}$(~`+&";

/// The number a string starts with, as C's `atof` reads it; 0 when it starts with none.
pub fn leading_number(text: &str) -> f64 {
    let text = text.trim_start();
    let end = text
        .find(|character: char| !(character.is_ascii_digit() || "+-.eE".contains(character)))
        .unwrap_or(text.len());
    (0..=end)
        .rev()
        .find_map(|length| text[..length].parse::<f64>().ok())
        .unwrap_or(0.0)
}

/// The whole number a string starts with, as C's `atoi` reads it.
pub fn leading_integer(text: &str) -> i32 {
    let text = text.trim_start();
    let digits = text
        .char_indices()
        .take_while(|&(index, character)| character.is_ascii_digit() || (index == 0 && matches!(character, '+' | '-')))
        .count();
    text[..digits]
        .parse::<i64>()
        .map_or(0, |value| value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32)
}

/// A small random number generator, so playback can be repeated in tests.
#[derive(Clone, Debug)]
pub struct Random(u64);

impl Random {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// A number from 0 up to, but not including, 1.
    pub fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A number between `min` and `max`, as the engine's `RandomFloat`.
    pub fn between(&mut self, min: f64, max: f64) -> f64 {
        min + (max - min) * self.unit()
    }

    /// A whole number from 0 up to, but not including, `count`.
    pub fn index(&mut self, count: usize) -> usize {
        ((self.unit() * count as f64) as usize).min(count.saturating_sub(1))
    }
}

/// A value or a range to pick from at random, written `"5"` or `"5,10"` in scripts.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Interval {
    pub start: f64,
    /// How far past `start` a pick may land; 0 for a fixed value.
    pub range: f64,
}

impl Interval {
    pub fn fixed(value: f64) -> Self {
        Self {
            start: value,
            range: 0.0,
        }
    }

    /// Reads `"start"` or `"start,end"`, as the engine's `ReadInterval`.
    pub fn parse(text: &str) -> Self {
        let mut parts = text.split(',').filter(|part| !part.is_empty());
        let start = parts.next().map_or(0.0, leading_number);
        let range = parts.next().map_or(0.0, |end| leading_number(end) - start);
        Self { start, range }
    }

    /// A value from the interval, as the engine's `RandomInterval`.
    pub fn pick(&self, random: &mut Random) -> f64 {
        if self.range == 0.0 {
            self.start
        } else {
            self.start + random.between(0.0, self.range)
        }
    }
}

/// The sound level of an attenuation factor (`ATTN_TO_SNDLVL`).
pub fn attenuation_to_soundlevel(attenuation: f64) -> i32 {
    if attenuation == 0.0 {
        0
    } else {
        (50.0 + 20.0 / attenuation) as i32
    }
}

/// The sound level a `SNDLVL_` name stands for, as `TextToSoundLevel`; an unknown name is
/// `SNDLVL_NORM`.
pub fn text_to_soundlevel(text: &str) -> i32 {
    let name = text.trim().to_ascii_uppercase();
    match name.as_str() {
        "SNDLVL_NONE" => 0,
        "SNDLVL_IDLE" => 60,
        "SNDLVL_STATIC" => 66,
        "SNDLVL_NORM" => 75,
        "SNDLVL_TALKING" => 80,
        "SNDLVL_GUNFIRE" => 140,
        _ => name
            .strip_prefix("SNDLVL_")
            .and_then(|rest| rest.strip_suffix("DB"))
            .and_then(|number| number.parse().ok())
            .unwrap_or(SOUNDLEVEL_NORM),
    }
}

/// The attenuation an `ATTN_` name stands for in a game sound script.
fn text_to_attenuation(text: &str) -> Option<f64> {
    Some(match text.trim().to_ascii_uppercase().as_str() {
        "ATTN_NONE" => 0.0,
        "ATTN_NORM" => 0.8,
        "ATTN_IDLE" => 2.0,
        "ATTN_STATIC" => 1.25,
        "ATTN_RICOCHET" => 1.5,
        "ATTN_GUNFIRE" => 0.27,
        _ => return None,
    })
}

/// How much of its volume a sound of `soundlevel` keeps `distance` units away, as the
/// engine's `SND_GetGain`: inverse distance from the reference distance, compressed
/// towards 1 close by and faded to nothing where it gets very quiet. Level 0 never fades.
pub fn distance_gain(soundlevel: i32, distance: f64) -> f64 {
    if soundlevel <= 0 {
        return 1.0;
    }
    let level = f64::from(soundlevel);
    let distance_scale = 10f64.powf(REFERENCE_DB / 20.0) / 10f64.powf(level / 20.0) / REFERENCE_DISTANCE;
    let relative = distance * distance_scale;
    let mut gain = if relative > 0.1 { 1.0 / relative } else { 10.0 };
    if gain > GAIN_COMPRESSION_THRESHOLD {
        let exponent = if level > DB_MEDIUM {
            let blend = ((level - DB_MEDIUM) / (DB_MAX - DB_MEDIUM)).clamp(0.0, 1.0);
            GAIN_COMPRESSION_EXPONENT_MAX + (GAIN_COMPRESSION_EXPONENT_MIN - GAIN_COMPRESSION_EXPONENT_MAX) * blend
        } else {
            GAIN_COMPRESSION_EXPONENT_MAX
        };
        let crossover = -1.0 / (GAIN_COMPRESSION_THRESHOLD.powf(exponent) * (GAIN_COMPRESSION_THRESHOLD - 1.0));
        gain = 1.0 - 1.0 / (crossover * gain.powf(exponent));
    }
    if gain < GAIN_MIN {
        gain = GAIN_MIN * (2.0 - relative * GAIN_MIN);
        if gain <= 0.0 {
            return 0.0;
        }
    }
    gain
}

/// The share of a sound each of two speakers gets, left then right, for a source in the
/// unit direction `to_source` from a listener whose right is `right`: the engine's linear
/// cross-fade, half to each speaker for a sound straight ahead.
pub fn pan(to_source: DVec3, right: DVec3) -> [f64; 2] {
    let side = to_source.dot(right).clamp(-1.0, 1.0);
    [(1.0 - side) / 2.0, (1.0 + side) / 2.0]
}

/// How large the engine takes a source of `soundlevel` to be when it tests whether walls
/// hide it (`dB_To_Radius`).
pub fn source_radius(soundlevel: i32) -> f64 {
    let share = ((f64::from(soundlevel) - DB_MIN) / (DB_MAX - DB_MIN)).clamp(0.0, 1.0);
    SOURCE_RADIUS_MIN + (SOURCE_RADIUS_MAX - SOURCE_RADIUS_MIN) * share
}

/// The gain left of a sound behind walls, as `SND_GetGainObscured`: nothing is lost
/// while the listener sees the middle of the source. Otherwise four points on two
/// diagonals around it, facing the listener, are tested as well, and
/// [`OBSCURED_LOSS_DB`] is lost for the middle and for each hidden point.
pub fn obscured_gain(ear: DVec3, source: DVec3, soundlevel: i32, blocked: &dyn Fn(DVec3, DVec3) -> bool) -> f64 {
    if !blocked(ear, source) {
        return 1.0;
    }
    let toward = (source - ear).normalize_or_zero();
    if toward == DVec3::ZERO {
        return 1.0;
    }
    let (side, mut up) = perpendiculars(toward);
    let first = (up + side).normalize_or_zero();
    if source.z > ear.z + 120.0 {
        up.z = -up.z;
    }
    let second = (up - side).normalize_or_zero();
    let radius = source_radius(soundlevel);
    let points = [
        source + first * (radius / 2.0),
        source + second * (radius / 2.0),
        source + first * radius,
        source + second * radius,
    ];
    let hidden = 1 + points.iter().filter(|point| blocked(ear, **point)).count();
    10f64.powf(OBSCURED_LOSS_DB * hidden as f64 / 20.0)
}

/// A right and an up direction for a forward one, as the engine's `VectorVectors`.
fn perpendiculars(forward: DVec3) -> (DVec3, DVec3) {
    if forward.x == 0.0 && forward.y == 0.0 {
        return (DVec3::new(0.0, -1.0, 0.0), DVec3::new(-forward.z, 0.0, 0.0));
    }
    let right = forward.cross(DVec3::Z).normalize_or_zero();
    (right, right.cross(forward).normalize_or_zero())
}

/// How far a `^` sound has faded from its near recording (0) to its far one (1).
pub fn distant_variant_mix(distance: f64) -> f64 {
    ((distance - DISTANT_VARIANT_NEAR) / (DISTANT_VARIANT_FAR - DISTANT_VARIANT_NEAR)).clamp(0.0, 1.0)
}

/// What the characters before a sound's file name ask of the engine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SoundCharacters {
    /// `@`: heard equally from every direction, though it still fades with distance.
    pub omni: bool,
    /// `)`: a stereo file placed in the world like a mono one.
    pub spatial_stereo: bool,
    /// `^`: a stereo file holding a near and a far recording to fade between.
    pub distant_variant: bool,
    /// `<`: a stereo file holding the sound from in front and from behind its source.
    pub directional: bool,
    /// `!`: a sentence from `sentences.txt` rather than a file.
    pub sentence: bool,
}

/// Splits a sound name such as `)ambient/wind.wav` into its leading characters and the
/// file name under `sound/`, lower case with forward slashes.
pub fn split_sound_name(name: &str) -> (SoundCharacters, String) {
    let mut characters = SoundCharacters::default();
    let file = name.trim().trim_start_matches(|character| {
        match character {
            '@' => characters.omni = true,
            ')' => characters.spatial_stereo = true,
            '^' => characters.distant_variant = true,
            '<' => characters.directional = true,
            '!' => characters.sentence = true,
            _ => {}
        }
        SOUND_CHARACTERS.contains(character)
    });
    (characters, file.replace('\\', "/").to_ascii_lowercase())
}

/// One soundscape of a script file: its name and the commands in its block.
#[derive(Clone, Debug, PartialEq)]
pub struct Soundscape {
    pub name: String,
    pub block: Block,
    /// The script file it was read from.
    pub file: String,
}

/// Every soundscape the game knows, in the order the script files define them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SoundscapeLibrary {
    pub soundscapes: Vec<Soundscape>,
}

impl SoundscapeLibrary {
    /// The script files a `soundscapes_manifest.txt` lists, in order.
    pub fn manifest_files(text: &str) -> Result<Vec<String>, String> {
        manifest_values(text, &["file"])
    }

    /// Adds every soundscape of a script file: each top-level block that holds anything.
    /// Returns how many were added.
    pub fn add_file(&mut self, file: &str, text: &str) -> Result<usize, String> {
        let items = mallet_kv::parse(text).map_err(|error| format!("{file}: {error}"))?;
        let before = self.soundscapes.len();
        for item in items {
            if let Item::Block(block) = item {
                if !block.items.is_empty() {
                    self.soundscapes.push(Soundscape {
                        name: block.name.clone(),
                        block,
                        file: file.to_owned(),
                    });
                }
            }
        }
        Ok(self.soundscapes.len() - before)
    }

    /// The first soundscape of that name, ignoring case, as the engine finds it.
    pub fn find(&self, name: &str) -> Option<&Soundscape> {
        let name = name.trim();
        self.soundscapes
            .iter()
            .find(|soundscape| soundscape.name.eq_ignore_ascii_case(name))
    }

    /// Every sound file a soundscape can play, sub-soundscapes included, as written in
    /// the scripts.
    pub fn waves(&self, name: &str) -> Vec<String> {
        let mut waves = Vec::new();
        self.collect_waves(name, 0, &mut waves);
        waves.sort();
        waves.dedup();
        waves
    }

    fn collect_waves(&self, name: &str, depth: u32, waves: &mut Vec<String>) {
        let Some(soundscape) = self.find(name) else {
            return;
        };
        if depth > MAX_SOUNDSCAPE_RECURSION {
            return;
        }
        for command in soundscape.block.blocks() {
            if command.name.eq_ignore_ascii_case("playlooping") {
                waves.extend(
                    command
                        .pairs()
                        .filter(|pair| pair.key.eq_ignore_ascii_case("wave"))
                        .map(|pair| pair.value.clone()),
                );
            } else if command.name.eq_ignore_ascii_case("playrandom") {
                for list in command
                    .blocks()
                    .filter(|list| list.name.eq_ignore_ascii_case("rndwave"))
                {
                    waves.extend(list.pairs().map(|pair| pair.value.clone()));
                }
            } else if command.name.eq_ignore_ascii_case("playsoundscape") {
                if let Some(sub) = command.get("name") {
                    self.collect_waves(sub, depth + 1, waves);
                }
            }
        }
    }
}

/// The values of a manifest's lines whose key is one of `keys`.
fn manifest_values(text: &str, keys: &[&str]) -> Result<Vec<String>, String> {
    let items = mallet_kv::parse(text).map_err(|error| error.to_string())?;
    let mut values = Vec::new();
    for item in items {
        if let Item::Block(block) = item {
            values.extend(
                block
                    .pairs()
                    .filter(|pair| keys.iter().any(|key| pair.key.eq_ignore_ascii_case(key)))
                    .map(|pair| pair.value.replace('\\', "/")),
            );
        }
    }
    Ok(values)
}

/// One entry of a game sound script, such as `Town.d1_town_02a_spawn`.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptSound {
    pub name: String,
    pub volume: Interval,
    pub pitch: Interval,
    pub soundlevel: i32,
    /// The files it picks from at random, as written, with their leading characters.
    pub waves: Vec<String>,
}

/// The game's sound scripts: the names `ambient_generic` and other entities play.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SoundScripts {
    sounds: HashMap<String, ScriptSound>,
}

impl SoundScripts {
    /// The script files a `game_sounds_manifest.txt` lists, in order.
    pub fn manifest_files(text: &str) -> Result<Vec<String>, String> {
        manifest_values(text, &["precache_file", "preload_file", "autocache_file"])
    }

    /// Adds a script file's sounds. A name already known is kept, as the engine keeps
    /// the first, unless `replace` is set, as it is for a map's own `_level_sounds` file.
    pub fn add_file(&mut self, text: &str, replace: bool) -> Result<usize, String> {
        let items = mallet_kv::parse(text).map_err(|error| error.to_string())?;
        let mut added = 0;
        for item in items {
            let Item::Block(block) = item else {
                continue;
            };
            let key = block.name.to_ascii_lowercase();
            if !replace && self.sounds.contains_key(&key) {
                continue;
            }
            self.sounds.insert(key, script_sound(&block));
            added += 1;
        }
        Ok(added)
    }

    /// The sound of that name, ignoring case.
    pub fn get(&self, name: &str) -> Option<&ScriptSound> {
        self.sounds.get(&name.trim().to_ascii_lowercase())
    }

    pub fn len(&self) -> usize {
        self.sounds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sounds.is_empty()
    }
}

fn script_sound(block: &Block) -> ScriptSound {
    let mut sound = ScriptSound {
        name: block.name.clone(),
        volume: Interval::fixed(1.0),
        pitch: Interval::fixed(f64::from(PITCH_NORM)),
        soundlevel: SOUNDLEVEL_NORM,
        waves: Vec::new(),
    };
    for item in &block.items {
        match item {
            Item::Pair(pair) => {
                let value = pair.value.trim();
                match pair.key.to_ascii_lowercase().as_str() {
                    "volume" => {
                        sound.volume = if value.eq_ignore_ascii_case("VOL_NORM") {
                            Interval::fixed(1.0)
                        } else {
                            Interval::parse(value)
                        };
                    }
                    "pitch" => {
                        sound.pitch = match value.to_ascii_uppercase().as_str() {
                            "PITCH_NORM" => Interval::fixed(100.0),
                            "PITCH_LOW" => Interval::fixed(95.0),
                            "PITCH_HIGH" => Interval::fixed(120.0),
                            _ => Interval::parse(value),
                        };
                    }
                    "soundlevel" => {
                        sound.soundlevel = if value.to_ascii_uppercase().starts_with("SNDLVL_") {
                            text_to_soundlevel(value)
                        } else {
                            leading_integer(value)
                        };
                    }
                    "attenuation" => {
                        let attenuation = text_to_attenuation(value).unwrap_or_else(|| leading_number(value));
                        sound.soundlevel = attenuation_to_soundlevel(attenuation);
                    }
                    "wave" => sound.waves.push(pair.value.clone()),
                    _ => {}
                }
            }
            Item::Block(list) if list.name.eq_ignore_ascii_case("rndwave") => {
                sound.waves.extend(list.pairs().map(|pair| pair.value.clone()));
            }
            Item::Block(_) => {}
        }
    }
    sound
}

/// Which kind of soundscape entity an entity is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundscapeKind {
    /// `env_soundscape`.
    Normal,
    /// `env_soundscape_proxy`: another soundscape entity's soundscape and positions,
    /// with its own place and radius.
    Proxy,
    /// `env_soundscape_triggerable`: the soundscape a `trigger_soundscape` turns on. The
    /// engine also lets it take over by its radius like any other.
    Triggerable,
}

/// A soundscape entity as the server sets it up when the map starts.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundscapeEntity {
    pub entity: u32,
    pub kind: SoundscapeKind,
    pub targetname: String,
    pub origin: DVec3,
    /// How close the listener has to be; -1 for anywhere it can be seen from.
    pub radius: f64,
    /// The soundscape's name in the scripts; a proxy's is its main soundscape's.
    pub soundscape: String,
    /// Off until an input enables it (`StartDisabled`).
    pub enabled: bool,
    /// Where the entities named by `position0` to `position7` are, for the ones found.
    pub positions: [Option<DVec3>; 8],
    /// What is wrong with the entity, when the game would warn about it.
    pub problem: Option<String>,
}

/// A `trigger_soundscape`: brushes that turn a soundscape on while the player is inside.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundscapeTrigger {
    pub entity: u32,
    /// The `env_soundscape_triggerable` it turns on, by entity id, when one was found.
    pub soundscape: Option<u32>,
    /// The planes of each of its brushes, facing out.
    pub volumes: Vec<Vec<Plane>>,
    pub bounds: Bounds,
    pub enabled: bool,
}

impl SoundscapeTrigger {
    /// Whether a box touches any of the trigger's brushes.
    pub fn touches(&self, hull: &Bounds) -> bool {
        if hull.min.cmpgt(self.bounds.max).any() || hull.max.cmplt(self.bounds.min).any() {
            return false;
        }
        self.volumes.iter().any(|planes| {
            !planes.is_empty()
                && planes.iter().all(|plane| {
                    let nearest = DVec3::select(plane.normal.cmpge(DVec3::ZERO), hull.min, hull.max);
                    plane.distance(nearest) <= 0.0
                })
        })
    }
}

/// An `ambient_generic` as the server sets it up when the map starts.
#[derive(Clone, Debug, PartialEq)]
pub struct AmbientGeneric {
    pub entity: u32,
    pub targetname: String,
    /// Where the sound comes from: the entity named by `SourceEntityName` when it
    /// exists, otherwise the entity itself.
    pub origin: DVec3,
    /// The `message`: a sound file, or a name from the game's sound scripts.
    pub sound: String,
    /// The volume it runs at, from 0 to 1 (`health` over 10).
    pub volume: f64,
    /// The volume it starts at when it fades in, and how much it gains each second.
    pub start_volume: f64,
    pub fade_in: f64,
    /// The pitch it runs at, the pitch it starts at when it spins up, and how much it
    /// gains each second.
    pub pitch: f64,
    pub start_pitch: f64,
    pub spin_up: f64,
    /// The sound level its `radius` gives a sound file; 0 with Play Everywhere. A name
    /// from the sound scripts uses the script's own level instead.
    pub soundlevel: i32,
    pub radius: f64,
    pub everywhere: bool,
    pub start_silent: bool,
    pub looped: bool,
    /// Uses a modulation preset or an LFO, which the preview leaves out.
    pub modulated: bool,
}

impl AmbientGeneric {
    /// Whether the sound plays from the moment the map starts: it must be looped and
    /// not start silent, and have a volume.
    pub fn plays_at_start(&self) -> bool {
        self.looped && !self.start_silent && !self.sound.is_empty() && (self.volume > 0.0 || self.fade_in > 0.0)
    }

    /// Whether `sound` names a file rather than an entry of the sound scripts, by the
    /// engine's own test.
    pub fn is_file(&self) -> bool {
        let lower = self.sound.to_ascii_lowercase();
        lower.contains(".wav") || lower.contains(".mp3") || lower.starts_with('!')
    }
}

/// Everything about a map's ambient sound.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapSounds {
    /// Soundscape entities in map order, which is the order the server tests them in.
    pub soundscapes: Vec<SoundscapeEntity>,
    pub triggers: Vec<SoundscapeTrigger>,
    pub ambients: Vec<AmbientGeneric>,
    /// Brush entities no brush trace hits, such as triggers and area portals.
    pub non_solid: BTreeSet<u32>,
    /// Brush entities the compiler makes part of the world (`func_detail`), which hide
    /// sounds the way world brushes do.
    pub detail: BTreeSet<u32>,
}

/// Brush entity classes whose brushes are not solid in the game.
const NON_SOLID_CLASSES: [&str; 22] = [
    "func_areaportal",
    "func_areaportalwindow",
    "func_viscluster",
    "func_occluder",
    "func_illusionary",
    "func_precipitation",
    "func_smokevolume",
    "func_dustcloud",
    "func_dustmotes",
    "func_fish_pool",
    "func_clip_vphysics",
    "func_vehicleclip",
    "func_nav_blocker",
    "func_nav_avoid",
    "func_buyzone",
    "func_bomb_target",
    "func_hostage_rescue",
    "func_nobuild",
    "func_respawnroom",
    "func_regenerate",
    "func_ladder",
    "env_embers",
];

impl MapSounds {
    /// Reads a map's sound entities.
    pub fn read(vmf: &Vmf) -> Self {
        let mut sounds = Self::default();
        let entities: Vec<&Entity> = vmf.entities().collect();
        let find = |name: &str| find_entity(&entities, name);
        let mut proxies = Vec::new();
        for entity in &entities {
            let classname = entity.classname().to_ascii_lowercase();
            let id = entity.id().unwrap_or(0);
            if entity.solids().next().is_some() {
                let never_solid = classname == "func_brush" && entity.get("Solidity").map(leading_integer) == Some(1);
                if classname.starts_with("trigger_") || NON_SOLID_CLASSES.contains(&classname.as_str()) || never_solid {
                    sounds.non_solid.insert(id);
                }
                if classname == "func_detail" {
                    sounds.detail.insert(id);
                }
            }
            let kind = match classname.as_str() {
                "env_soundscape" => SoundscapeKind::Normal,
                "env_soundscape_proxy" => SoundscapeKind::Proxy,
                "env_soundscape_triggerable" => SoundscapeKind::Triggerable,
                "trigger_soundscape" => {
                    sounds.triggers.push(soundscape_trigger(entity, id, &find));
                    continue;
                }
                "ambient_generic" => {
                    sounds.ambients.extend(ambient_generic(entity, id, &find));
                    continue;
                }
                _ => continue,
            };
            let Some(origin) = entity.origin() else {
                continue;
            };
            let name = entity.get("soundscape").unwrap_or_default().trim().to_owned();
            let mut soundscape = SoundscapeEntity {
                entity: id,
                kind,
                targetname: entity.get("targetname").unwrap_or_default().trim().to_owned(),
                origin,
                radius: entity.get("radius").map_or(2000.0, leading_number),
                soundscape: name,
                enabled: entity.get("StartDisabled").map(leading_integer).unwrap_or(0) == 0,
                positions: positions(entity, &find),
                problem: None,
            };
            if kind == SoundscapeKind::Proxy {
                proxies.push((
                    sounds.soundscapes.len(),
                    entity.get("MainSoundscapeName").unwrap_or_default().trim().to_owned(),
                ));
                soundscape.soundscape.clear();
            } else if soundscape.soundscape.is_empty() {
                soundscape.problem = Some("no soundscape name".to_owned());
            }
            sounds.soundscapes.push(soundscape);
        }
        for (index, main_name) in proxies {
            let main = find(&main_name).filter(|main| {
                matches!(
                    main.classname().to_ascii_lowercase().as_str(),
                    "env_soundscape" | "env_soundscape_proxy" | "env_soundscape_triggerable"
                )
            });
            match main {
                Some(main) => {
                    sounds.soundscapes[index].soundscape = main.get("soundscape").unwrap_or_default().trim().to_owned();
                    sounds.soundscapes[index].positions = positions(main, &find);
                }
                None => {
                    sounds.soundscapes[index].problem =
                        Some(format!("cannot find the main soundscape \"{main_name}\""));
                }
            }
        }
        sounds
    }

    /// The soundscape entity with that entity id.
    pub fn soundscape(&self, entity: u32) -> Option<&SoundscapeEntity> {
        self.soundscapes.iter().find(|soundscape| soundscape.entity == entity)
    }
}

/// The first entity of that targetname, ignoring case, with a trailing `*` matching any
/// ending, as the engine's `FindEntityByName`.
fn find_entity<'a>(entities: &[&'a Entity], name: &str) -> Option<&'a Entity> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    entities.iter().copied().find(|entity| {
        let Some(targetname) = entity.get("targetname") else {
            return false;
        };
        let targetname = targetname.trim().to_ascii_lowercase();
        match lower.strip_suffix('*') {
            Some(prefix) => targetname.starts_with(prefix),
            None => targetname == lower,
        }
    })
}

/// Where the game takes an entity to be: its `origin`, or the map's origin for a brush
/// entity without one.
fn entity_origin(entity: &Entity) -> DVec3 {
    entity.origin().unwrap_or(DVec3::ZERO)
}

/// The bounds of an entity's brushes.
fn brush_bounds(entity: &Entity) -> Option<Bounds> {
    let mut bounds = Bounds::default();
    let mut any = false;
    for solid in entity.solids() {
        let faces = solid_faces(solid);
        if !faces.is_empty() {
            bounds.add_bounds(&faces_bounds(&faces));
            any = true;
        }
    }
    any.then_some(bounds)
}

fn positions<'a>(entity: &Entity, find: &dyn Fn(&str) -> Option<&'a Entity>) -> [Option<DVec3>; 8] {
    std::array::from_fn(|index| {
        let name = entity.get(&format!("position{index}"))?;
        find(name).map(entity_origin)
    })
}

fn soundscape_trigger<'a>(entity: &Entity, id: u32, find: &dyn Fn(&str) -> Option<&'a Entity>) -> SoundscapeTrigger {
    let soundscape = entity
        .get("soundscape")
        .and_then(find)
        .filter(|target| target.classname().eq_ignore_ascii_case("env_soundscape_triggerable"))
        .and_then(Entity::id);
    let mut bounds = Bounds::default();
    let mut volumes = Vec::new();
    for solid in entity.solids() {
        let faces = solid_faces(solid);
        if faces.is_empty() {
            continue;
        }
        bounds.add_bounds(&faces_bounds(&faces));
        volumes.push(faces.iter().map(|face| face.plane).collect());
    }
    SoundscapeTrigger {
        entity: id,
        soundscape,
        volumes,
        bounds,
        enabled: entity.get("StartDisabled").map(leading_integer).unwrap_or(0) == 0,
    }
}

/// `ambient_generic` spawn flags.
const AMBIENT_EVERYWHERE: i32 = 1;
const AMBIENT_START_SILENT: i32 = 16;
const AMBIENT_NOT_LOOPED: i32 = 32;
/// How many times a second an `ambient_generic` changes its volume and pitch.
const AMBIENT_UPDATE_RATE: f64 = 5.0;

fn ambient_generic<'a>(entity: &Entity, id: u32, find: &dyn Fn(&str) -> Option<&'a Entity>) -> Option<AmbientGeneric> {
    let sound = entity.get("message").unwrap_or_default().trim().to_owned();
    if sound.is_empty() {
        return None;
    }
    let flags = entity.get("spawnflags").map_or(0, leading_integer);
    let integer = |key: &str, low: i32, high: i32| entity.get(key).map_or(0, leading_integer).clamp(low, high);
    let everywhere = flags & AMBIENT_EVERYWHERE != 0;
    let radius = entity.get("radius").map_or(0.0, leading_number);
    let soundlevel = if radius > 0.0 && !everywhere {
        (40.0 + 20.0 * (radius / REFERENCE_DISTANCE).log10()) as i32
    } else {
        SOUNDLEVEL_NONE
    };
    let volume = f64::from((integer("health", i32::MIN / 10, i32::MAX / 10) * 10).clamp(0, 100)) / 100.0;
    let fade_seconds = integer("fadeinsecs", 0, 100);
    let fade_steps = integer("fadein", 0, 100);
    let fade_in = if fade_seconds > 0 {
        1.0 / f64::from(fade_seconds)
    } else if fade_steps > 0 {
        f64::from(101 - fade_steps) * 64.0 / 256.0 * AMBIENT_UPDATE_RATE / 100.0
    } else {
        0.0
    };
    let run_pitch = integer("pitch", 0, 255);
    let spin_steps = integer("spinup", 0, 100);
    let spin_up = if spin_steps > 0 {
        f64::from(101 - spin_steps) * 64.0 / 256.0 * AMBIENT_UPDATE_RATE
    } else {
        0.0
    };
    let pitch = |value: i32| {
        if value == 0 {
            f64::from(PITCH_NORM)
        } else {
            f64::from(value)
        }
    };
    let source = entity.get("SourceEntityName").and_then(find);
    let origin = match source {
        Some(source) => brush_bounds(source).map_or_else(|| entity_origin(source), |bounds| bounds.center()),
        None => entity_origin(entity),
    };
    Some(AmbientGeneric {
        entity: id,
        targetname: entity.get("targetname").unwrap_or_default().trim().to_owned(),
        origin,
        sound,
        volume,
        start_volume: if fade_in > 0.0 {
            f64::from(integer("volstart", 0, 10)) / 10.0
        } else {
            volume
        },
        fade_in,
        pitch: pitch(run_pitch),
        start_pitch: if spin_up > 0.0 {
            pitch(integer("pitchstart", 0, 255))
        } else {
            pitch(run_pitch)
        },
        spin_up,
        soundlevel,
        radius,
        everywhere,
        start_silent: flags & AMBIENT_START_SILENT != 0,
        looped: flags & AMBIENT_NOT_LOOPED == 0,
        modulated: integer("preset", 0, i32::MAX) != 0 || integer("lfotype", 0, i32::MAX) != 0,
    })
}

/// The box of a standing player whose eyes are at `eye`, which trigger volumes are
/// tested against.
pub fn player_hull(eye: DVec3) -> Bounds {
    Bounds {
        min: eye - DVec3::new(HULL_HALF_WIDTH, HULL_HALF_WIDTH, HULL_BELOW_EYES),
        max: eye + DVec3::new(HULL_HALF_WIDTH, HULL_HALF_WIDTH, HULL_ABOVE_EYES),
    }
}

/// What the server tells the client about its soundscape (`audioparams_t`).
#[derive(Clone, Debug, PartialEq)]
pub struct AudioParams {
    /// The soundscape entity in charge; `None` after the last `trigger_soundscape` was
    /// left, which keeps the sounds playing until another entity takes over.
    pub entity: Option<u32>,
    /// The soundscape's name in the scripts.
    pub soundscape: String,
    pub positions: [Option<DVec3>; 8],
}

/// Picks the soundscape a listener hears, as the server does for a player.
#[derive(Clone, Debug, Default)]
pub struct SoundscapeChooser {
    audio: Option<AudioParams>,
    /// Whether the current soundscape entity was in range and in sight at the last update.
    in_range: bool,
    /// The triggerable soundscapes of the triggers the listener is in, newest first.
    trigger_stack: Vec<u32>,
    /// The triggers the listener was touching at the last update.
    touching: BTreeSet<u32>,
}

impl SoundscapeChooser {
    /// What the client was told last.
    pub fn audio(&self) -> Option<&AudioParams> {
        self.audio.as_ref()
    }

    /// Whether the soundscape entity in charge is still in range and in sight; when it
    /// is not, its soundscape keeps playing until another takes over.
    pub fn in_range(&self) -> bool {
        self.in_range
    }

    /// Runs one server update for a listener whose ears are at `ear`. `blocked` says
    /// whether a solid brush or water lies between two points.
    pub fn update(&mut self, map: &MapSounds, ear: DVec3, blocked: &dyn Fn(DVec3, DVec3) -> bool) {
        self.update_triggers(map, ear);
        let mut current = self
            .audio
            .as_ref()
            .and_then(|audio| audio.entity)
            .and_then(|entity| map.soundscape(entity))
            .map(|soundscape| soundscape.entity);
        let mut in_range = false;
        let mut current_distance = 0.0;
        let reaches = |soundscape: &SoundscapeEntity, range: f64| {
            (soundscape.radius > range || soundscape.radius == -1.0) && !blocked(soundscape.origin, ear)
        };
        if let Some(soundscape) = current.and_then(|entity| map.soundscape(entity)) {
            if soundscape.enabled {
                current_distance = soundscape.origin.distance(ear);
                in_range = reaches(soundscape, current_distance);
            } else {
                current = None;
            }
        }
        for soundscape in &map.soundscapes {
            if Some(soundscape.entity) == current || !soundscape.enabled {
                continue;
            }
            let range = soundscape.origin.distance(ear);
            if (!in_range || range < current_distance) && reaches(soundscape, range) {
                self.audio = Some(audio_params(soundscape));
                current = Some(soundscape.entity);
                in_range = true;
                current_distance = range;
            }
        }
        self.in_range = in_range;
    }

    /// Follows the map after an edit: the soundscape entity in charge is read again, so
    /// a renamed soundscape or a moved position takes effect, and one that was deleted
    /// lets go.
    pub fn refresh(&mut self, map: &MapSounds) {
        let Some(audio) = &mut self.audio else {
            return;
        };
        if let Some(entity) = audio.entity {
            match map.soundscape(entity) {
                Some(soundscape) => *audio = audio_params(soundscape),
                None => audio.entity = None,
            }
        }
        self.trigger_stack.retain(|entity| map.soundscape(*entity).is_some());
        self.touching
            .retain(|entity| map.triggers.iter().any(|trigger| trigger.entity == *entity));
    }

    fn update_triggers(&mut self, map: &MapSounds, ear: DVec3) {
        let hull = player_hull(ear);
        let touching: BTreeSet<u32> = map
            .triggers
            .iter()
            .filter(|trigger| trigger.enabled && trigger.touches(&hull))
            .map(|trigger| trigger.entity)
            .collect();
        let target = |entity: &u32| {
            map.triggers
                .iter()
                .find(|trigger| trigger.entity == *entity)
                .and_then(|trigger| trigger.soundscape)
        };
        for left in self
            .touching
            .difference(&touching)
            .filter_map(target)
            .collect::<Vec<_>>()
        {
            self.trigger_stack.retain(|entity| *entity != left);
            match self.trigger_stack.first().and_then(|entity| map.soundscape(*entity)) {
                Some(soundscape) => self.audio = Some(audio_params(soundscape)),
                None => {
                    if let Some(audio) = &mut self.audio {
                        audio.entity = None;
                    }
                }
            }
        }
        for entered in touching
            .difference(&self.touching)
            .filter_map(target)
            .collect::<Vec<_>>()
        {
            self.trigger_stack.retain(|entity| *entity != entered);
            self.trigger_stack.insert(0, entered);
            if let Some(soundscape) = map.soundscape(entered) {
                self.audio = Some(audio_params(soundscape));
            }
        }
        self.touching = touching;
    }
}

fn audio_params(soundscape: &SoundscapeEntity) -> AudioParams {
    AudioParams {
        entity: Some(soundscape.entity),
        soundscape: soundscape.soundscape.clone(),
        positions: soundscape.positions,
    }
}

/// A sound the engine is asked to play.
#[derive(Clone, Debug, PartialEq)]
pub struct Emit {
    /// The sound as the script names it, with its leading characters.
    pub wave: String,
    pub volume: f64,
    pub pitch: i32,
    pub soundlevel: i32,
    /// Where it plays in the world; `None` for a sound heard in both ears wherever the
    /// listener stands.
    pub origin: Option<DVec3>,
    /// Whether the script means it to loop, which only happens if the file has a loop
    /// point.
    pub looping: bool,
}

/// Whatever plays the sounds a [`SoundscapePlayer`] asks for.
pub trait SoundSink {
    /// Starts a sound and returns a handle for it.
    fn start(&mut self, sound: &Emit) -> u64;
    /// Changes the volume of a sound still playing.
    fn set_volume(&mut self, handle: u64, volume: f64);
    /// Stops a sound.
    fn stop(&mut self, handle: u64);
}

/// The listener's eyes, for sounds placed around them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Listener {
    pub position: DVec3,
    pub forward: DVec3,
    pub right: DVec3,
}

/// A looping sound of the soundscape (`loopingsound_t`).
#[derive(Clone, Debug, PartialEq)]
pub struct LoopingSound {
    pub wave: String,
    pub position: Option<DVec3>,
    pub volume_target: f64,
    pub volume_current: f64,
    pub soundlevel: i32,
    pub pitch: i32,
    /// The soundscape start that last asked for it; older ones are fading out.
    generation: u64,
    pub handle: u64,
}

/// A set of sounds played now and then (`randomsound_t`).
#[derive(Clone, Debug, PartialEq)]
pub struct RandomSound {
    pub position: Option<DVec3>,
    /// Plays at a new place around the listener each time.
    pub random_position: bool,
    pub next_play_time: f64,
    pub time: Interval,
    pub volume: Interval,
    pub pitch: Interval,
    pub soundlevel: Interval,
    pub master_volume: f64,
    pub waves: Vec<String>,
}

/// What a sub-soundscape inherits (`subsoundscapeparams_t`).
#[derive(Clone, Copy, Debug)]
struct SubParams {
    allow_dsp: bool,
    master_volume: f64,
    starting_position: i32,
    recurse_level: u32,
    position_override: i32,
    ambient_position_override: i32,
}

/// Runs soundscape scripts, as the client's soundscape system does.
#[derive(Clone, Debug)]
pub struct SoundscapePlayer {
    params: Option<AudioParams>,
    /// The soundscape that was started last, as the scripts spell it.
    playing: Option<String>,
    looping: Vec<LoopingSound>,
    random: Vec<RandomSound>,
    generation: u64,
    next_random_time: f64,
    /// The room effect the soundscape asked for with `dsp`.
    dsp: Option<i32>,
    /// What the game would print to the console about the soundscape started last.
    warnings: Vec<String>,
    random_source: Random,
}

impl SoundscapePlayer {
    pub fn new(seed: u64) -> Self {
        Self {
            params: None,
            playing: None,
            looping: Vec::new(),
            random: Vec::new(),
            generation: 0,
            next_random_time: 0.0,
            dsp: None,
            warnings: Vec::new(),
            random_source: Random::new(seed),
        }
    }

    /// The name of the soundscape started last.
    pub fn playing(&self) -> Option<&str> {
        self.playing.as_deref()
    }

    pub fn looping(&self) -> &[LoopingSound] {
        &self.looping
    }

    pub fn random(&self) -> &[RandomSound] {
        &self.random
    }

    pub fn dsp(&self) -> Option<i32> {
        self.dsp
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Takes what the server chose, as `UpdateAudioParams`: when the soundscape or the
    /// entity in charge changed and the soundscape is known, it starts. An unknown
    /// soundscape leaves the old sounds playing. With `restart`, the soundscape starts
    /// again even if nothing changed, for a script that was edited.
    pub fn set_params(
        &mut self,
        audio: Option<&AudioParams>,
        restart: bool,
        library: &SoundscapeLibrary,
        time: f64,
        sink: &mut dyn SoundSink,
    ) {
        let Some(audio) = audio else {
            return;
        };
        let same = self.params.as_ref().is_some_and(|params| {
            params.entity == audio.entity
                && params.soundscape.eq_ignore_ascii_case(&audio.soundscape)
                && params.positions == audio.positions
        });
        if same && !restart {
            return;
        }
        self.params = Some(audio.clone());
        if audio.entity.is_none() {
            return;
        }
        match library.find(&audio.soundscape) {
            Some(soundscape) => self.start(soundscape, library, time, sink),
            None => {
                self.warnings = vec![if audio.soundscape.is_empty() {
                    "The soundscape entity names no soundscape.".to_owned()
                } else {
                    format!("No script defines the soundscape \"{}\".", audio.soundscape)
                }];
            }
        }
    }

    /// Stops everything at once, as when the map ends.
    pub fn stop(&mut self, sink: &mut dyn SoundSink) {
        for sound in self.looping.drain(..) {
            sink.stop(sound.handle);
        }
        self.random.clear();
        self.params = None;
        self.playing = None;
        self.dsp = None;
        self.warnings.clear();
    }

    /// One frame: fades looping sounds and plays the random sounds that are due.
    pub fn update(&mut self, time: f64, frame_time: f64, listener: &Listener, sink: &mut dyn SoundSink) {
        let amount = frame_time / SOUNDSCAPE_FADE_TIME;
        let mut index = self.looping.len();
        while index > 0 {
            index -= 1;
            let sound = &mut self.looping[index];
            if sound.volume_current == sound.volume_target {
                continue;
            }
            sound.volume_current = approach(sound.volume_target, sound.volume_current, amount);
            if sound.volume_target == 0.0 && sound.volume_current == 0.0 {
                sink.stop(sound.handle);
                self.looping.swap_remove(index);
            } else {
                sink.set_volume(sound.handle, sound.volume_current);
            }
        }
        if time < self.next_random_time {
            return;
        }
        self.next_random_time = time + 3600.0;
        for index in (0..self.random.len()).rev() {
            if time >= self.random[index].next_play_time {
                self.play_random(index, listener, sink);
                let sound = &mut self.random[index];
                sound.next_play_time = time + sound.time.pick(&mut self.random_source);
            }
            self.next_random_time = self.next_random_time.min(self.random[index].next_play_time);
        }
    }

    fn start(&mut self, soundscape: &Soundscape, library: &SoundscapeLibrary, time: f64, sink: &mut dyn SoundSink) {
        for sound in &mut self.looping {
            sound.volume_target = 0.0;
        }
        self.generation += 1;
        self.random.clear();
        self.next_random_time = time;
        self.warnings.clear();
        self.playing = Some(soundscape.name.clone());
        let params = SubParams {
            allow_dsp: true,
            master_volume: 1.0,
            starting_position: 0,
            recurse_level: 0,
            position_override: -1,
            ambient_position_override: -1,
        };
        self.start_sub(soundscape, params, library, time, sink);
    }

    fn start_sub(
        &mut self,
        soundscape: &Soundscape,
        params: SubParams,
        library: &SoundscapeLibrary,
        time: f64,
        sink: &mut dyn SoundSink,
    ) {
        for item in &soundscape.block.items {
            let name = match item {
                Item::Pair(pair) => pair.key.to_ascii_lowercase(),
                Item::Block(block) => block.name.to_ascii_lowercase(),
            };
            match (name.as_str(), item) {
                ("dsp", Item::Pair(pair)) => {
                    if params.allow_dsp {
                        self.dsp = Some(leading_integer(&pair.value));
                    }
                }
                ("playlooping", Item::Block(block)) => self.play_looping(block, &params, sink),
                ("playrandom", Item::Block(block)) => self.add_random(block, &params, time),
                ("playsoundscape", Item::Block(block)) => self.play_soundscape(block, &params, library, time, sink),
                ("dsp_player" | "soundmixer" | "dsp_volume", _) => {}
                _ => self
                    .warnings
                    .push(format!("Soundscape {}: unknown command {name}", soundscape.name)),
            }
        }
    }

    /// The position a sound index stands for, when the soundscape entity has it.
    fn position(&self, index: i32) -> Option<DVec3> {
        let index = usize::try_from(index).ok()?;
        self.params.as_ref()?.positions.get(index).copied().flatten()
    }

    fn play_looping(&mut self, block: &Block, params: &SubParams, sink: &mut dyn SoundSink) {
        let mut volume = 0.0;
        let mut soundlevel = attenuation_to_soundlevel(0.8);
        let mut wave = None;
        let mut pitch = PITCH_NORM;
        let mut position_index = -1;
        for pair in block.pairs() {
            let value = pair.value.as_str();
            match pair.key.to_ascii_lowercase().as_str() {
                "volume" => volume = params.master_volume * Interval::parse(value).pick(&mut self.random_source),
                "pitch" => pitch = Interval::parse(value).pick(&mut self.random_source) as i32,
                "wave" => wave = Some(value),
                "position" => position_index = params.starting_position + leading_integer(value),
                "attenuation" => {
                    soundlevel = attenuation_to_soundlevel(Interval::parse(value).pick(&mut self.random_source));
                }
                "soundlevel" => soundlevel = self.soundlevel(value),
                "suppress_on_restore" => {}
                other => self
                    .warnings
                    .push(format!("Ambient {}: unknown command {other}", block.name)),
            }
        }
        if position_index < 0 {
            position_index = params.ambient_position_override;
        } else if params.position_override >= 0 {
            position_index = params.position_override;
        }
        let Some(wave) = wave else {
            return;
        };
        if volume == 0.0 {
            return;
        }
        if position_index < 0 {
            self.add_looping(wave, None, volume, SOUNDLEVEL_NORM, pitch, sink);
        } else if let Some(position) = self.position(position_index) {
            self.add_looping(wave, Some(position), volume, soundlevel, pitch, sink);
        } else {
            self.warnings.push(format!(
                "{wave} is silent: the soundscape entity has no position{position_index}."
            ));
        }
    }

    fn soundlevel(&mut self, value: &str) -> i32 {
        if value.trim().to_ascii_uppercase().starts_with("SNDLVL_") {
            text_to_soundlevel(value)
        } else {
            Interval::parse(value).pick(&mut self.random_source) as i32
        }
    }

    /// Starts a looping sound, or takes over one of an older soundscape start with the
    /// same file and pitch, as `AddLoopingSound`: one heard everywhere keeps playing and
    /// fades to the new volume, and so does one at the same place. One at another place
    /// is stopped and started again at the new place, at the volume it had reached.
    fn add_looping(
        &mut self,
        wave: &str,
        position: Option<DVec3>,
        volume: f64,
        soundlevel: i32,
        pitch: i32,
        sink: &mut dyn SoundSink,
    ) {
        let generation = self.generation;
        let reused = self.looping.iter().rposition(|sound| {
            sound.generation != generation
                && sound.pitch == pitch
                && sound.wave.eq_ignore_ascii_case(wave)
                && sound.position.is_some() == position.is_some()
        });
        let index = match reused {
            Some(index) => {
                let sound = &mut self.looping[index];
                let moved = match (sound.position, position) {
                    (Some(old), Some(new)) => (old - new).abs().max_element() > SAME_PLACE_TOLERANCE,
                    _ => false,
                };
                if moved {
                    sink.stop(sound.handle);
                    sound.handle = sink.start(&Emit {
                        wave: wave.to_owned(),
                        volume: sound.volume_current,
                        pitch,
                        soundlevel,
                        origin: position,
                        looping: true,
                    });
                }
                index
            }
            None => {
                let start_volume = if position.is_some() {
                    PLACED_LOOP_START_VOLUME
                } else {
                    0.0
                };
                let handle = sink.start(&Emit {
                    wave: wave.to_owned(),
                    volume: start_volume,
                    pitch,
                    soundlevel,
                    origin: position,
                    looping: true,
                });
                self.looping.push(LoopingSound {
                    wave: wave.to_owned(),
                    position,
                    volume_target: volume,
                    volume_current: start_volume,
                    soundlevel,
                    pitch,
                    generation,
                    handle,
                });
                self.looping.len() - 1
            }
        };
        let sound = &mut self.looping[index];
        sound.wave = wave.to_owned();
        sound.volume_target = volume;
        sound.pitch = pitch;
        sound.generation = generation;
        sound.position = position;
        sound.soundlevel = soundlevel;
    }

    fn add_random(&mut self, block: &Block, params: &SubParams, time: f64) {
        let mut sound = RandomSound {
            position: None,
            random_position: false,
            next_play_time: 0.0,
            time: Interval::default(),
            volume: Interval::default(),
            pitch: Interval::default(),
            soundlevel: Interval::default(),
            master_volume: params.master_volume,
            waves: Vec::new(),
        };
        let mut position_index = -1;
        let mut random_position = false;
        for item in &block.items {
            match item {
                Item::Pair(pair) => {
                    let value = pair.value.as_str();
                    match pair.key.to_ascii_lowercase().as_str() {
                        "volume" => sound.volume = Interval::parse(value),
                        "pitch" => sound.pitch = Interval::parse(value),
                        "attenuation" => {
                            let attenuation = Interval::parse(value);
                            let start = attenuation_to_soundlevel(attenuation.start);
                            let end = attenuation_to_soundlevel(attenuation.start + attenuation.range);
                            sound.soundlevel = Interval {
                                start: f64::from(start),
                                range: f64::from(end - start),
                            };
                        }
                        "soundlevel" => {
                            sound.soundlevel = if value.trim().to_ascii_uppercase().starts_with("SNDLVL_") {
                                Interval::fixed(f64::from(text_to_soundlevel(value)))
                            } else {
                                Interval::parse(value)
                            };
                        }
                        "time" => sound.time = Interval::parse(value),
                        "position" => {
                            if value.trim().eq_ignore_ascii_case("random") {
                                random_position = true;
                            } else {
                                position_index = params.starting_position + leading_integer(value);
                            }
                        }
                        "suppress_on_restore" => {}
                        other => self
                            .warnings
                            .push(format!("Random Sound {}: unknown command {other}", block.name)),
                    }
                }
                Item::Block(list) if list.name.eq_ignore_ascii_case("rndwave") => {
                    sound.waves = list.pairs().map(|pair| pair.value.clone()).collect();
                }
                Item::Block(other) => self
                    .warnings
                    .push(format!("Random Sound {}: unknown command {}", block.name, other.name)),
            }
        }
        if position_index < 0 {
            position_index = params.ambient_position_override;
        } else if params.position_override >= 0 {
            position_index = params.position_override;
            random_position = false;
        }
        if sound.waves.is_empty() {
            return;
        }
        if random_position {
            sound.random_position = true;
        } else if position_index >= 0 {
            match self.position(position_index) {
                Some(position) => sound.position = Some(position),
                None => {
                    self.warnings.push(format!(
                        "Random sounds are silent: the soundscape entity has no position{position_index}."
                    ));
                    return;
                }
            }
        }
        sound.next_play_time = time + 0.5 * sound.time.pick(&mut self.random_source);
        self.random.push(sound);
    }

    fn play_random(&mut self, index: usize, listener: &Listener, sink: &mut dyn SoundSink) {
        let sound = &self.random[index];
        let wave = sound.waves[self.random_source.index(sound.waves.len())].clone();
        let volume = sound.master_volume * sound.volume.pick(&mut self.random_source);
        let positioned = sound.position.is_some() || sound.random_position;
        let soundlevel = if positioned {
            sound.soundlevel.pick(&mut self.random_source) as i32
        } else {
            SOUNDLEVEL_NONE
        };
        let pitch = sound.pitch.pick(&mut self.random_source) as i32;
        let origin = if sound.random_position {
            let angle = self.random_source.between(-180.0, 180.0);
            let around = listener.right * angle.cos() + listener.forward * angle.sin();
            let position = listener.position + around * RANDOM_POSITION_RADIUS;
            self.random[index].position = Some(position);
            Some(position)
        } else {
            sound.position
        };
        sink.start(&Emit {
            wave,
            volume,
            pitch,
            soundlevel,
            origin,
            looping: false,
        });
    }

    fn play_soundscape(
        &mut self,
        block: &Block,
        params: &SubParams,
        library: &SoundscapeLibrary,
        time: f64,
        sink: &mut dyn SoundSink,
    ) {
        let mut sub = *params;
        sub.allow_dsp = false;
        sub.recurse_level += 1;
        if sub.recurse_level > MAX_SOUNDSCAPE_RECURSION {
            self.warnings.push("Soundscape recursion overrun".to_owned());
            return;
        }
        let mut name = None;
        for pair in block.pairs() {
            let value = pair.value.as_str();
            match pair.key.to_ascii_lowercase().as_str() {
                "volume" => {
                    sub.master_volume = params.master_volume * Interval::parse(value).pick(&mut self.random_source);
                }
                "position" => sub.starting_position = params.starting_position + leading_integer(value),
                "name" => name = Some(value),
                "positionoverride" => {
                    if params.position_override < 0 {
                        sub.position_override = params.starting_position + leading_integer(value);
                        sub.ambient_position_override = params.starting_position + leading_integer(value);
                    }
                }
                "ambientpositionoverride" => {
                    if params.ambient_position_override < 0 {
                        sub.ambient_position_override = params.starting_position + leading_integer(value);
                    }
                }
                other => self.warnings.push(format!("Playsoundscape: unknown command {other}")),
            }
        }
        let Some(name) = name else {
            return;
        };
        match library.find(name) {
            Some(soundscape) => self.start_sub(soundscape, sub, library, time, sink),
            None => self.warnings.push(format!("Trying to play unknown soundscape {name}")),
        }
    }
}

/// Moves `value` towards `target` by at most `amount`.
fn approach(target: f64, value: f64, amount: f64) -> f64 {
    if value < target {
        (value + amount).min(target)
    } else {
        (value - amount).max(target)
    }
}
