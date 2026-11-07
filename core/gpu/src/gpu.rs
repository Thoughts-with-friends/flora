use wiiu_interrupt::PendingInterrupts;
use wiiu_memory::Memory;

use crate::{
    GpuError, RenderCommand, Screen,
    command::{CommandProcessor, executor::CpContext},
    display::Display,
    latte::Latte,
};

/// Counters exposed to the front end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GpuStats {
    pub packets: u64,
    pub draws: u64,
    pub flips: u64,
    /// Draws that could not be issued (no shaders, geometry shaders, ...).
    pub skipped_draws: u64,
}

/// Latte: owns the command processor and display controller.
#[derive(Debug, Default)]
pub struct Gpu {
    cp: CommandProcessor,
    display: Display,
    latte: Latte,
    interrupts: PendingInterrupts,
    timestamp: u64,
}

impl Gpu {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Executes a PM4 command buffer.
    ///
    /// # Errors
    /// Fails on malformed packets or accesses outside guest memory.
    pub fn submit(&mut self, memory: &mut Memory, words: &[u32]) -> Result<(), GpuError> {
        self.timestamp += 1;
        let mut ctx = CpContext {
            memory,
            display: &mut self.display,
            interrupts: &mut self.interrupts,
            latte: &mut self.latte,
            timestamp: self.timestamp,
        };
        self.cp.execute(words, &mut ctx)
    }

    /// Draws the title's pictures this many times larger, as far as that stays within
    /// [`ResolutionScale::MAX_WIDTH`](crate::render::ResolutionScale::MAX_WIDTH).
    #[inline]
    pub fn set_resolution_factor(&mut self, factor: u32) {
        self.latte.set_resolution_factor(factor);
    }

    /// Makes the host renderer forget everything of the previous boot.
    #[inline]
    pub fn reset_renderer(&mut self) {
        self.latte.commands.push(RenderCommand::Reset);
    }

    /// Blanks both screens (boot / shutdown).
    pub fn blank_screens(&mut self) {
        for screen in Screen::ALL {
            Display::clear(&mut self.latte, screen, [0.0, 0.0, 0.0, 1.0]);
        }
    }

    /// Drains pending render commands for the host renderer.
    #[inline]
    pub fn take_render_commands(&mut self) -> Vec<RenderCommand> {
        std::mem::take(&mut self.latte.commands)
    }

    /// Render commands waiting for the host, to keep the queue bounded.
    #[inline]
    pub const fn pending_commands(&self) -> usize {
        self.latte.commands.len()
    }

    /// Drains interrupts raised since the last call.
    #[inline]
    pub fn take_interrupts(&mut self) -> PendingInterrupts {
        std::mem::take(&mut self.interrupts)
    }

    #[inline]
    pub const fn registers(&self) -> &crate::regs::RegisterFile {
        &self.cp.regs
    }

    pub const fn stats(&self) -> GpuStats {
        GpuStats {
            packets: self.cp.stats.packets,
            draws: self.cp.stats.draws,
            flips: self.display.flips(),
            skipped_draws: self.latte.skipped_draws,
        }
    }
}
