//! Host audio output: plays what the AX mixer produced.
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, PoisonError},
};

use cpal::{
    SampleFormat,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};

/// Interleaved stereo samples queued before the oldest are dropped (about 150 ms).
const MAX_QUEUED: usize = 2 * 7200;

type Queue = Arc<Mutex<VecDeque<i16>>>;

/// The open output stream. Without an audio device the samples are discarded.
pub struct AudioOutput {
    queue: Queue,
    _stream: Option<cpal::Stream>,
}

impl std::fmt::Debug for AudioOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioOutput").finish_non_exhaustive()
    }
}

impl AudioOutput {
    /// Opens the default output device.
    pub fn open(input_rate: u32) -> Self {
        let queue = Queue::default();
        let stream = Self::open_stream(input_rate, &queue);
        if let Err(error) = &stream {
            log::warn!("no audio output: {error}");
        }
        Self { queue, _stream: stream.ok() }
    }

    fn open_stream(
        input_rate: u32,
        queue: &Queue,
    ) -> Result<cpal::Stream, Box<dyn std::error::Error>> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no default output device")?;
        let supported = device.default_output_config()?;
        let config = supported.config();
        let channels = usize::from(config.channels);
        let step = input_rate as f32 / config.sample_rate as f32;
        let on_error = |error| log::warn!("audio stream: {error}");
        let stream = match supported.sample_format() {
            SampleFormat::I16 => {
                let (queue, mut phase, mut last) = (Arc::clone(queue), 0.0, [0, 0]);
                device.build_output_stream(
                    &config,
                    move |data: &mut [i16], _: &_| {
                        let mut guard = queue.lock().unwrap_or_else(PoisonError::into_inner);
                        for frame in data.chunks_mut(channels) {
                            advance(&mut guard, step, &mut phase, &mut last);
                            for (channel, slot) in frame.iter_mut().enumerate() {
                                *slot = last[channel.min(1)];
                            }
                        }
                    },
                    on_error,
                    None,
                )?
            }
            _ => {
                let (queue, mut phase, mut last) = (Arc::clone(queue), 0.0, [0, 0]);
                device.build_output_stream(
                    &config,
                    move |data: &mut [f32], _: &_| {
                        let mut guard = queue.lock().unwrap_or_else(PoisonError::into_inner);
                        for frame in data.chunks_mut(channels) {
                            advance(&mut guard, step, &mut phase, &mut last);
                            for (channel, slot) in frame.iter_mut().enumerate() {
                                *slot = f32::from(last[channel.min(1)]) / 32768.0;
                            }
                        }
                    },
                    on_error,
                    None,
                )?
            }
        };
        stream.play()?;
        Ok(stream)
    }

    /// Appends interleaved stereo samples.
    pub fn push(&self, samples: &[i16]) {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        queue.extend(samples.iter().copied());
        let excess = queue.len().saturating_sub(MAX_QUEUED);
        queue.drain(..excess);
    }
}

fn advance(samples: &mut VecDeque<i16>, step: f32, phase: &mut f32, last: &mut [i16; 2]) {
    *phase += step;
    while *phase >= 1.0 {
        *phase -= 1.0;
        if samples.len() >= 2 {
            last[0] = samples.pop_front().unwrap_or(0);
            last[1] = samples.pop_front().unwrap_or(0);
        }
    }
}
