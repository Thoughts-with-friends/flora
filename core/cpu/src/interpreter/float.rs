//! Scalar floating point and FPSCR access.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use super::{ExecResult, unimplemented};
use crate::{CpuState, Fpr, Instr, Opcode, StepEvent};

/// Rounds to single precision, keeping the `f64` container.
pub(super) fn round_single(value: f64) -> f64 {
    f64::from(value as f32)
}

/// Bit pattern hardware leaves in the upper word of `fctiw` results.
const FCTIW_UPPER: u64 = 0xFFF8_0000_0000_0000;

/// Rounding modes of FPSCR[RN].
fn to_int32(value: f64, rounding: u32) -> u32 {
    if value.is_nan() {
        return 0x8000_0000;
    }
    let rounded = match rounding {
        0 => value.round_ties_even(),
        1 => value.trunc(),
        2 => value.ceil(),
        _ => value.floor(),
    };
    if rounded >= 2_147_483_647.0 {
        0x7FFF_FFFF
    } else if rounded <= -2_147_483_648.0 {
        0x8000_0000
    } else {
        rounded as i32 as u32
    }
}

/// Compares two values, writing CR field `field` and FPSCR[FPCC].
pub(super) fn compare(s: &mut CpuState, field: usize, a: f64, b: f64) {
    let result = if a.is_nan() || b.is_nan() {
        0b0001
    } else if a < b {
        0b1000
    } else if a > b {
        0b0100
    } else {
        0b0010
    };
    s.set_cr_field(field, result);
    s.fpscr = (s.fpscr & !0xF000) | (result << 12);
}

/// Sign-bit manipulation on raw bits (does not canonicalise NaNs).
pub(super) fn with_sign(value: f64, op: Opcode) -> f64 {
    const SIGN: u64 = 1 << 63;
    let bits = value.to_bits();
    f64::from_bits(match op {
        Opcode::Fneg | Opcode::PsNeg => bits ^ SIGN,
        Opcode::Fabs | Opcode::PsAbs => bits & !SIGN,
        Opcode::Fnabs | Opcode::PsNabs => bits | SIGN,
        _ => bits,
    })
}

/// Arithmetic shared by double, single and paired-single forms.
pub(super) fn arith(op: Opcode, a: f64, b: f64, c: f64) -> f64 {
    use Opcode as O;
    match op {
        O::Fadd | O::Fadds | O::PsAdd => a + b,
        O::Fsub | O::Fsubs | O::PsSub => a - b,
        O::Fmul | O::Fmuls | O::PsMul => a * c,
        O::Fdiv | O::Fdivs | O::PsDiv => a / b,
        O::Fmadd | O::Fmadds | O::PsMadd => a.mul_add(c, b),
        O::Fmsub | O::Fmsubs | O::PsMsub => a.mul_add(c, -b),
        O::Fnmadd | O::Fnmadds | O::PsNmadd => -a.mul_add(c, b),
        O::Fnmsub | O::Fnmsubs | O::PsNmsub => -a.mul_add(c, -b),
        O::Fres | O::PsRes => 1.0 / b,
        O::Frsqrte | O::PsRsqrte => 1.0 / b.sqrt(),
        O::Fsel | O::PsSel => {
            if a >= 0.0 {
                c
            } else {
                b
            }
        }
        _ => f64::NAN,
    }
}

