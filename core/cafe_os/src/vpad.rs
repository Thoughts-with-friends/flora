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
    /// Three `VPADTPData`: the raw sample and two filtered ones.
    pub const TOUCH: [u32; 3] = [0x52, 0x5A, 0x62];
}

/// `VPADTPData::validity`.
const TOUCH_VALID: u16 = 0;
const TOUCH_INVALID: u16 = 3;

/// What the touch panel reports across the screen and where it starts: its Y axis
/// runs upwards. Cemu's `VPADController::update_touch`.
const PANEL_WIDTH: f32 = 3883.0;
const PANEL_HEIGHT: f32 = 3694.0;
const PANEL_LEFT: f32 = 92.0;
const PANEL_TOP: f32 = 4095.0 - 254.0;

/// Screen pixels of the GamePad, which `GamePadState::touch` counts in.
const SCREEN: (f32, f32) = (854.0, 480.0);

/// Screen position to raw panel coordinates.
fn panel_from_screen(x: u16, y: u16) -> (u16, u16) {
    let (x, y) = ((f32::from(x) / SCREEN.0).min(1.0), (f32::from(y) / SCREEN.1).min(1.0));
    ((x * PANEL_WIDTH + PANEL_LEFT) as u16, (PANEL_TOP - y * PANEL_HEIGHT) as u16)
}

/// Raw panel coordinates to a position on a `width` x `height` screen.
fn screen_from_panel(x: u16, y: u16, width: f32, height: f32) -> (u16, u16) {
    let x = (f32::from(x) - PANEL_LEFT).max(0.0) / PANEL_WIDTH;
    let y = (PANEL_TOP - f32::from(y)).max(0.0) / PANEL_HEIGHT;
    ((x.min(1.0) * width) as u16, (y.min(1.0) * height) as u16)
}

/// Copies `VPADTPData` converted to `width` x `height` from `raw` to `out`.
fn calibrate(
    ctx: &mut HleContext<'_>,
    out: u32,
    raw: u32,
    width: f32,
    height: f32,
) -> Result<(), HleError> {
    let (x, y) = (ctx.memory.read_u16(raw)?, ctx.memory.read_u16(raw + 2)?);
    let (touch, validity) = (ctx.memory.read_u16(raw + 4)?, ctx.memory.read_u16(raw + 6)?);
    let (x, y) = screen_from_panel(x, y, width, height);
    ctx.memory.write_u16(out, x)?;
    ctx.memory.write_u16(out + 2, y)?;
    ctx.memory.write_u16(out + 4, touch)?;
    ctx.memory.write_u16(out + 6, validity)?;
    Ok(())
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
    let held = pad.held_with_stick_directions();
    let (trigger, release) = ctx.os.gamepad_edges.update(held);
    let base = buffers.address();
    ctx.memory.fill(base, VPAD_STATUS_SIZE, 0)?;
    ctx.memory.write_u32(base + offset::HOLD, held.bits())?;
    ctx.memory.write_u32(base + offset::TRIGGER, trigger.bits())?;
    ctx.memory.write_u32(base + offset::RELEASE, release.bits())?;
    for (at, stick) in
        [(offset::LEFT_STICK, pad.left_stick), (offset::RIGHT_STICK, pad.right_stick)]
    {
        ctx.memory.write_f32(base + at, stick.x)?;
        ctx.memory.write_f32(base + at + 4, stick.y)?;
    }
    // Coordinates stay after the finger leaves: some titles read them then.
    if let Some((x, y)) = pad.touch {
        ctx.os.last_touch = panel_from_screen(x, y);
    }
    let (x, y) = ctx.os.last_touch;
    let (touching, validity) =
        if pad.touch.is_some() { (1, TOUCH_VALID) } else { (0, TOUCH_INVALID) };
    for at in offset::TOUCH {
        ctx.memory.write_u16(base + at, x)?;
        ctx.memory.write_u16(base + at + 2, y)?;
        ctx.memory.write_u16(base + at + 4, touching)?;
        ctx.memory.write_u16(base + at + 6, validity)?;
    }
    if !error.is_null() {
        error.write(ctx.memory, READ_SUCCESS)?;
    }
    Ok(1)
}

/// Raw panel coordinates to the 1280 x 720 titles use by default.
#[hle_export(lib = "vpad", name = "VPADGetTPCalibratedPoint")]
fn vpad_get_tp_calibrated_point(
    ctx: &mut HleContext<'_>,
    _channel: u32,
    out: u32,
    raw: u32,
) -> Result<(), HleError> {
    calibrate(ctx, out, raw, 1280.0, 720.0)
}

/// `VPADTPResolution`: 0 is 1920 x 1080, 1 is 1280 x 720, 2 is 854 x 480.
#[hle_export(lib = "vpad", name = "VPADGetTPCalibratedPointEx")]
fn vpad_get_tp_calibrated_point_ex(
    ctx: &mut HleContext<'_>,
    _channel: u32,
    resolution: u32,
    out: u32,
    raw: u32,
) -> Result<(), HleError> {
    let (width, height) = match resolution {
        0 => (1920.0, 1080.0),
        1 => (1280.0, 720.0),
        _ => SCREEN,
    };
    calibrate(ctx, out, raw, width, height)
}

pub const EXPORTS: &[HleExport] =
    &[VPAD_INIT, VPAD_READ, VPAD_GET_TP_CALIBRATED_POINT, VPAD_GET_TP_CALIBRATED_POINT_EX];

#[cfg(test)]
mod tests {
    use wiiu_peripherals::VpadButtons;

    use super::offset;
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn touches_reach_the_title_and_convert_back_to_the_screen() {
        let mut env = TestEnv::new();
        env.os.gamepad.touch = Some((427, 240));
        env.call("vpad", "VPADRead", &[0, SCRATCH, 1, 0]);
        let raw = SCRATCH + offset::TOUCH[0];
        let word = |env: &TestEnv, at: u32| env.memory.read_u16(at).unwrap();
        // Cemu's formula: x * 3883 + 92, 4095 - y * 3694 - 254.
        assert_eq!((word(&env, raw), word(&env, raw + 2)), (2033, 1994));
        assert_eq!((word(&env, raw + 4), word(&env, raw + 6)), (1, 0));

        let out = SCRATCH + 0x200;
        env.call("vpad", "VPADGetTPCalibratedPoint", &[0, out, raw]);
        let (x, y) = (word(&env, out), word(&env, out + 2));
        assert!(x.abs_diff(640) <= 1 && y.abs_diff(360) <= 1, "{x},{y}");

        // Released: not valid any more, the position stays.
        env.os.gamepad.touch = None;
        env.call("vpad", "VPADRead", &[0, SCRATCH, 1, 0]);
        assert_eq!(word(&env, raw), 2033);
        assert_eq!((word(&env, raw + 4), word(&env, raw + 6)), (0, 3));
    }

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
