//! Small process-level exports with no further machinery behind them.
use wiiu_hle_macros::hle_export;

use crate::hle::HleContext;

/// Threads are torn down with the process; nothing to block.
#[hle_export(lib = "coreinit", name = "OSBlockThreadsOnExit")]
fn os_block_threads_on_exit(_ctx: &mut HleContext<'_>) {}

/// Returns `true` (the setting was applied).
#[hle_export(lib = "coreinit", name = "OSEnableHomeButtonMenu")]
fn os_enable_home_button_menu(ctx: &mut HleContext<'_>, enable: bool) -> bool {
    ctx.os.coreinit.home_button_menu = enable;
    true
}

#[hle_export(lib = "coreinit", name = "OSIsHomeButtonMenuEnabled")]
fn os_is_home_button_menu_enabled(ctx: &mut HleContext<'_>) -> bool {
    ctx.os.coreinit.home_button_menu
}
