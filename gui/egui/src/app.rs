//! Main window: menu, the two screens, status bar and guest log.
use std::path::{Path, PathBuf};

use eframe::egui;
use shocora_game_library::{DEFAULT_PATH, GameLibrary, RomPaths, RomPathsError};
use shocora_gui_common::{
    config::AppConfig,
    console::{Console, ConsoleCommand},
    input::{Controllers, merge},
};
use shocora_netplay::{DEFAULT_PORT, HostConfig, LinkState, Refusal};
use shocora_render::{GraphicsApi, Screen, UpscaleMethod, upscale};
use wiiu_emu::{GamePadState, Status};

use crate::{
    launcher::Launcher,
    netplay::{self, Netplay},
    presenter::Presenter,
};

/// Lines of guest output kept in the log panel.
const LOG_LINES: usize = 500;

/// The choices of the frame rate setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameRate {
    Title,
    Max30,
    Force60,
}

/// The cheat editor window.
#[derive(Debug)]
struct CheatEditor {
    text: String,
    /// What is wrong with the text, or what saving it did.
    message: String,
}

pub struct ShocoraApp {
    /// Comes first, to be dropped before the console it sends commands to.
    netplay: Netplay,
    /// What went wrong when starting to host.
    netplay_error: Option<String>,
    /// Asked for each time: passwords are not written to the settings file.
    netplay_password: String,
    /// The network being hosted, for the status bar.
    netplay_network: String,
    console: Console,
    presenter: Presenter,
    config: AppConfig,
    config_path: PathBuf,
    /// API eframe is actually running with (config may hold a pending change).
    active_api: GraphicsApi,
    status: Option<Status>,
    paused: bool,
    title: Option<String>,
    fps: f32,
    instructions: u64,
    log: Vec<String>,
    show_log: bool,
    last_gamepad: GamePadState,
    controllers: Controllers,
    /// Titles found in the directories listed in `roms.json`.
    library: GameLibrary,
    library_error: Option<String>,
    show_library: bool,
    /// The start screen, shown while no title runs.
    launcher: Launcher,
    /// A boot was requested and the emulator has not answered yet.
    boot_pending: bool,
    /// Where the mouse holds the GamePad's screen, in its pixels.
    touch: Option<(u16, u16)>,
    /// The cheat editor's text, while it is open.
    cheats: Option<CheatEditor>,
    /// Where the running title's cheat codes are kept.
    cheats_path: Option<PathBuf>,
    /// What the backdrop picture's path field shows while being edited.
    picture_path: String,
}

impl ShocoraApp {
    /// # Errors
    /// Fails if no graphics context exists or the emulator cannot start.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: AppConfig,
        config_path: PathBuf,
        active_api: GraphicsApi,
        boot: Option<PathBuf>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let presenter = Presenter::new(cc)?;
        presenter.set_upscale(config.upscale);
        let ctx = cc.egui_ctx.clone();

