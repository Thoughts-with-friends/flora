//! `cargo xtask icon`: turns `gui/egui/assets/shocora.svg` into the images the program uses.
//!
//! ```text
//!   shocora.svg ──resvg──► shocora.png  window icon, compiled in by main.rs
//!               └────────► shocora.ico  executable icon, compiled in by build.rs
//! ```
//!
//! Every size is rendered from the SVG itself, so that small icons stay sharp.
use std::path::{Path, PathBuf};

use resvg::{tiny_skia, usvg};

/// Side of the window icon, in pixels.
const PNG_SIZE: u32 = 256;
/// Sides stored in the `.ico`: what Windows picks from for lists, the taskbar and Explorer.
const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// `gui/egui/assets`, wherever cargo was started from.
fn assets_dir() -> PathBuf {
    let xtask = Path::new(env!("CARGO_MANIFEST_DIR"));
    xtask.parent().unwrap_or(xtask).join("gui").join("egui").join("assets")
}

/// Renders the SVG to a PNG file's bytes, `size` pixels on each side.
fn render_png(tree: &usvg::Tree, size: u32) -> Result<Vec<u8>, String> {
    let mut pixmap = tiny_skia::Pixmap::new(size, size).ok_or("icon size must not be zero")?;
    let svg = tree.size();
    let scale = size as f32 / svg.width().max(svg.height());
    let transform = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(tree, transform, &mut pixmap.as_mut());
    pixmap.encode_png().map_err(|error| error.to_string())
}

/// An `.ico` file holding `images`, each `(side, PNG bytes)`.
fn build_ico(images: &[(u32, Vec<u8>)]) -> Result<Vec<u8>, String> {
    const HEADER_SIZE: usize = 6;
    const ENTRY_SIZE: usize = 16;
    let count = u16::try_from(images.len()).map_err(|_| "too many icon images")?;
    let mut file = Vec::new();
    file.extend_from_slice(&[0, 0, 1, 0]); // reserved, type: icon
    file.extend_from_slice(&count.to_le_bytes());
    let mut offset = HEADER_SIZE + ENTRY_SIZE * images.len();
    for (side, png) in images {
        // A side of 256 is stored as 0: the field is one byte.
        let stored = u8::try_from(*side % 256).map_err(|_| "icon side out of range")?;
        file.extend_from_slice(&[stored, stored, 0, 0]); // width, height, palette, reserved
        file.extend_from_slice(&1_u16.to_le_bytes()); // colour planes
        file.extend_from_slice(&32_u16.to_le_bytes()); // bits per pixel
        let size = u32::try_from(png.len()).map_err(|_| "icon image too large")?;
        let at = u32::try_from(offset).map_err(|_| "icon file too large")?;
        file.extend_from_slice(&size.to_le_bytes());
        file.extend_from_slice(&at.to_le_bytes());
        offset += png.len();
    }
    for (_, png) in images {
        file.extend_from_slice(png);
    }
    Ok(file)
}

/// Converts the SVG and writes both files.
pub fn run() -> Result<(), String> {
    let assets = assets_dir();
    let source = assets.join("shocora.svg");
    let svg = std::fs::read(&source).map_err(|error| format!("{}: {error}", source.display()))?;
    let tree = usvg::Tree::from_data(&svg, &usvg::Options::default())
        .map_err(|error| format!("{}: {error}", source.display()))?;

    let write = |name: &str, bytes: &[u8]| {
        let path = assets.join(name);
        std::fs::write(&path, bytes).map_err(|error| format!("{}: {error}", path.display()))?;
        println!("{} ({} bytes)", path.display(), bytes.len());
        Ok::<(), String>(())
    };
    write("shocora.png", &render_png(&tree, PNG_SIZE)?)?;
    let images = ICO_SIZES
        .iter()
        .map(|side| Ok((*side, render_png(&tree, *side)?)))
        .collect::<Result<Vec<_>, String>>()?;
    write("shocora.ico", &build_ico(&images)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ico_directory_points_at_each_image() {
        let file = build_ico(&[(16, vec![1, 2, 3]), (256, vec![4, 5])]).unwrap();
        assert_eq!(&file[..6], &[0, 0, 1, 0, 2, 0]);
        // First entry: 16 x 16, three bytes, right after the directory.
        assert_eq!((file[6], file[7]), (16, 16));
        assert_eq!(&file[14..18], &3_u32.to_le_bytes());
        assert_eq!(&file[18..22], &38_u32.to_le_bytes());
        // Second entry: 256 is stored as 0.
        assert_eq!((file[22], file[23]), (0, 0));
        assert_eq!(&file[34..38], &41_u32.to_le_bytes());
        assert_eq!(&file[38..], &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn the_project_svg_renders() {
        let svg = std::fs::read(assets_dir().join("shocora.svg")).unwrap();
        let tree = usvg::Tree::from_data(&svg, &usvg::Options::default()).unwrap();
        let png = render_png(&tree, 32).unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }
}
