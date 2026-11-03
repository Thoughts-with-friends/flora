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

/// Decodes an uncompressed or run-length TGA (what `iconTex.tga` is) to RGBA, top row first.
fn decode_tga(file: &[u8]) -> Option<([usize; 2], Vec<u8>)> {
    let header = file.get(..18)?;
    let id_size = usize::from(header[0]);
    let (kind, depth) = (header[2], header[16]);
    if header[1] != 0 || !matches!(kind, 2 | 10) || !matches!(depth, 24 | 32) {
        return None;
    }
    let width = usize::from(u16::from_le_bytes([header[12], header[13]]));
    let height = usize::from(u16::from_le_bytes([header[14], header[15]]));
    let top_down = header[17] & 0x20 != 0;
    let bytes = usize::from(depth) / 8;
    let mut data = file.get(18 + id_size..)?;
    let mut pixels: Vec<&[u8]> = Vec::with_capacity(width * height);
    while pixels.len() < width * height {
        if kind == 2 {
            pixels.push(data.get(..bytes)?);
            data = &data[bytes..];
            continue;
        }
        let packet = *data.first()?;
        data = &data[1..];
        let count = usize::from(packet & 0x7F) + 1;
        if packet & 0x80 != 0 {
            let pixel = data.get(..bytes)?;
            data = &data[bytes..];
            pixels.extend(std::iter::repeat_n(pixel, count));
        } else {
            for _ in 0..count {
                pixels.push(data.get(..bytes)?);
                data = &data[bytes..];
            }
        }
    }
    let mut rgba = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        let row = if top_down { row } else { height - 1 - row };
        for pixel in &pixels[row * width..(row + 1) * width] {
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
            rgba.push(pixel.get(3).copied().unwrap_or(255));
        }
    }
    Some(([width, height], rgba))
}

fn load_icon(ctx: &egui::Context, title: &Path) -> Option<TextureHandle> {
    let file = std::fs::read(title.join("meta").join("iconTex.tga")).ok()?;
    let (size, rgba) = decode_tga(&file)?;
    let image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
    Some(ctx.load_texture(title.display().to_string(), image, TextureOptions::LINEAR))
}

/// Largest side of a backdrop picture kept: more only takes memory.
const PICTURE_MAX_SIDE: u32 = 4096;

fn load_picture(ctx: &egui::Context, path: &Path) -> Result<TextureHandle, String> {
    let picture = image::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let picture = if picture.width().max(picture.height()) > PICTURE_MAX_SIDE {
        picture.thumbnail(PICTURE_MAX_SIDE, PICTURE_MAX_SIDE)
    } else {
        picture
    };
    let rgba = picture.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
    Ok(ctx.load_texture("launcher picture", image, TextureOptions::LINEAR))
}

