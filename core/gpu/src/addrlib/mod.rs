//! Latte address library: surface size/alignment/pitch computation.
//!
//! A port of AMD's R6xx AddrLib as configured for Latte (via Cemu's
//! `LatteAddrLib`). [`surface_info`] is what `GX2CalcSurfaceSizeAndAlignment`
//! uses; the same layout rules drive (de)tiling.
#![allow(clippy::cast_possible_truncation, clippy::too_many_arguments, clippy::too_many_lines)]

pub mod tile_mode;

use tile_mode::*;

const BANKS: u32 = 4;
const PIPES: u32 = 2;
const PIPE_INTERLEAVE_BYTES: u32 = 256;
const ROW_SIZE: u32 = 2048;
const SWAP_SIZE: u32 = 256;
const SPLIT_SIZE: u32 = 2048;
const CHIP_FAMILY: u32 = 2;

/// `GX2SurfaceDim`.
pub mod dim {
    pub const D1: u32 = 0;
    pub const D2: u32 = 1;
    pub const D3: u32 = 2;
    pub const CUBE: u32 = 3;
    pub const D1_ARRAY: u32 = 4;
    pub const D2_ARRAY: u32 = 5;
    pub const D2_MSAA: u32 = 6;
    pub const D2_ARRAY_MSAA: u32 = 7;
}

/// Bits per element of a hardware format (`format & 0x3F`).
pub const fn format_bits(hw_format: u32) -> u32 {
    const TABLE: [u8; 0x40] = [
        0x00, 0x08, 0x08, 0x00, 0x00, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x20, 0x20,
        0x20, //
        0x20, 0x20, 0x00, 0x20, 0x00, 0x00, 0x20, 0x00, 0x00, 0x20, 0x20, 0x20, 0x40, 0x40, 0x40,
        0x40, //
        0x40, 0x00, 0x80, 0x80, 0x00, 0x00, 0x00, 0x10, 0x10, 0x20, 0x20, 0x20, 0x00, 0x00, 0x00,
        0x60, //
        0x60, 0x40, 0x80, 0x80, 0x40, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00,
    ];
    TABLE[(hw_format & 0x3F) as usize] as u32
}

/// BC1..=BC5.
#[inline]
pub const fn is_compressed(hw_format: u32) -> bool {
    let f = hw_format & 0x3F;
    f >= 0x31 && f <= 0x35
}

/// Surface flags that influence the layout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    pub depth: bool,
    pub dim_cube: bool,
    pub dim_3d: bool,
    pub fmask: bool,
    pub cube_as_array: bool,
    pub linear_wa: bool,
    pub input_is_base: bool,
    pub display: bool,
}

/// Result of a layout computation for one mip level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SurfaceInfo {
    pub pitch: u32,
    pub height: u32,
    pub depth: u32,
    pub surf_size: u64,
    pub hw_tile_mode: u32,
    pub base_align: u32,
    pub pitch_align: u32,
    pub height_align: u32,
    pub depth_align: u32,
    pub bpp: u32,
    pub pixel_pitch: u32,
    pub pixel_height: u32,
    pub pixel_bits: u32,
    pub slice_size: u32,
}

const fn pow2_align(x: u32, align: u32) -> u32 {
    (x + align - 1) & !(align - 1)
}

const fn next_pow2(x: u32) -> u32 {
    if x <= 1 { 1 } else { x.next_power_of_two() }
}

/// `(bpp, elem_mode, expand_x, expand_y)` of a hardware format.
const fn bits_per_pixel(format: u32) -> (u32, u32, u32, u32) {
    match format {
        0 => (0, 3, 1, 1),
        0x01..=0x03 => (8, 3, 1, 1),
        0x05..=0x0C => (16, 3, 1, 1),
        0x0D..=0x1B => (32, 3, 1, 1),
        0x1C..=0x20 => (64, 3, 1, 1),
        0x22 | 0x23 => (128, 3, 1, 1),
        0x31 => (64, 9, 4, 4),
        0x32 => (128, 10, 4, 4),
        0x33 => (128, 11, 4, 4),
        0x34 => (64, 12, 4, 4),
        0x35 => (128, 13, 4, 4),
        _ => (0, 3, 1, 1),
    }
}

