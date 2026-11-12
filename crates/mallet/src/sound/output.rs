//! Plays a [`Mixer`] on the computer's default sound output.
//!
//! The device belongs to a thread of its own, since a stream may not move between
//! threads. On Windows and macOS it is opened with `cpal`. On Linux the mix is piped to
//! the first of `pw-cat`, `paplay` and `aplay` that starts, so Mallet links no sound
//! library there and still runs where none is installed.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use super::mixer::Mixer;

/// How long opening the device may take before the preview gives up on it.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// An open sound device playing a mixer. Dropping it closes the device.
pub struct Output {
    /// The device's name, for the status display.
    pub device: String,
    stop: Sender<()>,
    failure: Arc<Mutex<Option<String>>>,
}

impl Output {
    /// Opens the default output device and starts playing `mixer` on it.
    pub fn open(mixer: Mixer) -> Result<Self, String> {
        let (stop, stopped) = mpsc::channel();
        let (opened, open_result) = mpsc::channel();
        let failure = Arc::new(Mutex::new(None));
        let thread_failure = Arc::clone(&failure);
        thread::Builder::new()
            .name("sound output".to_owned())
            .spawn(move || play(mixer, &opened, &stopped, &thread_failure))
            .map_err(|error| format!("could not start the sound thread: {error}"))?;
        let device = open_result
            .recv_timeout(OPEN_TIMEOUT)
            .map_err(|_| "the sound device did not answer".to_owned())??;
        Ok(Self { device, stop, failure })
    }

    /// Why the device stopped playing, if it did, such as headphones being unplugged.
    pub fn failure(&self) -> Option<String> {
        self.failure.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.stop.send(());
    }
}

#[cfg(not(target_os = "linux"))]
fn play(
    mixer: Mixer,
    opened: &Sender<Result<String, String>>,
    stopped: &Receiver<()>,
    failure: &Arc<Mutex<Option<String>>>,
) {
    use cpal::traits::StreamTrait;

    match device_stream(mixer, failure) {
        Ok((stream, device)) => match stream.play() {
            Ok(()) => {
                let _ = opened.send(Ok(device));
                let _ = stopped.recv();
            }
            Err(error) => {
                let _ = opened.send(Err(format!("{device}: {error}")));
            }
        },
        Err(error) => {
            let _ = opened.send(Err(error));
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn device_stream(mixer: Mixer, failure: &Arc<Mutex<Option<String>>>) -> Result<(cpal::Stream, String), String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    use cpal::SampleFormat;

    let device = cpal::default_host()
        .default_output_device()
        .ok_or("no sound output device was found")?;
    let name = device.description().map_or_else(
        |_| "Sound output".to_owned(),
        |description| description.name().to_owned(),
    );
    let supported = device
        .default_output_config()
        .map_err(|error| format!("{name}: {error}"))?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let stream = match format {
        SampleFormat::F32 => typed_stream::<f32>(&device, config, mixer, failure),
        SampleFormat::F64 => typed_stream::<f64>(&device, config, mixer, failure),
        SampleFormat::I8 => typed_stream::<i8>(&device, config, mixer, failure),
        SampleFormat::I16 => typed_stream::<i16>(&device, config, mixer, failure),
        SampleFormat::I32 => typed_stream::<i32>(&device, config, mixer, failure),
        SampleFormat::U8 => typed_stream::<u8>(&device, config, mixer, failure),
        SampleFormat::U16 => typed_stream::<u16>(&device, config, mixer, failure),
        SampleFormat::U32 => typed_stream::<u32>(&device, config, mixer, failure),
        other => return Err(format!("{name}: the sample format {other} is not supported")),
    };
    stream
        .map(|stream| (stream, name.clone()))
        .map_err(|error| format!("{name}: {error}"))
}

#[cfg(not(target_os = "linux"))]
fn typed_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mixer: Mixer,
    failure: &Arc<Mutex<Option<String>>>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    use cpal::traits::DeviceTrait;
    use cpal::ErrorKind;

    let channels = usize::from(config.channels);
    let rate = config.sample_rate;
    let failure = Arc::clone(failure);
    let mut mixed: Vec<f32> = Vec::new();
    device.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            mixed.resize(data.len(), 0.0);
            mixer.mix(&mut mixed, channels, rate);
            for (sample, value) in data.iter_mut().zip(&mixed) {
                *sample = T::from_sample(*value);
            }
        },
        move |error: cpal::Error| {
            if !matches!(error.kind(), ErrorKind::Xrun | ErrorKind::RealtimeDenied) {
                *failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(error.to_string());
            }
        },
        None,
    )
}