/// Part of a picture that covers an area without distorting: the rest is cut off evenly.
fn cover_uv(picture: Vec2, area: Vec2) -> Rect {
    let (picture_aspect, area_aspect) = (picture.x / picture.y, area.x / area.y);
    let (width, height) = if picture_aspect > area_aspect {
        (area_aspect / picture_aspect, 1.0)
    } else {
        (1.0, picture_aspect / area_aspect)
    };
    Rect::from_center_size(pos2(0.5, 0.5), vec2(width, height))
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

/// Soft rings spreading from where the title was, ending in plain white.
fn paint_wave(painter: &Painter, screen: Rect, from: Pos2, t: f32) {
    let reach =
        [screen.left_top(), screen.right_top(), screen.left_bottom(), screen.right_bottom()]
            .iter()
            .map(|corner| corner.distance(from))
            .fold(0.0, f32::max);

    // White spreads behind the front and takes over at the end.
    painter.circle_filled(
        from,
        reach * ease_out(t),
        Color32::from_white_alpha((t * t * 255.0) as u8),
    );

    for ring in 0..4 {
        let lag = ring as f32 * 0.09;
        let local = ((t - lag) / (1.0 - lag)).clamp(0.0, 1.0);
        if local <= 0.0 {
            continue;
        }
        let radius = reach * ease_out(local);
        let fade = (1.0 - local) * (ring as f32).mul_add(-0.2, 1.0);
        // Several strokes make the edge feather instead of being a hard line.
        for layer in 0..5 {
            let spread = (layer as f32).mul_add(7.0, 3.0);
            let alpha = (fade * 60.0 / (layer as f32 + 1.0)) as u8;
            painter.circle_stroke(
                from,
                (radius - spread).max(0.0),
                Stroke::new(spread, Color32::from_rgba_unmultiplied(200, 232, 248, alpha)),
            );
        }
    }
}

/// The pale, faintly woven backdrop.
fn paint_background(
    painter: &Painter,
    area: Rect,
    color: Color32,
    picture: Option<&TextureHandle>,
) {
    if let Some(picture) = picture {
        painter.image(
            picture.id(),
            area,
            cover_uv(picture.size_vec2(), area.size()),
            Color32::WHITE,
        );
    }
    painter.rect_filled(area, 0.0, color);
    // The weave is for a plain color: over a picture it is only a grid in the way.
    if picture.is_some() {
        return;
    }
    let line = Stroke::new(1.0, Color32::from_white_alpha(70));
    let every_5 = |from: f32, to: f32| {
        std::iter::successors(Some(from), |at| Some(at + 5.0)).take_while(move |at| *at < to)
    };
    for x in every_5(area.left(), area.right()) {
        painter.line_segment([pos2(x, area.top()), pos2(x, area.bottom())], line);
    }
    for y in every_5(area.top(), area.bottom()) {
        painter.line_segment([pos2(area.left(), y), pos2(area.right(), y)], line);
    }
}

fn paint_page_dots(painter: &Painter, center: Pos2, scale: f32, page: usize, pages: usize) {
    let shown = pages.max(6);
    for index in 0..shown {
        let x = (index as f32 - (shown as f32 - 1.0) / 2.0) * 26.0 * scale;
        let size = if index == page { 18.0 } else { 11.0 } * scale;
        let color = if index == page { SELECTION_BLUE } else { Color32::from_gray(190) };
        let rect = Rect::from_center_size(center + vec2(x, 0.0), Vec2::splat(size));
        painter.rect_filled(rect, 2.0 * scale, color);
    }
}

/// An unused tile: frosted glass, more solid when it holds something.
fn paint_glass_tile(painter: &Painter, rect: Rect, scale: f32, alpha: u8) {
    let radius = 22.0 * scale;
    painter.rect_filled(rect, radius, Color32::from_white_alpha(alpha));
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(2.0 * scale, Color32::from_white_alpha(230)),
        StrokeKind::Inside,
    );
    painter.rect_stroke(
        rect.expand(scale),
        radius,
        Stroke::new(scale, Color32::from_gray(205)),
        StrokeKind::Outside,
    );
}

fn paint_game_tile(
    ui: &egui::Ui,
    painter: &Painter,
    rect: Rect,
    icon: Option<&TextureHandle>,
    game: &GameEntry,
    scale: f32,
) {
    paint_glass_tile(painter, rect, scale, 235);
    let inner = rect.shrink(8.0 * scale);
    if let Some(icon) = icon {
        egui::Image::new((icon.id(), inner.size())).corner_radius(16.0 * scale).paint_at(ui, inner);
    } else {
        // No icon file (a bare RPX): the name stands in.
        let galley = painter.layout(
            game.name.clone(),
            FontId::proportional(15.0 * scale),
            INK,
            8.0_f32.mul_add(-scale, inner.width()),
        );
        let position = inner.center() - galley.size() * 0.5;
        painter.galley(position, galley, INK);
    }
}

