//! The start screen, laid out like the console's own menu: pick a title, start it.
//!
//! ```text
//!   +----------------------------------------------------+
//!   |              . . . . (page dots)            (TV<>) |  <- transfer icon (disabled)
//!   |   [disc]  [game]  [tool]  [    ]  [    ]           |
//!   |   [    ]  [    ]  [    ]  [    ]  [    ]        >  |  <- 5 x 3 tiles, next page
//!   |   [Wii ]  [    ]  [    ]  [    ]  [    ]           |
//!   |   usb   :) people bag globe tv bubble box          |  <- feature icons (disabled)
//!   +----------------------------------------------------+
//!
//!   click a title      -> blue frame, "start" band and its name in a balloon
//!   click it again     -> the launch transition, then the emulator's screens:
//!
//!     Launch (tile pops, soft rings spread, white fills) -> boot -> Reveal (white fades)
//! ```
//!
//! Only titles respond to input: the other icons are pictures, drawn here so that
//! the program ships no image files.
//!
//! The backdrop, from the bottom up: what is behind the window, the picture chosen in the
//! settings if any, the chosen color with its alpha, and without a picture a faint weave.
use std::{
    collections::HashMap,
    f32::consts::{PI, TAU},
    path::{Path, PathBuf},
};

use eframe::egui::{
    self, Align2, Color32, FontId, Id, LayerId, Order, Painter, Pos2, Rect, Sense, Shape, Stroke,
    StrokeKind, TextureHandle, TextureOptions, Vec2, pos2, vec2,
};
use shocora_game_library::GameEntry;

use crate::lang::tr;

mod icons;
mod images;
mod paint;
use icons::*;
use images::*;
use paint::*;

/// Tiles across and down one page.
const COLUMNS: usize = 5;
const ROWS: usize = 3;
const PAGE_SLOTS: usize = COLUMNS * ROWS;
/// Size the layout was designed for; everything scales from it.
const DESIGN_SIZE: Vec2 = vec2(1072.0, 606.0);
/// Length of the wave before the title boots, in seconds.
const WAVE_SECONDS: f64 = 0.75;
/// How long the white screen takes to clear after the boot, in seconds.
const REVEAL_SECONDS: f64 = 0.45;

const SELECTION_BLUE: Color32 = Color32::from_rgb(66, 180, 230);
const INK: Color32 = Color32::from_rgb(70, 70, 76);

/// What a tile of the grid holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// Index into the library.
    Game(usize),
    Disc,
    Settings,
    Wii,
    Empty,
}

/// The transition from the menu to a running title.
#[derive(Debug)]
enum Transition {
    /// The wave spreads from `from`; `path` boots when it covers the window.
    Launch { from: Pos2, started: f64, path: PathBuf, game: usize },
    /// The title booted: the white clears.
    Reveal { started: f64 },
}

/// State of the start screen.
#[derive(Default)]
pub struct Launcher {
    /// Icon of each title by path; `None` if it has none we can read.
    icons: HashMap<PathBuf, Option<TextureHandle>>,
    /// Library index of the selected title.
    selected: Option<usize>,
    page: usize,
    transition: Option<Transition>,
    /// The backdrop picture and the path it came from.
    picture: Option<(PathBuf, Result<TextureHandle, String>)>,
}

impl std::fmt::Debug for Launcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Launcher")
            .field("selected", &self.selected)
            .field("page", &self.page)
            .finish_non_exhaustive()
    }
}

/// Where the grid puts titles and the fixed pictures: pictures keep their tiles on
/// the first page, titles take the free ones in order, over as many pages as needed.
fn layout_slots(games: usize) -> Vec<Slot> {
    let mut slots = Vec::with_capacity(PAGE_SLOTS.max(games + 3));
    let mut next = 0;
    while next < games || slots.len() < PAGE_SLOTS || slots.len() % PAGE_SLOTS != 0 {
        slots.push(match slots.len() {
            0 => Slot::Disc,
            2 => Slot::Settings,
            10 => Slot::Wii,
            _ if next < games => {
                next += 1;
                Slot::Game(next - 1)
            }
            _ => Slot::Empty,
        });
    }
    slots
}

/// Quick out, gentle in: the feel of something released rather than pushed.
fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

