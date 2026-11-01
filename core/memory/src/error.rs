/// A guest memory access that could not be served.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MemoryError {
    #[error("unmapped guest access at {address:#010x} (+{len} bytes)")]
    Unmapped { address: u32, len: u32 },
    #[error("region {name} at {start:#010x} overlaps an existing mapping")]
    Overlap { name: &'static str, start: u32 },
}
