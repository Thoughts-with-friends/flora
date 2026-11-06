//! Pictures the start screen loads: title icons (TGA) and the backdrop picture.
use super::*;

/// Decodes an uncompressed or run-length TGA (what `iconTex.tga` is) to RGBA, top row first.
pub(super) fn decode_tga(file: &[u8]) -> Option<([usize; 2], Vec<u8>)> {
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

pub(super) fn load_icon(ctx: &egui::Context, title: &Path) -> Option<TextureHandle> {
    let file = std::fs::read(title.join("meta").join("iconTex.tga")).ok()?;
    let (size, rgba) = decode_tga(&file)?;
    let image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);

    Some(ctx.load_texture(title.display().to_string(), image, TextureOptions::LINEAR))
}

/// Largest side of a backdrop picture kept: more only takes memory.
const PICTURE_MAX_SIDE: u32 = 4096;

pub(super) fn load_picture(ctx: &egui::Context, path: &Path) -> Result<TextureHandle, String> {
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
pub(super) fn cover_uv(picture: Vec2, area: Vec2) -> Rect {
    let (picture_aspect, area_aspect) = (picture.x / picture.y, area.x / area.y);
    let (width, height) = if picture_aspect > area_aspect {
        (area_aspect / picture_aspect, 1.0)
    } else {
        (1.0, picture_aspect / area_aspect)
    };
    Rect::from_center_size(pos2(0.5, 0.5), vec2(width, height))
}