fn adjust_surface_info(
    elem_mode: u32,
    expand_x: u32,
    expand_y: u32,
    bpp: &mut u32,
    width: &mut u32,
    height: &mut u32,
) {
    let is_bc = matches!(elem_mode, 9..=13);
    *bpp = match elem_mode {
        4 => *bpp / expand_x / expand_y,
        5 | 6 => expand_y * expand_x * *bpp,
        9 | 12 => 64,
        10 | 11 | 13 => 128,
        _ => *bpp,
    };
    if expand_x > 1 || expand_y > 1 {
        let (w, h) = if elem_mode == 4 {
            (expand_x * *width, expand_y * *height)
        } else if is_bc {
            (*width / expand_x, *height / expand_y)
        } else {
            (width.div_ceil(expand_x), height.div_ceil(expand_y))
        };
        *width = w.max(1);
        *height = h.max(1);
    }
}

fn restore_surface_info(
    elem_mode: u32,
    expand_x: u32,
    expand_y: u32,
    width: &mut u32,
    height: &mut u32,
) {
    if expand_x > 1 || expand_y > 1 {
        if elem_mode == 4 {
            *width /= expand_x;
            *height /= expand_y;
        } else {
            *width *= expand_x;
            *height *= expand_y;
        }
    }
    *width = (*width).max(1);
    *height = (*height).max(1);
}

fn compute_mip_level_dimensions(
    width: &mut u32,
    height: &mut u32,
    slices: &mut u32,
    flags: Flags,
    format: u32,
    level: u32,
) {
    let is_bc = (0x31..=0x35).contains(&format);
    if is_bc && (level == 0 || flags.input_is_base) {
        *width = pow2_align(*width, 4);
        *height = pow2_align(*height, 4);
    }
    if is_bc {
        if level != 0 {
            let (mut w, mut h, mut s) = (*width, *height, *slices);
            if flags.input_is_base {
                if !flags.dim_cube {
                    s >>= level;
                }
                w = (w >> level).max(1);
                h = (h >> level).max(1);
                s = s.max(1);
            }
            *width = next_pow2(w);
            *height = next_pow2(h);
            *slices = s;
        }
    } else if level != 0 && flags.input_is_base {
        let mut w = (*width >> level).max(1);
        let mut h = (*height >> level).max(1);
        let mut s = if flags.dim_cube { *slices } else { *slices >> level }.max(1);
        if format != 0x2F && format != 0x30 {
            w = next_pow2(w);
            h = next_pow2(h);
            s = next_pow2(s);
        }
        *width = w;
        *height = h;
        *slices = s;
    }
}

const fn surface_tile_slices(tile_mode: u32, bpp: u32, num_samples: u32) -> u32 {
    let bytes_per_sample = ((bpp << 6) + 7) >> 3;
    let samples = if thickness(tile_mode) > 1 { 4 } else { num_samples };
    if bytes_per_sample == 0 {
        return 1;
    }
    let samples_per_tile = SPLIT_SIZE / bytes_per_sample;
    if samples_per_tile == 0 {
        return 1;
    }
    let slices = samples / samples_per_tile;
    if slices == 0 { 1 } else { slices }
}

