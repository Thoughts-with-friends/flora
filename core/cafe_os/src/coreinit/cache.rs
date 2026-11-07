//! Cache maintenance and hardware queries.
//!
//! The emulator has no caches, so flush/invalidate are no-ops; only
//! `DCZeroRange` has a visible effect (it zeroes whole cache lines).
use wiiu_hle_macros::hle_export;

use crate::hle::{HleContext, HleError};

const CACHE_LINE: u32 = 32;
/// Retail console (`OSGetConsoleType`).
const CONSOLE_TYPE_RETAIL: u32 = 0x0300_0050;
/// Latte hardware version reported by `bspGetHardwareVersion`.
const HARDWARE_VERSION: u32 = 0x1100_0000;

/// Memory barriers order the guest's own accesses: one host thread runs them in order.
#[hle_export(lib = "coreinit", name = "OSMemoryBarrier")]
fn os_memory_barrier(_ctx: &mut HleContext<'_>) {}

#[hle_export(lib = "coreinit", name = "OSCoherencyBarrier")]
fn os_coherency_barrier(_ctx: &mut HleContext<'_>) {}

#[hle_export(lib = "coreinit", name = "OSIsDebuggerPresent")]
fn os_is_debugger_present(_ctx: &mut HleContext<'_>) -> bool {
    false
}

#[hle_export(lib = "coreinit", name = "DCFlushRange")]
fn dc_flush_range(_ctx: &mut HleContext<'_>, _address: u32, _size: u32) {}

#[hle_export(lib = "coreinit", name = "DCFlushRangeNoSync")]
fn dc_flush_range_no_sync(_ctx: &mut HleContext<'_>, _address: u32, _size: u32) {}

#[hle_export(lib = "coreinit", name = "DCStoreRange")]
fn dc_store_range(_ctx: &mut HleContext<'_>, _address: u32, _size: u32) {}

#[hle_export(lib = "coreinit", name = "DCStoreRangeNoSync")]
fn dc_store_range_no_sync(_ctx: &mut HleContext<'_>, _address: u32, _size: u32) {}

#[hle_export(lib = "coreinit", name = "DCInvalidateRange")]
fn dc_invalidate_range(_ctx: &mut HleContext<'_>, _address: u32, _size: u32) {}

#[hle_export(lib = "coreinit", name = "DCTouchRange")]
fn dc_touch_range(_ctx: &mut HleContext<'_>, _address: u32, _size: u32) {}

#[hle_export(lib = "coreinit", name = "ICInvalidateRange")]
fn ic_invalidate_range(_ctx: &mut HleContext<'_>, _address: u32, _size: u32) {}

#[hle_export(lib = "coreinit", name = "DCZeroRange")]
fn dc_zero_range(ctx: &mut HleContext<'_>, address: u32, size: u32) -> Result<(), HleError> {
    let start = address & !(CACHE_LINE - 1);
    let end = address.saturating_add(size).next_multiple_of(CACHE_LINE);
    Ok(ctx.memory.fill(start, end - start, 0)?)
}

#[hle_export(lib = "coreinit", name = "OSEnforceInorderIO")]
fn os_enforce_inorder_io(_ctx: &mut HleContext<'_>) {}

#[hle_export(lib = "coreinit", name = "OSGetConsoleType")]
fn os_get_console_type(_ctx: &mut HleContext<'_>) -> u32 {
    CONSOLE_TYPE_RETAIL
}

#[hle_export(lib = "coreinit", name = "bspGetHardwareVersion")]
fn bsp_get_hardware_version(ctx: &mut HleContext<'_>, version_out: u32) -> Result<bool, HleError> {
    ctx.memory.write_u32(version_out, HARDWARE_VERSION)?;
    Ok(true)
}
