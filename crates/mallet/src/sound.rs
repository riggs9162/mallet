//! The sound preview: what a player standing at the 3D camera would hear of the map's
//! soundscapes and of the `ambient_generic`s that play from the start.
//!
//! [`mallet_vmf::sound`] holds the engine's rules. This module feeds them the camera, the
//! map's brushes for the traces, the game's scripts and sound files, and plays the result
//! through a [`Mixer`]. Scripts and sound files are read on worker threads, and only the
//! ones the map asks for, since a game's files often sit on a slow disk.

mod mixer;
mod output;

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use glam::DVec3;
use mallet_content::{load_sound, GameFs, Materials, SoundData, Surface};
use mallet_vmf::sound::{
    distance_gain, distant_variant_mix, obscured_gain, pan, split_sound_name, AmbientGeneric, Emit, Listener,
    MapSounds, Random, SoundCharacters, SoundScripts, SoundSink, SoundscapeChooser, SoundscapeLibrary,
    SoundscapePlayer, PITCH_NORM,
};
use serde::Serialize;

use crate::config::SoundOptions;
use crate::scene::{Owner, Scene};

use mixer::{Mixer, VoiceLevels};
use output::Output;

/// The shortest time between two updates of the preview.
const UPDATE_INTERVAL: Duration = Duration::from_millis(40);
/// The longest step one update may take, so a stalled window does not skip fades.
const MAX_STEP: f64 = 0.25;
/// How often the soundscape in charge is chosen again, in seconds.
const CHOOSE_INTERVAL: f64 = 0.1;
/// How long a one-shot sound may wait for its file before it is dropped, in seconds.
const ONE_SHOT_PATIENCE: f64 = 1.5;
/// How long a hidden sound takes to reach its quieter gain, as the engine's
/// `SND_GAIN_FADE_TIME`.
const OBSCURED_FADE_TIME: f64 = 0.25;
/// How long to wait before trying a failed sound device again.
const DEVICE_RETRY: Duration = Duration::from_secs(3);
/// Decoded sounds kept beyond the ones playing, in bytes.
const SOUND_CACHE_BYTES: usize = 192 * 1024 * 1024;
/// Tool materials that stop no trace, for when the game's materials cannot be read.
const UNSOLID_TOOL_NAMES: [&str; 14] = [
    "toolstrigger",
    "toolsplayerclip",
    "toolsnpcclip",
    "toolsclip",
    "toolshint",
    "toolsskip",
    "toolsareaportal",
    "toolsoccluder",
    "toolsorigin",
    "toolsfog",
    "toolsblock_los",
    "toolsskyfog",
    "toolsinvisibleladder",
    "toolscontrolclip",
];

/// A sound file being read, read, or missing.
enum SoundFile {
    Loading,
    Ready(Arc<SoundData>),
    Failed(String),
}

/// The game's sound files decoded so far, read by one worker thread.
#[derive(Default)]
struct SoundFiles {
    filesystem: Option<Arc<GameFs>>,
    files: HashMap<String, SoundFile>,
    requests: Option<Sender<String>>,
    results: Option<Receiver<(String, Result<SoundData, String>)>>,
}

impl SoundFiles {
    /// Starts over for another game's files.
    fn set_game(&mut self, filesystem: Option<Arc<GameFs>>) {
        let same = match (&self.filesystem, &filesystem) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        *self = Self::default();
        let Some(filesystem) = filesystem else {
            return;
        };
        let (requests, wanted) = mpsc::channel::<String>();
        let (done, results) = mpsc::channel();
        let reader = Arc::clone(&filesystem);
        let spawned = thread::Builder::new().name("sound files".to_owned()).spawn(move || {
            for file in wanted {
                let sound = load_sound(&reader, &file);
                if done.send((file, sound)).is_err() {
                    break;
                }
            }
        });
        if spawned.is_ok() {
            self.requests = Some(requests);
            self.results = Some(results);
        }
        self.filesystem = Some(filesystem);
    }

    /// Takes in the files the worker finished and forgets unused ones past the budget.
    fn poll(&mut self) {
        let Some(results) = &self.results else {
            return;
        };
        for (file, sound) in results.try_iter() {
            let loaded = match sound {
                Ok(sound) => SoundFile::Ready(Arc::new(sound)),
                Err(error) => SoundFile::Failed(error),
            };
            self.files.insert(file, loaded);
        }
        let size = |file: &SoundFile| match file {
            SoundFile::Ready(sound) => sound.samples.len() * 2,
            _ => 0,
        };
        if self.files.values().map(size).sum::<usize>() > SOUND_CACHE_BYTES {
            self.files
                .retain(|_, file| !matches!(file, SoundFile::Ready(sound) if Arc::strong_count(sound) == 1));
        }
    }