pub(crate) const fn rotation_from_tile_mode(tile_mode: u32) -> u32 {
    match tile_mode {
        TM_2D_TILED_THIN1 | TM_2D_TILED_THIN2 | TM_2D_TILED_THIN4 | TM_2D_TILED_THICK
        | TM_2B_TILED_THIN1 | TM_2B_TILED_THIN2 | TM_2B_TILED_THIN4 | TM_2B_TILED_THICK => {
            PIPES * ((BANKS >> 1) - 1)
        }
        TM_3D_TILED_THIN1 | TM_3D_TILED_THICK | TM_3B_TILED_THIN1 | TM_3B_TILED_THICK => {
            if PIPES >= 4 { (PIPES >> 1) - 1 } else { 1 }
        }
        _ => 0,
    }
}

fn mip_level_tile_mode(
    base: u32,
    mut bpp: u32,
    level: u32,
    width: u32,
    height: u32,
    slices: u32,
    samples: u32,
    is_depth: bool,
    no_recursive: bool,
) -> u32 {
    let tile_slices = surface_tile_slices(base, bpp, samples);
    let mut mode = match base {
        // GetFlagNo1DTiledMSAA() is set in Latte's configuration.
        TM_1D_TILED_THIN1 if samples > 1 => TM_2D_TILED_THIN1,
        TM_1D_TILED_THICK => {
            if samples == 2 || samples == 4 {
                TM_2D_TILED_THICK
            } else if samples > 1 || is_depth {
                TM_1D_TILED_THIN1
            } else {
                base
            }
        }
        TM_2D_TILED_THIN2 if 2 * PIPE_INTERLEAVE_BYTES > SPLIT_SIZE => TM_2D_TILED_THIN1,
        TM_2D_TILED_THIN4 if 4 * PIPE_INTERLEAVE_BYTES > SPLIT_SIZE => TM_2D_TILED_THIN2,
        TM_2D_TILED_THICK if samples > 1 || tile_slices > 1 || is_depth => TM_2D_TILED_THIN1,
        TM_2B_TILED_THIN2 if 2 * PIPE_INTERLEAVE_BYTES > SPLIT_SIZE => TM_2B_TILED_THIN1,
        TM_2B_TILED_THIN4 if 4 * PIPE_INTERLEAVE_BYTES > SPLIT_SIZE => TM_2B_TILED_THIN2,
        TM_2B_TILED_THICK if samples > 1 || tile_slices > 1 || is_depth => TM_2B_TILED_THIN1,
        TM_3D_TILED_THICK if samples > 1 || tile_slices > 1 || is_depth => TM_3D_TILED_THIN1,
        TM_3B_TILED_THICK if samples > 1 || tile_slices > 1 || is_depth => TM_3B_TILED_THIN1,
        _ => base,
    };
    if rotation_from_tile_mode(mode).is_multiple_of(PIPES) {
        mode = match mode {
            TM_3D_TILED_THIN1 => TM_2D_TILED_THIN1,
            TM_3D_TILED_THICK => TM_2D_TILED_THICK,
            TM_3B_TILED_THIN1 => TM_2B_TILED_THIN1,
            TM_3B_TILED_THICK => TM_2B_TILED_THICK,
            m => m,
        };
    }
    if no_recursive || level == 0 {
        return mode;
    }
    if matches!(bpp, 96 | 48 | 24) {
        bpp /= 3;
    }
    let (w, h, s) = (next_pow2(width), next_pow2(height), next_pow2(slices));
    mode = non_bank_swapped(mode);
    let micro_tile_bytes = (samples * bpp * (thickness(mode) << 6) + 7) >> 3;
    let width_factor = if micro_tile_bytes >= PIPE_INTERLEAVE_BYTES {
        1
    } else {
        PIPE_INTERLEAVE_BYTES / micro_tile_bytes
    };
    let (mut macro_w, mut macro_h) = (8 * BANKS, 8 * PIPES);
    match mode {
        TM_2D_TILED_THIN1 | TM_3D_TILED_THIN1 => {
            if w < width_factor * macro_w || h < macro_h {
                mode = TM_1D_TILED_THIN1;
            }
        }
        TM_2D_TILED_THIN2 => {
            macro_w >>= 1;
            macro_h *= 2;
            if w < width_factor * macro_w || h < macro_h {
                mode = TM_1D_TILED_THIN1;
            }
        }
        TM_2D_TILED_THIN4 => {
            macro_w >>= 2;
            macro_h *= 4;
            if w < width_factor * macro_w || h < macro_h {
                mode = TM_1D_TILED_THIN1;
            }
        }
        TM_2D_TILED_THICK | TM_3D_TILED_THICK if (w < width_factor * macro_w || h < macro_h) => {
            mode = TM_1D_TILED_THICK;
        }
        _ => {}
    }
    mode = match mode {
        TM_1D_TILED_THICK if s < 4 => TM_1D_TILED_THIN1,
        TM_2D_TILED_THICK if s < 4 => TM_2D_TILED_THIN1,
        TM_3D_TILED_THICK if s < 4 => TM_3D_TILED_THIN1,
        m => m,
    };
    mip_level_tile_mode(mode, bpp, level, w, h, s, samples, is_depth, true)
}