/// Blue frame with corner brackets and the "start" band.
fn paint_selection(painter: &Painter, rect: Rect, scale: f32) {
    let radius = 20.0 * scale;
    painter.rect_stroke(
        rect.expand(2.0 * scale),
        radius,
        Stroke::new(7.0 * scale, SELECTION_BLUE),
        StrokeKind::Outside,
    );
    let band = Rect::from_min_max(
        pos2(rect.left(), 30.0_f32.mul_add(-scale, rect.bottom())),
        rect.right_bottom(),
    );
    painter.rect_filled(band, 8.0 * scale, SELECTION_BLUE);
    painter.text(
        band.center(),
        Align2::CENTER_CENTER,
        tr("launcher.start"),
        FontId::proportional(19.0 * scale),
        Color32::WHITE,
    );
    let outer = rect.expand(10.0 * scale);
    let arm = 18.0 * scale;
    let stroke = Stroke::new(7.0 * scale, SELECTION_BLUE);
    for (corner, dx, dy) in [
        (outer.left_top(), 1.0, 1.0),
        (outer.right_top(), -1.0, 1.0),
        (outer.left_bottom(), 1.0, -1.0),
        (outer.right_bottom(), -1.0, -1.0),
    ] {
        painter.line_segment([corner, corner + vec2(arm * dx, 0.0)], stroke);
        painter.line_segment([corner, corner + vec2(0.0, arm * dy)], stroke);
    }
}

/// The selected title's name in a speech balloon under its tile, above everything else.
fn paint_balloon(ui: &egui::Ui, tile: Rect, name: &str, scale: f32) {
    let painter =
        ui.ctx().layer_painter(LayerId::new(Order::Tooltip, Id::NULL.with("launcher balloon")));
    let galley =
        painter.layout(name.to_owned(), FontId::proportional(30.0 * scale), INK, 620.0 * scale);
    let size = galley.size() + vec2(40.0, 26.0) * scale;
    let top = 26.0_f32.mul_add(scale, tile.bottom());
    let rect = Rect::from_min_size(pos2(tile.center().x - size.x / 2.0, top), size);
    let tip = pos2(tile.center().x, 8.0_f32.mul_add(scale, tile.bottom()));
    painter.add(Shape::convex_polygon(
        vec![
            tip,
            pos2(14.0_f32.mul_add(scale, tip.x), top + 1.0),
            pos2(14.0_f32.mul_add(-scale, tip.x), top + 1.0),
        ],
        Color32::from_white_alpha(245),
        Stroke::NONE,
    ));
    painter.rect_filled(rect, 18.0 * scale, Color32::from_white_alpha(245));
    painter.galley(rect.min + vec2(20.0, 13.0) * scale, galley, INK);
}

// ---- Pictures of the console's other features. They do nothing. ---------------------

fn paint_disc(painter: &Painter, center: Pos2, radius: f32) {
    painter.circle_filled(center, radius, Color32::from_rgb(214, 218, 224));
    for (fraction, color) in [
        (0.92, Color32::from_rgb(190, 214, 150)),
        (0.78, Color32::from_rgb(232, 210, 150)),
        (0.64, Color32::from_rgb(200, 186, 226)),
        (0.5, Color32::from_rgb(170, 214, 228)),
    ] {
        painter.circle_stroke(center, radius * fraction, Stroke::new(radius * 0.12, color));
    }
    painter.circle_filled(center, radius * 0.3, Color32::from_gray(150));
    painter.circle_filled(center, radius * 0.14, Color32::from_gray(238));
}

fn paint_wrench(painter: &Painter, center: Pos2, size: f32) {
    let gray = Color32::from_gray(125);
    let along = vec2(1.0, -1.0).normalized();
    let head = center + along * size * 0.62;
    painter.line_segment([center - along * size * 0.85, head], Stroke::new(size * 0.42, gray));
    painter.circle_filled(center - along * size * 0.85, size * 0.21, gray);
    painter.circle_filled(head, size * 0.46, gray);
    // The jaw: a bite out of the head.
    let bite = head + along * size * 0.34;
    let side = vec2(along.y, -along.x) * size * 0.2;
    painter.add(Shape::convex_polygon(
        vec![
            bite - side - along * size * 0.34,
            bite + side - along * size * 0.34,
            bite + side + along * size * 0.3,
            bite - side + along * size * 0.3,
        ],
        Color32::from_white_alpha(235),
        Stroke::NONE,
    ));
}

fn paint_wii(painter: &Painter, center: Pos2, size: f32) {
    painter.text(
        center - vec2(0.0, size * 0.2),
        Align2::CENTER_CENTER,
        "Wii",
        FontId::proportional(size * 1.05),
        Color32::from_gray(135),
    );
    let points: Vec<Pos2> = (0..=24)
        .map(|step| {
            let t = step as f32 / 24.0;
            center
                + vec2((t - 0.5) * size * 2.2, ((t * PI).sin() * size).mul_add(0.14, size * 0.72))
        })
        .collect();
    painter.add(Shape::line(points, Stroke::new(size * 0.09, SELECTION_BLUE)));
}