        crate::fonts::setup_custom_fonts(&ctx, None::<std::path::PathBuf>);
        let render = presenter.sender().ok_or("the render thread is not running")?;
        let console =
            Console::spawn(config.emulator_config(), render, move || ctx.request_repaint())?;
        let mut config = config;
        if config.netplay_address.is_empty() {
            if let Some(address) = shocora_netplay::local_address() {
                config.netplay_address = address.to_string();
            }
        }
        let picture_path = config
            .launcher_image
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string());
        presenter.set_frame_generation(config.frame_generation);
        let mut app = Self {
            netplay: Netplay::Off,
            netplay_error: None,
            netplay_password: String::new(),
            netplay_network: String::new(),
            cheats: None,
            cheats_path: None,
            picture_path,
            console,
            presenter,
            config,
            config_path,
            active_api,
            status: None,
            paused: false,
            title: None,
            fps: 0.0,
            instructions: 0,
            log: Vec::new(),
            show_log: true,
            last_gamepad: GamePadState::default(),
            controllers: Controllers::new(),
            library: GameLibrary::default(),
            library_error: None,
            show_library: false,
            launcher: Launcher::default(),
            boot_pending: false,
            touch: None,
        };
        app.reload_library();
        if let Some(path) = boot {
            app.boot(path);
        }
        Ok(app)
    }

    fn boot(&mut self, path: PathBuf) {
        self.log.push(format!("loading {}", path.display()));
        self.boot_pending = true;
        self.console.send(ConsoleCommand::Load(path.clone()));
        self.config.last_title = Some(path);
        self.save_config();
    }

    /// Re-reads `roms.json` and rescans its directories.
    fn reload_library(&mut self) {
        let path = std::path::Path::new(shocora_game_library::DEFAULT_PATH);
        match RomPaths::load(path) {
            Ok(paths) => {
                self.library = GameLibrary::scan(&paths);
                self.library_error = None;
                self.log.extend(self.library.problems.iter().cloned());
                self.log.push(format!("library: {} titles", self.library.games.len()));
            }
            Err(error) => {
                self.library = GameLibrary::default();
                self.library_error = Some(error.to_string());
            }
        }
    }

    /// Asks for a directory of titles, adds it to `roms.json` and rescans.
    fn pick_rom_folder(&mut self) {
        let Some(dir) = rfd::FileDialog::new().set_title("ROM folder").pick_folder() else {
            return;
        };
        let path = Path::new(DEFAULT_PATH);
        let mut paths = match RomPaths::load(path) {
            Ok(paths) => paths,
            Err(RomPathsError::Missing(_)) => RomPaths::default(),
            Err(error) => {
                // Never overwrite a file that only failed to parse: it holds the user's list.
                self.log.push(error.to_string());
                self.library_error = Some(error.to_string());
                return;
            }
        };
        if !paths.rom_dirs.contains(&dir) {
            paths.rom_dirs.push(dir);
        }
        if let Err(error) = paths.save(path) {
            self.log.push(format!("{}: {error}", path.display()));
        }
        self.reload_library();
    }

    fn save_config(&self) {
        if let Err(error) = self.config.save(&self.config_path) {
            log::warn!("{}: {error}", self.config_path.display());
        }
    }

    fn pick_and_boot(&mut self, folder: bool) {
        let dialog = rfd::FileDialog::new();
        let picked = if folder {
            dialog.set_title("Open title folder").pick_folder()
        } else {
            dialog.set_title("Open RPX").add_filter("Wii U executable", &["rpx"]).pick_file()
        };
        if let Some(path) = picked {
            self.boot(path);
        }
    }

    fn pull_emulator_state(&mut self) {
        let snapshot = self.console.drain();
        self.presenter.sync();
        self.log.extend(snapshot.console_lines.iter().map(|l| l.trim_end().to_owned()));
        if snapshot.title.is_some() {
            self.boot_pending = false;
        }
        if let Some(error) = snapshot.load_error {
            self.log.push(error);
            self.boot_pending = false;
        }
        if let Some(Status::Crashed(message)) = &snapshot.status {
            if self.status != snapshot.status {
                self.log.push(format!("emulation stopped: {message}"));
            }
        }
        let overflow = self.log.len().saturating_sub(LOG_LINES);
        self.log.drain(..overflow);
        self.status = snapshot.status;
        self.cheats_path = snapshot.cheats_path;
        self.paused = snapshot.paused;
        self.title = snapshot.title;
        self.fps = snapshot.fps;
        self.instructions = snapshot.stats.instructions;
    }

    fn poll_input(&mut self, ctx: &egui::Context) {
        let changes = self.controllers.poll();
        self.log.extend(changes);
        let keyboard = ctx.input(|input| {
            self.config
                .key_bindings
                .gamepad(|name| egui::Key::from_name(name).is_some_and(|key| input.key_down(key)))
        });
        let mut pad = merge(keyboard, self.controllers.gamepad(&self.config.pad_bindings));
        pad.touch = self.touch;
        if pad != self.last_gamepad {
            self.last_gamepad = pad;
            // A guest's controller belongs to the host's second player.
            if let Netplay::Guest(guest) = &self.netplay {
                guest.client.set_input(pad);
            } else {
                self.console.send(ConsoleCommand::GamePad(pad));
            }
        }
    }

    fn start_hosting(&mut self) {
        self.netplay = Netplay::Off;
        let Some(commands) = self.console.commands() else { return };
        let config = match HostConfig::new(
            &self.config.netplay_address,
            DEFAULT_PORT,
            &self.netplay_password,
        ) {
            Ok(config) => config,
            Err(error) => {
                self.netplay_error = Some(error);
                return;
            }
        };
        self.netplay_network = config.network();
        let address = config.address;
        match netplay::host(config, commands, self.presenter.stream_tap()) {
            Ok(host) => {
                self.console.send(ConsoleCommand::AudioTap(Some(host.audio_sink())));
                self.netplay = Netplay::Host(host);
                self.netplay_error = None;
                self.save_config();
            }
            Err(error) if error.kind() == std::io::ErrorKind::AddrNotAvailable => {
                self.netplay_error = Some(format!("{address} はこの PC のアドレスではありません"));
            }
            Err(error) => {
                self.netplay_error = Some(format!("port {DEFAULT_PORT}: {error}"));
            }
        }
    }

    fn join(&mut self, ctx: &egui::Context) {
        self.stop_netplay();
        match netplay::join(&self.config.netplay_host, &self.netplay_password, ctx.clone()) {
            Ok(guest) => {
                self.netplay = Netplay::Guest(guest);
                self.netplay_error = None;
                self.save_config();
            }
            Err(error) => self.netplay_error = Some(error.to_string()),
        }
    }

    fn stop_netplay(&mut self) {
        if matches!(self.netplay, Netplay::Host(_)) {
            self.console.send(ConsoleCommand::AudioTap(None));
        }
        self.netplay = Netplay::Off;
    }

    fn netplay_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Netplay", |ui| match self.netplay {
            Netplay::Off => {
                ui.label("パスワード（なしでも可）");
                ui.add(egui::TextEdit::singleline(&mut self.netplay_password).password(true));
                ui.separator();
                ui.label("この PC の IP アドレス");
                ui.text_edit_singleline(&mut self.config.netplay_address).on_hover_text(
                    "同じネットワーク（192.168.1.10 なら 192.168.1.x）の相手だけが参加できます。\n\
                     広いネットワークは 10.1.2.3/16 のように指定します。",
                );
                let ready = !self.config.netplay_address.trim().is_empty();
                if ui
                    .add_enabled(ready, egui::Button::new("2P ホストを開始"))
                    .on_hover_text(
                        "パスワードそのものは送りませんが、通信は暗号化されません。\n\
                         信頼できるネットワークで使ってください。",
                    )
                    .clicked()
                {
                    ui.close();
                    self.start_hosting();
                }
                ui.separator();
                ui.label("ホストの IP アドレス");
                ui.text_edit_singleline(&mut self.config.netplay_host);
                let ready = !self.config.netplay_host.trim().is_empty();
                if ui.add_enabled(ready, egui::Button::new("参加")).clicked() {
                    ui.close();
                    self.join(ui.ctx());
                }
            }
            Netplay::Host(_) | Netplay::Guest(_) => {
                if ui.button("切断").clicked() {
                    ui.close();
                    self.stop_netplay();
                }
            }
        });
    }

    /// The guest's screen: what the host sends.
    fn guest_screen(&mut self, ui: &mut egui::Ui) {
        let Netplay::Guest(guest) = &mut self.netplay else { return };
        let Some(texture) = guest.texture(ui.ctx()) else {
            ui.centered_and_justified(|ui| ui.label("ホストの画面を待っています…"));
            return;
        };
        let size = texture.size_vec2();
        let available = ui.available_size();
        let scale = (available.x / size.x).min(available.y / size.y).max(0.01);
        ui.centered_and_justified(|ui| {
            ui.add(egui::Image::new((texture.id(), size * scale)));
        });
    }

    fn netplay_status(&self) -> Option<String> {
        let link = |state: &LinkState| match state {
            LinkState::Waiting => "待機中".to_owned(),
            LinkState::Connecting => "接続中…".to_owned(),
            LinkState::Unreachable(_) => "接続できません（再試行中）".to_owned(),
            LinkState::Refused(Refusal::WrongPassword) => "パスワードが違います".to_owned(),
            LinkState::Refused(Refusal::Network) => {
                "ホストと同じネットワークではありません".to_owned()
            }
            LinkState::Connected { peer, round_trip: Some(time) } => {
                format!("{peer} ping {} ms", time.as_millis())
            }
            LinkState::Connected { peer, round_trip: None } => peer.clone(),
        };
        match &self.netplay {
            Netplay::Off => self.netplay_error.as_ref().map(|error| format!("2P: {error}")),
            Netplay::Host(host) => Some(format!(
                "2P ホスト {} ポート {}: {}",
                self.netplay_network,
                host.port(),
                link(&host.state())
            )),
            Netplay::Guest(guest) => Some(format!("2P: {}", link(&guest.client.state()))),
        }
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open RPX…").clicked() {
                    ui.close();
                    self.pick_and_boot(false);
                }
                if ui.button("Open title folder…").clicked() {
                    ui.close();
                    self.pick_and_boot(true);
                }
                if let Some(last) = self.config.last_title.clone() {
                    if ui.button(format!("Reload {}", short_name(&last))).clicked() {
                        ui.close();
                        self.boot(last);
                    }
                }
                if ui.button("Select ROM folder…").clicked() {
                    ui.close();
                    self.pick_rom_folder();
                }
                if ui.button("Reload game library (roms.json)").clicked() {
                    ui.close();
                    self.reload_library();
                }
                ui.separator();
                if ui.button("Exit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Emulation", |ui| {
                let running = self.status == Some(Status::Running);
                let label = if self.paused { "Resume" } else { "Pause" };
                if ui.add_enabled(running, egui::Button::new(label)).clicked() {
                    self.console.send(if self.paused {
                        ConsoleCommand::Resume
                    } else {
                        ConsoleCommand::Pause
                    });
                    ui.close();
                }
                if ui.add_enabled(self.cheats_path.is_some(), egui::Button::new("Cheats…")).clicked() {
                    self.open_cheats();
                    ui.close();
                }
                if ui.add_enabled(self.title.is_some(), egui::Button::new("Reset")).clicked() {
                    self.log.push("reset".to_owned());
                    self.console.send(ConsoleCommand::Reset);
                    ui.close();
                }
                if ui.add_enabled(self.title.is_some(), egui::Button::new("Stop")).clicked() {
                    self.console.send(ConsoleCommand::Stop);
                    ui.close();
                }
            });
            self.netplay_menu(ui);
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut self.show_library, "Game library");
                ui.checkbox(&mut self.show_log, "Guest log");
                ui.separator();
                self.upscale_menu(ui);
                ui.separator();
                self.frame_rate_menu(ui);
                let mut generate = self.config.frame_generation;
                if ui
                    .checkbox(&mut generate, "Frame generation (30 → 60 fps)")
                    .on_hover_text("ゲーム速度を変えずに、フレームの間の絵を作って表示します。\nxBRZ の使用中は働きません。")
                    .changed()
                {
                    self.set_frame_generation(generate);
                }
                ui.separator();
                if ui.button("System fonts folder…").clicked() {
                    ui.close();
                    self.pick_fonts_dir();
                }
                ui.separator();
                ui.label("Graphics API (applies on restart)");
                for api in GraphicsApi::ALL {
                    if ui.radio(self.config.graphics_api == api, api.label()).clicked() {
                        self.config.graphics_api = api;
                        self.save_config();
                    }
                }
            });
        });
    }

    /// Asks for the folder with `CafeStd.ttf`; titles booted from now on use it.
    fn pick_fonts_dir(&mut self) {
        let dialog = rfd::FileDialog::new()
            .set_title("Folder with CafeStd.ttf (Cemu: resources/sharedFonts)");
        if let Some(dir) = dialog.pick_folder() {
            self.log.push(format!("system fonts: {} (applies on the next boot)", dir.display()));
            self.console.send(ConsoleCommand::FontsDir(Some(dir.clone())));
            self.config.fonts_dir = Some(dir);
            self.save_config();
        }
    }

    fn frame_rate_menu(&mut self, ui: &mut egui::Ui) {
        ui.label("Frame rate");
        let current = self.frame_rate();
        let title = ui.radio(current == FrameRate::Title, "As the title asks");
        let capped = ui.radio(current == FrameRate::Max30, "Max 30 fps").on_hover_text(
            "ゲームの処理がフレーム数で進むタイトルが速すぎるときに使います。\n\
             30 fps より遅いタイトルはそのままです。",
        );
        let forced = ui.radio(current == FrameRate::Force60, "Force 60 fps").on_hover_text(
            "A title made for 30 fps whose logic counts frames, such as Twilight Princess HD, \
             then runs at up to double speed.",
        );
        let chosen = if title.clicked() {
            FrameRate::Title
        } else if capped.clicked() {
            FrameRate::Max30
        } else if forced.clicked() {
            FrameRate::Force60
        } else {
            current
        };
        if chosen != current {
            self.set_frame_rate(chosen);
        }
    }

    fn frame_rate(&self) -> FrameRate {
        if self.config.swap_interval.is_some() {
            FrameRate::Force60
        } else if self.config.min_swap_interval.is_some() {
            FrameRate::Max30
        } else {
            FrameRate::Title
        }
    }

    fn set_frame_rate(&mut self, rate: FrameRate) {
        let (forced, least) = match rate {
            FrameRate::Title => (None, None),
            FrameRate::Max30 => (None, Some(2)),
            FrameRate::Force60 => (Some(1), None),
        };
        self.config.swap_interval = forced;
        self.config.min_swap_interval = least;
        self.console.send(ConsoleCommand::SwapInterval(forced));
        self.console.send(ConsoleCommand::MinSwapInterval(least));
        self.save_config();
    }

    fn upscale_menu(&mut self, ui: &mut egui::Ui) {
        ui.add_enabled_ui(upscale::available(), |ui| {
            ui.label("Screen upscaling (xBRZ)");
            let mut changed =
                ui.radio(self.config.upscale.method == UpscaleMethod::None, "Off").clicked();
            if changed {
                self.config.upscale.method = UpscaleMethod::None;
            }
            for factor in 2..=upscale::MAX_FACTOR {
                let selected = self.config.upscale.method == UpscaleMethod::Xbrz
                    && self.config.upscale.factor == factor;
                if ui.radio(selected, format!("{factor}x")).clicked() {
                    self.config.upscale.method = UpscaleMethod::Xbrz;
                    self.config.upscale.factor = factor;
                    changed = true;
                }
            }
            if changed {
                self.presenter.set_upscale(self.config.upscale);
                self.save_config();
            }
        });
    }

    fn set_frame_generation(&mut self, enabled: bool) {
        self.config.frame_generation = enabled;
        self.presenter.set_frame_generation(enabled);
        self.save_config();
    }

    /// Opens the cheat editor on the running title's file.
    fn open_cheats(&mut self) {
        let text = self
            .cheats_path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        self.cheats = Some(CheatEditor { text, message: String::new() });
    }

    /// Checks the editor's text; if it is fine, writes it and has the emulator use it.
    fn save_cheats(&mut self) {
        let Some(editor) = &mut self.cheats else { return };
        let count = match wiiu_emu::cheat::parse(&editor.text) {
            Ok(cheats) => cheats.iter().filter(|cheat| cheat.enabled).count(),
            Err(error) => {
                editor.message = error.to_string();
                return;
            }
        };
        let Some(path) = &self.cheats_path else { return };
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(path, &editor.text));
        editor.message = match written {
            Ok(()) => {
                self.console.send(ConsoleCommand::ReloadCheats);
                format!("適用しました（有効なコード: {count}）")
            }
            Err(error) => format!("{}: {error}", path.display()),
        };
    }

    fn cheat_window(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.cheats else { return };
        let (mut open, mut save) = (true, false);
        egui::Window::new("チートコード").open(&mut open).default_size([460.0, 380.0]).show(
            ctx,
            |ui| {
                ui.label("「アドレス 値」を 16 進数で 1 行ずつ。値の桁数で書き込み幅が決まります（2 桁: 8bit、4 桁: 16bit、8 桁: 32bit）。");
                ui.label("// で名前を付け、// disabled で次のコードを無効にできます。");
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut editor.text)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .hint_text("// ルピー 999\n1050_6A1C 03E7"),
                    );
                });
                ui.horizontal(|ui| {
                    save = ui.button("保存して適用").clicked();
                    ui.label(&editor.message);
                });
            },
        );
        if save {
            self.save_cheats();
        }
        if !open {
            self.cheats = None;
        }
    }

    /// Follows the title's frame rate or forces 60 fps, at once.
    fn toggle_frame_rate(&mut self) {
        self.set_frame_rate(match self.frame_rate() {
            FrameRate::Title => FrameRate::Max30,
            FrameRate::Max30 => FrameRate::Force60,
            FrameRate::Force60 => FrameRate::Title,
        });
    }

    /// Off, then every factor of xBRZ in turn.
    fn cycle_upscale(&mut self) {
        let upscale = &mut self.config.upscale;
        if upscale.method == UpscaleMethod::None {
            upscale.method = UpscaleMethod::Xbrz;
            upscale.factor = 2;
        } else if upscale.factor < upscale::MAX_FACTOR {
            upscale.factor += 1;
        } else {
            upscale.method = UpscaleMethod::None;
        }
        self.presenter.set_upscale(self.config.upscale);
        self.save_config();
    }

    /// The next graphics API, used from the next start.
    fn cycle_graphics_api(&mut self) {
        let position = GraphicsApi::ALL.iter().position(|api| *api == self.config.graphics_api);
        let next = position.map_or(0, |index| (index + 1) % GraphicsApi::ALL.len());
        self.config.graphics_api = GraphicsApi::ALL[next];
        self.save_config();
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        // Parts of the bar that change a setting look like plain text but respond to clicks.
        fn setting(ui: &mut egui::Ui, text: String, hint: &str) -> bool {
            ui.add(egui::Label::new(text).sense(egui::Sense::click()))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(hint)
                .clicked()
        }
        ui.horizontal(|ui| {
            let running = self.status == Some(Status::Running);
            let state = match (&self.status, self.paused) {
                (Some(Status::Running), true) => "Paused".to_owned(),
                (Some(Status::Running), false) => "Running".to_owned(),
                (Some(Status::Exited(code)), _) => format!("Exited ({code})"),
                (Some(Status::Crashed(_)), _) => "Crashed".to_owned(),
                (Some(Status::Idle) | None, _) => "Idle".to_owned(),
            };
            if running {
                if setting(ui, state, "クリックで一時停止/再開") {
                    self.console.send(if self.paused {
                        ConsoleCommand::Resume
                    } else {
                        ConsoleCommand::Pause
                    });
                }
            } else {
                ui.label(state);
            }
            ui.separator();
            let rate = match self.frame_rate() {
                FrameRate::Title => "タイトル指定",
                FrameRate::Max30 => "最大 30 fps",
                FrameRate::Force60 => "60 fps 強制",
            };
            let fps = format!("{:.1} fps ({rate})", self.fps);
            if setting(
                ui,
                fps,
                "クリックでタイトル指定 → 最大 30 fps → 60 fps 強制 と切り替え。\n\
                 30 fps 用のタイトルは 60 fps にすると最大で倍速になります。",
            ) {
                self.toggle_frame_rate();
            }
            ui.separator();
            let generation = if self.config.frame_generation {
                "フレーム生成 ON"
            } else {
                "フレーム生成 OFF"
            };
            if setting(ui, generation.to_owned(), "クリックでフレーム生成（30 → 60 fps）を切り替え")
            {
                self.set_frame_generation(!self.config.frame_generation);
            }
            ui.separator();
            let scale = match self.config.upscale.method {
                UpscaleMethod::None => "xBRZ off".to_owned(),
                UpscaleMethod::Xbrz => format!("xBRZ {}x", self.config.upscale.factor),
            };
            if upscale::available() && setting(ui, scale, "クリックで画面の拡大方法を切り替え")
            {
                self.cycle_upscale();
            }
            ui.separator();
            ui.label(self.title.as_deref().unwrap_or("no title"));
            ui.separator();
            #[allow(clippy::cast_precision_loss)]
            ui.label(format!("{:.1} M instructions", self.instructions as f64 / 1e6));
            if let Some(status) = self.netplay_status() {
                ui.separator();
                ui.label(status);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let api = format!("{} ({})", self.active_api.label(), Presenter::backend_name());
                if setting(ui, api, "クリックでグラフィックス API を切り替え（次回起動から）")
                {
                    self.cycle_graphics_api();
                }
                if self.config.graphics_api != self.active_api {
                    ui.label(format!("→ {} after restart", self.config.graphics_api.label()));
                }
            });
        });
    }

    /// Titles from `roms.json`; double-click or "Boot" starts one.
    fn library_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Games");
            if ui.small_button("⟳").on_hover_text("Rescan roms.json directories").clicked() {
                self.reload_library();
            }
        });
        ui.separator();
        if let Some(error) = &self.library_error {
            ui.colored_label(egui::Color32::YELLOW, error);
            return;
        }
        if self.library.games.is_empty() {
            ui.label("No titles found in the roms.json directories.");
            return;
        }
        let mut boot = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for game in &self.library.games {
                let selected = self.title.as_deref() == Some(game.name.as_str());
                let response = ui.selectable_label(selected, &game.name);
                let response = match game.title_id {
                    Some(id) => {
                        response.on_hover_text(format!("{id:016X}\n{}", game.rpx_path.display()))
                    }
                    None => response.on_hover_text(game.rpx_path.display().to_string()),
                };
                if response.double_clicked() {
                    boot = Some(game.path.clone());
                }
                response.context_menu(|ui| {
                    if ui.button("Boot").clicked() {
                        boot = Some(game.path.clone());
                        ui.close();
                    }
                });
            }
        });
        if let Some(path) = boot {
            self.boot(path);
        }
    }

    /// The console-like start screen; tells what is wrong if it has no titles to offer.
    fn menu_screen(&mut self, ui: &mut egui::Ui) {
        let area = ui.available_rect_before_wrap();
        let [r, g, b, a] = self.config.launcher_background;
        let background = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
        let picture = self.config.launcher_image.clone();
        if let Some(path) =
            self.launcher.show(ui, &self.library.games, background, picture.as_deref())
        {
            self.boot(path);
        }
        let notice = self.library_error.clone().or_else(|| {
            self.library
                .games
                .is_empty()
                .then(|| "No titles found in the roms.json directories.".to_owned())
        });
        let button = egui::Rect::from_min_size(
            area.left_top() + egui::vec2(16.0, 16.0),
            egui::vec2(150.0, 32.0),
        );
        if ui.put(button, egui::Button::new("ROMフォルダを選択…")).clicked() {
            self.pick_rom_folder();
        }
        let picture_rect = egui::Rect::from_min_size(
            button.left_bottom() + egui::vec2(0.0, 8.0 + 32.0 + 8.0),
            egui::vec2(230.0, 26.0),
        );
        let picker =
            egui::Rect::from_min_size(button.left_bottom() + egui::vec2(0.0, 8.0), button.size());
        let mut color = background;
        let changed = ui
            .scope_builder(egui::UiBuilder::new().max_rect(picker), |ui| {
                ui.horizontal(|ui| {
                    let response = egui::color_picker::color_edit_button_srgba(
                        ui,
                        &mut color,
                        egui::color_picker::Alpha::OnlyBlend,
                    );
                    egui::Frame::new()
                        .fill(egui::Color32::from_black_alpha(170))
                        .corner_radius(4.0)
                        .inner_margin(egui::vec2(6.0, 2.0))
                        .show(ui, |ui| ui.colored_label(egui::Color32::WHITE, "背景色"));
                    response.changed()
                })
                .inner
            })
            .inner;
        if changed {
            // Not `r()` and so on: those are premultiplied, and would darken at every change.
            self.config.launcher_background = color.to_srgba_unmultiplied();
            self.save_config();
        }
        self.picture_controls(ui, picture_rect);
        if let Some(error) = self.launcher.picture_error() {
            ui.painter().text(
                picture_rect.left_bottom() + egui::vec2(0.0, 6.0),
                egui::Align2::LEFT_TOP,
                error,
                egui::FontId::proportional(12.0),
                egui::Color32::from_rgb(200, 60, 40),
            );
        }
        if let Some(notice) = notice {
            ui.painter().text(
                area.center_top() + egui::vec2(0.0, 12.0),
                egui::Align2::CENTER_TOP,
                notice,
                egui::FontId::proportional(14.0),
                egui::Color32::from_rgb(170, 90, 30),
            );
        }
    }

    /// Path of the backdrop picture: typed in, or chosen with the button next to it.
    fn picture_controls(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let mut chosen = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.horizontal(|ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.picture_path)
                        .hint_text("背景画像のパス")
                        .desired_width(rect.width() - 36.0),
                );
                if field.lost_focus() {
                    chosen = Some(self.picture_path.clone());
                }
                if ui.button("…").on_hover_text("背景画像を選ぶ").clicked() {
                    let dialog = rfd::FileDialog::new()
                        .set_title("背景画像")
                        .add_filter("画像", &["png", "jpg", "jpeg", "webp", "bmp", "gif", "tga"]);
                    if let Some(path) = dialog.pick_file() {
                        chosen = Some(path.display().to_string());
                    }
                }
            });
        });
        if let Some(text) = chosen {
            self.set_picture(&text);
        }
    }

    /// Applies a backdrop picture path; empty text removes the picture.
    fn set_picture(&mut self, text: &str) {
        // Explorer's "copy as path" adds quotes.
        let text = text.trim().trim_matches('"');
        self.picture_path = text.to_owned();
        let path = (!text.is_empty()).then(|| PathBuf::from(text));
        if path == self.config.launcher_image {
            return;
        }
        // An opaque color would hide the picture just chosen.
        if path.is_some() && self.config.launcher_background[3] == 255 {
            self.config.launcher_background[3] = 0;
        }
        self.config.launcher_image = path;
        self.save_config();
    }

    fn log_panel(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(
            ui,
            |ui| {
                for line in &self.log {
                    ui.monospace(line);
                }
            },
        );
    }

    /// TV and GamePad side by side, scaled to fit while keeping aspect ratio.
    fn screens(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_size();
        let sizes = Screen::ALL.map(|screen| {
            self.presenter.view(screen).map_or_else(
                || {
                    let (w, h) = screen.default_size();
                    #[allow(clippy::cast_precision_loss)]
                    egui::vec2(w as f32, h as f32)
                },
                |view| view.size,
            )
        });
        let spacing = ui.spacing().item_spacing.x;
        let total_width = sizes.iter().map(|s| s.x).sum::<f32>();
        let max_height = sizes.iter().map(|s| s.y).fold(0.0, f32::max);
        let scale = ((available.x - spacing) / total_width).min(available.y / max_height).max(0.01);

        let mut touch = None;
        ui.horizontal_centered(|ui| {
            for (screen, size) in Screen::ALL.into_iter().zip(sizes) {
                let size = size * scale;
                match self.presenter.view(screen) {
                    // The GamePad's screen is a touch screen: the mouse is the finger.
                    Some(view) if screen == Screen::Drc => {
                        let response = ui
                            .add(egui::Image::new((view.texture, size)).sense(egui::Sense::drag()));
                        touch = touch_position(&response);
                        if response.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                    }
                    Some(view) => {
                        ui.add(egui::Image::new((view.texture, size)));
                    }
                    None => {
                        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                        ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
                        let label = match screen {
                            Screen::Tv => "TV",
                            Screen::Drc => "GamePad",
                        };
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            label,
                            egui::FontId::proportional(16.0),
                            egui::Color32::DARK_GRAY,
                        );
                    }
                }
            }
        });
        self.touch = touch;
    }
}