const fn macro_tile_aspect_ratio(tile_mode: u32) -> u32 {
    match tile_mode {
        TM_2D_TILED_THIN2 | TM_2B_TILED_THIN2 => 2,
        TM_2D_TILED_THIN4 | TM_2B_TILED_THIN4 => 4,
        _ => 1,
    }
}

/// `(base_align, pitch_align, height_align)` of a macro-tiled surface.
fn alignments_macro_tiled(tile_mode: u32, mut bpp: u32, samples: u32) -> (u32, u32, u32) {
    let aspect = macro_tile_aspect_ratio(tile_mode);
    let thick = thickness(tile_mode);
    if matches!(bpp, 96 | 48 | 24) {
        bpp /= 3;
    }
    if bpp == 3 {
        bpp = 1;
    }
    let macro_w = (8 * BANKS) / aspect;
    let macro_h = aspect * 8 * PIPES;
    // Note: the display-surface pitch adjustment is (as in Cemu) not applied here.
    let pitch_align = macro_w.max(macro_w * (PIPE_INTERLEAVE_BYTES / bpp / (8 * thick) / samples));
    let height_align = macro_h;
    let mut macro_bytes = samples * ((bpp * macro_h * macro_w + 7) >> 3);
    if CHIP_FAMILY == 1 && samples == 1 {
        macro_bytes *= 2;
    }
    let mut base_align = if thick == 1 {
        macro_bytes.max((samples * height_align * bpp * pitch_align + 7) >> 3)
    } else {
        PIPE_INTERLEAVE_BYTES.max((4 * height_align * bpp * pitch_align + 7) >> 3)
    };
    let micro_bytes = (thick * samples * (bpp << 6) + 7) >> 3;
    let splits = if micro_bytes < SPLIT_SIZE { 1 } else { micro_bytes / SPLIT_SIZE };
    base_align /= splits;
    (base_align, pitch_align, height_align)
}

pub(crate) fn bank_swapped_width(tile_mode: u32, bpp: u32, mut samples: u32, pitch: u32) -> u32 {
    let bytes_per_sample = 8 * bpp;
    let samples_per_tile = SPLIT_SIZE / bytes_per_sample;
    let slices_per_tile = samples.checked_div(samples_per_tile).map_or(1, |slices| slices.max(1));
    if is_thick_and_macro_tiled(tile_mode) {
        samples = 4;
    }
    if !is_bank_swapped(tile_mode) {
        return 0;
    }
    let bytes_per_tile_slice = samples * bytes_per_sample / slices_per_tile;
    let aspect = macro_tile_aspect_ratio(tile_mode);
    let swap_tiles = ((SWAP_SIZE >> 1) / bpp).max(1);
    let swap_width = swap_tiles * 8 * BANKS;
    let height_bytes = samples * aspect * PIPES * bpp / slices_per_tile;
    let swap_max = PIPES * BANKS * ROW_SIZE / height_bytes;
    let swap_min = PIPE_INTERLEAVE_BYTES * 8 * BANKS / bytes_per_tile_slice;
    let mut width = if swap_max >= swap_width { swap_width.max(swap_min) } else { swap_max };
    while width >= 2 * pitch {
        width >>= 1;
    }
    width
}

