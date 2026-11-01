use std::{path::PathBuf, time::Duration};

/// Host-side emulator settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Upper bound of guest instructions executed per [`Emulator::run_frame`](crate::Emulator::run_frame).
    pub instructions_per_frame: u64,
    /// Wall-clock length of a frame: at most this much guest code runs between vertical blanks.
    pub frame_time: Duration,
    /// Host directory holding per-title save data (`<dir>/<title id>/`).
    pub save_dir: PathBuf,
    /// Translate guest code; off means the (slow, precise-fault) interpreter only.
    pub jit: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            instructions_per_frame: 400_000_000,
            frame_time: Duration::from_micros(16_683),
            save_dir: PathBuf::from("save"),
            jit: true,
        }
    }
}
