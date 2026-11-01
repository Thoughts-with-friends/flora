//! `OSGetSharedData`: the system fonts every title can map.
//!
//! Fonts are not part of flora. They are read on demand from `FLORA_FONTS`, a
//! `resources/sharedFonts` folder next to the working directory or executable
//! (`CafeCn.ttf`, `CafeKr.ttf`, `CafeStd.ttf`, `CafeTw.ttf`, as in a Cemu
//! installation), or else a host font, so text stays readable and titles that
//! insist on a font keep working.
use std::path::PathBuf;

use wiiu_hle_macros::hle_export;
use wiiu_mem_addr::SHARED;

use crate::hle::{HleContext, HleError};

const FONT_FILES: [&str; 4] = ["CafeCn.ttf", "CafeKr.ttf", "CafeStd.ttf", "CafeTw.ttf"];
/// Fonts available on any Windows machine, used when the real ones are missing.
const HOST_FALLBACKS: [&str; 2] = ["C:/Windows/Fonts/arial.ttf", "C:/Windows/Fonts/segoeui.ttf"];

/// Where the shared area is filled up to, and what is mapped.
#[derive(Debug, Default)]
pub struct SharedData {
    cursor: u32,
    /// `(address, size)` per font id.
    fonts: [Option<(u32, u32)>; 4],
}

fn font_bytes(id: usize) -> Option<Vec<u8>> {
    let name = FONT_FILES[id];
    let mut directories: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("FLORA_FONTS") {
        directories.push(dir.into());
    }
    directories.push(PathBuf::from("resources/sharedFonts"));
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join("resources/sharedFonts")))
    {
        directories.push(dir);
    }
    for dir in directories {
        if let Ok(bytes) = std::fs::read(dir.join(name)) {
            return Some(bytes);
        }
    }
    log::warn!("system font {name} not found; see FLORA_FONTS");
    HOST_FALLBACKS.iter().find_map(|path| std::fs::read(path).ok())
}

#[hle_export(lib = "coreinit", name = "OSGetSharedData")]
fn os_get_shared_data(
    ctx: &mut HleContext<'_>,
    area: u32,
    _flags: u32,
    pointer_out: u32,
    size_out: u32,
) -> Result<bool, HleError> {
    let id = area as usize;
    if id >= FONT_FILES.len() {
        return Ok(false);
    }
    let entry = match ctx.os.coreinit.shared_data.fonts[id] {
        Some(entry) => Some(entry),
        None => {
            let loaded = font_bytes(id).and_then(|bytes| {
                let state = &mut ctx.os.coreinit.shared_data;
                let size = u32::try_from(bytes.len()).ok()?;
                let address = (SHARED.start + state.cursor).next_multiple_of(0x100);
                if u64::from(address) + u64::from(size) > SHARED.end() {
                    log::warn!("shared area exhausted: font {id} does not fit");
                    return None;
                }
                state.cursor = address + size - SHARED.start;
                Some((address, size, bytes))
            });
            match loaded {
                Some((address, size, bytes)) => {
                    ctx.memory.write_bytes(address, &bytes)?;
                    ctx.os.coreinit.shared_data.fonts[id] = Some((address, size));
                    Some((address, size))
                }
                None => None,
            }
        }
    };
    let Some((address, size)) = entry else { return Ok(false) };
    ctx.memory.write_u32(pointer_out, address)?;
    ctx.memory.write_u32(size_out, size)?;
    Ok(true)
}
