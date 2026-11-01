//! Espresso timebase.
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Timebase frequency: bus clock (248.625 MHz) / 4.
pub const FREQUENCY: u64 = 62_156_250;

/// Seconds between the Unix epoch and the Wii U epoch (2000-01-01 00:00:00).
pub const WIIU_EPOCH_UNIX_SECONDS: u64 = 946_684_800;

/// Converts a duration to timebase ticks.
#[allow(clippy::cast_possible_truncation)]
pub const fn duration_to_ticks(duration: Duration) -> u64 {
    (duration.as_nanos() * FREQUENCY as u128 / 1_000_000_000) as u64
}

/// Converts timebase ticks to a duration.
#[allow(clippy::cast_possible_truncation)]
pub const fn ticks_to_duration(ticks: u64) -> Duration {
    Duration::from_nanos((ticks as u128 * 1_000_000_000 / FREQUENCY as u128) as u64)
}

/// Monotonic tick counter plus the wall-clock offset used by `OSGetTime`.
#[derive(Debug, Clone, Copy)]
pub struct TimeBase {
    boot: Instant,
    /// Ticks since the Wii U epoch at `boot`.
    epoch_ticks_at_boot: u64,
}

impl TimeBase {
    pub fn new() -> Self {
        let since_unix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let since_wiiu = since_unix.saturating_sub(Duration::from_secs(WIIU_EPOCH_UNIX_SECONDS));
        Self { boot: Instant::now(), epoch_ticks_at_boot: duration_to_ticks(since_wiiu) }
    }

    /// Ticks since power-on (`mftb`, `OSGetSystemTime`).
    pub fn ticks_since_boot(&self) -> u64 {
        duration_to_ticks(self.boot.elapsed())
    }

    /// Ticks since 2000-01-01 (`OSGetTime`).
    pub fn ticks_since_epoch(&self) -> u64 {
        self.epoch_ticks_at_boot + self.ticks_since_boot()
    }
}

impl Default for TimeBase {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_conversions() {
        assert_eq!(duration_to_ticks(Duration::from_secs(1)), FREQUENCY);
        assert_eq!(ticks_to_duration(FREQUENCY * 3), Duration::from_secs(3));
        let tb = TimeBase::new();
        assert!(tb.ticks_since_epoch() > FREQUENCY * 60 * 60 * 24 * 365 * 20);
    }
}
