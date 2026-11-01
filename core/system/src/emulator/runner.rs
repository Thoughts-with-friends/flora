//! The frame loop: run the main core, dispatch HLE calls, stop on events.
use wiiu_cafe_os::{HleContext, coreinit::thread::TIME_SLICE, dispatch};
use wiiu_cpu::{CpuError, StepEvent, disassemble::disassemble};

use super::{Emulator, MAIN_CORE, Status, bus::CoreBus};

impl Emulator {
    /// Runs until the guest finishes a frame (vsync wait, flip, yield), exits,
    /// crashes, or the per-frame instruction budget is used up.
    pub fn run_frame(&mut self) {
        if self.status != Status::Running {
            return;
        }
        let started = std::time::Instant::now();
        let mut budget = self.config.instructions_per_frame;
        let mut slice = TIME_SLICE;

        // Alarms, pending callbacks, and threads that became runnable since the last frame.
        if let Err(error) = self.os.begin_frame(&mut self.cores[MAIN_CORE], &mut self.memory) {
            self.crash(error.to_string());
            return;
        }

        while budget > 0 && !self.os.frame_done {
            let result = {
                let mut bus = CoreBus {
                    memory: &mut self.memory,
                    timebase: self.os.timebase,
                    jit_memory: self.config.jit,
                };
                self.jit.run(&mut self.cores[MAIN_CORE], &mut bus, budget.min(slice))
            };
            let (event, executed) = match result {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.crash_on_cpu_error(&error);
                    return;
                }
            };
            self.stats.instructions += executed;
            budget = budget.saturating_sub(executed);
            slice = slice.saturating_sub(executed);

            match event {
                StepEvent::Continue => {
                    if slice == 0 {
                        // Time slice used up: let threads of equal priority run.
                        slice = TIME_SLICE;
                        if let Err(error) =
                            self.os.end_time_slice(&mut self.cores[MAIN_CORE], &mut self.memory)
                        {
                            self.crash(error.to_string());
                            return;
                        }
                    }
                }
                StepEvent::HleCall(index) => {
                    self.stats.hle_calls += 1;
                    let mut ctx = HleContext {
                        cpu: &mut self.cores[MAIN_CORE],
                        memory: &mut self.memory,
                        os: &mut self.os,
                        gpu: &mut self.gpu,
                        iosu: &mut self.iosu,
                    };
                    if let Err(error) = dispatch(&mut ctx, index) {
                        self.crash(format!("{error} (lr {:#010x})", self.cores[MAIN_CORE].lr));
                        return;
                    }
                }
                StepEvent::SystemCall => {
                    self.crash(format!(
                        "unsupported syscall at {:#010x}",
                        self.cores[MAIN_CORE].pc
                    ));
                    return;
                }
                StepEvent::Trap => {
                    self.crash(format!(
                        "trap at {:#010x}",
                        self.cores[MAIN_CORE].pc.wrapping_sub(4)
                    ));
                    return;
                }
            }

            if let Some(code) = self.os.exit_code {
                self.status = Status::Exited(code);
                break;
            }
            if self.os.frame_done || started.elapsed() >= self.config.frame_time {
                break;
            }
        }
        self.finish_gx2_frame();
        self.stats.frames += 1;
    }

    /// What happens at every vertical blank.
    fn finish_gx2_frame(&mut self) {
        self.os.vsync();
    }

    fn crash_on_cpu_error(&mut self, error: &CpuError) {
        let pc = self.cores[MAIN_CORE].pc;
        let context = self
            .memory
            .read_u32(pc)
            .map_or_else(|_| String::new(), |raw| format!(": {}", disassemble(pc, raw)));
        self.crash(format!("{error}{context}"));
    }

    fn crash(&mut self, message: String) {
        log::error!("emulation stopped: {message}");
        self.status = Status::Crashed(message);
    }
}