    /// Asks for a file to be read, if it has not been asked for yet.
    fn request(&mut self, file: &str) {
        if self.files.contains_key(file) {
            return;
        }
        let state = match &self.requests {
            Some(requests) if requests.send(file.to_owned()).is_ok() => SoundFile::Loading,
            _ => SoundFile::Failed("no game folder is set".to_owned()),
        };
        self.files.insert(file.to_owned(), state);
    }

    fn get(&self, file: &str) -> Option<&SoundFile> {
        self.files.get(file)
    }
}

/// What a worker thread read of the game's scripts.
struct LoadedScripts {
    soundscapes: SoundscapeLibrary,
    soundscape_files: usize,
    game_sounds: Option<SoundScripts>,
    errors: Vec<String>,
}

/// The soundscape scripts and game sound scripts of the game, for one map.
#[derive(Default)]
struct Scripts {
    /// The game folder and map name the scripts were read for.
    key: Option<(PathBuf, String)>,
    loading: Option<Receiver<LoadedScripts>>,
    loaded: bool,
    soundscapes: SoundscapeLibrary,
    soundscape_files: usize,
    game_sounds: Option<SoundScripts>,
    /// Whether the load in progress or done includes the game sound scripts.
    with_game_sounds: bool,
    errors: Vec<String>,
}

impl Scripts {
    /// Reads the scripts again when the game or the map's name changed, or when the game
    /// sound scripts are wanted for the first time. Returns whether a load finished.
    fn update(&mut self, filesystem: Option<&Arc<GameFs>>, map_name: &str, want_game_sounds: bool) -> bool {
        let Some(filesystem) = filesystem else {
            *self = Self::default();
            return false;
        };
        let key = (filesystem.game_dir().to_owned(), map_name.to_ascii_lowercase());
        let stale = self.key.as_ref() != Some(&key) || (want_game_sounds && !self.with_game_sounds);
        if stale {
            let (done, loading) = mpsc::channel();
            let reader = Arc::clone(filesystem);
            let name = key.1.clone();
            let spawned = thread::Builder::new().name("sound scripts".to_owned()).spawn(move || {
                let _ = done.send(load_scripts(&reader, &name, want_game_sounds));
            });
            self.loading = spawned.is_ok().then_some(loading);
            self.key = Some(key);
            self.with_game_sounds = want_game_sounds;
        }
        let Some(loading) = &self.loading else {
            return false;
        };
        match loading.try_recv() {
            Ok(scripts) => {
                self.soundscapes = scripts.soundscapes;
                self.soundscape_files = scripts.soundscape_files;
                if scripts.game_sounds.is_some() {
                    self.game_sounds = scripts.game_sounds;
                }
                self.errors = scripts.errors;
                self.loaded = true;
                self.loading = None;
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.loading = None;
                false
            }
        }
    }

    /// Forgets what was read, so the next update reads the scripts again.
    fn reload(&mut self) {
        self.key = None;
    }
}

/// Reads the soundscape scripts the way the client does: every file the manifest lists,
/// then the map's own `scripts/soundscapes_<map>.txt` unless the manifest listed it.
/// With `game_sounds`, also the game sound scripts and the map's `_level_sounds` file.
fn load_scripts(filesystem: &GameFs, map_name: &str, game_sounds: bool) -> LoadedScripts {
    let mut scripts = LoadedScripts {
        soundscapes: SoundscapeLibrary::default(),
        soundscape_files: 0,
        game_sounds: None,
        errors: Vec::new(),
    };
    let text = |path: &str| {
        filesystem
            .read(path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    };
    let map_file = (!map_name.is_empty()).then(|| format!("scripts/soundscapes_{map_name}.txt"));
    let mut files = Vec::new();
    match text("scripts/soundscapes_manifest.txt") {
        Some(manifest) => match SoundscapeLibrary::manifest_files(&manifest) {
            Ok(listed) => files = listed,
            Err(error) => scripts
                .errors
                .push(format!("scripts/soundscapes_manifest.txt: {error}")),
        },
        None => scripts
            .errors
            .push("The game has no scripts/soundscapes_manifest.txt.".to_owned()),
    }
    if let Some(map_file) = map_file {
        if !files.iter().any(|file| file.eq_ignore_ascii_case(&map_file)) && filesystem.exists(&map_file) {
            files.push(map_file);
        }
    }
    for file in files {
        match text(&file) {
            Some(content) => match scripts.soundscapes.add_file(&file, &content) {
                Ok(_) => scripts.soundscape_files += 1,
                Err(error) => scripts.errors.push(error),
            },
            None => scripts.errors.push(format!("{file} is listed but missing.")),
        }
    }
    if game_sounds {
        let mut sounds = SoundScripts::default();
        if let Some(manifest) = text("scripts/game_sounds_manifest.txt") {
            match SoundScripts::manifest_files(&manifest) {
                Ok(listed) => {
                    for file in listed {
                        if let Some(content) = text(&file) {
                            if let Err(error) = sounds.add_file(&content, false) {
                                scripts.errors.push(format!("{file}: {error}"));
                            }
                        }
                    }
                }
                Err(error) => scripts
                    .errors
                    .push(format!("scripts/game_sounds_manifest.txt: {error}")),
            }
        }
        if !map_name.is_empty() {
            let file = format!("maps/{map_name}_level_sounds.txt");
            if let Some(content) = text(&file) {
                if let Err(error) = sounds.add_file(&content, true) {
                    scripts.errors.push(format!("{file}: {error}"));
                }
            }
        }
        scripts.game_sounds = Some(sounds);
    }
    scripts
}

/// What started a voice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VoiceSource {
    Soundscape,
    Ambient(u32),
}

