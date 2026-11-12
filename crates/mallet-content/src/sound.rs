//! Sound files as the engine plays them: WAV with 8 to 32 bit samples or Microsoft
//! ADPCM, and MP3. A WAV's loop point is read from its `cue ` or `smpl` chunk, as the
//! engine reads it: a file with one repeats from that sample to its end, and a file
//! without one plays once however it is started.

use std::io::Cursor;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as DecodeError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::GameFs;

/// `WAVE_FORMAT_PCM`, `WAVE_FORMAT_ADPCM`, `WAVE_FORMAT_IEEE_FLOAT` and
/// `WAVE_FORMAT_EXTENSIBLE`.
const FORMAT_PCM: u16 = 1;
const FORMAT_ADPCM: u16 = 2;
const FORMAT_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// How Microsoft ADPCM scales its step after each sample, by the sample's four bits.
const ADPCM_ADAPTATION: [i32; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230,
];
/// The predictor coefficient pairs every Microsoft ADPCM file may use.
const ADPCM_COEFFICIENTS: [(i32, i32); 7] = [
    (256, 0),
    (512, -256),
    (0, 0),
    (192, 64),
    (240, 0),
    (460, -208),
    (392, -232),
];

/// A decoded sound.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundData {
    /// Frames per second.
    pub rate: u32,
    /// One for mono, two for stereo.
    pub channels: u16,
    /// Samples, left then right for stereo.
    pub samples: Vec<i16>,
    /// The frame the sound repeats from when it reaches its end; `None` for a sound that
    /// plays once.
    pub loop_start: Option<u32>,
}

impl SoundData {
    /// How many frames the sound has: samples for mono, pairs of them for stereo.
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }

    /// How long the sound plays at its own pitch, in seconds.
    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / f64::from(self.rate.max(1))
    }
}

/// Reads the sound `file` under the game's `sound/` folders, such as
/// `ambient/atmosphere/town_ambience.wav`.
pub fn load_sound(filesystem: &GameFs, file: &str) -> Result<SoundData, String> {
    let path = format!("sound/{}", file.trim_start_matches(['/', '\\']));
    let bytes = filesystem
        .read(&path)
        .ok_or_else(|| format!("{path} is not among the game's files"))?;
    decode_sound(&bytes).map_err(|error| format!("{path}: {error}"))
}

/// Decodes a WAV or MP3 file.
pub fn decode_sound(bytes: &[u8]) -> Result<SoundData, String> {
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        decode_wav(bytes)
    } else {
        decode_mp3(bytes)
    }
}

/// What a WAV's `fmt ` chunk says.
struct WavFormat {
    tag: u16,
    channels: usize,
    rate: u32,
    block_align: usize,
    bits: u16,
    /// Frames in each ADPCM block.
    samples_per_block: usize,
    coefficients: Vec<(i32, i32)>,
}

fn decode_wav(bytes: &[u8]) -> Result<SoundData, String> {
    let mut format = None;
    let mut data = None;
    let mut loop_start = None;
    let mut fact_frames = None;
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = read_u32(bytes, at + 4) as usize;
        let body = &bytes[at + 8..(at + 8).saturating_add(size).min(bytes.len())];
        match id {
            b"fmt " => format = Some(wav_format(body)?),
            b"data" => data = Some(body),
            b"fact" if body.len() >= 4 => fact_frames = Some(read_u32(body, 0) as usize),
            b"cue " if body.len() >= 28 && read_u32(body, 0) > 0 => loop_start = Some(read_u32(body, 24)),
            b"smpl" if body.len() >= 60 && read_u32(body, 28) > 0 => {
                loop_start = loop_start.or(Some(read_u32(body, 44)));
            }
            _ => {}
        }
        at = at.saturating_add(8).saturating_add(size).saturating_add(size & 1);
    }
    let format = format.ok_or("the file has no format chunk")?;
    let data = data.ok_or("the file has no sound data")?;
    if format.channels == 0 || format.rate == 0 {
        return Err("the file has no channels or no sample rate".to_owned());
    }
    let mut samples = match format.tag {
        FORMAT_PCM | FORMAT_FLOAT => pcm_samples(data, &format)?,
        FORMAT_ADPCM => adpcm_samples(data, &format, fact_frames)?,
        other => return Err(format!("the WAV format {other} is not one the engine plays")),
    };
    let channels = if format.channels > 2 {
        samples = samples
            .chunks_exact(format.channels)
            .flat_map(|frame| [frame[0], frame[1]])
            .collect();
        2
    } else {
        format.channels as u16
    };
    let frames = samples.len() / usize::from(channels);
    Ok(SoundData {
        rate: format.rate,
        channels,
        samples,
        loop_start: loop_start.filter(|start| (*start as usize) < frames),
    })
}

fn wav_format(body: &[u8]) -> Result<WavFormat, String> {
    if body.len() < 16 {
        return Err("the format chunk is cut short".to_owned());
    }
    let mut tag = read_u16(body, 0);
    if tag == FORMAT_EXTENSIBLE && body.len() >= 26 {
        tag = read_u16(body, 24);
    }
    let mut format = WavFormat {
        tag,
        channels: usize::from(read_u16(body, 2)),
        rate: read_u32(body, 4),
        block_align: usize::from(read_u16(body, 12)),
        bits: read_u16(body, 14),
        samples_per_block: 0,
        coefficients: ADPCM_COEFFICIENTS.to_vec(),
    };
    if tag == FORMAT_ADPCM && body.len() >= 22 {
        format.samples_per_block = usize::from(read_u16(body, 18));
        let count = usize::from(read_u16(body, 20));
        if count > 0 && body.len() >= 22 + count * 4 {
            format.coefficients = (0..count)
                .map(|index| {
                    let at = 22 + index * 4;
                    (i32::from(read_i16(body, at)), i32::from(read_i16(body, at + 2)))
                })
                .collect();
        }
    }
    Ok(format)
}

