//! Tiling apertures (port of Cemu's GX2_TilingAperture): a linear window onto
//! one level of a tiled surface. The hardware translates every access; here the
//! texels are copied in on allocation and written back on free.
use wiiu_gpu::{addrlib, tiling::Layout};
use wiiu_hle_macros::hle_export;
use wiiu_mem_addr::TILING_APERTURE;
use wiiu_memory::{GuestPtr, Memory, MemoryError};

use super::Gx2Surface;
use crate::hle::{HleContext, HleError};

/// An open aperture.
#[derive(Debug, Clone, Copy)]
pub struct Aperture {
    address: u32,
    size: u32,
    handle: u32,
    surface: Gx2Surface,
    level: u32,
    slice: u32,
}

/// Open apertures of the process.
#[derive(Debug, Default)]
pub struct Apertures {
    open: Vec<Aperture>,
    last_handle: u32,
}

impl Apertures {
    /// First free page-aligned range of the aperture region.
    fn find_space(&self, size: u32) -> Option<u32> {
        let mut offset = 0_u32;
        loop {
            let start = (offset + 0xFFF) & !0xFFF;
            if u64::from(start) + u64::from(size) >= u64::from(TILING_APERTURE.size) {
                return None;
            }
            let address = TILING_APERTURE.start + start;
            let overlapping = self
                .open
                .iter()
                .filter(|a| address < a.address + a.size && a.address < address + size);
            match overlapping.map(|a| a.address + a.size).max() {
                Some(end) => offset = end - TILING_APERTURE.start,
                None => return Some(address),
            }
        }
    }
}

/// Copies the level's texels between the aperture and the surface.
fn retile(memory: &mut Memory, aperture: &Aperture, write_back: bool) -> Result<(), MemoryError> {
    let surface = &aperture.surface;
    let level = aperture.level;
    let image = match level {
        0 => surface.image,
        1 => surface.mip_ptr,
        _ => surface
            .mip_ptr
            .wrapping_add(surface.mip_offset.get(level as usize - 1).copied().unwrap_or(0)),
    };
    if image == 0 {
        return Ok(());
    }
    let info = surface.info(level);
    let layout = Layout {
        tile_mode: info.hw_tile_mode,
        bpp: info.bpp,
        pitch: info.pitch,
        height: info.height,
        samples: 1,
        is_depth: false,
        pipe_swizzle: (surface.swizzle >> 8) & 1,
        bank_swizzle: (surface.swizzle >> 9) & 3,
    };
    let step = if addrlib::is_compressed(surface.format) { 4 } else { 1 };
    let width = (surface.width >> level).max(1).div_ceil(step);
    let height = (surface.height >> level).max(1).div_ceil(step);
    let bytes = info.bpp / 8;
    for y in 0..height {
        for x in 0..width {
            // 8-bit apertures are addressed with the swap of the hardware's endian mode.
            let (column, row) = if bytes == 1 { (x ^ 8, y ^ 2) } else { (x, y) };
            let linear = aperture.address + (column + row * info.pitch) * bytes;
            let tiled = image.wrapping_add(layout.address(x, y, aperture.slice));
            let (from, to) = if write_back { (linear, tiled) } else { (tiled, linear) };
            memory.copy_within(to, from, bytes)?;
        }
    }
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2AllocateTilingApertureEx")]
fn gx2_allocate_tiling_aperture_ex(
    ctx: &mut HleContext<'_>,
    surface: u32,
    level: u32,
    slice: u32,
    _endian: u32,
    handle: GuestPtr<u32>,
    address: GuestPtr<u32>,
) -> Result<(), HleError> {
    let surface = Gx2Surface::read(ctx.memory, surface)?;
    let bits = addrlib::format_bits(surface.format);
    let bits = if addrlib::is_compressed(surface.format) { bits / 16 } else { bits };
    let aligned_height = ((surface.height >> level).max(1) + 3) & !3;
    let size = (u64::from(surface.info(0).pitch) * u64::from(aligned_height) * u64::from(bits))
        .div_ceil(8) as u32;
    let Some(start) = ctx.os.gx2.apertures.find_space(size) else {
        log::warn!("no tiling aperture space for {size:#x} bytes");
        handle.write(ctx.memory, 0)?;
        address.write(ctx.memory, 0)?;
        return Ok(());
    };
    ctx.os.gx2.apertures.last_handle += 1;
    let aperture = Aperture {
        address: start,
        size,
        handle: ctx.os.gx2.apertures.last_handle,
        surface,
        level,
        slice,
    };
    ctx.os.gx2.apertures.open.push(aperture);
    handle.write(ctx.memory, aperture.handle)?;
    address.write(ctx.memory, start)?;
    retile(ctx.memory, &aperture, false)?;
    Ok(())
}

#[hle_export(lib = "gx2", name = "GX2FreeTilingAperture")]
fn gx2_free_tiling_aperture(ctx: &mut HleContext<'_>, handle: u32) -> Result<(), HleError> {
    let open = &mut ctx.os.gx2.apertures.open;
    if let Some(index) = open.iter().position(|a| a.handle == handle) {
        let aperture = open.swap_remove(index);
        retile(ctx.memory, &aperture, true)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use wiiu_gpu::addrlib::{dim, tile_mode};

    use super::*;
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn writes_through_the_aperture_reach_the_tiled_surface() {
        let mut env = TestEnv::new();
        let image = SCRATCH + 0x1_0000;
        let mut surface = Gx2Surface {
            dim: dim::D2,
            width: 64,
            height: 64,
            depth: 1,
            num_levels: 1,
            format: 0x1A,
            tile_mode: tile_mode::TM_1D_TILED_THIN1,
            ..Gx2Surface::default()
        };
        surface.calc_size_and_alignment();
        surface.image = image;
        surface.write(&mut env.memory, SCRATCH).unwrap();

        let (handle, address) = (SCRATCH + 0x100, SCRATCH + 0x104);
        env.call("gx2", "GX2AllocateTilingApertureEx", &[SCRATCH, 0, 0, 0, handle, address]);
        let linear = env.memory.read_u32(address).unwrap();
        assert!(TILING_APERTURE.contains(linear));
        // Pixel (9, 2): the second micro tile, third row.
        env.memory.write_u32(linear + (9 + 2 * surface.pitch) * 4, 0x1234_5678).unwrap();
        let handle = env.memory.read_u32(handle).unwrap();
        env.call("gx2", "GX2FreeTilingAperture", &[handle]);

        let layout = Layout {
            tile_mode: tile_mode::TM_1D_TILED_THIN1,
            bpp: 32,
            pitch: surface.pitch,
            height: surface.info(0).height,
            samples: 1,
            is_depth: false,
            pipe_swizzle: 0,
            bank_swizzle: 0,
        };
        assert_eq!(env.memory.read_u32(image + layout.address(9, 2, 0)).unwrap(), 0x1234_5678);
    }
}
