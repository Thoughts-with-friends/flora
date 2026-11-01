//! Wii U audio: AX-style voices mixed to host-rate stereo.
//!
//! ```text
//!  guest sample memory ──► Voice (PCM8/PCM16/DSP-ADPCM decode, looping)
//!                              │ resample (ratio)
//!                              ▼
//!                           Mixer ──► interleaved f32 stereo for the host
//! ```
//! Voice parameters are written by the `sndcore2` HLE library; this crate only
//! does the signal processing.
pub mod adpcm;
pub mod mixer;
pub mod voice;

pub use mixer::Mixer;
pub use voice::{SampleFormat, SampleMemory, Voice};

/// Output sample rate of the mixer.
pub const OUTPUT_RATE: u32 = 48_000;
