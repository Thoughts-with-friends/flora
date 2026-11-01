//! Process termination.
use wiiu_hle_macros::hle_export;
use wiiu_memory::GuestPtr;

use crate::hle::{HleContext, HleError};

fn terminate(ctx: &mut HleContext<'_>, code: i32) {
    log::info!("guest exited with status {code}");
    ctx.os.exit_code = Some(code);
    ctx.os.frame_done = true;
}

#[hle_export(lib = "coreinit", name = "exit")]
fn exit(ctx: &mut HleContext<'_>, code: i32) {
    terminate(ctx, code);
}

#[hle_export(lib = "coreinit", name = "_Exit")]
fn underscore_exit(ctx: &mut HleContext<'_>, code: i32) {
    terminate(ctx, code);
}

#[hle_export(lib = "coreinit", name = "OSFatal")]
fn os_fatal(ctx: &mut HleContext<'_>, message: GuestPtr<u8>) -> Result<(), HleError> {
    let message = ctx.memory.read_c_string(message.address(), 0x1000)?;
    Err(HleError::Fatal(message))
}
