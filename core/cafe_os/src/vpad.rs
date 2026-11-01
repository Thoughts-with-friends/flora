//! `vpad.rpl`: the Wii U GamePad.
use wiiu_hle_macros::hle_export;
use wiiu_memory::GuestPtr;

use crate::hle::{HleContext, HleError, HleExport};

/// `sizeof(VPADStatus)`.
const VPAD_STATUS_SIZE: u32 = 0xAC;

mod offset {
    pub const HOLD: u32 = 0x00;
    pub const TRIGGER: u32 = 0x04;
    pub const RELEASE: u32 = 0x08;
    pub const LEFT_STICK: u32 = 0x0C;
    pub const RIGHT_STICK: u32 = 0x14;
}

/// `VPAD_READ_SUCCESS`.
const READ_SUCCESS: i32 = 0;

#[hle_export(lib = "vpad", name = "VPADInit")]
fn vpad_init(_ctx: &mut HleContext<'_>) {}

/// Returns the number of samples written (one: the latest state).
#[hle_export(lib = "vpad", name = "VPADRead")]
fn vpad_read(
    ctx: &mut HleContext<'_>,
    channel: u32,
    buffers: GuestPtr<u8>,
    count: u32,
    error: GuestPtr<i32>,
) -> Result<i32, HleError> {
    if channel != 0 || count == 0 || buffers.is_null() {
        return Ok(0);
    }
    let pad = ctx.os.gamepad;
    let (trigger, release) = ctx.os.gamepad_edges.update(pad.held);
    let base = buffers.address();
    ctx.memory.fill(base, VPAD_STATUS_SIZE, 0)?;
    ctx.memory.write_u32(base + offset::HOLD, pad.held.bits())?;
    ctx.memory.write_u32(base + offset::TRIGGER, trigger.bits())?;
    ctx.memory.write_u32(base + offset::RELEASE, release.bits())?;
    for (at, stick) in
        [(offset::LEFT_STICK, pad.left_stick), (offset::RIGHT_STICK, pad.right_stick)]
    {
        ctx.memory.write_f32(base + at, stick.x)?;
        ctx.memory.write_f32(base + at + 4, stick.y)?;
    }
    if !error.is_null() {
        error.write(ctx.memory, READ_SUCCESS)?;
    }
    Ok(1)
}

pub const EXPORTS: &[HleExport] = &[VPAD_INIT, VPAD_READ];

#[cfg(test)]
mod tests {
    use wiiu_peripherals::VpadButtons;

    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn reports_held_and_edges() {
        let mut env = TestEnv::new();
        env.os.gamepad.held = VpadButtons::A;
        assert_eq!(env.call("vpad", "VPADRead", &[0, SCRATCH, 1, SCRATCH + 0x100]), 1);
        assert_eq!(env.memory.read_u32(SCRATCH).unwrap(), VpadButtons::A.bits());
        assert_eq!(env.memory.read_u32(SCRATCH + 4).unwrap(), VpadButtons::A.bits());
        env.call("vpad", "VPADRead", &[0, SCRATCH, 1, 0]);
        assert_eq!(env.memory.read_u32(SCRATCH + 4).unwrap(), 0);
    }
}
