//! The CPU's view of the machine.
use wiiu_cpu::{Bus, BusError};
use wiiu_memory::{Memory, MemoryError};
use wiiu_peripherals::TimeBase;

/// Memory plus the timebase, as seen by one core.
pub struct CoreBus<'a> {
    pub memory: &'a mut Memory,
    pub timebase: TimeBase,
    /// Lets the JIT address guest memory directly.
    pub jit_memory: bool,
}

const fn bus_error(error: MemoryError) -> BusError {
    match error {
        MemoryError::Unmapped { address, .. } | MemoryError::Overlap { start: address, .. } => {
            BusError { address }
        }
    }
}

macro_rules! forward {
    ($($read:ident, $write:ident: $ty:ty;)*) => {$(
        #[inline]
        fn $read(&mut self, address: u32) -> Result<$ty, BusError> {
            self.memory.$read(address).map_err(bus_error)
        }

        #[inline]
        fn $write(&mut self, address: u32, value: $ty) -> Result<(), BusError> {
            self.memory.$write(address, value).map_err(bus_error)
        }
    )*};
}

impl Bus for CoreBus<'_> {
    forward! {
        read_u8, write_u8: u8;
        read_u16, write_u16: u16;
        read_u32, write_u32: u32;
        read_u64, write_u64: u64;
    }

    fn timebase(&mut self) -> u64 {
        self.timebase.ticks_since_boot()
    }

    fn fast_memory(&mut self) -> Option<*mut u8> {
        self.jit_memory.then(|| self.memory.host_base())
    }
}
