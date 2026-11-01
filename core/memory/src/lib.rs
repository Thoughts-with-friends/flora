//! Wii U guest memory.
//!
//! [`Memory`] owns the backing storage of every mapped [`Region`] and offers
//! big-endian accessors. [`GuestPtr`] is a typed effective address that reads
//! and writes through a [`Memory`].
mod error;
mod guest_ptr;
mod host;
mod memory;

pub use error::MemoryError;
pub use guest_ptr::{GuestPtr, GuestValue};
pub use memory::Memory;
pub use wiiu_mem_addr::Region;