fn pad_dimensions(
    tile_mode: u32,
    mut pad_dims: u32,
    is_cube: bool,
    pitch: &mut u32,
    pitch_align: u32,
    height: &mut u32,
    height_align: u32,
    slices: &mut u32,
    slice_align: u32,
) {
    let thick = thickness(tile_mode);
    if pad_dims == 0 {
        pad_dims = 3;
    }
    *pitch = if pitch_align.is_power_of_two() {
        pow2_align(*pitch, pitch_align)
    } else {
        (*pitch).div_ceil(pitch_align) * pitch_align
    };
    if pad_dims > 1 {
        *height = pow2_align(*height, height_align);
    }
    if pad_dims > 2 || thick > 1 {
        // GetFlagNoCubeMipSlicesPad() is clear in Latte's configuration.
        if is_cube {
            *slices = next_pow2(*slices);
        }
        if thick > 1 {
            *slices = pow2_align(*slices, slice_align);
        }
    }
}

fn info_linear(
    tile_mode: u32,
    bpp: u32,
    samples: u32,
    pitch: u32,
    height: u32,
    slices: u32,
    level: u32,
    mut pad_dims: u32,
    flags: Flags,
) -> SurfaceInfo {
    let (base_align, mut pitch_align, height_align) = if tile_mode == TM_LINEAR_ALIGNED {
        (PIPE_INTERLEAVE_BYTES, 64.max(8 * PIPE_INTERLEAVE_BYTES / bpp), 1)
    } else {
        (1, 1, 1)
    };
    if flags.display {
        pitch_align = pow2_align(pitch_align, 32);
    }
    let (mut p, mut h, mut s) = (pitch, height, slices);
    if flags.linear_wa && level == 0 {
        p = next_pow2(p / 3);
    }
    if level != 0 {
        p = next_pow2(p);
        h = next_pow2(h);
        if flags.dim_cube {
            s = slices;
            pad_dims = if slices <= 1 { 2 } else { 0 };
        } else {
            s = next_pow2(slices);
        }
    }
    let thick = thickness(tile_mode);
    pad_dimensions(
        tile_mode,
        pad_dims,
        flags.dim_cube,
        &mut p,
        pitch_align,
        &mut h,
        height_align,
        &mut s,
        thick,
    );
    if flags.linear_wa && level == 0 {
        p *= 3;
    }
    let slice_count = s * samples / thick;
    SurfaceInfo {
        pitch: p,
        height: h,
        depth: s,
        surf_size: (u64::from(h)
            * u64::from(p)
            * u64::from(slice_count)
            * u64::from(bpp)
            * u64::from(samples))
        .div_ceil(8),
        hw_tile_mode: tile_mode,
        base_align,
        pitch_align,
        height_align,
        depth_align: thick,
        ..SurfaceInfo::default()
    }
}