/// One sound the engine would be playing: a channel.
struct Voice {
    handle: u64,
    source: VoiceSource,
    /// The sound as scripted, and its file under `sound/`.
    wave: String,
    file: String,
    characters: SoundCharacters,
    volume: f64,
    pitch: f64,
    soundlevel: i32,
    origin: Option<DVec3>,
    /// Whether whoever started it means it to keep playing.
    looping: bool,
    started: f64,
    mixer_id: Option<u64>,
    /// The gain left by walls in the way, gliding to its target.
    obscured: f64,
    /// The file played to its end and has no loop point.
    ended: bool,
    /// Why the file cannot be played.
    problem: Option<String>,
    /// The latest gain of each speaker, for the status.
    gains: [f32; 2],
    /// Whether the file is stereo, once it is read.
    stereo: bool,
    distance: Option<f64>,
}

/// The preview's voices, which the soundscape player starts sounds on.
struct Channels<'a> {
    voices: &'a mut Vec<Voice>,
    next_handle: &'a mut u64,
    mixer: &'a Mixer,
    time: f64,
    source: VoiceSource,
}

impl SoundSink for Channels<'_> {
    fn start(&mut self, sound: &Emit) -> u64 {
        *self.next_handle += 1;
        let (characters, file) = split_sound_name(&sound.wave);
        self.voices.push(Voice {
            handle: *self.next_handle,
            source: self.source,
            wave: sound.wave.clone(),
            file,
            characters,
            volume: sound.volume,
            pitch: f64::from(sound.pitch),
            soundlevel: sound.soundlevel,
            origin: sound.origin,
            looping: sound.looping,
            started: self.time,
            mixer_id: None,
            obscured: 1.0,
            ended: false,
            problem: None,
            gains: [0.0; 2],
            stereo: false,
            distance: None,
        });
        *self.next_handle
    }

    fn set_volume(&mut self, handle: u64, volume: f64) {
        if let Some(voice) = self.voices.iter_mut().find(|voice| voice.handle == handle) {
            voice.volume = volume;
        }
    }

    fn stop(&mut self, handle: u64) {
        if let Some(index) = self.voices.iter().position(|voice| voice.handle == handle) {
            if let Some(id) = self.voices.swap_remove(index).mixer_id {
                self.mixer.stop(id);
            }
        }
    }
}

/// An `ambient_generic` that plays from the start of the map.
struct Ambient {
    spec: AmbientGeneric,
    handle: Option<u64>,
    volume: f64,
    pitch: f64,
    /// Why it stays silent.
    problem: Option<String>,
}

/// What the preview is doing, for the viewport's notice and the `sound_preview` tool.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SoundStatus {
    /// Whether the preview is switched on.
    pub running: bool,
    /// Silent because Mallet is not the active window.
    pub muted: bool,
    /// The sound device playing the preview, or why none could be opened.
    pub device: Option<String>,
    pub device_error: Option<String>,
    /// The loudest sample of the mix lately, 1 being full scale.
    pub peak: f32,
    pub scripts: ScriptStatus,
    /// The soundscape entity in charge, when one is.
    pub soundscape: Option<SoundscapeStatus>,
    pub voices: Vec<VoiceStatus>,
    pub ambient_generics: Vec<AmbientStatus>,
}

impl SoundStatus {
    /// One line on what is playing, and whether it reports a problem; `None` while the
    /// preview is off.
    pub fn notice(&self, map: &MapSounds) -> Option<(String, bool)> {
        if !self.running {
            return None;
        }
        if let Some(error) = &self.device_error {
            return Some((format!("No sound device: {error}"), true));
        }
        let missing = self.voices.iter().filter(|voice| voice.state == "missing").count();
        let ambients = self.ambient_generics.iter().filter(|ambient| ambient.playing).count();
        let mut warning = missing > 0;
        let mut text = match &self.soundscape {
            _ if self.scripts.loading && self.scripts.soundscapes == 0 => {
                "Reading the game's sound scripts...".to_owned()
            }
            Some(soundscape) if !soundscape.known => {
                warning = true;
                let mut text = if soundscape.name.is_empty() {
                    "This soundscape entity names no soundscape".to_owned()
                } else {
                    format!("No script defines the soundscape \"{}\"", soundscape.name)
                };
                if let Some(playing) = &soundscape.playing {
                    text.push_str(&format!(", so {playing} plays on as in the game"));
                }
                text
            }
            Some(soundscape) => {
                let mut text = format!("Soundscape: {}", soundscape.name);
                if !soundscape.in_range {
                    text.push_str(" (left behind, still playing as in the game)");
                }
                text
            }
            None if map.soundscapes.is_empty() => "No soundscape entity in this map".to_owned(),
            None => "No soundscape here: none is in range and in sight of the camera".to_owned(),
        };
        if ambients > 0 {
            text.push_str(&format!(", {ambients} ambient_generic"));
        }
        if missing > 0 {
            text.push_str(&format!(", {missing} sound files missing"));
        }
        if self.muted {
            text.push_str(" (silent while Mallet is not the active window)");
        }
        Some((text, warning))
    }
}