pub(super) fn execute(op: Opcode, i: Instr, s: &mut CpuState) -> ExecResult {
    use Opcode as O;
    let (d, a, b, c) = (i.rd(), i.ra(), i.rb(), i.rc_reg());
    let (fa, fb, fc) = (s.fpr[a].ps0, s.fpr[b].ps0, s.fpr[c].ps0);
    match op {
        // Single precision: result rounded and written to both slots.
        O::Fadds
        | O::Fsubs
        | O::Fmuls
        | O::Fdivs
        | O::Fmadds
        | O::Fmsubs
        | O::Fnmadds
        | O::Fnmsubs
        | O::Fres => {
            s.fpr[d] = Fpr::splat(round_single(arith(op, fa, fb, fc)));
        }
        // Double precision: ps0 only.
        O::Fadd
        | O::Fsub
        | O::Fmul
        | O::Fdiv
        | O::Fmadd
        | O::Fmsub
        | O::Fnmadd
        | O::Fnmsub
        | O::Frsqrte
        | O::Fsel => {
            s.fpr[d].ps0 = arith(op, fa, fb, fc);
        }
        O::Frsp => s.fpr[d] = Fpr::splat(round_single(fb)),
        O::Fctiw | O::Fctiwz => {
            let rounding = if op == O::Fctiwz { 1 } else { s.fpscr & 3 };
            s.fpr[d].ps0 = f64::from_bits(FCTIW_UPPER | u64::from(to_int32(fb, rounding)));
        }
        O::Fmr | O::Fneg | O::Fabs | O::Fnabs => s.fpr[d].ps0 = with_sign(fb, op),
        O::Fcmpu | O::Fcmpo => compare(s, i.crfd(), fa, fb),
        O::Mffs => s.fpr[d].ps0 = f64::from_bits(u64::from(s.fpscr)),
        O::Mtfsf => {
            let value = s.fpr[b].ps0.to_bits() as u32;
            let mask = (0..8)
                .filter(|field| i.fm() & (0x80 >> field) != 0)
                .fold(0_u32, |mask, field| mask | (0xF000_0000 >> (4 * field)));
            s.fpscr = (s.fpscr & !mask) | (value & mask);
        }
        O::Mtfsfi => {
            let shift = 28 - 4 * i.crfd() as u32;
            s.fpscr = (s.fpscr & !(0xF << shift)) | (i.fpscr_imm() << shift);
        }
        O::Mtfsb0 => s.fpscr &= !(0x8000_0000 >> d),
        O::Mtfsb1 => s.fpscr |= 0x8000_0000 >> d,
        O::Mcrfs => {
            let shift = 28 - 4 * i.crfs() as u32;
            s.set_cr_field(i.crfd(), (s.fpscr >> shift) & 0xF);
        }
        _ => return unimplemented(op),
    }
    if i.rc() && !matches!(op, O::Fcmpu | O::Fcmpo | O::Mcrfs) {
        s.update_cr1();
    }
    Ok(StepEvent::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::asm::{a, x};

    fn exec(s: &mut CpuState, raw: u32) {
        execute(crate::decode(raw), Instr(raw), s).unwrap();
    }

    #[test]
    fn single_ops_fill_both_slots() {
        let mut s = CpuState::default();
        s.fpr[2].ps0 = 1.0;
        s.fpr[3].ps0 = 0.1;
        exec(&mut s, a(59, 1, 2, 3, 0, 21)); // fadds f1, f2, f3
        assert_eq!(s.fpr[1].ps0, f64::from(1.1_f32));
        assert_eq!(s.fpr[1].ps0, s.fpr[1].ps1);
    }

    #[test]
    fn fused_multiply_add() {
        let mut s = CpuState::default();
        s.fpr[2].ps0 = 2.0;
        s.fpr[3].ps0 = 3.0;
        s.fpr[4].ps0 = 4.0;
        exec(&mut s, a(63, 1, 2, 4, 3, 29)); // fmadd f1, f2, f3, f4 = 2*3+4
        assert_eq!(s.fpr[1].ps0, 10.0);
    }

    #[test]
    fn convert_to_integer() {
        let mut s = CpuState::default();
        s.fpr[2].ps0 = -2.7;
        exec(&mut s, x(63, 1, 0, 2, 15, false)); // fctiwz f1, f2
        assert_eq!(s.fpr[1].ps0.to_bits() as u32 as i32, -2);
        s.fpr[2].ps0 = 1e20;
        exec(&mut s, x(63, 1, 0, 2, 14, false)); // fctiw (round to nearest)
        assert_eq!(s.fpr[1].ps0.to_bits() as u32, 0x7FFF_FFFF);
    }

    #[test]
    fn compare_sets_cr() {
        let mut s = CpuState::default();
        s.fpr[1].ps0 = f64::NAN;
        exec(&mut s, x(63, 6 << 2, 1, 2, 0, false)); // fcmpu cr6, f1, f2
        assert_eq!(s.cr_field(6), 0b0001);
    }
}
