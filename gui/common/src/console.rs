//! The emulator on its own thread, driven by commands from the UI.
//!
//! ```text
//!  UI thread ──ConsoleCommand──► emulation thread (Emulator::run_frame @ 60 Hz)
//!      ▲                                   │
//!      └──────── Shared (Mutex) ◄──────────┘ render commands, console, status
//! ```
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError,
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use wiiu_emu::{EmuStats, Emulator, GamePadState, RenderCommand, Status};

use crate::audio::AudioOutput;

/// Target frame time (59.94 Hz).
const FRAME: Duration = Duration::from_micros(16_683);
/// Rate of the samples the emulator produces.
const AUDIO_RATE: u32 = 48_000;

/// Requests from the UI.
#[derive(Debug, Clone)]
pub enum ConsoleCommand {
    Load(PathBuf),
    Pause,
    Resume,
    Stop,
    GamePad(GamePadState),
}

/// State published by the emulation thread.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub console_lines: Vec<String>,
    pub status: Option<Status>,
    pub paused: bool,
    pub title: Option<String>,
    pub stats: EmuStats,
    /// Emulated frames per second.
    pub fps: f32,
    /// Error from the last `Load`.
    pub load_error: Option<String>,
}

type Shared = Arc<Mutex<Snapshot>>;

/// Handle to the emulation thread. Dropping it stops the thread.
pub struct Console {
    tx: Option<Sender<ConsoleCommand>>,
    shared: Shared,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Console {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Console").finish_non_exhaustive()
    }
}

impl Console {
    /// Starts the emulation thread. `repaint` is called after each frame so
    /// the UI can wake up.
    ///
    /// # Errors
    /// Fails if the emulator cannot be created or the thread cannot spawn.
    pub fn spawn(
        config: wiiu_emu::Config,
        render: SyncSender<Vec<RenderCommand>>,
        repaint: impl Fn() + Send + 'static,
    ) -> Result<Self, String> {
        let emulator = Emulator::new(config).map_err(|e| e.to_string())?;
        let shared = Shared::default();
        let (tx, rx) = mpsc::channel();
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("flora-emu".into())
            .spawn(move || {
                EmuThread {
                    emulator,
                    rx,
                    render,
                    audio: AudioOutput::open(AUDIO_RATE),
                    shared: thread_shared,
                    paused: false,
                }
                .run(&repaint);
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { tx: Some(tx), shared, thread: Some(thread) })
    }

    pub fn send(&self, command: ConsoleCommand) {
        if let Some(tx) = &self.tx {
            // The thread only exits when `tx` is dropped, so this cannot fail while we hold it.
            let _ = tx.send(command);
        }
    }

    /// Takes accumulated output; status/stats are copied, lists drained.
    pub fn drain(&self) -> Snapshot {
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        Snapshot {
            console_lines: std::mem::take(&mut shared.console_lines),
            load_error: shared.load_error.take(),
            ..shared.clone()
        }
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct EmuThread {
    emulator: Emulator,
    rx: Receiver<ConsoleCommand>,
    render: SyncSender<Vec<RenderCommand>>,
    audio: AudioOutput,
    shared: Shared,
    paused: bool,
}

impl EmuThread {
    fn running(&self) -> bool {
        !self.paused && *self.emulator.status() == Status::Running
    }

    fn handle(&mut self, command: ConsoleCommand) {
        match command {
            ConsoleCommand::Load(path) => {
                self.paused = false;
                let result = self.emulator.load_title(&path);
                let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
                match result {
                    Ok(()) => {
                        shared.title = Some(self.emulator.title().map_or_else(
                            || path.display().to_string(),
                            wiiu_emu::TitleInfo::display_name,
                        ));
                    }
                    Err(error) => shared.load_error = Some(format!("{}: {error}", path.display())),
                }
            }
            ConsoleCommand::Pause => self.paused = true,
            ConsoleCommand::Resume => self.paused = false,
            ConsoleCommand::Stop => {
                self.emulator.stop();
                self.shared.lock().unwrap_or_else(PoisonError::into_inner).title = None;
            }
            ConsoleCommand::GamePad(state) => self.emulator.set_gamepad(state),
        }
    }

    fn publish(&mut self, fps: f32) {
        let commands = self.emulator.take_render_commands();
        // Waits while the renderer is behind: frames must not be dropped, they
        // carry uploads and draws later frames build on.
        if !commands.is_empty() && self.render.send(commands).is_err() {
            log::warn!("render thread stopped");
        }
        self.audio.push(&self.emulator.take_audio());
        let lines = self.emulator.take_console();
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        shared.console_lines.extend(lines);
        shared.status = Some(self.emulator.status().clone());
        shared.paused = self.paused;
        shared.stats = self.emulator.stats();
        shared.fps = fps;
    }

    fn run(mut self, repaint: &dyn Fn()) {
        let mut next_frame = Instant::now();
        let (mut fps_frames, mut fps_start, mut fps) = (0_u32, Instant::now(), 0.0_f32);
        loop {
            // Block while idle; otherwise wait at most until the next frame.
            let timeout = if self.running() {
                next_frame.saturating_duration_since(Instant::now())
            } else {
                FRAME * 4
            };
            match self.rx.recv_timeout(timeout) {
                Ok(command) => {
                    self.handle(command);
                    while let Ok(command) = self.rx.try_recv() {
                        self.handle(command);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }

            if self.running() && Instant::now() >= next_frame {
                self.emulator.run_frame();
                next_frame = (next_frame + FRAME).max(Instant::now());
                fps_frames += 1;
            }
            let elapsed = fps_start.elapsed();
            if elapsed >= Duration::from_secs(1) {
                #[allow(clippy::cast_precision_loss)]
                {
                    fps = fps_frames as f32 / elapsed.as_secs_f32();
                }
                (fps_frames, fps_start) = (0, Instant::now());
            }
            self.publish(fps);
            repaint();
        }
    }
}