/// What was read of the game's sound scripts.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ScriptStatus {
    pub loading: bool,
    pub soundscape_files: usize,
    pub soundscapes: usize,
    /// How many game sounds the sound scripts define; `None` until a map needs them.
    pub game_sounds: Option<usize>,
    pub errors: Vec<String>,
}

/// The soundscape the listener hears.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SoundscapeStatus {
    /// The entity in charge; `None` after the last `trigger_soundscape` was left.
    pub entity: Option<u32>,
    /// The soundscape the entity names.
    pub name: String,
    /// Whether the game's scripts define that soundscape.
    pub known: bool,
    /// The soundscape whose sounds are playing: the last one started, which stays when
    /// the entity in charge names one the scripts do not define.
    pub playing: Option<String>,
    /// Whether the entity is still in range and in sight; if not, its sounds keep
    /// playing until another soundscape takes over, as in the game.
    pub in_range: bool,
    /// The room effect the soundscape sets, which the preview does not apply.
    pub dsp: Option<i32>,
    /// Sets of random sounds waiting for their turn.
    pub random_sounds: usize,
    pub warnings: Vec<String>,
}

/// One sound of the preview.
#[derive(Clone, Debug, Default, Serialize)]
pub struct VoiceStatus {
    /// `soundscape` or `ambient_generic`.
    pub source: &'static str,
    pub entity: Option<u32>,
    pub wave: String,
    /// `playing`, `loading`, `missing` or `ended`.
    pub state: &'static str,
    pub looping: bool,
    pub volume: f64,
    pub pitch: f64,
    pub soundlevel: i32,
    pub origin: Option<[f64; 3]>,
    pub distance: Option<f64>,
    /// The gain of the left and right speaker, with distance, walls and direction.
    pub left: f32,
    pub right: f32,
    pub note: Option<String>,
}

/// An `ambient_generic` and whether it plays.
#[derive(Clone, Debug, Default, Serialize)]
pub struct AmbientStatus {
    pub entity: u32,
    pub sound: String,
    pub playing: bool,
    pub note: Option<String>,
}

/// What the preview needs to know about the editor for one update.
pub struct SoundFrame<'a> {
    pub options: &'a SoundOptions,
    /// Whether Mallet is the active window.
    pub focused: bool,
    /// The 3D camera's position, forward and right directions.
    pub listener: Listener,
    pub scene: &'a Scene,
    pub materials: Option<&'a mut Materials>,
    /// The map's file name without its extension; empty for a map never saved.
    pub map_name: String,
    /// Which map tab is showing, so another tab starts from silence.
    pub tab: u64,
    /// Whether a map is open.
    pub open: bool,
}

/// The sound preview's state.
pub struct SoundPreview {
    mixer: Mixer,
    output: Option<Output>,
    output_error: Option<String>,
    output_retry: Option<Instant>,
    files: SoundFiles,
    scripts: Scripts,
    chooser: SoundscapeChooser,
    player: SoundscapePlayer,
    voices: Vec<Voice>,
    ambients: Vec<Ambient>,
    next_handle: u64,
    random: Random,
    last_update: Option<Instant>,
    time: f64,
    last_choice: f64,
    /// The tab and scene generation the preview last followed.
    tab: Option<u64>,
    scene_generation: u64,
    /// Whether each brush material stops traces, by lower-case name.
    solid_materials: HashMap<String, bool>,
    /// Start the soundscape again at the next update, after its scripts were read anew.
    restart: bool,
    status: SoundStatus,
}

impl Default for SoundPreview {
    fn default() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x5EED, |since| since.as_nanos() as u64);
        Self {
            mixer: Mixer::default(),
            output: None,
            output_error: None,
            output_retry: None,
            files: SoundFiles::default(),
            scripts: Scripts::default(),
            chooser: SoundscapeChooser::default(),
            player: SoundscapePlayer::new(seed),
            voices: Vec::new(),
            ambients: Vec::new(),
            next_handle: 0,
            random: Random::new(seed.rotate_left(17)),
            last_update: None,
            time: 0.0,
            last_choice: f64::NEG_INFINITY,
            tab: None,
            scene_generation: 0,
            solid_materials: HashMap::new(),
            restart: false,
            status: SoundStatus::default(),
        }
    }
}

