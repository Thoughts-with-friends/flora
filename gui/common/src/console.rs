//! The emulator on its own thread, driven by commands from the UI.
//!
//! ```text
//!  UI thread ──ConsoleCommand──► emulation thread (Emulator::run_frame @ 60 Hz)
//!      ▲                                   │ (run_frame keeps real time)
//!      └──────── Shared (Mutex) ◄──────────┘ render commands, console, status
//! ```
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError,
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use wiiu_emu::{EmuStats, Emulator, GamePadState, GameSpeed, RenderCommand, Status};

use crate::audio::AudioOutput;

/// How often a stopped emulator looks for commands.
const IDLE_POLL: Duration = Duration::from_millis(66);
/// Rate of the samples the emulator produces.
const AUDIO_RATE: u32 = 48_000;
/// Longest time between repaints without a new picture: controllers are read when painting
/// (keys repaint by themselves). Titles flip all the time; this is for loading and pauses,
/// where drawing the window 30 times a second only kept the GPU busy.
const MAX_REPAINT_INTERVAL: Duration = Duration::from_millis(100);

/// Requests from the UI.
#[derive(Debug, Clone)]
pub enum ConsoleCommand {
    Load(PathBuf),
    Pause,
    Resume,
    Stop,
    /// Reboots the running title.
    Reset,
    /// Reads the running title's cheat codes again.
    ReloadCheats,
    GamePad(GamePadState),
    /// A Pro Controller on a `WPAD` channel; `None` unplugs it.
    ProController(usize, Option<GamePadState>),
    /// Where a copy of the sound goes (48 kHz stereo). Full means that part is dropped.
    AudioTap(Option<SyncSender<Vec<i16>>>),
    /// Where to find the system fonts from the next boot on.
    FontsDir(Option<PathBuf>),
    /// Vertical blanks per flip from now on, whatever the title asks for.
    SwapInterval(Option<u32>),
    /// How many times larger the title's pictures are drawn from now on.
    ResolutionFactor(u32),
    /// How fast the title runs from now on.
    Speed(GameSpeed),
    /// Least vertical blanks per flip from now on: never faster than this.
    MinSwapInterval(Option<u32>),
}

/// State published by the emulation thread.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub console_lines: Vec<String>,
    pub status: Option<Status>,
    pub paused: bool,
    pub title: Option<String>,
    pub stats: EmuStats,
    /// Frames the guest flips to the screen per second.
    pub fps: f32,
    /// Error from the last `Load`.
    pub load_error: Option<String>,
    /// Where the running title's cheat codes are kept.
    pub cheats_path: Option<PathBuf>,
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
            .name("shocora-emu".into())
            .spawn(move || {
                EmuThread {
                    emulator,
                    rx,
                    render,
                    audio: AudioOutput::open(AUDIO_RATE),
                    audio_tap: None,
                    shared: thread_shared,
                    paused: false,
                }
                .run(&repaint);
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { tx: Some(tx), shared, thread: Some(thread) })
    }

    /// A handle for sending commands from another thread. Keeps the emulation thread alive:
    /// drop it before the console.
    #[inline]
    pub fn commands(&self) -> Option<Sender<ConsoleCommand>> {
        self.tx.clone()
    }

    #[inline]
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
    audio_tap: Option<SyncSender<Vec<i16>>>,
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
            ConsoleCommand::Reset => {
                self.paused = false;
                if let Err(error) = self.emulator.reset() {
                    self.shared.lock().unwrap_or_else(PoisonError::into_inner).load_error =
                        Some(format!("reset: {error}"));
                }
            }
            ConsoleCommand::ReloadCheats => {
                if let Err(error) = self.emulator.reload_cheats() {
                    self.shared.lock().unwrap_or_else(PoisonError::into_inner).load_error =
                        Some(error);
                }
            }
            ConsoleCommand::GamePad(state) => self.emulator.set_gamepad(state),
            ConsoleCommand::ProController(channel, state) => {
                self.emulator.set_pro_controller(channel, state);
            }
            ConsoleCommand::AudioTap(tap) => self.audio_tap = tap,
            ConsoleCommand::FontsDir(dir) => self.emulator.config.fonts_dir = dir,
            ConsoleCommand::SwapInterval(interval) => self.emulator.set_swap_interval(interval),
            ConsoleCommand::ResolutionFactor(factor) => {
                self.emulator.set_resolution_factor(factor);
            }
            ConsoleCommand::Speed(speed) => self.emulator.set_speed(speed),
            ConsoleCommand::MinSwapInterval(interval) => {
                self.emulator.set_min_swap_interval(interval);
            }
        }
    }

    fn publish(&mut self, fps: f32) {
        // Before the renderer, which may take a while: sound must not run out meanwhile.
        let sound = self.emulator.take_audio();
        self.audio.push(&sound);
        if let Some(tap) = self.audio_tap.as_ref().filter(|_| !sound.is_empty()) {
            let _ = tap.try_send(sound);
        }
        let commands = self.emulator.take_render_commands();
        // Waits while the renderer is behind: frames must not be dropped, they
        // carry uploads and draws later frames build on.
        if !commands.is_empty() && self.render.send(commands).is_err() {
            log::warn!("render thread stopped");
        }
        let lines = self.emulator.take_console();
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        shared.console_lines.extend(lines);
        shared.status = Some(self.emulator.status().clone());
        shared.paused = self.paused;
        shared.stats = self.emulator.stats();
        shared.fps = fps;
        shared.cheats_path = self.emulator.cheats_path();
    }

    fn run(mut self, repaint: &dyn Fn()) {
        let (mut fps_flips, mut fps_start, mut fps) = (0_u64, Instant::now(), 0.0_f32);
        let (mut painted_flips, mut painted_at) = (0_u64, Instant::now());
        loop {
            // `run_frame` paces a running guest; otherwise wait for something to do.
            let command = if self.running() {
                self.rx.try_recv().map_err(|error| match error {
                    TryRecvError::Empty => RecvTimeoutError::Timeout,
                    TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
                })
            } else {
                self.rx.recv_timeout(IDLE_POLL)
            };
            match command {
                Ok(command) => {
                    self.handle(command);
                    while let Ok(command) = self.rx.try_recv() {
                        self.handle(command);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }

            if self.running() {
                self.emulator.run_frame();
            }
            let elapsed = fps_start.elapsed();
            if elapsed >= Duration::from_secs(1) {
                let flips = self.emulator.stats().gpu.flips;
                #[allow(clippy::cast_precision_loss)]
                {
                    fps = flips.saturating_sub(fps_flips) as f32 / elapsed.as_secs_f32();
                }
                (fps_flips, fps_start) = (flips, Instant::now());
            }
            self.publish(fps);
            // A title at 30 fps need not have the window and the desktop drawn 60 times a second.
            let flips = self.emulator.stats().gpu.flips;
            if flips != painted_flips || painted_at.elapsed() >= MAX_REPAINT_INTERVAL {
                (painted_flips, painted_at) = (flips, Instant::now());
                repaint();
            }
        }
    }
}