impl Launcher {
    /// Draws the menu on `background`; returns the title to boot when its launch wave has
    /// finished.
    pub fn show(
        &mut self,
        ui: &egui::Ui,
        games: &[GameEntry],
        background: Color32,
        picture: Option<&Path>,
    ) -> Option<PathBuf> {
        let area = ui.available_rect_before_wrap();
        let scale = (area.width() / DESIGN_SIZE.x).min(area.height() / DESIGN_SIZE.y).max(0.2);
        let origin = area.center() - DESIGN_SIZE * scale * 0.5;
        let at = |x: f32, y: f32| origin + vec2(x, y) * scale;
        let painter = ui.painter_at(area);
        self.update_picture(ui.ctx(), picture);
        let texture = self.picture.as_ref().and_then(|(_, loaded)| loaded.as_ref().ok());
        paint_background(&painter, area, background, texture);

        let slots = layout_slots(games.len());
        let pages = slots.len() / PAGE_SLOTS;
        self.page = self.page.min(pages - 1);
        self.keyboard(ui, games, &slots);
        paint_page_dots(&painter, at(536.0, 44.0), scale, self.page, pages);

        let now = ui.input(|input| input.time);
        let launching = match &self.transition {
            Some(Transition::Launch { game, started, .. }) => {
                Some((*game, ((now - started) / WAVE_SECONDS).min(1.0) as f32))
            }
            _ => None,
        };

        let tile = 128.0 * scale;
        let mut balloon = None;
        let mut clicked_empty = false;

        for (index, slot) in slots.iter().skip(self.page * PAGE_SLOTS).take(PAGE_SLOTS).enumerate()
        {
            let (column, row) = (index % COLUMNS, index / COLUMNS);
            let center =
                at((column as f32).mul_add(156.0, 228.0), (row as f32).mul_add(142.0, 148.0));
            let rect = Rect::from_center_size(center, Vec2::splat(tile));

            match *slot {
                Slot::Game(game) => {
                    let response = ui.interact(
                        rect,
                        ui.scope_id().with(("launcher tile", game)),
                        Sense::click(),
                    );
                    if response.clicked() && launching.is_none() {
                        if self.selected == Some(game) {
                            self.start(game, center, games, now);
                        } else {
                            self.selected = Some(game);
                        }
                    }
                    let selected = self.selected == Some(game);
                    // The tile leaps a little as the wave leaves it.
                    let pop = launching
                        .filter(|(launched, _)| *launched == game)
                        .map_or(0.0, |(_, t)| (t * PI).sin() * 0.14);
                    let hover = if response.hovered() && !selected { 0.03 } else { 0.0 };
                    let rect = rect.expand(tile * (pop + hover));
                    let icon = self
                        .icons
                        .entry(games[game].path.clone())
                        .or_insert_with(|| load_icon(ui.ctx(), &games[game].path));
                    paint_game_tile(ui, &painter, rect, icon.as_ref(), &games[game], scale);
                    if selected {
                        paint_selection(&painter, rect, scale);
                        balloon = Some((rect, games[game].name.as_str()));
                    }
                }
                Slot::Empty => {
                    paint_glass_tile(&painter, rect, scale, 90);
                    clicked_empty |= ui
                        .interact(
                            rect,
                            ui.scope_id().with(("launcher empty", index)),
                            Sense::click(),
                        )
                        .clicked();
                }
                Slot::Disc => {
                    paint_glass_tile(&painter, rect, scale, 200);
                    paint_disc(&painter, center, tile * 0.32);
                }
                Slot::Settings => {
                    paint_glass_tile(&painter, rect, scale, 200);
                    paint_wrench(&painter, center, tile * 0.34);
                }
                Slot::Wii => {
                    paint_glass_tile(&painter, rect, scale, 200);
                    paint_wii(&painter, center, tile * 0.34);
                }
            }
        }

        if clicked_empty {
            self.selected = None;
        }
        if let Some((rect, name)) = balloon {
            paint_balloon(ui, rect, name, scale);
        }

        self.page_arrow(ui, &painter, at(1012.0, 290.0), scale, pages);
        paint_dock(&painter, &at, scale);
        paint_transfer(&painter, at(1014.0, 54.0), scale);

        self.finish_launch(now)
    }

    /// Loads the backdrop picture when the path changed.
    fn update_picture(&mut self, ctx: &egui::Context, path: Option<&Path>) {
        let Some(path) = path else {
            self.picture = None;
            return;
        };
        if self.picture.as_ref().is_none_or(|(loaded, _)| loaded != path) {
            self.picture = Some((path.to_owned(), load_picture(ctx, path)));
        }
    }

    /// Why the backdrop picture could not be shown.
    pub fn picture_error(&self) -> Option<&str> {
        self.picture.as_ref().and_then(|(_, loaded)| loaded.as_ref().err()).map(String::as_str)
    }