impl SoundPreview {
    /// What the preview was doing at its last update.
    pub fn status(&self) -> &SoundStatus {
        &self.status
    }

    /// The names of the game's soundscapes that contain `filter`, ignoring case, in the
    /// order the scripts define them. Empty until the preview has read the scripts.
    pub fn soundscape_names(&self, filter: &str) -> Vec<String> {
        let filter = filter.trim().to_ascii_lowercase();
        self.scripts
            .soundscapes
            .soundscapes
            .iter()
            .filter(|soundscape| soundscape.name.to_ascii_lowercase().contains(&filter))
            .map(|soundscape| soundscape.name.clone())
            .collect()
    }

    /// Whether the preview is running and should be updated again soon.
    pub fn running(&self) -> bool {
        self.status.running
    }

    /// Reads the game's sound scripts again and starts the soundscape over, after a
    /// script was edited outside Mallet.
    pub fn reload_scripts(&mut self) {
        self.scripts.reload();
        self.restart = true;
    }

    /// One update: follows the camera and the map, starts and stops sounds, and sets
    /// every playing sound's volume for each speaker.
    pub fn update(&mut self, mut frame: SoundFrame<'_>) {
        if !frame.options.preview || !frame.open {
            self.shut_down();
            return;
        }
        let now = Instant::now();
        let step = match self.last_update {
            Some(last) if now.duration_since(last) < UPDATE_INTERVAL => {
                self.mixer.keep_alive(self.master(&frame));
                return;
            }
            Some(last) => now.duration_since(last).as_secs_f64().min(MAX_STEP),
            None => 0.0,
        };
        self.last_update = Some(now);
        self.time += step;
        self.open_output(now);
        self.mixer.keep_alive(self.master(&frame));

        let filesystem = frame.materials.as_ref().map(|materials| materials.shared_filesystem());
        self.files.set_game(filesystem.clone());
        self.files.poll();

        let map = &frame.scene.sounds;
        if self.tab != Some(frame.tab) {
            self.silence();
            self.tab = Some(frame.tab);
            self.scene_generation = 0;
        }
        if self.scene_generation != frame.scene.geometry_generation {
            self.scene_generation = frame.scene.geometry_generation;
            self.chooser.refresh(map);
            self.last_choice = f64::NEG_INFINITY;
            self.learn_materials(frame.scene, frame.materials.as_deref_mut());
        }
        let want_game_sounds = frame.options.ambient_generics
            && map
                .ambients
                .iter()
                .any(|ambient| ambient.plays_at_start() && !ambient.is_file());
        if self
            .scripts
            .update(filesystem.as_ref(), &frame.map_name, want_game_sounds)
        {
            self.restart = true;
        }

        let solid = std::mem::take(&mut self.solid_materials);
        let scene = frame.scene;
        let stops =
            |name: Option<&str>| name.is_none_or(|name| solid.get(&name.to_ascii_lowercase()).copied().unwrap_or(true));
        let brush_blocked = |from: DVec3, to: DVec3| {
            scene.brush_line_blocked(from, to, |mesh, face| {
                let entity_solid = match mesh.owner {
                    Some(Owner::Entity(entity)) => !map.non_solid.contains(&entity),
                    _ => true,
                };
                entity_solid && stops(face.material.as_deref())
            })
        };
        let world_blocked = |from: DVec3, to: DVec3| {
            scene.brush_line_blocked(from, to, |mesh, face| {
                let world = match mesh.owner {
                    Some(Owner::Entity(entity)) => map.detail.contains(&entity),
                    _ => true,
                };
                world && stops(face.material.as_deref())
            })
        };

        if frame.options.soundscapes {
            if self.time - self.last_choice >= CHOOSE_INTERVAL {
                self.last_choice = self.time;
                self.chooser.update(map, frame.listener.position, &brush_blocked);
            }
            let mut channels = Channels {
                voices: &mut self.voices,
                next_handle: &mut self.next_handle,
                mixer: &self.mixer,
                time: self.time,
                source: VoiceSource::Soundscape,
            };
            if self.scripts.loaded {
                let restart = std::mem::take(&mut self.restart);
                self.player.set_params(
                    self.chooser.audio(),
                    restart,
                    &self.scripts.soundscapes,
                    self.time,
                    &mut channels,
                );
            }
            self.player.update(self.time, step, &frame.listener, &mut channels);
        } else {
            self.stop_soundscape();
        }

        self.update_ambients(map, frame.options.ambient_generics, step);
        self.update_voices(&frame.listener, frame.options.occlusion, step, &world_blocked);
        self.status = self.describe(&frame);
        self.solid_materials = solid;
    }

    /// The overall volume: the option's, or silence while another window is active.
    fn master(&self, frame: &SoundFrame<'_>) -> f32 {
        if frame.options.mute_unfocused && !frame.focused {
            0.0
        } else {
            frame.options.volume.clamp(0.0, 2.0) as f32
        }
    }