/// The row of feature icons along the bottom.
fn paint_dock(painter: &Painter, at: &impl Fn(f32, f32) -> Pos2, scale: f32) {
    let size = 28.0 * scale;
    let y = 556.0;
    paint_usb(painter, at(64.0, y + 12.0), size * 0.75);
    paint_smile(painter, at(200.0, y), size);
    paint_people(painter, at(314.0, y), size);
    paint_bag(painter, at(428.0, y), size);
    paint_globe(painter, at(542.0, y), size);
    paint_television(painter, at(656.0, y), size);
    paint_bubble(painter, at(775.0, y), size);
    paint_download(painter, at(885.0, y), size);
}

fn paint_usb(painter: &Painter, center: Pos2, size: f32) {
    let body = Rect::from_center_size(center, vec2(size * 1.9, size * 1.2));
    painter.rect_filled(body, size * 0.25, Color32::from_rgb(92, 188, 222));
    painter.rect_filled(
        Rect::from_center_size(center + vec2(0.0, size * 0.18), vec2(size * 1.3, size * 0.32)),
        size * 0.08,
        Color32::WHITE,
    );
}

fn paint_smile(painter: &Painter, center: Pos2, size: f32) {
    let orange = Color32::from_rgb(232, 138, 54);
    let rect = Rect::from_center_size(center, Vec2::splat(size * 2.0));
    painter.rect_filled(rect, size * 0.26, orange);
    painter.rect_filled(rect.shrink(size * 0.2), size * 0.14, Color32::from_rgb(252, 240, 224));
    for x in [-0.42, 0.42] {
        painter.circle_filled(center + vec2(x, -0.22) * size, size * 0.13, orange);
    }
    let smile: Vec<Pos2> = (0..=16)
        .map(|step| {
            let a = PI * step as f32 / 16.0;
            center + vec2(a.cos() * 0.5, a.sin().mul_add(0.36, 0.12)) * size
        })
        .collect();
    painter.add(Shape::line(smile, Stroke::new(size * 0.13, orange)));
}

fn paint_people(painter: &Painter, center: Pos2, size: f32) {
    let green = Color32::from_rgb(66, 158, 74);
    for (x, y, scale) in [(-0.68, 0.14, 0.72), (0.68, 0.14, 0.72), (0.0, -0.1, 1.0)] {
        let at = center + vec2(x, y) * size;
        let edge = Stroke::new(size * 0.07, Color32::WHITE);
        painter.circle(at - vec2(0.0, 0.46) * size * scale, size * 0.34 * scale, green, edge);
        painter.rect(
            Rect::from_center_size(
                at + vec2(0.0, 0.36) * size * scale,
                vec2(0.86, 0.96) * size * scale,
            ),
            size * 0.32 * scale,
            green,
            edge,
            StrokeKind::Outside,
        );
    }
}

fn paint_bag(painter: &Painter, center: Pos2, size: f32) {
    let gold = Color32::from_rgb(232, 170, 40);
    let handle: Vec<Pos2> = (0..=16)
        .map(|step| {
            let a = PI * step as f32 / 16.0;
            center + vec2(a.cos() * 0.44, a.sin().mul_add(-0.5, -0.42)) * size
        })
        .collect();
    painter.add(Shape::line(handle, Stroke::new(size * 0.11, Color32::from_rgb(150, 110, 30))));
    painter.add(Shape::convex_polygon(
        vec![
            center + vec2(-0.82, -0.44) * size,
            center + vec2(0.82, -0.44) * size,
            center + vec2(0.98, 1.0) * size,
            center + vec2(-0.98, 1.0) * size,
        ],
        gold,
        Stroke::NONE,
    ));
}