fn info_micro_tiled(
    tile_mode: u32,
    bpp: u32,
    samples: u32,
    pitch: u32,
    height: u32,
    slices: u32,
    level: u32,
    mut pad_dims: u32,
    flags: Flags,
) -> SurfaceInfo {
    let mut mode = tile_mode;
    let (mut p, mut h, mut s) = (pitch, height, slices);
    let mut thick = thickness(tile_mode);
    if level != 0 {
        p = next_pow2(pitch);
        h = next_pow2(height);
        if flags.dim_cube {
            s = slices;
            pad_dims = if slices <= 1 { 2 } else { 0 };
        } else {
            s = next_pow2(slices);
        }
        if mode == TM_1D_TILED_THICK && s < 4 {
            mode = TM_1D_TILED_THIN1;
            thick = 1;
        }
    }
    let b = if matches!(bpp, 96 | 48 | 24) { bpp / 3 } else { bpp };
    let base_align = PIPE_INTERLEAVE_BYTES;
    let mut pitch_align = 8.max(PIPE_INTERLEAVE_BYTES / b / samples / thickness(mode));
    let height_align = 8;
    if flags.display {
        pitch_align = pow2_align(pitch_align, 32);
    }
    pad_dimensions(
        mode,
        pad_dims,
        flags.dim_cube,
        &mut p,
        pitch_align,
        &mut h,
        height_align,
        &mut s,
        thick,
    );
    SurfaceInfo {
        pitch: p,
        height: h,
        depth: s,
        surf_size: (u64::from(h)
            * u64::from(p)
            * u64::from(s)
            * u64::from(bpp)
            * u64::from(samples))
        .div_ceil(8),
        hw_tile_mode: mode,
        base_align,
        pitch_align,
        height_align,
        depth_align: thick,
        ..SurfaceInfo::default()
    }
}

fn info_macro_tiled(
    tile_mode: u32,
    base_tile_mode: u32,
    bpp: u32,
    samples: u32,
    pitch: u32,
    height: u32,
    slices: u32,
    level: u32,
    mut pad_dims: u32,
    flags: Flags,
) -> SurfaceInfo {
    let (mut p, mut h, mut s) = (pitch, height, slices);
    let mut mode = tile_mode;
    let mut thick = thickness(tile_mode);
    if level != 0 {
        p = next_pow2(pitch);
        h = next_pow2(height);
        s = next_pow2(slices);
        if flags.dim_cube {
            s = slices;
            pad_dims = if slices <= 1 { 2 } else { 0 };
        }
        if mode == TM_2D_TILED_THICK && s < 4 {
            mode = TM_2D_TILED_THIN1;
            thick = 1;
        }
    }
    let pitch_align_factor = ((PIPE_INTERLEAVE_BYTES >> 3) / bpp).max(1);
    if tile_mode != base_tile_mode
        && level != 0
        && is_thick_and_macro_tiled(base_tile_mode)
        && !is_thick_and_macro_tiled(tile_mode)
    {
        let (_, pitch_align, height_align) = alignments_macro_tiled(base_tile_mode, bpp, samples);
        if p < pitch_align * pitch_align_factor || h < height_align {
            return info_micro_tiled(
                TM_1D_TILED_THIN1,
                bpp,
                samples,
                pitch,
                height,
                slices,
                level,
                pad_dims,
                flags,
            );
        }
    }
    let (base_align, mut pitch_align, height_align) =
        alignments_macro_tiled(tile_mode, bpp, samples);
    let swapped = bank_swapped_width(tile_mode, bpp, samples, pitch);
    if swapped > pitch_align {
        pitch_align = swapped;
    }
    pad_dimensions(
        tile_mode,
        pad_dims,
        flags.dim_cube,
        &mut p,
        pitch_align,
        &mut h,
        height_align,
        &mut s,
        thick,
    );
    SurfaceInfo {
        pitch: p,
        height: h,
        depth: s,
        surf_size: (u64::from(h)
            * u64::from(p)
            * u64::from(s)
            * u64::from(bpp)
            * u64::from(samples))
        .div_ceil(8),
        hw_tile_mode: mode,
        base_align,
        pitch_align,
        height_align,
        depth_align: thick,
        ..SurfaceInfo::default()
    }
}