    /// Opens the sound device, trying again now and then after a failure.
    fn open_output(&mut self, now: Instant) {
        if let Some(failure) = self.output.as_ref().and_then(Output::failure) {
            self.output = None;
            self.output_error = Some(failure);
            self.output_retry = Some(now + DEVICE_RETRY);
        }
        if self.output.is_some() || self.output_retry.is_some_and(|retry| now < retry) {
            return;
        }
        match Output::open(self.mixer.clone()) {
            Ok(output) => {
                self.output = Some(output);
                self.output_error = None;
                self.output_retry = None;
            }
            Err(error) => {
                self.output_error = Some(error);
                self.output_retry = Some(now + DEVICE_RETRY);
            }
        }
    }

    /// Stops every sound and forgets which soundscape was playing, keeping the device
    /// and the files read so far.
    fn silence(&mut self) {
        self.mixer.stop_all();
        self.voices.clear();
        self.ambients.clear();
        self.chooser = SoundscapeChooser::default();
        let mut nothing = Channels {
            voices: &mut self.voices,
            next_handle: &mut self.next_handle,
            mixer: &self.mixer,
            time: self.time,
            source: VoiceSource::Soundscape,
        };
        self.player.stop(&mut nothing);
        self.last_choice = f64::NEG_INFINITY;
    }

    /// Switches the preview off: silence, and the sound device is given back.
    fn shut_down(&mut self) {
        if !self.status.running && self.output.is_none() {
            return;
        }
        self.silence();
        self.output = None;
        self.output_error = None;
        self.output_retry = None;
        self.last_update = None;
        self.tab = None;
        self.status = SoundStatus::default();
    }

    fn stop_soundscape(&mut self) {
        if self.player.playing().is_none() && self.chooser.audio().is_none() {
            return;
        }
        let mixer = &self.mixer;
        self.voices.retain(|voice| {
            let keep = voice.source != VoiceSource::Soundscape;
            if let (false, Some(id)) = (keep, voice.mixer_id) {
                mixer.stop(id);
            }
            keep
        });
        let mut channels = Channels {
            voices: &mut self.voices,
            next_handle: &mut self.next_handle,
            mixer: &self.mixer,
            time: self.time,
            source: VoiceSource::Soundscape,
        };
        self.player.stop(&mut channels);
        self.chooser = SoundscapeChooser::default();
        self.last_choice = f64::NEG_INFINITY;
    }

    /// Looks up which of the scene's brush materials stop traces: everything but the
    /// tool materials the compilers remove, such as triggers, clips and hints.
    fn learn_materials(&mut self, scene: &Scene, materials: Option<&mut Materials>) {
        let names: BTreeSet<String> = scene
            .meshes
            .iter()
            .flat_map(|mesh| &mesh.faces)
            .filter(|face| face.side.is_some())
            .filter_map(|face| face.material.as_deref())
            .map(str::to_ascii_lowercase)
            .filter(|name| !self.solid_materials.contains_key(name))
            .collect();
        if names.is_empty() {
            return;
        }
        match materials {
            Some(materials) => {
                let list: Vec<&str> = names.iter().map(String::as_str).collect();
                let found = materials.materials(&list);
                for (name, material) in names.iter().zip(found) {
                    self.solid_materials
                        .insert(name.clone(), material.surface != Surface::Tool);
                }
            }
            None => {
                for name in names {
                    let solid = !UNSOLID_TOOL_NAMES.iter().any(|tool| name.ends_with(tool));
                    self.solid_materials.insert(name, solid);
                }
            }
        }
    }

    /// Starts the `ambient_generic`s that play from the start, follows edits to them,
    /// and runs their fade in and spin up.
    fn update_ambients(&mut self, map: &MapSounds, enabled: bool, step: f64) {
        let wanted: Vec<&AmbientGeneric> = if enabled {
            map.ambients.iter().filter(|ambient| ambient.plays_at_start()).collect()
        } else {
            Vec::new()
        };
        let mixer = &self.mixer;
        let voices = &mut self.voices;
        self.ambients.retain_mut(|ambient| {
            let current = wanted.iter().find(|spec| spec.entity == ambient.spec.entity);
            let unchanged = current.is_some_and(|spec| {
                let mut moved = ambient.spec.clone();
                moved.origin = spec.origin;
                moved == **spec
            });
            if let (true, Some(spec)) = (unchanged, current) {
                ambient.spec.origin = spec.origin;
                return true;
            }
            if let Some(handle) = ambient.handle {
                if let Some(index) = voices.iter().position(|voice| voice.handle == handle) {
                    if let Some(id) = voices.swap_remove(index).mixer_id {
                        mixer.stop(id);
                    }
                }
            }
            false
        });
        for spec in wanted {
            if !self.ambients.iter().any(|ambient| ambient.spec.entity == spec.entity) {
                self.ambients.push(Ambient {
                    spec: spec.clone(),
                    handle: None,
                    volume: spec.start_volume,
                    pitch: spec.start_pitch,
                    problem: None,
                });
            }
        }
        for index in 0..self.ambients.len() {
            self.run_ambient(index, step);
        }
    }