/// Frames a second and channels of the mix piped to a player program.
#[cfg(target_os = "linux")]
const PIPE_RATE: u32 = 44100;
#[cfg(target_os = "linux")]
const PIPE_CHANNELS: usize = 2;
/// How much sound is written at a time, and how far ahead of the clock writing may run.
#[cfg(target_os = "linux")]
const PIPE_BLOCK: Duration = Duration::from_millis(10);
#[cfg(target_os = "linux")]
const PIPE_LEAD: Duration = Duration::from_millis(60);
/// How long a player program gets to refuse its arguments before it is trusted.
#[cfg(target_os = "linux")]
const PIPE_SETTLE: Duration = Duration::from_millis(200);

#[cfg(target_os = "linux")]
fn play(
    mixer: Mixer,
    opened: &Sender<Result<String, String>>,
    stopped: &Receiver<()>,
    failure: &Arc<Mutex<Option<String>>>,
) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::sync::mpsc::TryRecvError;
    use std::time::Instant;

    let rate = PIPE_RATE.to_string();
    let players: [(&str, Vec<&str>); 3] = [
        (
            "pw-cat",
            vec![
                "--playback",
                "--format",
                "s16",
                "--rate",
                &rate,
                "--channels",
                "2",
                "--latency",
                "40ms",
                "-",
            ],
        ),
        (
            "paplay",
            vec![
                "--raw",
                "--format=s16le",
                "--rate=44100",
                "--channels=2",
                "--latency-msec=40",
            ],
        ),
        ("aplay", vec!["-q", "-t", "raw", "-f", "S16_LE", "-r", &rate, "-c", "2"]),
    ];
    let started = players.iter().find_map(|(program, arguments)| {
        let mut child = Command::new(program)
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        thread::sleep(PIPE_SETTLE);
        matches!(child.try_wait(), Ok(None)).then_some((child, *program))
    });
    let Some((mut child, program)) = started else {
        let _ = opened.send(Err("none of pw-cat, paplay and aplay could be started".to_owned()));
        return;
    };
    let Some(mut pipe) = child.stdin.take() else {
        let _ = opened.send(Err(format!("{program} takes no sound")));
        return;
    };
    let _ = opened.send(Ok(program.to_owned()));
    let block_frames = (PIPE_RATE as usize * PIPE_BLOCK.as_millis() as usize) / 1000;
    let mut mixed = vec![0.0f32; block_frames * PIPE_CHANNELS];
    let mut bytes = Vec::with_capacity(mixed.len() * 2);
    let start = Instant::now();
    let mut written = Duration::ZERO;
    loop {
        match stopped.try_recv() {
            Ok(()) | Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        let elapsed = start.elapsed();
        if written > elapsed + PIPE_LEAD {
            thread::sleep(written - elapsed - PIPE_LEAD);
            continue;
        }
        if written + PIPE_LEAD < elapsed {
            written = elapsed;
        }
        mixer.mix(&mut mixed, PIPE_CHANNELS, PIPE_RATE);
        bytes.clear();
        for value in &mixed {
            bytes.extend_from_slice(&((value * 32767.0) as i16).to_le_bytes());
        }
        if let Err(error) = pipe.write_all(&bytes) {
            *failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(format!("{program} stopped: {error}"));
            break;
        }
        written += PIPE_BLOCK;
    }
    drop(pipe);
    let _ = child.kill();
    let _ = child.wait();
}
