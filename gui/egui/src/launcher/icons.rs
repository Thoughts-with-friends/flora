//! The console menu's feature icons, drawn from shapes so that no image files ship.
use super::*;

pub(super) fn paint_disc(painter: &Painter, center: Pos2, radius: f32) {
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

pub(super) fn paint_wrench(painter: &Painter, center: Pos2, size: f32) {
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

pub(super) fn paint_wii(painter: &Painter, center: Pos2, size: f32) {
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
pub(super) fn paint_dock(painter: &Painter, at: &impl Fn(f32, f32) -> Pos2, scale: f32) {
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
pub(super) fn paint_transfer(painter: &Painter, center: Pos2, scale: f32) {
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
