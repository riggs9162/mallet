//! Mixes playing sounds into stereo. The sound preview sets each voice's gain for the
//! left and right speaker a few dozen times a second; the mixer glides between them so
//! a moving camera makes no clicks, repeats sounds from their loop point, and plays them
//! faster or slower for their pitch.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use mallet_content::SoundData;

/// How long a gain takes to travel from silence to full volume, in seconds.
const GAIN_GLIDE_SECONDS: f32 = 0.03;
/// How long the preview may go without an update before the mixer fades to silence, so a
/// minimized or frozen window does not keep sounding.
const SILENCE_AFTER: Duration = Duration::from_millis(700);
/// The scale of a 16 bit sample.
const SAMPLE_SCALE: f32 = 1.0 / 32768.0;

/// How one playing sound should sound right now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceLevels {
    /// The sound's volume, with whatever distance and walls take off it.
    pub volume: f32,
    /// The share of the sound each speaker gets, left then right, for a sound placed in
    /// the world; `None` plays it in both at its volume.
    pub pan: Option<[f32; 2]>,
    /// Playback speed: 1 plays the file as recorded.
    pub speed: f32,
    /// For a stereo file placed in the world as one sound: how much of it comes from
    /// its right channel, from 0 to 1. `None` plays a stereo file's channels to their own
    /// speakers, as the engine does, without spreading them by direction.
    pub blend: Option<f32>,
}

impl VoiceLevels {
    /// The gain of the left and the right speaker, for a mono or a stereo file.
    pub fn speaker_gains(&self, stereo: bool) -> [f32; 2] {
        match self.pan {
            Some([left, right]) if !stereo || self.blend.is_some() => [self.volume * left, self.volume * right],
            _ => [self.volume, self.volume],
        }
    }
}

struct Voice {
    id: u64,
    sound: Arc<SoundData>,
    /// Where playback is, in frames of the sound.
    cursor: f64,
    levels: VoiceLevels,
    gains: [f32; 2],
    blend: f32,
    stopping: bool,
}

#[derive(Default)]
struct State {
    voices: Vec<Voice>,
    next_id: u64,
    master: f32,
    master_target: f32,
    touched: Option<Instant>,
    peak: f32,
    scratch: Vec<[f32; 2]>,
}

/// A set of playing sounds, shared between the preview and the audio device's thread.
#[derive(Clone, Default)]
pub struct Mixer {
    state: Arc<Mutex<State>>,
}

impl Mixer {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts a sound and returns its id.
    pub fn play(&self, sound: Arc<SoundData>, levels: VoiceLevels) -> u64 {
        let mut state = self.lock();
        state.next_id += 1;
        let id = state.next_id;
        let gains = levels.speaker_gains(sound.channels >= 2);
        state.voices.push(Voice {
            id,
            sound,
            cursor: 0.0,
            levels,
            gains,
            blend: levels.blend.unwrap_or(0.0),
            stopping: false,
        });
        id
    }

    /// Changes how a playing sound sounds.
    pub fn set_levels(&self, id: u64, levels: VoiceLevels) {
        if let Some(voice) = self
            .lock()
            .voices
            .iter_mut()
            .find(|voice| voice.id == id && !voice.stopping)
        {
            voice.levels = levels;
        }
    }

    /// Fades a sound out quickly and drops it.
    pub fn stop(&self, id: u64) {
        if let Some(voice) = self.lock().voices.iter_mut().find(|voice| voice.id == id) {
            voice.stopping = true;
            voice.levels.volume = 0.0;
        }
    }

    /// Fades every sound out and drops it.
    pub fn stop_all(&self) {
        for voice in &mut self.lock().voices {
            voice.stopping = true;
            voice.levels.volume = 0.0;
        }
    }

    /// Whether a sound is still playing; one without a loop point ends by itself.
    pub fn is_playing(&self, id: u64) -> bool {
        self.lock().voices.iter().any(|voice| voice.id == id && !voice.stopping)
    }

    /// Sets the overall volume and tells the mixer the preview is still running.
    pub fn keep_alive(&self, master: f32) {
        let mut state = self.lock();
        state.master_target = master;
        state.touched = Some(Instant::now());
    }

