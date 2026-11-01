//! Small Wii U devices, each independent of the rest of the system.
pub mod controller;
pub mod rtc;
pub mod timebase;

pub use controller::{GamePadState, VpadButtons};
pub use rtc::CalendarTime;
pub use timebase::TimeBase;