    fn run_ambient(&mut self, index: usize, step: f64) {
        let ambient = &mut self.ambients[index];
        let spec = &ambient.spec;
        if ambient.volume < spec.volume {
            ambient.volume = (ambient.volume + spec.fade_in * step).min(spec.volume);
        }
        if ambient.pitch < spec.pitch {
            ambient.pitch = (ambient.pitch + spec.spin_up * step).min(spec.pitch);
        } else if ambient.pitch > spec.pitch {
            ambient.pitch = (ambient.pitch - spec.spin_up * step).max(spec.pitch);
        }
        if let Some(handle) = ambient.handle {
            if let Some(voice) = self.voices.iter_mut().find(|voice| voice.handle == handle) {
                voice.volume = ambient.volume;
                voice.pitch = ambient.pitch;
                voice.origin = Some(spec.origin);
            }
            return;
        }
        if ambient.problem.is_some() || ambient.volume <= 0.0 {
            return;
        }
        let (wave, soundlevel) = if spec.is_file() {
            if spec.sound.starts_with('!') {
                ambient.problem = Some("Sentences are not previewed.".to_owned());
                return;
            }
            (spec.sound.clone(), spec.soundlevel)
        } else {
            let Some(sounds) = &self.scripts.game_sounds else {
                return;
            };
            match sounds.get(&spec.sound) {
                Some(sound) if !sound.waves.is_empty() => (
                    sound.waves[self.random.index(sound.waves.len())].clone(),
                    sound.soundlevel,
                ),
                Some(_) => {
                    ambient.problem = Some(format!("The game sound \"{}\" names no file.", spec.sound));
                    return;
                }
                None => {
                    ambient.problem = Some(format!("No sound script defines \"{}\".", spec.sound));
                    return;
                }
            }
        };
        let mut channels = Channels {
            voices: &mut self.voices,
            next_handle: &mut self.next_handle,
            mixer: &self.mixer,
            time: self.time,
            source: VoiceSource::Ambient(spec.entity),
        };
        let handle = channels.start(&Emit {
            wave,
            volume: ambient.volume,
            pitch: PITCH_NORM,
            soundlevel,
            origin: Some(spec.origin),
            looping: true,
        });
        if let Some(voice) = self.voices.last_mut() {
            voice.pitch = ambient.pitch;
        }
        ambient.handle = Some(handle);
    }

    /// Starts voices whose files arrived, drops one-shots that ended, and sets every
    /// voice's gain for each speaker from where the listener is.
    fn update_voices(
        &mut self,
        listener: &Listener,
        occlusion: bool,
        step: f64,
        world_blocked: &dyn Fn(DVec3, DVec3) -> bool,
    ) {
        let time = self.time;
        let mixer = &self.mixer;
        let files = &mut self.files;
        self.voices.retain_mut(|voice| {
            if voice.ended || voice.problem.is_some() {
                return voice.looping;
            }
            let levels = voice_levels(voice, listener, occlusion, step, world_blocked);
            voice.gains = levels.speaker_gains(voice.stereo);
            match voice.mixer_id {
                Some(id) if mixer.is_playing(id) => mixer.set_levels(id, levels),
                Some(_) => {
                    voice.mixer_id = None;
                    voice.ended = true;
                    voice.gains = [0.0; 2];
                    return voice.looping;
                }
                None => {
                    files.request(&voice.file);
                    match files.get(&voice.file) {
                        Some(SoundFile::Ready(sound)) => {
                            voice.stereo = sound.channels >= 2;
                            voice.gains = levels.speaker_gains(voice.stereo);
                            voice.mixer_id = Some(mixer.play(Arc::clone(sound), levels));
                        }
                        Some(SoundFile::Failed(error)) => {
                            voice.problem = Some(error.clone());
                            voice.gains = [0.0; 2];
                            return voice.looping;
                        }
                        _ => {
                            voice.gains = [0.0; 2];
                            return voice.looping || time - voice.started < ONE_SHOT_PATIENCE;
                        }
                    }
                }
            }
            true
        });
    }

