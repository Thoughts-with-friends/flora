//! Main window: menu, the two screens, status bar and guest log.
use std::path::{Path, PathBuf};

use eframe::egui;
use shocora_game_library::{GameLibrary, LEGACY_PATH, legacy_rom_dirs};
use shocora_gui_common::{
    config::AppConfig,
    console::{Console, ConsoleCommand},
    input::{Controllers, merge},
};
use shocora_netplay::{
    DEFAULT_PORT, FrameRate, HostConfig, HostState, HostStatus, LinkState, Refusal,
};
use shocora_render::{GraphicsApi, ResolutionScale, Screen, UpscaleMethod, upscale};
use wiiu_emu::{GamePadState, GameSpeed, Status, VpadButtons};

use crate::{
    lang::{self, tr, trf},
    launcher::Launcher,
    netplay::{self, Netplay},
    presenter::{Presenter, ScreenView},
};

/// Lines of guest output kept in the log panel.
const LOG_LINES: usize = 500;

/// `width` x `height` drawn `factor` times larger, as far as the limit allows.
fn scaled_size(factor: u32, width: u32, height: u32) -> String {
    let factor = ResolutionScale::for_tv(factor, width).factor;
    format!("{} x {}", width * factor, height * factor)
}

/// The pages of the settings window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    General,
    Graphics,
    Input,
    Netplay,
}

impl SettingsTab {
    const ALL: [Self; 4] = [Self::General, Self::Graphics, Self::Input, Self::Netplay];