fn pcm_samples(data: &[u8], format: &WavFormat) -> Result<Vec<i16>, String> {
    let float = format.tag == FORMAT_FLOAT;
    Ok(match (format.bits, float) {
        (8, false) => data.iter().map(|&byte| (i16::from(byte) - 128) << 8).collect(),
        (16, false) => data.as_chunks::<2>().0.iter().map(|bytes| read_i16(bytes, 0)).collect(),
        (24, false) => data.as_chunks::<3>().0.iter().map(|bytes| read_i16(bytes, 1)).collect(),
        (32, false) => data.as_chunks::<4>().0.iter().map(|bytes| read_i16(bytes, 2)).collect(),
        (32, true) => data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| {
                let value = f32::from_le_bytes(*bytes);
                (value.clamp(-1.0, 1.0) * 32767.0) as i16
            })
            .collect(),
        (bits, _) => return Err(format!("{bits} bit samples are not supported")),
    })
}

/// Decodes Microsoft ADPCM: each block starts every channel with a predictor, a step
/// and two whole samples, and four bits for each sample after them.
fn adpcm_samples(data: &[u8], format: &WavFormat, fact_frames: Option<usize>) -> Result<Vec<i16>, String> {
    let channels = format.channels;
    let header = 7 * channels;
    if channels > 2 || format.block_align <= header {
        return Err("the ADPCM block layout is not one the engine plays".to_owned());
    }
    let mut samples = Vec::new();
    for block in data.chunks(format.block_align) {
        if block.len() < header {
            break;
        }
        let mut coefficients = [(0, 0); 2];
        let mut step = [0i32; 2];
        let mut newer = [0i32; 2];
        let mut older = [0i32; 2];
        for channel in 0..channels {
            let predictor = usize::from(block[channel]).min(format.coefficients.len().saturating_sub(1));
            coefficients[channel] = format.coefficients.get(predictor).copied().unwrap_or((256, 0));
            step[channel] = i32::from(read_i16(block, channels + channel * 2));
            newer[channel] = i32::from(read_i16(block, channels * 3 + channel * 2));
            older[channel] = i32::from(read_i16(block, channels * 5 + channel * 2));
        }
        let block_start = samples.len();
        samples.extend(older[..channels].iter().map(|&sample| sample as i16));
        samples.extend(newer[..channels].iter().map(|&sample| sample as i16));
        let mut channel = 0;
        for &byte in &block[header..] {
            for nibble in [byte >> 4, byte & 0x0F] {
                let signed = if nibble >= 8 {
                    i32::from(nibble) - 16
                } else {
                    i32::from(nibble)
                };
                let (first, second) = coefficients[channel];
                let predicted = (newer[channel] * first + older[channel] * second) >> 8;
                let sample = (predicted + signed * step[channel]).clamp(-32768, 32767);
                older[channel] = newer[channel];
                newer[channel] = sample;
                step[channel] = ((ADPCM_ADAPTATION[usize::from(nibble)] * step[channel]) >> 8).max(16);
                samples.push(sample as i16);
                channel = (channel + 1) % channels;
            }
        }
        if format.samples_per_block > 0 {
            samples.truncate(block_start + format.samples_per_block * channels);
        }
    }
    if let Some(frames) = fact_frames {
        samples.truncate(frames * channels);
    }
    samples.truncate(samples.len() / channels * channels);
    Ok(samples)
}

fn decode_mp3(bytes: &[u8]) -> Result<SoundData, String> {
    let stream = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let mut reader = symphonia::default::get_probe()
        .probe(&hint, stream, FormatOptions::default(), MetadataOptions::default())
        .map_err(|error| format!("not a WAV or MP3 file the engine plays ({error})"))?;
    let track = reader
        .default_track(TrackType::Audio)
        .ok_or("the file has no sound in it")?;
    let track_id = track.id;
    let parameters = track
        .codec_params
        .as_ref()
        .and_then(|parameters| parameters.audio())
        .ok_or("the file has no sound in it")?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(parameters, &AudioDecoderOptions::default())
        .map_err(|error| error.to_string())?;
    let mut rate = 0;
    let mut channels = 0;
    let mut samples: Vec<i16> = Vec::new();
    let mut packet_samples: Vec<f32> = Vec::new();
    while let Ok(Some(packet)) = reader.next_packet() {
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buffer) => {
                let spec = buffer.spec();
                let count = spec.channels().count();
                if count == 0 {
                    continue;
                }
                rate = spec.rate();
                channels = count.min(2);
                packet_samples.resize(buffer.samples_interleaved(), 0.0);
                buffer.copy_to_slice_interleaved(&mut packet_samples);
                for frame in packet_samples.chunks_exact(count) {
                    samples.extend(
                        frame[..channels]
                            .iter()
                            .map(|&value| (value.clamp(-1.0, 1.0) * 32767.0) as i16),
                    );
                }
            }
            Err(DecodeError::DecodeError(_)) => {}
            Err(_) => break,
        }
    }
    if samples.is_empty() || rate == 0 {
        return Err("the file has no sound in it".to_owned());
    }
    Ok(SoundData {
        rate,
        channels: channels as u16,
        samples,
        loop_start: None,
    })
}

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn read_i16(bytes: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
