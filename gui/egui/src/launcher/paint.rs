//! Painting the start screen: backdrop, tiles, selection and the launch wave.
use super::*;

/// Soft rings spreading from where the title was, ending in plain white.
pub(super) fn paint_wave(painter: &Painter, screen: Rect, from: Pos2, t: f32) {
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
pub(super) fn paint_background(
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

pub(super) fn paint_page_dots(
    painter: &Painter,
    center: Pos2,
    scale: f32,
    page: usize,
    pages: usize,
) {
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
pub(super) fn paint_glass_tile(painter: &Painter, rect: Rect, scale: f32, alpha: u8) {
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

pub(super) fn paint_game_tile(
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
pub(super) fn paint_selection(painter: &Painter, rect: Rect, scale: f32) {
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
pub(super) fn paint_balloon(ui: &egui::Ui, tile: Rect, name: &str, scale: f32) {
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