    fn describe(&mut self, frame: &SoundFrame<'_>) -> SoundStatus {
        let audio = self.chooser.audio();
        let soundscape = audio.map(|audio| SoundscapeStatus {
            entity: audio.entity,
            name: audio.soundscape.clone(),
            known: self.scripts.soundscapes.find(&audio.soundscape).is_some(),
            playing: self.player.playing().map(str::to_owned),
            in_range: self.chooser.in_range(),
            dsp: self.player.dsp(),
            random_sounds: self.player.random().len(),
            warnings: self.player.warnings().to_vec(),
        });
        let voices = self
            .voices
            .iter()
            .map(|voice| {
                let (source, entity) = match voice.source {
                    VoiceSource::Soundscape => ("soundscape", audio.and_then(|audio| audio.entity)),
                    VoiceSource::Ambient(entity) => ("ambient_generic", Some(entity)),
                };
                let state = if voice.problem.is_some() {
                    "missing"
                } else if voice.ended {
                    "ended"
                } else if voice.mixer_id.is_some() {
                    "playing"
                } else {
                    "loading"
                };
                let note = voice.problem.clone().or_else(|| {
                    (voice.ended && voice.looping).then(|| {
                        "Played once: the file has no loop point, so the game does not repeat it either.".to_owned()
                    })
                });
                VoiceStatus {
                    source,
                    entity,
                    wave: voice.wave.clone(),
                    state,
                    looping: voice.looping,
                    volume: voice.volume,
                    pitch: voice.pitch,
                    soundlevel: voice.soundlevel,
                    origin: voice.origin.map(|origin| origin.to_array()),
                    distance: voice.distance,
                    left: voice.gains[0],
                    right: voice.gains[1],
                    note,
                }
            })
            .collect();
        let ambient_generics = frame
            .scene
            .sounds
            .ambients
            .iter()
            .map(|spec| {
                let running = self.ambients.iter().find(|ambient| ambient.spec.entity == spec.entity);
                let note = match running {
                    Some(ambient) => ambient.problem.clone().or_else(|| {
                        spec.modulated
                            .then(|| "Its preset or LFO is left out of the preview.".to_owned())
                    }),
                    None if !frame.options.ambient_generics => None,
                    None if spec.start_silent => Some("Starts silent: plays when an input turns it on.".to_owned()),
                    None if !spec.looped => Some("Not looped: plays when an input asks for it.".to_owned()),
                    None => Some("Its volume is 0.".to_owned()),
                };
                AmbientStatus {
                    entity: spec.entity,
                    sound: spec.sound.clone(),
                    playing: running.is_some_and(|ambient| ambient.handle.is_some()),
                    note,
                }
            })
            .collect();
        SoundStatus {
            running: true,
            muted: frame.options.mute_unfocused && !frame.focused,
            device: self.output.as_ref().map(|output| output.device.clone()),
            device_error: self.output_error.clone(),
            peak: self.mixer.take_peak(),
            scripts: ScriptStatus {
                loading: self.scripts.loading.is_some(),
                soundscape_files: self.scripts.soundscape_files,
                soundscapes: self.scripts.soundscapes.soundscapes.len(),
                game_sounds: self.scripts.game_sounds.as_ref().map(SoundScripts::len),
                errors: self.scripts.errors.clone(),
            },
            soundscape,
            voices,
            ambient_generics,
        }
    }
}

/// How a voice should sound from where the listener is, as the engine spreads a
/// channel: a sound with no place plays in both ears at its volume. A placed sound fades
/// with distance for its sound level, loses a little for each wall in the way, and is
/// shared between the speakers by its direction; a plain stereo file keeps its channels.
fn voice_levels(
    voice: &mut Voice,
    listener: &Listener,
    occlusion: bool,
    step: f64,
    world_blocked: &dyn Fn(DVec3, DVec3) -> bool,
) -> VoiceLevels {
    let pitch = if voice.pitch > 0.0 {
        voice.pitch
    } else {
        f64::from(PITCH_NORM)
    };
    let speed = (pitch / f64::from(PITCH_NORM)) as f32;
    let Some(origin) = voice.origin else {
        voice.distance = None;
        return VoiceLevels {
            volume: voice.volume as f32,
            pan: None,
            speed,
            blend: None,
        };
    };
    let offset = origin - listener.position;
    let distance = offset.length();
    voice.distance = Some(distance);
    let attenuated = voice.soundlevel > 0;
    let target = if occlusion && attenuated {
        obscured_gain(listener.position, origin, voice.soundlevel, world_blocked)
    } else {
        1.0
    };
    let fade = if voice.mixer_id.is_some() {
        step / OBSCURED_FADE_TIME
    } else {
        1.0
    };
    voice.obscured += (target - voice.obscured).clamp(-fade, fade);
    let volume = voice.volume * distance_gain(voice.soundlevel, distance) * voice.obscured;
    let characters = voice.characters;
    let direction = if distance > 1e-6 && !characters.omni {
        offset / distance
    } else {
        listener.forward
    };
    let shares = pan(direction, listener.right);
    let blend = if characters.distant_variant {
        Some(distant_variant_mix(distance) as f32)
    } else if characters.spatial_stereo {
        Some(0.5)
    } else if characters.directional {
        Some(0.0)
    } else {
        None
    };
    VoiceLevels {
        volume: volume as f32,
        pan: Some([shares[0] as f32, shares[1] as f32]),
        speed,
        blend,
    }
}
