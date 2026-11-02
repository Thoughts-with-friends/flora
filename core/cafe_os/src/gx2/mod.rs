//! `gx2.rpl`: the graphics library.
//!
//! GX2 never touches Latte directly: it appends PM4 packets to its command
//! buffer and submits them to the GPU on flush, exactly like the real library
//! writes to the ring buffer. While a display list is being recorded the same
//! packets land in that list instead.
pub mod aperture;
pub mod command;
pub(crate) mod display;
pub mod draw;
pub mod render_target;
mod setup;
pub mod shader;
pub mod state;
pub mod surface;

pub use surface::Gx2Surface;
use wiiu_gpu::{
    command::{opcode, pm4::type3_header},
    regs::base,
};

use crate::hle::{HleContext, HleError, HleExport};

/// Commands submitted to Latte once this many words are queued.
const FLUSH_THRESHOLD: usize = 0x4_0000;

/// A display list being recorded.
#[derive(Debug, Default)]
pub struct DisplayList {
    pub address: u32,
    pub capacity: u32,
    pub words: Vec<u32>,
}

/// Host-side copy of the GX2 command buffer, plus the state of the library.
///
/// Every core records on its own, like the per-core write gatherers of the
/// real library: a worker building a display list must not catch packets of
/// the thread rendering on another core.
#[derive(Debug)]
pub struct Gx2State {
    commands: [Vec<u32>; 3],
    lists: [Option<DisplayList>; 3],
    /// Core of the HLE call being executed.
    pub active_core: usize,
    pub initialized: bool,
    /// Core that called `GX2Init`.
    pub main_core: u32,
    pub tv_enabled: bool,
    pub drc_enabled: bool,
    /// `(alloc, free)` callbacks set by `GX2RSetAllocator`.
    pub gx2r_allocator: (u32, u32),
    pub flip_requests: u32,
    pub flips_executed: u32,
    pub swap_interval: u32,
    /// Replaces the interval the title asks for (`Config::swap_interval`).
    pub forced_swap_interval: Option<u32>,
    /// Timebase value of the last flip shown.
    pub last_flip_time: u64,
    /// Timebase value of the last vertical blank.
    pub last_vsync_time: u64,
    pub vsync_count: u64,
    /// sync_count at the last flip shown.
    pub last_flip_vsync: u64,
    /// Number of command buffers submitted so far.
    pub timestamp: u64,
    /// `(function, user data)` per `GX2CallbackEventType`.
    pub event_callbacks: [(u32, u32); command::event::COUNT],
    pub apertures: aperture::Apertures,
}

impl Default for Gx2State {
    fn default() -> Self {
        Self {
            commands: [Vec::new(), Vec::new(), Vec::new()],
            lists: [None, None, None],
            active_core: 1,
            initialized: false,
            main_core: 0,
            tv_enabled: false,
            drc_enabled: false,
            gx2r_allocator: (0, 0),
            flip_requests: 0,
            flips_executed: 0,
            swap_interval: 1,
            forced_swap_interval: None,
            last_flip_time: 0,
            last_vsync_time: 0,
            vsync_count: 0,
            last_flip_vsync: 0,
            timestamp: 0,
            event_callbacks: [(0, 0); command::event::COUNT],
            apertures: aperture::Apertures::default(),
        }
    }
}

impl Gx2State {
    fn sink(&mut self) -> &mut Vec<u32> {
        let core = self.active_core;
        match &mut self.lists[core] {
            Some(list) => &mut list.words,
            None => &mut self.commands[core],
        }
    }

    /// Appends a type-3 packet.
    pub(crate) fn emit(&mut self, opcode: u8, body: &[u32]) {
        let sink = self.sink();
        sink.push(type3_header(opcode, body.len().max(1)));
        if body.is_empty() {
            sink.push(0);
        } else {
            sink.extend_from_slice(body);
        }
    }

    /// Appends a word that already is part of a packet (`GX2CopyDisplayList`).
    pub(crate) fn push_raw(&mut self, word: u32) {
        self.sink().push(word);
    }

    /// `SET_CONTEXT_REG` for consecutive registers starting at `register`.
    pub(crate) fn set_context(&mut self, register: u32, values: &[u32]) {
        let sink = self.sink();
        sink.push(type3_header(opcode::SET_CONTEXT_REG, 1 + values.len()));
        sink.push(register - base::CONTEXT);
        sink.extend_from_slice(values);
    }

    /// `SET_CONFIG_REG` for consecutive registers starting at `register`.
    pub(crate) fn set_config(&mut self, register: u32, values: &[u32]) {
        let sink = self.sink();
        sink.push(type3_header(opcode::SET_CONFIG_REG, 1 + values.len()));
        sink.push(register - base::CONFIG);
        sink.extend_from_slice(values);
    }

    pub(crate) fn begin_display_list(&mut self, address: u32, capacity: u32) {
        self.lists[self.active_core] = Some(DisplayList { address, capacity, words: Vec::new() });
    }

    pub(crate) fn end_display_list(&mut self) -> Option<DisplayList> {
        self.lists[self.active_core].take()
    }

    pub(crate) fn recording(&self) -> bool {
        self.lists[self.active_core].is_some()
    }
}