/// `ComputeSurfaceInfo`: layout of mip `level` described by the inputs.
pub fn compute_surface_info(
    tile_mode: u32,
    format: u32,
    mut bpp: u32,
    samples: u32,
    mut width: u32,
    mut height: u32,
    mut slices: u32,
    slice: u32,
    level: u32,
    mut flags: Flags,
) -> SurfaceInfo {
    compute_mip_level_dimensions(&mut width, &mut height, &mut slices, flags, format, level);
    let pixel_bits = bpp;
    let (mut elem_mode, mut expand_x, mut expand_y) = (3, 1, 1);
    if format != 0 {
        let (b, e, x, y) = bits_per_pixel(format);
        (bpp, elem_mode, expand_x, expand_y) = (b, e, x, y);
        if tile_mode == TM_LINEAR_ALIGNED && elem_mode == 4 && expand_x == 3 {
            flags.linear_wa = true;
        }
        adjust_surface_info(elem_mode, expand_x, expand_y, &mut bpp, &mut width, &mut height);
    } else if bpp != 0 {
        width = width.max(1);
        height = height.max(1);
    } else {
        return SurfaceInfo::default();
    }

    let samples = samples.max(1);
    let pad_dims = if flags.dim_cube && level == 0 { 2 } else { 0 };
    let mode = if flags.fmask {
        non_bank_swapped(tile_mode)
    } else {
        mip_level_tile_mode(
            tile_mode,
            bpp,
            level,
            width,
            height,
            slices,
            samples,
            flags.depth,
            false,
        )
    };
    let mut out = match mode {
        TM_LINEAR_GENERAL | TM_LINEAR_ALIGNED => {
            info_linear(mode, bpp, samples, width, height, slices, level, pad_dims, flags)
        }
        TM_1D_TILED_THIN1 | TM_1D_TILED_THICK => {
            info_micro_tiled(mode, bpp, samples, width, height, slices, level, pad_dims, flags)
        }
        TM_2D_TILED_THIN1..=TM_3B_TILED_THICK => info_macro_tiled(
            mode, tile_mode, bpp, samples, width, height, slices, level, pad_dims, flags,
        ),
        _ => return SurfaceInfo::default(),
    };
    out.bpp = bpp;
    out.pixel_bits = pixel_bits;
    out.pixel_pitch = out.pitch;
    out.pixel_height = out.height;
    if format != 0 && (!flags.linear_wa || level == 0) {
        restore_surface_info(
            elem_mode,
            expand_x,
            expand_y,
            &mut out.pixel_pitch,
            &mut out.pixel_height,
        );
    }
    out.slice_size = if flags.dim_3d {
        out.surf_size as u32
    } else if out.depth == 0 {
        0
    } else {
        let mut size = (out.surf_size / u64::from(out.depth)) as u32;
        if slice + 1 == slices && slices > 1 {
            size += size * (out.depth - slices);
        }
        size
    };
    out
}

/// `GX2CalculateSurfaceInfo`: layout of mip `level` of a GX2 surface.
pub fn gx2_surface_info(
    format: u32,
    width: u32,
    height: u32,
    depth: u32,
    surface_dim: u32,
    gx2_tile_mode: u32,
    aa: u32,
    level: u32,
    is_depth: bool,
    is_scan_buffer: bool,
) -> SurfaceInfo {
    let hw_format = format & 0x3F;
    if gx2_tile_mode == TM_LINEAR_SPECIAL {
        return linear_special(hw_format, width, height, depth, surface_dim, aa, level);
    }
    let tile_mode = gx2_tile_mode & 0xF;
    let samples = 1 << aa;
    let w = (width >> level).max(1);
    let h = (height >> level).max(1);
    let mut flags = Flags {
        input_is_base: level == 0,
        depth: is_depth,
        display: is_scan_buffer,
        ..Flags::default()
    };
    let (h, slices) = match surface_dim {
        dim::D1 => (1, 1),
        dim::D2 | dim::D2_MSAA => (h, 1),
        dim::D3 => {
            flags.dim_3d = true;
            (h, (depth >> level).max(1))
        }
        dim::CUBE => {
            flags.dim_cube = true;
            (h, depth.max(6))
        }
        dim::D1_ARRAY => (1, depth),
        dim::D2_ARRAY | dim::D2_ARRAY_MSAA => (h, depth),
        _ => (h, 1),
    };
    compute_surface_info(
        tile_mode,
        hw_format,
        format_bits(hw_format),
        samples,
        w,
        h,
        slices,
        0,
        level,
        flags,
    )
}