    /// The loudest sample mixed since the last call, from 0 to 1 and beyond when the mix
    /// clips.
    pub fn take_peak(&self) -> f32 {
        std::mem::take(&mut self.lock().peak)
    }

    /// Fills `out` with the next frames for a device with `channels` channels at `rate`
    /// frames a second. Channels after the first two stay silent.
    pub fn mix(&self, out: &mut [f32], channels: usize, rate: u32) {
        out.fill(0.0);
        if channels == 0 || rate == 0 {
            return;
        }
        let frames = out.len() / channels;
        let mut state = self.lock();
        let state = &mut *state;
        let alive = state.touched.is_some_and(|touched| touched.elapsed() < SILENCE_AFTER);
        let master_target = if alive { state.master_target } else { 0.0 };
        let glide = 1.0 / (GAIN_GLIDE_SECONDS * rate as f32);
        state.scratch.clear();
        state.scratch.resize(frames, [0.0; 2]);
        for voice in &mut state.voices {
            mix_voice(voice, &mut state.scratch, rate, glide);
        }
        state
            .voices
            .retain(|voice| voice.cursor >= 0.0 && !(voice.stopping && voice.gains == [0.0; 2]));
        let mut peak = state.peak;
        for (frame, mixed) in out.chunks_exact_mut(channels).zip(&state.scratch) {
            state.master += (master_target - state.master).clamp(-glide, glide);
            let left = mixed[0] * state.master;
            let right = mixed[1] * state.master;
            peak = peak.max(left.abs()).max(right.abs());
            if channels == 1 {
                frame[0] = ((left + right) * 0.5).clamp(-1.0, 1.0);
            } else {
                frame[0] = left.clamp(-1.0, 1.0);
                frame[1] = right.clamp(-1.0, 1.0);
            }
        }
        state.peak = peak;
    }
}

/// Adds one voice to the mix and moves its cursor on. A sound that reaches its end
/// repeats from its loop point, or is marked finished with a negative cursor.
fn mix_voice(voice: &mut Voice, out: &mut [[f32; 2]], rate: u32, glide: f32) {
    let sound = &voice.sound;
    let frames = sound.frames();
    if frames == 0 || sound.rate == 0 {
        voice.cursor = -1.0;
        return;
    }
    let stereo = sound.channels >= 2;
    let step = f64::from(sound.rate) / f64::from(rate) * f64::from(voice.levels.speed.clamp(0.01, 16.0));
    let loop_start = sound.loop_start.map(|start| start as usize);
    let sample = |frame: usize| -> [f32; 2] {
        if stereo {
            [
                f32::from(sound.samples[frame * 2]) * SAMPLE_SCALE,
                f32::from(sound.samples[frame * 2 + 1]) * SAMPLE_SCALE,
            ]
        } else {
            let value = f32::from(sound.samples[frame]) * SAMPLE_SCALE;
            [value, value]
        }
    };
    let blend_target = voice.levels.blend.unwrap_or(0.0);
    let targets = voice.levels.speaker_gains(stereo);
    for mixed in out {
        if voice.cursor >= frames as f64 {
            match loop_start {
                Some(start) => {
                    let length = (frames - start).max(1) as f64;
                    voice.cursor = start as f64 + (voice.cursor - frames as f64) % length;
                }
                None => {
                    voice.cursor = -1.0;
                    return;
                }
            }
        }
        let index = voice.cursor as usize;
        let fraction = (voice.cursor - index as f64) as f32;
        let next = if index + 1 < frames {
            index + 1
        } else {
            loop_start.unwrap_or(index)
        };
        let [first_left, first_right] = sample(index);
        let [next_left, next_right] = sample(next);
        let mut left = first_left + (next_left - first_left) * fraction;
        let mut right = first_right + (next_right - first_right) * fraction;
        if stereo && voice.levels.blend.is_some() {
            voice.blend += (blend_target - voice.blend).clamp(-glide, glide);
            left += (right - left) * voice.blend;
            right = left;
        }
        for (gain, target) in voice.gains.iter_mut().zip(targets) {
            *gain += (target - *gain).clamp(-glide, glide);
        }
        mixed[0] += left * voice.gains[0];
        mixed[1] += right * voice.gains[1];
        voice.cursor += step;
        if voice.stopping && voice.gains == [0.0; 2] {
            return;
        }
    }
}