    /// Arrow keys walk the titles, Enter starts the selected one.
    fn keyboard(&mut self, ui: &egui::Ui, games: &[GameEntry], slots: &[Slot]) {
        // Typing in a settings field must not start a game.
        if games.is_empty() || self.transition.is_some() || ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        let step = ui.input(|input| {
            if input.key_pressed(egui::Key::ArrowRight) {
                1
            } else if input.key_pressed(egui::Key::ArrowLeft) {
                games.len() - 1
            } else {
                0
            }
        });

        if step != 0 {
            let next = self.selected.map_or(0, |selected| (selected + step) % games.len());
            self.selected = Some(next);
            if let Some(position) = slots.iter().position(|slot| *slot == Slot::Game(next)) {
                self.page = position / PAGE_SLOTS;
            }
        }

        if let Some(selected) = self.selected {
            if ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                let center = ui.available_rect_before_wrap().center();
                let now = ui.input(|input| input.time);
                self.start(selected, center, games, now);
            }
        }
    }

    fn start(&mut self, game: usize, from: Pos2, games: &[GameEntry], now: f64) {
        self.transition =
            Some(Transition::Launch { from, started: now, path: games[game].path.clone(), game });
    }

    /// The wave is over: the title may boot while the screen is white.
    fn finish_launch(&mut self, now: f64) -> Option<PathBuf> {
        match &self.transition {
            Some(Transition::Launch { started, path, .. }) if now - started >= WAVE_SECONDS => {
                let path = path.clone();
                self.transition = Some(Transition::Reveal { started: now });
                Some(path)
            }
            _ => None,
        }
    }

    /// The next page, if there is one, or back to the first.
    fn page_arrow(
        &mut self,
        ui: &egui::Ui,
        painter: &Painter,
        center: Pos2,
        scale: f32,
        pages: usize,
    ) {
        if pages < 2 {
            return;
        }
        let rect = Rect::from_center_size(center, vec2(70.0, 130.0) * scale);
        let response = ui.interact(rect, ui.scope_id().with("launcher next page"), Sense::click());
        let alpha = if response.hovered() { 255 } else { 190 };

        painter.add(Shape::convex_polygon(
            vec![
                center + vec2(-28.0, -60.0) * scale,
                center + vec2(32.0, 0.0) * scale,
                center + vec2(-28.0, 60.0) * scale,
            ],
            Color32::from_white_alpha(alpha),
            Stroke::new(1.5 * scale, Color32::from_gray(200)),
        ));

        if response.clicked() {
            self.page = (self.page + 1) % pages;
        }
    }

    /// Paints the transition over everything; call last in the frame.
    pub fn paint_transition(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|input| input.time);
        let screen = ctx.content_rect();
        let painter =
            ctx.layer_painter(LayerId::new(Order::Foreground, Id::NULL.with("launcher wave")));

        match &self.transition {
            None => return,
            Some(Transition::Launch { from, started, .. }) => {
                let t = ((now - started) / WAVE_SECONDS).clamp(0.0, 1.0) as f32;
                paint_wave(&painter, screen, *from, t);
            }
            Some(Transition::Reveal { started }) => {
                let t = ((now - started) / REVEAL_SECONDS).clamp(0.0, 1.0) as f32;
                if t >= 1.0 {
                    self.transition = None;
                    return;
                }
                let alpha = ((1.0 - t) * 255.0) as u8;
                painter.rect_filled(screen, 0.0, Color32::from_white_alpha(alpha));
            }
        }
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(kind: u8, width: u8, height: u8, depth: u8, descriptor: u8) -> Vec<u8> {
        let mut file = vec![0; 18];
        file[2] = kind;
        file[12] = width;
        file[14] = height;
        file[16] = depth;
        file[17] = descriptor;
        file
    }

    #[test]
    fn tga_rows_come_out_top_first_as_rgba() {
        let mut file = header(2, 1, 2, 32, 0);
        // Stored bottom row first, each pixel BGRA.
        file.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let (size, rgba) = decode_tga(&file).unwrap();
        assert_eq!(size, [1, 2]);
        assert_eq!(rgba, [7, 6, 5, 8, 3, 2, 1, 4]);
    }

    #[test]
    fn tga_runs_expand_and_short_files_fail() {
        let mut file = header(10, 3, 1, 24, 0x20);
        file.extend_from_slice(&[0x82, 9, 8, 7]);
        let (_, rgba) = decode_tga(&file).unwrap();
        assert_eq!(rgba, [7, 8, 9, 255, 7, 8, 9, 255, 7, 8, 9, 255]);
        assert!(decode_tga(&file[..20]).is_none());
    }

    #[test]
    fn pictures_cover_the_area_by_cutting_the_long_side() {
        let wide = cover_uv(vec2(200.0, 100.0), vec2(100.0, 100.0));
        assert!((wide.width() - 0.5).abs() < 1e-5 && (wide.height() - 1.0).abs() < 1e-5);
        assert!((wide.center().x - 0.5).abs() < 1e-5);
        let tall = cover_uv(vec2(100.0, 400.0), vec2(100.0, 100.0));
        assert!((tall.width() - 1.0).abs() < 1e-5 && (tall.height() - 0.25).abs() < 1e-5);
    }

    #[test]
    fn titles_avoid_the_pictures_and_spill_to_a_next_page() {
        let slots = layout_slots(2);
        assert_eq!(slots.len(), PAGE_SLOTS);
        assert_eq!(slots[0], Slot::Disc);
        assert_eq!(slots[1], Slot::Game(0));
        assert_eq!(slots[2], Slot::Settings);
        assert_eq!(slots[3], Slot::Game(1));
        assert_eq!(slots[10], Slot::Wii);

        let slots = layout_slots(13);
        assert_eq!(slots.len(), 2 * PAGE_SLOTS);
        assert_eq!(slots[PAGE_SLOTS], Slot::Game(12));
    }
}