/// Where the mouse button holds `response`, in pixels of the GamePad's screen. A drag that
/// leaves the picture keeps touching its edge, like a finger at the rim of the screen.
fn touch_position(response: &egui::Response) -> Option<(u16, u16)> {
    let position =
        response.interact_pointer_pos().filter(|_| response.is_pointer_button_down_on())?;
    let (width, height) = Screen::Drc.default_size();
    let rect = response.rect;
    let x = ((position.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * (width - 1) as f32;
    let y = ((position.y - rect.top()) / rect.height()).clamp(0.0, 1.0) * (height - 1) as f32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some((x as u16, y as u16))
}

fn short_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

impl eframe::App for ShocoraApp {
    /// Nothing of its own behind the panels: a see-through backdrop shows the desktop.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pull_emulator_state();
        self.poll_input(ui.ctx());

        egui::Panel::top("menu").show(ui, |ui| self.menu(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.show_library {
            egui::Panel::left("library")
                .resizable(true)
                .default_size(260.0)
                .show(ui, |ui| self.library_panel(ui));
        }
        let in_menu = self.title.is_none() && !self.boot_pending;
        if self.show_log && !in_menu {
            egui::Panel::bottom("log")
                .resizable(true)
                .default_size(140.0)
                .show(ui, |ui| self.log_panel(ui));
        }
        if matches!(self.netplay, Netplay::Guest(_)) {
            egui::CentralPanel::default().show(ui, |ui| self.guest_screen(ui));
        } else if in_menu {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| self.menu_screen(ui));
        } else {
            egui::CentralPanel::default().show(ui, |ui| self.screens(ui));
        }
        self.cheat_window(ui.ctx());
        self.launcher.paint_transition(ui.ctx());
    }
}