fn linear_special(
    hw_format: u32,
    width: u32,
    height: u32,
    depth: u32,
    surface_dim: u32,
    aa: u32,
    level: u32,
) -> SurfaceInfo {
    let samples: u32 = 1 << aa;
    let block = if is_compressed(hw_format) { 4 } else { 1 };
    let bpp = format_bits(hw_format);
    let w = pow2_align(width >> level, block);
    let mut out = SurfaceInfo {
        bpp,
        pitch: w / block,
        pixel_bits: bpp,
        base_align: 1,
        pitch_align: 1,
        height_align: 1,
        depth_align: 1,
        ..SurfaceInfo::default()
    };
    (out.height, out.depth) = match surface_dim {
        dim::D1 => (1, 1),
        dim::D2 => ((height >> level).max(1), 1),
        dim::D3 => ((height >> level).max(1), (depth >> level).max(1)),
        dim::CUBE => ((height >> level).max(1), depth.max(6)),
        dim::D1_ARRAY => (1, depth),
        dim::D2_ARRAY => ((height >> level).max(1), depth),
        _ => (0, 0),
    };
    out.height = pow2_align(out.height, block) / block;
    out.pixel_pitch = pow2_align(width >> level, block).max(block);
    out.pixel_height = pow2_align(height >> level, block).max(block);
    out.pitch = out.pitch.max(1);
    out.height = out.height.max(1);
    out.surf_size = (u64::from(out.bpp)
        * u64::from(samples)
        * u64::from(out.depth)
        * u64::from(out.height)
        * u64::from(out.pitch))
        >> 3;
    out.slice_size = if surface_dim == dim::D3 {
        out.surf_size as u32
    } else if out.depth == 0 {
        0
    } else {
        (out.surf_size / u64::from(out.depth)) as u32
    };
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tv_color_buffer_layout() {
        // 1280x720 RGBA8, 2D tiled thin1: pitch 1280, macro-tile aligned height.
        let info =
            gx2_surface_info(0x1A, 1280, 720, 1, dim::D2, TM_2D_TILED_THIN1, 0, 0, false, false);
        assert_eq!(info.hw_tile_mode, TM_2D_TILED_THIN1);
        assert_eq!(info.pitch, 1280);
        assert_eq!(info.height, 720);
        assert_eq!(info.bpp, 32);
        assert_eq!(info.surf_size, 1280 * 720 * 4);
        assert_eq!(info.base_align, 0x800);
    }

    #[test]
    fn small_mips_fall_back_to_micro_tiling() {
        let info =
            gx2_surface_info(0x1A, 256, 256, 1, dim::D2, TM_2D_TILED_THIN1, 0, 5, false, false);
        assert_eq!(info.hw_tile_mode, TM_1D_TILED_THIN1);
        assert_eq!((info.pitch, info.height), (8, 8));
    }

    #[test]
    fn bc1_uses_4x4_blocks() {
        let info =
            gx2_surface_info(0x31, 128, 128, 1, dim::D2, TM_1D_TILED_THIN1, 0, 0, false, false);
        assert_eq!((info.pitch, info.height, info.bpp), (32, 32, 64));
        assert_eq!(info.surf_size, 32 * 32 * 8);
        assert_eq!((info.pixel_pitch, info.pixel_height), (128, 128));
    }

    #[test]
    fn linear_aligned_pitch() {
        let info =
            gx2_surface_info(0x1A, 100, 10, 1, dim::D2, TM_LINEAR_ALIGNED, 0, 0, false, false);
        assert_eq!(info.pitch, 128);
        assert_eq!(info.base_align, 256);
    }
}
