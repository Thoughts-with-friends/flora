//! The [`Emulator`] aggregate and its public API.
mod bus;
pub mod emu_config;
mod load;
mod runner;

use wiiu_cafe_os::CafeOs;
use wiiu_cpu::{CpuState, Jit};
use wiiu_gpu::{Gpu, GpuStats, RenderCommand};
use wiiu_iosu::Iosu;
use wiiu_memory::Memory;
use wiiu_peripherals::GamePadState;
use wiiu_title::TitleInfo;

use self::emu_config::Config;
use crate::EmuError;

/// Number of Espresso cores.
pub const CORE_COUNT: usize = 3;

/// The core running the main thread.
pub const MAIN_CORE: usize = 1;

/// Lifecycle of the emulated process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Nothing loaded.
    Idle,
    Running,
    /// The process exited with this status.
    Exited(i32),
    /// Emulation stopped on an error.
    Crashed(String),
}

/// Counters for the front end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmuStats {
    pub frames: u64,
    pub instructions: u64,
    pub hle_calls: u64,
    /// Host time in translated guest code, when profiling.
    pub guest_nanos: u64,
    /// Host time in HLE functions, when profiling.
    pub hle_nanos: u64,
    /// Host time translating guest code.
    pub compile_nanos: u64,
    /// Blocks of guest code translated.
    pub compiled_blocks: u64,
    pub gpu: GpuStats,
}

/// A Wii U.
pub struct Emulator {
    pub config: Config,
    memory: Memory,
    cores: Vec<CpuState>,
    jit: Jit,
    os: CafeOs,
    iosu: Iosu,
    gpu: Gpu,
    title: Option<TitleInfo>,
    status: Status,
    stats: EmuStats,
    /// When the next vertical blank is due.
    next_vsync: Option<std::time::Instant>,
}

impl std::fmt::Debug for Emulator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Emulator")
            .field("status", &self.status)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Emulator {
    /// Creates a powered-on console with nothing loaded.
    ///
    /// # Errors
    /// Fails if the guest address space cannot be mapped.
    pub fn new(config: Config) -> Result<Self, EmuError> {
        Ok(Self {
            config,
            memory: Memory::with_game_layout()?,
            cores: (0..CORE_COUNT as u32).map(CpuState::new).collect(),
            jit: Jit::new(),
            os: CafeOs::new(),
            iosu: Iosu::default(),
            gpu: Gpu::new(),
            title: None,
            status: Status::Idle,
            stats: EmuStats::default(),
            next_vsync: None,
        })
    }

    pub const fn status(&self) -> &Status {
        &self.status
    }

    pub const fn title(&self) -> Option<&TitleInfo> {
        self.title.as_ref()
    }

    pub fn stats(&self) -> EmuStats {
        EmuStats {
            gpu: self.gpu.stats(),
            compile_nanos: self.jit.compile_nanos(),
            compiled_blocks: self.jit.compiled_blocks(),
            ..self.stats
        }
    }

    /// Register state of core `index` (for the debugger).
    pub fn core(&self, index: usize) -> Option<&CpuState> {
        self.cores.get(index)
    }

    /// Feeds the latest GamePad sample.
    pub const fn set_gamepad(&mut self, state: GamePadState) {
        self.os.gamepad = state;
    }

    /// Drains render commands produced since the last call.
    pub fn take_render_commands(&mut self) -> Vec<RenderCommand> {
        self.gpu.take_render_commands()
    }

    /// Disassembly of `count` instructions starting `before` instructions ahead of `address`.
    pub fn disassemble_around(&self, address: u32, before: u32, count: u32) -> Vec<String> {
        (0..count)
            .map(|i| address.wrapping_sub(4 * before).wrapping_add(4 * i))
            .map(|at| match self.memory.read_u32(at) {
                Ok(raw) => format!(
                    "{at:#010x}: {raw:08x}  {}",
                    wiiu_cpu::disassemble::disassemble(at, raw)
                ),
                Err(_) => format!("{at:#010x}: <unmapped>"),
            })
            .collect()
    }

    /// Reads a guest word (debugging).
    pub fn peek_u32(&self, address: u32) -> Option<u32> {
        self.memory.read_u32(address).ok()
    }

    /// One line per guest thread (debugging).
    pub fn thread_report(&self) -> Vec<String> {
        self.os.coreinit.threads.report()
    }

    /// The most recent HLE calls (for crash reports).
    pub fn hle_trace(&self) -> Vec<String> {
        self.os.registry.recent_calls()
    }

    /// The most frequently called HLE functions (for profiling).
    pub fn hle_profile(&self, limit: usize) -> Vec<(String, u64)> {
        self.os.registry.hottest(limit)
    }

    /// The HLE functions that took longest, when profiling.
    pub fn hle_slowest(&self, limit: usize) -> Vec<(String, u64, u64)> {
        self.os.registry.slowest(limit)
    }

    /// Drains mixed audio: interleaved stereo at [`wiiu_cafe_os::sndcore2::OUTPUT_RATE`].
    pub fn take_audio(&mut self) -> Vec<i16> {
        self.os.ax.output.drain(..).collect()
    }

    /// Drains guest console output (`OSReport`, ...).
    pub fn take_console(&mut self) -> Vec<String> {
        std::mem::take(&mut self.os.console)
    }

    /// Stops emulation and frees the process.
    pub fn stop(&mut self) {
        self.gpu.blank_screens();
        self.status = Status::Idle;
        self.title = None;
    }
}
