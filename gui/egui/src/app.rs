//! Main window: menu, the two screens, status bar and guest log.
use std::path::{Path, PathBuf};

use eframe::egui;
use shocora_game_library::{DEFAULT_PATH, GameLibrary, RomPaths, RomPathsError};
use shocora_gui_common::{
    config::AppConfig,
    console::{Console, ConsoleCommand},
    input::{Controllers, merge},
};
use shocora_render::{GraphicsApi, Screen, UpscaleMethod, upscale};
use wiiu_emu::{GamePadState, Status};

use crate::{launcher::Launcher, presenter::Presenter};

/// Lines of guest output kept in the log panel.
const LOG_LINES: usize = 500;

pub struct ShocoraApp {
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
        let picture_path = config
            .launcher_image
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string());
        let mut app = Self {
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

    fn pull_emulator_state(&mut self, frame: &mut eframe::Frame) {
        let snapshot = self.console.drain();
        let _ = frame;
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
        ui.label("Frame rate");
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

    /// Follows the title's frame rate or forces 60 fps, at once.
    fn toggle_frame_rate(&mut self) {
        let chosen = if self.config.swap_interval.is_some() { None } else { Some(1) };
        self.config.swap_interval = chosen;
        self.console.send(ConsoleCommand::SwapInterval(chosen));
        self.save_config();
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
            let rate = if self.config.swap_interval.is_some() {
                "60 fps 強制"
            } else {
                "タイトル指定"
            };
            let fps = format!("{:.1} fps ({rate})", self.fps);
            if setting(
                ui,
                fps,
                "クリックでタイトル指定と 60 fps 強制を切り替え。\n\
                 30 fps 用のタイトルは強制すると最大で倍速になります。",
            ) {
                self.toggle_frame_rate();
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
        let in_menu = self.title.is_none() && !self.boot_pending;
        if self.show_log && !in_menu {
            egui::Panel::bottom("log")
                .resizable(true)
                .default_size(140.0)
                .show(ui, |ui| self.log_panel(ui));
        }
        if in_menu {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| self.menu_screen(ui));
        } else {
            egui::CentralPanel::default().show(ui, |ui| self.screens(ui));
        }
        self.launcher.paint_transition(ui.ctx());
    }
}
