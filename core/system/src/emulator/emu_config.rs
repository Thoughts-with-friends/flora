use std::{path::PathBuf, time::Duration};

/// Host-side emulator settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Upper bound of guest instructions executed per [`Emulator::run_frame`](crate::Emulator::run_frame).
    pub instructions_per_frame: u64,
    /// Time between vertical blanks.
    pub frame_time: Duration,
    /// Host directory holding per-title save data (`<dir>/<title id>/`).
    pub save_dir: PathBuf,
    /// Translate guest code; off means the (slow, precise-fault) interpreter only.
    pub jit: bool,
    /// Measure host time per HLE function (`SHOCORA_PROFILE`).
    pub profile: bool,
    /// Keep real time: [Emulator::run_frame](crate::Emulator::run_frame) returns at the next
    /// vertical blank. Off, it returns as soon as no guest thread can run.
    pub paced: bool,
    /// Directory holding the system fonts (CafeStd.ttf, ...), if not found automatically.
    pub fonts_dir: Option<PathBuf>,
    /// Vertical blanks per flip, whatever the title asks for. A title whose logic
    /// counts frames runs faster with less than it was made for.
    pub swap_interval: Option<u32>,
    /// Host directory holding per-title cheat codes (`<dir>/<title id>.txt`).
    pub cheats_dir: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            instructions_per_frame: 400_000_000,
            frame_time: Duration::from_micros(16_683),
            save_dir: PathBuf::from("save"),
            jit: true,
            profile: std::env::var_os("SHOCORA_PROFILE").is_some(),
            paced: true,
            fonts_dir: None,
            swap_interval: None,
            cheats_dir: PathBuf::from("cheats"),
        }
    }
}