fn paint_globe(painter: &Painter, center: Pos2, size: f32) {
    let blue = Color32::from_rgb(28, 88, 200);
    let line = Stroke::new(size * 0.08, Color32::WHITE);
    let radius = size * 1.08;
    painter.circle_filled(center, radius, blue);
    painter.line_segment([center - vec2(radius, 0.0), center + vec2(radius, 0.0)], line);
    painter.line_segment([center - vec2(0.0, radius), center + vec2(0.0, radius)], line);
    // A meridian, then the parallels above and below the equator.
    let meridian: Vec<Pos2> = (0..=32)
        .map(|step| {
            let a = TAU * step as f32 / 32.0;
            center + vec2(a.cos() * 0.45, a.sin()) * radius
        })
        .collect();
    painter.add(Shape::line(meridian, line));
    for y in [-0.55_f32, 0.55] {
        let half = f32::mul_add(y, -y, 1.0).sqrt();
        painter.line_segment(
            [center + vec2(-half, y) * radius, center + vec2(half, y) * radius],
            line,
        );
    }
    painter.circle_stroke(center, radius, Stroke::new(size * 0.09, Color32::from_rgb(18, 60, 150)));
}

fn paint_television(painter: &Painter, center: Pos2, size: f32) {
    let red = Color32::from_rgb(218, 66, 52);
    let frame = Rect::from_center_size(center - vec2(0.0, 0.14) * size, vec2(2.1, 1.6) * size);
    painter.rect_filled(frame, size * 0.2, red);
    painter.rect_filled(frame.shrink(size * 0.22), size * 0.05, Color32::from_rgb(250, 232, 228));
    painter.rect_filled(
        Rect::from_center_size(center + vec2(0.0, 0.92) * size, vec2(1.2, 0.2) * size),
        size * 0.08,
        red,
    );
}

fn paint_bubble(painter: &Painter, center: Pos2, size: f32) {
    let blue = Color32::from_rgb(70, 150, 210);
    let body = Rect::from_center_size(center - vec2(0.0, 0.14) * size, vec2(2.1, 1.7) * size);
    painter.rect_filled(body, size * 0.22, blue);
    painter.add(Shape::convex_polygon(
        vec![
            center + vec2(-0.26, 0.68) * size,
            center + vec2(0.26, 0.68) * size,
            center + vec2(0.0, 1.14) * size,
        ],
        blue,
        Stroke::NONE,
    ));
    for row in [-0.52, -0.14, 0.24] {
        painter.line_segment(
            [center + vec2(-0.62, row) * size, center + vec2(0.62, row) * size],
            Stroke::new(size * 0.13, Color32::WHITE),
        );
    }
}

fn paint_download(painter: &Painter, center: Pos2, size: f32) {
    let blue = Color32::from_rgb(52, 130, 200);
    painter.rect_filled(
        Rect::from_center_size(center + vec2(0.0, 0.3) * size, vec2(1.8, 1.5) * size),
        size * 0.18,
        blue,
    );
    let arrow = Color32::from_rgb(120, 200, 235);
    painter.rect_filled(
        Rect::from_center_size(center - vec2(0.0, 0.68) * size, vec2(0.62, 0.88) * size),
        0.0,
        arrow,
    );
    painter.add(Shape::convex_polygon(
        vec![
            center + vec2(-0.62, -0.26) * size,
            center + vec2(0.62, -0.26) * size,
            center + vec2(0.0, 0.42) * size,
        ],
        arrow,
        Stroke::NONE,
    ));
}

/// The corner badge: moving the picture between TV and GamePad.
fn paint_transfer(painter: &Painter, center: Pos2, scale: f32) {
    painter.circle_filled(center, 54.0 * scale, Color32::from_white_alpha(170));
    let gray = Color32::from_gray(120);
    let line = Stroke::new(4.0 * scale, gray);
    painter.rect_stroke(
        Rect::from_center_size(center - vec2(0.0, 16.0) * scale, vec2(44.0, 26.0) * scale),
        3.0 * scale,
        line,
        StrokeKind::Inside,
    );
    painter.rect_stroke(
        Rect::from_center_size(center + vec2(0.0, 22.0) * scale, vec2(38.0, 20.0) * scale),
        5.0 * scale,
        line,
        StrokeKind::Inside,
    );
    painter.line_segment(
        [center - vec2(0.0, 9.0) * scale, center + vec2(0.0, 14.0) * scale],
        Stroke::new(4.0 * scale, SELECTION_BLUE),
    );
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