    fn title(self) -> &'static str {
        match self {
            Self::General => tr("settings.general"),
            Self::Graphics => tr("settings.graphics"),
            Self::Input => tr("settings.input"),
            Self::Netplay => tr("settings.netplay"),
        }
    }

    fn menu_label(self) -> &'static str {
        match self {
            Self::General => tr("settings.general_menu"),
            Self::Graphics => tr("settings.graphics_menu"),
            Self::Input => tr("settings.input_menu"),
            Self::Netplay => tr("settings.netplay_menu"),
        }
    }
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
    /// The settings window's page, while it is open.
    settings_tab: Option<SettingsTab>,
    /// How fast the title runs. Back to normal at the next start of the program.
    speed: GameSpeed,
    /// The controller's speed button was down at the last look: a press changes it once.
    speed_button_down: bool,
    controllers: Controllers,
    /// Titles found in the directories listed in `shocora.toml`.
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
        lang::set_language(&config.language);
        let picture_path = config
            .launcher_image
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string());
        presenter.set_frame_generation(config.frame_generation);
        presenter.set_background_shaders(config.background_shaders);
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
            settings_tab: None,
            speed: GameSpeed::Normal,
            speed_button_down: false,
            controllers: Controllers::new(),
            library: GameLibrary::default(),
            library_error: None,
            show_library: false,
            launcher: Launcher::default(),
            boot_pending: false,
            touch: None,
        };
        app.adopt_old_rom_list();
        app.reload_library();
        if let Some(path) = boot {
            app.boot(path);
        }
        Ok(app)
    }

    fn boot(&mut self, path: PathBuf) {
        self.log.push(trf("log.loading", &[("path", &path.display())]));
        self.boot_pending = true;
        self.console.send(ConsoleCommand::Load(path.clone()));
        self.config.last_title = Some(path);
        self.save_config();
    }

    /// The directories of a `roms.json` from before `shocora.toml` listed them.
    fn adopt_old_rom_list(&mut self) {
        if self.config.rom_dirs.is_empty() {
            if let Some(dirs) = legacy_rom_dirs(Path::new(LEGACY_PATH)) {
                self.config.rom_dirs = dirs;
                self.save_config();
            }
        }
    }

    /// Re-reads the directories from `shocora.toml`, which may have been edited, and rescans.
    fn reload_library(&mut self) {
        match AppConfig::read(&self.config_path) {
            Ok(saved) => {
                // Nothing saved yet: what is known so far must not get lost.
                if self.config_path.exists() {
                    self.config.rom_dirs = saved.rom_dirs;
                }
                self.library = GameLibrary::scan(&self.config.rom_dirs);
                self.library_error = None;
                self.log.extend(self.library.problems.iter().cloned());
                self.log.push(trf("library.count", &[("count", &self.library.games.len())]));
            }
            Err(error) => {
                self.library = GameLibrary::default();
                self.library_error = Some(error);
            }
        }
    }

    /// Asks for a directory of titles, adds it to `shocora.toml` and rescans.
    fn pick_rom_folder(&mut self) {
        let Some(dir) = rfd::FileDialog::new().set_title(tr("dialog.rom_folder")).pick_folder()
        else {
            return;
        };
        // Never overwrite a file that only failed to parse: it holds the user's list.
        if let Err(error) = AppConfig::read(&self.config_path) {
            self.log.push(error.clone());
            self.library_error = Some(error);
            return;
        }
        if !self.config.rom_dirs.contains(&dir) {
            self.config.rom_dirs.push(dir);
        }
        self.save_config();
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
            dialog.set_title(tr("dialog.title_folder")).pick_folder()
        } else {
            dialog
                .set_title(tr("dialog.rpx"))
                .add_filter(tr("dialog.rpx_filter"), &["rpx"])
                .pick_file()
        };
        if let Some(path) = picked {
            self.boot(path);
        }
    }

    fn pull_emulator_state(&mut self) {
        let snapshot = self.console.drain();
        self.presenter.sync();
        self.log.extend(snapshot.console_lines.iter().map(|l| l.trim_end().to_owned()));
        if let Some(factor) = self.presenter.take_memory_shortage() {
            // Frames already on their way may report the old factor once more.
            let lower = ResolutionScale::lower_factor(factor);
            if lower < self.config.resolution_factor {
                self.log.push(trf(
                    "graphics.memory_ran_out",
                    &[("factor", &factor), ("lower", &lower)],
                ));
                self.set_resolution_factor(lower);
            }
        }
        if snapshot.title.is_some() {
            self.boot_pending = false;
        }
        if let Some(error) = snapshot.load_error {
            self.log.push(error);
            self.boot_pending = false;
        }
        if let Some(Status::Crashed(message)) = &snapshot.status {
            if self.status != snapshot.status {
                self.log.push(trf("log.stopped", &[("message", message)]));
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
        if let Netplay::Host(hosting) = &self.netplay {
            hosting.publish(self.host_status());
        }
    }

    /// What this emulator is doing, in the words of the status bars of both players.
    fn host_status(&self) -> HostStatus {
        HostStatus {
            state: match (&self.status, self.paused) {
                (Some(Status::Running), true) => HostState::Paused,
                (Some(Status::Running), false) => HostState::Running,
                (Some(Status::Exited(code)), _) => HostState::Exited(*code),
                (Some(Status::Crashed(_)), _) => HostState::Crashed,
                (Some(Status::Idle) | None, _) => HostState::Idle,
            },
            fps: self.fps,
            rate: self.frame_rate(),
            speed: self.speed.label().to_string(),
            frame_generation: self.config.frame_generation,
            xbrz: match self.config.upscale.method {
                UpscaleMethod::None => 0,
                UpscaleMethod::Xbrz => self.config.upscale.factor,
            },
            title: self.title.clone().unwrap_or_default(),
            instructions: self.instructions,
            player1: false,
        }
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
        // The host's clock is not a guest's to change: its buttons all go to the title.
        if !matches!(self.netplay, Netplay::Guest(_)) {
            let button = pad.held.contains(VpadButtons::STICK_L);
            pad.held.remove(VpadButtons::STICK_L);
            let pressed = button && !self.speed_button_down;
            self.speed_button_down = button;
            let tab = !ctx.egui_wants_keyboard_input()
                && ctx.input(|input| {
                    input.events.iter().any(|event| {
                        matches!(
                            event,
                            egui::Event::Key {
                                key: egui::Key::Tab,
                                pressed: true,
                                repeat: false,
                                ..
                            }
                        )
                    })
                });
            if pressed || tab {
                self.next_speed();
            }
        }
        if pad != self.last_gamepad {
            self.last_gamepad = pad;
            match &mut self.netplay {
                // A guest's controller belongs to the host's second player.
                Netplay::Guest(guest) => guest.set_pad(pad),
                // Who plays what depends on the guest, too.
                Netplay::Host(hosting) => hosting.set_pad(pad),
                Netplay::Off => self.console.send(ConsoleCommand::GamePad(pad)),
            }
        }
    }

    fn next_speed(&mut self) {
        self.speed = self.speed.next();
        self.console.send(ConsoleCommand::Speed(self.speed));
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
        let [tv, gamepad] = self.presenter.stream_taps();
        match netplay::host(config, commands, tv, gamepad, self.last_gamepad) {
            Ok(hosting) => {
                self.console.send(ConsoleCommand::AudioTap(Some(hosting.host.audio_sink())));
                self.netplay = Netplay::Host(hosting);
                self.netplay_error = None;
                self.save_config();
            }
            Err(error) if error.kind() == std::io::ErrorKind::AddrNotAvailable => {
                self.netplay_error = Some(trf("netplay.not_this_pc", &[("address", &address)]));
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

    /// What only applies while linked: the first player's rights on the host, and where the
    /// buttons go on a guest.
    fn netplay_controls(&mut self, ui: &mut egui::Ui) {
        match &mut self.netplay {
            Netplay::Off => {}
            Netplay::Host(hosting) => {
                let guest_here = matches!(hosting.host.state(), LinkState::Connected { .. });
                let player1 = hosting.player1();
                let label =
                    if player1 { tr("netplay.take_player1") } else { tr("netplay.give_player1") };
                if ui
                    .add_enabled(guest_here, egui::Button::new(label))
                    .on_hover_text(tr("netplay.give_player1_hint"))
                    .clicked()
                {
                    hosting.set_player1(!player1);
                }
            }
            Netplay::Guest(guest) => {
                if guest.status().is_some_and(|status| status.player1) {
                    ui.label(tr("netplay.you_are_player1"));
                } else {
                    let mut to_gamepad = guest.to_gamepad();
                    if ui
                        .checkbox(&mut to_gamepad, tr("netplay.to_gamepad"))
                        .on_hover_text(tr("netplay.to_gamepad_hint"))
                        .changed()
                    {
                        guest.set_to_gamepad(to_gamepad);
                    }
                }
            }
        }
    }

    fn netplay_settings(&mut self, ui: &mut egui::Ui) {
        match self.netplay {
            Netplay::Off => {
                ui.label(tr("netplay.password"));
                ui.add(egui::TextEdit::singleline(&mut self.netplay_password).password(true));
                ui.separator();
                ui.label(tr("netplay.own_address"));
                ui.text_edit_singleline(&mut self.config.netplay_address)
                    .on_hover_text(tr("netplay.own_address_hint"));
                let ready = !self.config.netplay_address.trim().is_empty();
                if ui
                    .add_enabled(ready, egui::Button::new(tr("netplay.start_host")))
                    .on_hover_text(tr("netplay.start_host_hint"))
                    .clicked()
                {
                    self.start_hosting();
                }
                ui.separator();
                ui.label(tr("netplay.host_address"));
                ui.text_edit_singleline(&mut self.config.netplay_host);
                let ready = !self.config.netplay_host.trim().is_empty();
                if ui.add_enabled(ready, egui::Button::new(tr("netplay.join"))).clicked() {
                    self.join(ui.ctx());
                }
            }
            Netplay::Host(_) | Netplay::Guest(_) => {
                if ui.button(tr("netplay.disconnect")).clicked() {
                    self.stop_netplay();
                    return;
                }
                self.netplay_controls(ui);
            }
        }
        if let Some(status) = self.netplay_status() {
            ui.separator();
            ui.label(status);
        }
    }

    fn set_resolution_factor(&mut self, factor: u32) {
        self.config.resolution_factor = factor;
        self.console.send(ConsoleCommand::ResolutionFactor(factor));
        self.save_config();
    }

    fn resolution_settings(&mut self, ui: &mut egui::Ui) {
        ui.label(tr("graphics.resolution")).on_hover_text(tr("graphics.resolution_hint"));
        let current = self.config.resolution_factor;
        for factor in ResolutionScale::FACTORS {
            let label = if factor == 1 {
                tr("graphics.native").to_owned()
            } else {
                trf(
                    "graphics.factor",
                    &[
                        ("factor", &factor),
                        ("at720", &scaled_size(factor, 1280, 720)),
                        ("at1080", &scaled_size(factor, 1920, 1080)),
                    ],
                )
            };
            if ui.radio(current == factor, label).clicked() && current != factor {
                self.set_resolution_factor(factor);
            }
        }
    }

    fn graphics_settings(&mut self, ui: &mut egui::Ui) {
        self.resolution_settings(ui);
        ui.separator();
        self.upscale_menu(ui);
        ui.separator();
        self.frame_rate_menu(ui);
        ui.separator();
        let mut generate = self.config.frame_generation;
        if ui
            .checkbox(&mut generate, tr("graphics.frame_generation"))
            .on_hover_text(tr("graphics.frame_generation_hint"))
            .changed()
        {
            self.set_frame_generation(generate);
        }
        let mut background = self.config.background_shaders;
        if ui
            .checkbox(&mut background, tr("graphics.background_shaders"))
            .on_hover_text(tr("graphics.background_shaders_hint"))
            .changed()
        {
            self.config.background_shaders = background;
            self.presenter.set_background_shaders(background);
            self.save_config();
        }
        ui.separator();
        ui.label(tr("graphics.api"));
        for api in GraphicsApi::ALL {
            if ui.radio(self.config.graphics_api == api, api.label()).clicked() {
                self.config.graphics_api = api;
                self.save_config();
            }
        }
    }

    fn general_settings(&mut self, ui: &mut egui::Ui) {
        ui.label(tr("general.language"));
        ui.horizontal(|ui| {
            for (code, name) in lang::LANGUAGES {
                let chosen = self.config.language == code;
                if ui.radio(chosen, name).clicked() && !chosen {
                    self.config.language = code.to_owned();
                    lang::set_language(code);
                    self.save_config();
                }
            }
        });
        ui.separator();
        ui.label(tr("general.speed")).on_hover_text(tr("general.speed_hint"));
        ui.horizontal(|ui| {
            for speed in GameSpeed::ALL {
                if ui.radio(self.speed == speed, speed.label()).clicked() && self.speed != speed {
                    self.speed = speed;
                    self.console.send(ConsoleCommand::Speed(speed));
                }
            }
        });
        ui.separator();
        if ui.button(tr("general.fonts_folder")).clicked() {
            self.pick_fonts_dir();
        }
        if ui.button(tr("general.rom_folder")).clicked() {
            self.pick_rom_folder();
        }
    }

    fn input_settings(&self, ui: &mut egui::Ui) {
        ui.label(tr("input.speed_keys"));
        ui.label(tr("input.edit_hint"));
        ui.separator();
        egui::Grid::new("bindings").striped(true).show(ui, |ui| {
            ui.strong(tr("input.action"));
            ui.strong(tr("input.keyboard"));
            ui.strong(tr("input.controller"));
            ui.end_row();
            for (action, key) in &self.config.key_bindings.0 {
                ui.label(action);
                ui.label(key);
                ui.label(self.config.pad_bindings.0.get(action).map_or("-", String::as_str));
                ui.end_row();
            }
        });
    }

    /// The settings window, with one tab per subject.
    fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(mut tab) = self.settings_tab else { return };
        let mut open = true;
        egui::Window::new(tr("menu.settings")).open(&mut open).resizable(true).show(ctx, |ui| {
            ui.horizontal(|ui| {
                for choice in SettingsTab::ALL {
                    ui.selectable_value(&mut tab, choice, choice.title());
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| match tab {
                SettingsTab::General => self.general_settings(ui),
                SettingsTab::Graphics => self.graphics_settings(ui),
                SettingsTab::Input => self.input_settings(ui),
                SettingsTab::Netplay => self.netplay_settings(ui),
            });
        });
        self.settings_tab = open.then_some(tab);
    }

    /// The guest's screens: what the host sends, laid out like the host's.
    fn guest_screen(&mut self, ui: &mut egui::Ui) {
        let Netplay::Guest(guest) = &mut self.netplay else { return };
        let views = guest.views(ui.ctx());
        if views.iter().all(Option::is_none) {
            self.touch = None;
            ui.centered_and_justified(|ui| ui.label(tr("netplay.waiting_picture")));
            return;
        }
        self.touch = show_screens(ui, views);
    }

    fn netplay_status(&self) -> Option<String> {
        let link = |state: &LinkState| match state {
            LinkState::Waiting => tr("netplay.waiting").to_owned(),
            LinkState::Connecting => tr("netplay.connecting").to_owned(),
            LinkState::Unreachable(_) => tr("netplay.unreachable").to_owned(),
            LinkState::Refused(Refusal::WrongPassword) => tr("netplay.wrong_password").to_owned(),
            LinkState::Refused(Refusal::Network) => tr("netplay.other_network").to_owned(),
            LinkState::Connected { peer, round_trip: Some(time) } => {
                format!("{peer} ping {} ms", time.as_millis())
            }
            LinkState::Connected { peer, round_trip: None } => peer.clone(),
        };
        match &self.netplay {
            Netplay::Off => self.netplay_error.as_ref().map(|error| format!("2P: {error}")),
            Netplay::Host(hosting) => Some(trf(
                "netplay.hosting",
                &[
                    ("network", &self.netplay_network),
                    ("port", &hosting.host.port()),
                    ("state", &link(&hosting.host.state())),
                ],
            )),
            Netplay::Guest(guest) => Some(format!("2P: {}", link(&guest.client.state()))),
        }
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button(tr("menu.file"), |ui| {
                if ui.button(tr("menu.open_rpx")).clicked() {
                    ui.close();
                    self.pick_and_boot(false);
                }
                if ui.button(tr("menu.open_title")).clicked() {
                    ui.close();
                    self.pick_and_boot(true);
                }
                if let Some(last) = self.config.last_title.clone() {
                    if ui
                        .button(trf("menu.reload_title", &[("title", &short_name(&last))]))
                        .clicked()
                    {
                        ui.close();
                        self.boot(last);
                    }
                }
                if ui.button(tr("menu.select_rom_folder")).clicked() {
                    ui.close();
                    self.pick_rom_folder();
                }
                if ui.button(tr("menu.reload_library")).clicked() {
                    ui.close();
                    self.reload_library();
                }
                ui.separator();
                if ui.button(tr("menu.exit")).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button(tr("menu.emulation"), |ui| {
                let running = self.status == Some(Status::Running);
                let label = if self.paused { tr("menu.resume") } else { tr("menu.pause") };
                if ui.add_enabled(running, egui::Button::new(label)).clicked() {
                    self.console.send(if self.paused {
                        ConsoleCommand::Resume
                    } else {
                        ConsoleCommand::Pause
                    });
                    ui.close();
                }
                if ui
                    .add_enabled(self.cheats_path.is_some(), egui::Button::new(tr("menu.cheats")))
                    .clicked()
                {
                    self.open_cheats();
                    ui.close();
                }
                if ui
                    .add_enabled(self.title.is_some(), egui::Button::new(tr("menu.reset")))
                    .clicked()
                {
                    self.log.push(tr("log.reset").to_owned());
                    self.console.send(ConsoleCommand::Reset);
                    ui.close();
                }
                if ui
                    .add_enabled(self.title.is_some(), egui::Button::new(tr("menu.stop")))
                    .clicked()
                {
                    self.console.send(ConsoleCommand::Stop);
                    ui.close();
                }
            });
            ui.menu_button(tr("menu.settings"), |ui| {
                for tab in SettingsTab::ALL {
                    if ui.button(tab.menu_label()).clicked() {
                        ui.close();
                        self.settings_tab = Some(tab);
                    }
                }
            });
            ui.menu_button(tr("menu.view"), |ui| {
                ui.checkbox(&mut self.show_library, tr("menu.game_library"));
                ui.checkbox(&mut self.show_log, tr("menu.guest_log"));
            });
        });
    }

    /// Asks for the folder with `CafeStd.ttf`; titles booted from now on use it.
    fn pick_fonts_dir(&mut self) {
        let dialog = rfd::FileDialog::new().set_title(tr("dialog.fonts_folder"));
        if let Some(dir) = dialog.pick_folder() {
            self.log.push(trf("log.fonts", &[("path", &dir.display())]));
            self.console.send(ConsoleCommand::FontsDir(Some(dir.clone())));
            self.config.fonts_dir = Some(dir);
            self.save_config();
        }
    }

    fn frame_rate_menu(&mut self, ui: &mut egui::Ui) {
        ui.label(tr("graphics.frame_rate"));
        let current = self.frame_rate();
        let title = ui.radio(current == FrameRate::Title, tr("graphics.as_title"));
        let capped = ui
            .radio(current == FrameRate::Max30, tr("graphics.max30"))
            .on_hover_text(tr("graphics.max30_hint"));
        let forced = ui
            .radio(current == FrameRate::Force60, tr("graphics.force60"))
            .on_hover_text(tr("graphics.force60_hint"));
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
        let usable = upscale::available() && self.config.resolution_factor == 1;
        ui.add_enabled_ui(usable, |ui| {
            ui.label(tr("graphics.xbrz")).on_disabled_hover_text(tr("graphics.xbrz_unavailable"));
            let mut changed = ui
                .radio(self.config.upscale.method == UpscaleMethod::None, tr("graphics.off"))
                .clicked();
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
                trf("cheats.applied", &[("count", &count)])
            }
            Err(error) => format!("{}: {error}", path.display()),
        };
    }

    fn cheat_window(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.cheats else { return };
        let (mut open, mut save) = (true, false);
        egui::Window::new(tr("cheats.title")).open(&mut open).default_size([460.0, 380.0]).show(
            ctx,
            |ui| {
                ui.label(tr("cheats.format"));
                ui.label(tr("cheats.comments"));
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut editor.text)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .hint_text(tr("cheats.hint")),
                    );
                });
                ui.horizontal(|ui| {
                    save = ui.button(tr("cheats.save")).clicked();
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
        if matches!(self.netplay, Netplay::Guest(_)) {
            self.guest_status_bar(ui);
            return;
        }
        ui.horizontal(|ui| {
            let running = self.status == Some(Status::Running);
            let state = state_text(self.host_status().state);
            if running {
                if setting(ui, state, tr("status.pause_hint")) {
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
            let fps = fps_text(self.fps, self.frame_rate());
            if setting(ui, fps, tr("status.rate_hint")) {
                self.toggle_frame_rate();
            }
            ui.separator();
            if !matches!(self.netplay, Netplay::Guest(_))
                && setting(
                    ui,
                    trf("status.speed", &[("speed", &self.speed.label())]),
                    tr("status.speed_hint"),
                )
            {
                self.next_speed();
            }
            ui.separator();
            let generation = if self.config.frame_generation {
                tr("status.framegen_on")
            } else {
                tr("status.framegen_off")
            };
            if setting(ui, generation.to_owned(), tr("status.framegen_hint")) {
                self.set_frame_generation(!self.config.frame_generation);
            }
            ui.separator();
            let scale = match self.config.upscale.method {
                UpscaleMethod::None => tr("status.xbrz_off").to_owned(),
                UpscaleMethod::Xbrz => format!("xBRZ {}x", self.config.upscale.factor),
            };
            if upscale::available() && setting(ui, scale, tr("status.xbrz_hint")) {
                self.cycle_upscale();
            }
            ui.separator();
            ui.label(self.title.as_deref().unwrap_or_else(|| tr("status.no_title")));
            ui.separator();
            #[allow(clippy::cast_precision_loss)]
            let millions = format!("{:.1}", self.instructions as f64 / 1e6);
            ui.label(trf("status.instructions", &[("millions", &millions)]));
            if let Some(status) = self.netplay_status() {
                ui.separator();
                ui.label(status);
            }
            if matches!(self.netplay, Netplay::Host(_)) {
                self.netplay_controls(ui);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let api = format!("{} ({})", self.active_api.label(), Presenter::backend_name());
                if setting(ui, api, tr("status.api_hint")) {
                    self.cycle_graphics_api();
                }
                if self.config.graphics_api != self.active_api {
                    ui.label(trf(
                        "status.after_restart",
                        &[("api", &self.config.graphics_api.label())],
                    ));
                }
            });
        });
    }

    /// What a guest's status bar shows: the host's state, which the guest cannot change.
    fn guest_status_bar(&mut self, ui: &mut egui::Ui) {
        let Netplay::Guest(guest) = &self.netplay else { return };
        let linked = matches!(guest.client.state(), LinkState::Connected { .. });
        let status = guest.status().filter(|_| linked).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(state_text(status.state));
            ui.separator();
            ui.label(fps_text(status.fps, status.rate));
            ui.separator();
            ui.label(trf("status.speed", &[("speed", &status.speed)]));
            ui.separator();
            ui.label(if status.frame_generation {
                tr("status.framegen_on")
            } else {
                tr("status.framegen_off")
            });
            ui.separator();
            ui.label(if status.xbrz == 0 {
                tr("status.xbrz_off").to_owned()
            } else {
                format!("xBRZ {}x", status.xbrz)
            });
            ui.separator();
            ui.label(if status.title.is_empty() { tr("status.no_title") } else { &status.title });
            ui.separator();
            #[allow(clippy::cast_precision_loss)]
            let millions = format!("{:.1}", status.instructions as f64 / 1e6);
            ui.label(trf("status.instructions", &[("millions", &millions)]));
            if let Some(link) = self.netplay_status() {
                ui.separator();
                ui.label(link);
            }
            self.netplay_controls(ui);
        });
    }

    /// Titles from `shocora.toml`'s directories; double-click or tr("library.boot") starts one.
    fn library_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(tr("library.games"));
            if ui.small_button("⟳").on_hover_text(tr("library.rescan")).clicked() {
                self.reload_library();
            }
        });
        ui.separator();
        if let Some(error) = &self.library_error {
            ui.colored_label(egui::Color32::YELLOW, error);
            return;
        }
        if self.library.games.is_empty() {
            ui.label(tr("library.none"));
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
                    if ui.button(tr("library.boot")).clicked() {
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
        let notice = self
            .library_error
            .clone()
            .or_else(|| self.library.games.is_empty().then(|| tr("library.none").to_owned()));
        let button = egui::Rect::from_min_size(
            area.left_top() + egui::vec2(16.0, 16.0),
            egui::vec2(150.0, 32.0),
        );
        if ui.put(button, egui::Button::new(tr("launcher.select_rom"))).clicked() {
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
                        .show(ui, |ui| {
                            ui.colored_label(egui::Color32::WHITE, tr("launcher.background_color"))
                        });
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
                        .hint_text(tr("launcher.picture_path"))
                        .desired_width(rect.width() - 36.0),
                );
                if field.lost_focus() {
                    chosen = Some(self.picture_path.clone());
                }
                if ui.button("…").on_hover_text(tr("launcher.pick_picture")).clicked() {
                    let dialog =
                        rfd::FileDialog::new().set_title(tr("launcher.picture")).add_filter(
                            tr("launcher.pictures"),
                            &["png", "jpg", "jpeg", "webp", "bmp", "gif", "tga"],
                        );
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
        let views = Screen::ALL.map(|screen| self.presenter.view(screen));
        self.touch = show_screens(ui, views);
    }
}

/// TV and GamePad side by side, scaled to fit while keeping aspect ratio. Returns where the
/// mouse touches the GamePad's screen.
fn show_screens(ui: &mut egui::Ui, views: [Option<ScreenView>; 2]) -> Option<(u16, u16)> {
    let available = ui.available_size();
    let sizes = [0, 1].map(|index| {
        views[index].map_or_else(
            || {
                let (w, h) = Screen::ALL[index].default_size();
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
        for ((screen, size), view) in Screen::ALL.into_iter().zip(sizes).zip(views) {
            let size = size * scale;
            match view {
                // The GamePad's screen is a touch screen: the mouse is the finger.
                Some(view) if screen == Screen::Drc => {
                    let response =
                        ui.add(egui::Image::new((view.texture, size)).sense(egui::Sense::drag()));
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
    touch
}

fn state_text(state: HostState) -> String {
    match state {
        HostState::Paused => tr("status.paused").to_owned(),
        HostState::Running => tr("status.running").to_owned(),
        HostState::Exited(code) => trf("status.exited", &[("code", &code)]),
        HostState::Crashed => tr("status.crashed").to_owned(),
        HostState::Idle => tr("status.idle").to_owned(),
    }
}

fn fps_text(fps: f32, rate: FrameRate) -> String {
    let rate = match rate {
        FrameRate::Title => tr("status.rate_title"),
        FrameRate::Max30 => tr("status.rate_max30"),
        FrameRate::Force60 => tr("status.rate_force60"),
    };
    format!("{fps:.1} fps ({rate})")
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
        self.settings_window(ui.ctx());
        self.launcher.paint_transition(ui.ctx());
    }
}
