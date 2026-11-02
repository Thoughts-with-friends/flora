//! Main window: menu, the two screens, status bar and guest log.
use std::path::{Path, PathBuf};

use eframe::egui;
use flora_game_library::{GameLibrary, RomPaths};
use flora_gui_common::{
    config::AppConfig,
    console::{Console, ConsoleCommand},
    input::{Controllers, merge},
};
use flora_render::{GraphicsApi, Screen, UpscaleMethod, upscale};
use wiiu_emu::{GamePadState, Status};

use crate::presenter::Presenter;

/// Lines of guest output kept in the log panel.
const LOG_LINES: usize = 500;

pub struct FloraApp {
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
}

impl FloraApp {
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
        let mut app = Self {
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
            show_library: true,
        };
        app.reload_library();
        if let Some(path) = boot {
            app.boot(path);
        }
        Ok(app)
    }

    fn boot(&mut self, path: PathBuf) {
        self.log.push(format!("loading {}", path.display()));
        self.console.send(ConsoleCommand::Load(path.clone()));
        self.config.last_title = Some(path);
        self.save_config();
    }

    /// Re-reads `roms.json` and rescans its directories.
    fn reload_library(&mut self) {
        let path = std::path::Path::new(flora_game_library::DEFAULT_PATH);
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

    fn pull_emulator_state(&mut self, frame: &mut eframe::Frame) {
        let snapshot = self.console.drain();
        let _ = frame;
        self.presenter.sync();
        self.log.extend(snapshot.console_lines.iter().map(|l| l.trim_end().to_owned()));
        if let Some(error) = snapshot.load_error {
            self.log.push(error);
        }
        if let Some(Status::Crashed(message)) = &snapshot.status {
            if self.status != snapshot.status {
                self.log.push(format!("emulation stopped: {message}"));
            }
        }
        let overflow = self.log.len().saturating_sub(LOG_LINES);
        self.log.drain(..overflow);
        self.status = snapshot.status;
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
        let pad = merge(keyboard, self.controllers.gamepad(&self.config.pad_bindings));
        if pad != self.last_gamepad {
            self.last_gamepad = pad;
            self.console.send(ConsoleCommand::GamePad(pad));
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
                if ui.add_enabled(self.title.is_some(), egui::Button::new("Stop")).clicked() {
                    self.console.send(ConsoleCommand::Stop);
                    ui.close();
                }
            });
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut self.show_library, "Game library");
                ui.checkbox(&mut self.show_log, "Guest log");
                ui.separator();
                self.upscale_menu(ui);
                ui.separator();
                self.frame_rate_menu(ui);
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
        ui.label("Frame rate (applies on the next boot)");
        let mut chosen = self.config.swap_interval;
        if ui.radio(chosen.is_none(), "As the title asks").clicked() {
            chosen = None;
        }
        let forced = ui.radio(chosen == Some(1), "Force 60 fps").on_hover_text(
            "A title made for 30 fps whose logic counts frames, such as Twilight Princess HD, \
             then runs at up to double speed.",
        );
        if forced.clicked() {
            chosen = Some(1);
        }
        if chosen != self.config.swap_interval {
            self.config.swap_interval = chosen;
            self.console.send(ConsoleCommand::SwapInterval(chosen));
            self.save_config();
        }
    }

    fn upscale_menu(&mut self, ui: &mut egui::Ui) {
        ui.add_enabled_ui(upscale::available(), |ui| {
            ui.label("Screen upscaling (xBRZ)");
            let mut changed = false;
            if ui.radio(self.config.upscale.method == UpscaleMethod::None, "Off").clicked() {
                self.config.upscale.method = UpscaleMethod::None;
                changed = true;
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

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let state = match (&self.status, self.paused) {
                (Some(Status::Running), true) => "Paused".to_owned(),
                (Some(Status::Running), false) => format!("Running · {:.1} fps", self.fps),
                (Some(Status::Exited(code)), _) => format!("Exited ({code})"),
                (Some(Status::Crashed(_)), _) => "Crashed".to_owned(),
                (Some(Status::Idle) | None, _) => "Idle".to_owned(),
            };
            ui.label(state);
            ui.separator();
            ui.label(self.title.as_deref().unwrap_or("no title"));
            ui.separator();
            #[allow(clippy::cast_precision_loss)]
            ui.label(format!("{:.1} M instructions", self.instructions as f64 / 1e6));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(format!("{} ({})", self.active_api.label(), Presenter::backend_name()));
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
    fn screens(&self, ui: &mut egui::Ui) {
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

        ui.horizontal_centered(|ui| {
            for (screen, size) in Screen::ALL.into_iter().zip(sizes) {
                let size = size * scale;
                match self.presenter.view(screen) {
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
    }
}

fn short_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

impl eframe::App for FloraApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.pull_emulator_state(frame);
        self.poll_input(ui.ctx());

        egui::Panel::top("menu").show(ui, |ui| self.menu(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.show_library {
            egui::Panel::left("library")
                .resizable(true)
                .default_size(260.0)
                .show(ui, |ui| self.library_panel(ui));
        }
        if self.show_log {
            egui::Panel::bottom("log")
                .resizable(true)
                .default_size(140.0)
                .show(ui, |ui| self.log_panel(ui));
        }
        egui::CentralPanel::default().show(ui, |ui| self.screens(ui));
    }
}
