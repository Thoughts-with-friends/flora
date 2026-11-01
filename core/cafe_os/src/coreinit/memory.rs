//! Memory primitives exported by coreinit.
#![allow(clippy::cast_possible_truncation)]

use wiiu_hle_macros::hle_export;
use wiiu_memory::GuestPtr;

use crate::hle::{HleContext, HleError};

#[hle_export(lib = "coreinit", name = "memcpy")]
fn memcpy(
    ctx: &mut HleContext<'_>,
    dst: GuestPtr<u8>,
    src: GuestPtr<u8>,
    size: u32,
) -> Result<GuestPtr<u8>, HleError> {
    ctx.memory.copy_within(dst.address(), src.address(), size)?;
    Ok(dst)
}

#[hle_export(lib = "coreinit", name = "memmove")]
fn memmove(
    ctx: &mut HleContext<'_>,
    dst: GuestPtr<u8>,
    src: GuestPtr<u8>,
    size: u32,
) -> Result<GuestPtr<u8>, HleError> {
    ctx.memory.copy_within(dst.address(), src.address(), size)?;
    Ok(dst)
}

#[hle_export(lib = "coreinit", name = "memset")]
fn memset(
    ctx: &mut HleContext<'_>,
    dst: GuestPtr<u8>,
    value: u32,
    size: u32,
) -> Result<GuestPtr<u8>, HleError> {
    ctx.memory.fill(dst.address(), size, value as u8)?;
    Ok(dst)
}

#[hle_export(lib = "coreinit", name = "OSBlockMove")]
fn os_block_move(
    ctx: &mut HleContext<'_>,
    dst: GuestPtr<u8>,
    src: GuestPtr<u8>,
    size: u32,
    _flush: bool,
) -> Result<GuestPtr<u8>, HleError> {
    ctx.memory.copy_within(dst.address(), src.address(), size)?;
    Ok(dst)
}

#[hle_export(lib = "coreinit", name = "OSBlockSet")]
fn os_block_set(
    ctx: &mut HleContext<'_>,
    dst: GuestPtr<u8>,
    value: u32,
    size: u32,
) -> Result<GuestPtr<u8>, HleError> {
    ctx.memory.fill(dst.address(), size, value as u8)?;
    Ok(dst)
}

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn copy_and_fill() {
        let mut env = TestEnv::new();
        env.memory.write_c_string(SCRATCH, "abc").unwrap();
        assert_eq!(env.call("coreinit", "memcpy", &[SCRATCH + 16, SCRATCH, 4]), SCRATCH + 16);
        assert_eq!(env.memory.read_c_string(SCRATCH + 16, 8).unwrap(), "abc");
        env.call("coreinit", "memset", &[SCRATCH, u32::from(b'z'), 2]);
        assert_eq!(env.memory.read_c_string(SCRATCH, 8).unwrap(), "zzc");
    }
}
