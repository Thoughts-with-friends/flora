//! One playing voice.
use crate::adpcm::{self, AdpcmState, NIBBLES_PER_FRAME};

/// Read access to guest sample data.
pub trait SampleMemory {
    fn read_u8(&self, address: u32) -> u8;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SampleFormat {
    Pcm8,
    Pcm16,
    Adpcm,
}

/// Voice parameters and playback state.
///
/// Offsets are in format units: samples for PCM, nibbles for ADPCM (as AX
/// stores them).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Voice {
    pub format: SampleFormat,
    pub base_address: u32,
    pub loop_offset: u32,
    pub end_offset: u32,
    pub current_offset: u32,
    pub looping: bool,
    pub playing: bool,
    /// Source samples advanced per output sample.
    pub ratio: f32,
    pub volume_left: f32,
    pub volume_right: f32,
    pub adpcm_coefficients: [i16; 16],
    pub adpcm_state: AdpcmState,
    /// Fractional part of the resampling position.
    phase: f32,
}

impl Voice {
    pub fn new(format: SampleFormat, base_address: u32, end_offset: u32) -> Self {
        Self {
            format,
            base_address,
            loop_offset: 0,
            end_offset,
            current_offset: if format == SampleFormat::Adpcm { 2 } else { 0 },
            looping: false,
            playing: true,
            ratio: 1.0,
            volume_left: 1.0,
            volume_right: 1.0,
            adpcm_coefficients: [0; 16],
            adpcm_state: AdpcmState::default(),
            phase: 0.0,
        }
    }

    /// Decodes the sample at the current offset and advances by one source sample.
    #[allow(clippy::cast_possible_wrap)]
    fn next_source_sample(&mut self, memory: &dyn SampleMemory) -> i16 {
        let sample = match self.format {
            SampleFormat::Pcm8 => {
                i16::from(memory.read_u8(self.base_address + self.current_offset) as i8) << 8
            }
            SampleFormat::Pcm16 => {
                let at = self.base_address + self.current_offset * 2;
                i16::from_be_bytes([memory.read_u8(at), memory.read_u8(at + 1)])
            }
            SampleFormat::Adpcm => {
                let nibble_offset = self.current_offset;
                let frame_start = self.base_address + nibble_offset / NIBBLES_PER_FRAME * 8;
                let header = memory.read_u8(frame_start);
                let byte = memory.read_u8(self.base_address + nibble_offset / 2);
                let nibble = if nibble_offset.is_multiple_of(2) { byte >> 4 } else { byte & 0xF };
                adpcm::decode_sample(
                    nibble,
                    header,
                    &self.adpcm_coefficients,
                    &mut self.adpcm_state,
                )
            }
        };
        self.advance();
        sample
    }

    fn advance(&mut self) {
        if self.current_offset >= self.end_offset {
            if self.looping {
                self.current_offset = self.loop_offset;
            } else {
                self.playing = false;
            }
            return;
        }
        self.current_offset += 1;
        // Skip ADPCM frame headers.
        if self.format == SampleFormat::Adpcm
            && self.current_offset.is_multiple_of(NIBBLES_PER_FRAME)
        {
            self.current_offset += 2;
        }
    }

    /// Produces one output sample (mono, `-1.0..=1.0`) at the voice's ratio.
    pub fn render(&mut self, memory: &dyn SampleMemory) -> f32 {
        if !self.playing {
            return 0.0;
        }
        let mut last = 0;
        self.phase += self.ratio;
        while self.phase >= 1.0 && self.playing {
            last = self.next_source_sample(memory);
            self.phase -= 1.0;
        }
        f32::from(last) / 32768.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Bytes(Vec<u8>);

    impl SampleMemory for Bytes {
        fn read_u8(&self, address: u32) -> u8 {
            self.0.get(address as usize).copied().unwrap_or(0)
        }
    }

    #[test]
    fn pcm16_one_shot_and_loop() {
        let memory = Bytes(vec![0x40, 0x00, 0xC0, 0x00]);
        let mut voice = Voice::new(SampleFormat::Pcm16, 0, 1);
        assert!((voice.render(&memory) - 0.5).abs() < 1e-6);
        assert!((voice.render(&memory) + 0.5).abs() < 1e-6);
        voice.render(&memory);
        assert!(!voice.playing);

        let mut looping = Voice::new(SampleFormat::Pcm16, 0, 1);
        looping.looping = true;
        let samples: Vec<f32> = (0..4).map(|_| looping.render(&memory)).collect();
        assert!((samples[2] - 0.5).abs() < 1e-6 && looping.playing);
    }

    #[test]
    fn half_ratio_repeats_samples() {
        let memory = Bytes(vec![0x7F, 0x80]);
        let mut voice = Voice::new(SampleFormat::Pcm8, 0, 1);
        voice.ratio = 0.5;
        let out: Vec<f32> = (0..4).map(|_| voice.render(&memory)).collect();
        assert_eq!(out[0], 0.0);
        assert!(out[1] > 0.9 && out[3] < -0.9);
    }
}
