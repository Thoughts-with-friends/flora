//! The command processor (CP): PM4 parsing and execution.
pub mod executor;
pub mod opcode;
pub mod pm4;

pub use executor::CommandProcessor;
pub use pm4::{Packet, parse};
