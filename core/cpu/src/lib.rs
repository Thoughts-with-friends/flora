//! Espresso: the Wii U's tri-core PowerPC 750CL derivative.
//!
//! ```text
//!   Bus::read_u32(pc) ──► decode::decode ──► Opcode ──► interpreter::{unit}
//!                              │                             │
//!                         instr::Instr                  state::CpuState
//!                        (field accessors)          (GPR/FPR/CR/XER/SPR...)
//! ```
//!
//! This crate knows nothing about memory layout, the OS or HLE: memory goes
//! through the [`Bus`] trait, and the special HLE-call opcode only surfaces as
//! [`StepEvent::HleCall`] for the system layer to dispatch.
pub mod bus;
pub mod decode;
pub mod disassemble;
pub mod instr;
pub mod interpreter;
pub mod jit;
pub mod state;

pub use bus::{Bus, BusError};
pub use decode::{Opcode, decode};
pub use instr::Instr;
pub use interpreter::{CpuError, StepEvent, run, step};
pub use jit::Jit;
pub use state::{CpuState, Fpr};

/// Primary opcode 1 is unused on Espresso; flora uses it for HLE calls.
/// The low 26 bits carry the HLE function index.
pub const HLE_CALL_PRIMARY_OPCODE: u32 = 1;

/// Encodes an HLE-call instruction for function `index`.
pub const fn encode_hle_call(index: u32) -> u32 {
    (HLE_CALL_PRIMARY_OPCODE << 26) | (index & 0x03FF_FFFF)
}

/// `blr`
pub const INSTR_BLR: u32 = 0x4E80_0020;