/// Effective → physical address, leaving anything unusual as it is.
pub(crate) const fn physical(address: u32) -> u32 {
    match wiiu_mem_addr::effective_to_physical(address) {
        Some(physical) => physical,
        None => address,
    }
}

/// Submits everything queued so far to Latte.
pub(crate) fn flush(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    let core = ctx.os.gx2.active_core;
    let commands = std::mem::take(&mut ctx.os.gx2.commands[core]);
    if !commands.is_empty() {
        ctx.os.gx2.timestamp += 1;
        ctx.gpu.submit(ctx.memory, &commands)?;
        // Keep the allocation for the next frame.
        let mut recycled = commands;
        recycled.clear();
        ctx.os.gx2.commands[core] = recycled;
    }
    Ok(())
}

/// Avoids unbounded command buffers in titles that rarely flush.
pub(crate) fn flush_if_large(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    if ctx.os.gx2.commands[ctx.os.gx2.active_core].len() >= FLUSH_THRESHOLD {
        flush(ctx)
    } else {
        Ok(())
    }
}

pub const EXPORTS: &[HleExport] = &[
    display::GX2_INIT,
    display::GX2_SHUTDOWN,
    display::GX2_GET_MAIN_CORE_ID,
    display::GX2_SET_TV_ENABLE,
    display::GX2_SET_DRC_ENABLE,
    display::GX2_CALC_TV_SIZE,
    display::GX2_CALC_DRC_SIZE,
    display::GX2_CLEAR_COLOR,
    display::GX2_CLEAR_BUFFERS_EX,
    display::GX2_CLEAR_DEPTH_STENCIL_EX,
    display::GX2_SET_CLEAR_DEPTH,
    display::GX2_SET_CLEAR_STENCIL,
    display::GX2_COPY_COLOR_BUFFER_TO_SCAN_BUFFER,
    display::GX2_COPY_SURFACE,
    display::GX2_RESOLVE_AA_COLOR_BUFFER,
    display::GX2_CONVERT_DEPTH_BUFFER_TO_TEXTURE_SURFACE,
    display::GX2_GET_SYSTEM_TV_SCAN_MODE,
    display::GX2_IS_VIDEO_OUT_READY,
    display::GX2_GET_GPU_TIMEOUT,
    render_target::GX2_INIT_COLOR_BUFFER_REGS,
    render_target::GX2_INIT_DEPTH_BUFFER_REGS,
    render_target::GX2_SET_COLOR_BUFFER,
    render_target::GX2_SET_DEPTH_BUFFER,
    render_target::GX2_SET_CLEAR_DEPTH_STENCIL,
    setup::GX2R_SET_ALLOCATOR,
    setup::GX2_INVALIDATE,
    setup::GX2_SET_TV_BUFFER,
    setup::GX2_SET_DRC_BUFFER,
    setup::GX2_SET_TV_SCALE,
    setup::GX2_SET_DRC_SCALE,
    setup::GX2_GET_VERTEX_SHADER_GPRS,
    setup::GX2_GET_VERTEX_SHADER_STACK_ENTRIES,
    setup::GX2_GET_PIXEL_SHADER_GPRS,
    setup::GX2_GET_PIXEL_SHADER_STACK_ENTRIES,
    setup::GX2_GET_GEOMETRY_SHADER_GPRS,
    setup::GX2_GET_GEOMETRY_SHADER_STACK_ENTRIES,
    setup::GX2_PERF_INIT,
    setup::GX2_PERF_SET_COLLECTION_METHOD,
    setup::GX2_PERF_METRICS_CLEAR,
    setup::GX2_PERF_TAG_ENABLE,
    shader::GX2_CALC_FETCH_SHADER_SIZE_EX,
    shader::GX2_INIT_FETCH_SHADER_EX,
    shader::GX2_TEMP_GET_GPU_VERSION,
    shader::GX2_INIT_SAMPLER,
    shader::GX2_INIT_SAMPLER_ZM_FILTER,
    shader::GX2_INIT_SAMPLER_XY_FILTER,
    shader::GX2_INIT_SAMPLER_CLAMPING,
    shader::GX2_INIT_SAMPLER_BORDER_TYPE,
    shader::GX2_INIT_SAMPLER_DEPTH_COMPARE,
    shader::GX2_INIT_SAMPLER_LOD,
    aperture::GX2_ALLOCATE_TILING_APERTURE_EX,
    aperture::GX2_FREE_TILING_APERTURE,
    surface::GX2_CALC_SURFACE_SIZE_AND_ALIGNMENT,
    surface::GX2_GET_SURFACE_MIP_PITCH,
    surface::GX2_GET_SURFACE_MIP_SLICE_SIZE,
    surface::GX2_GET_SURFACE_FORMAT_BITS,
    surface::GX2_GET_SURFACE_SWIZZLE_OFFSET,
    surface::GX2_GET_SURFACE_SWIZZLE,
    surface::GX2_SET_SURFACE_SWIZZLE,
    surface::GX2_SURFACE_IS_COMPRESSED,
    surface::GX2_CHECK_SURFACE_USE_VS_FORMAT,
    surface::GX2_CALC_DEPTH_BUFFER_HIZ_INFO,
    surface::GX2_CALC_COLOR_BUFFER_AUX_INFO,
];
