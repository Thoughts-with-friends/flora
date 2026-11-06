//! RPX/RPL loader.
//!
//! ```text
//!  bytes ──► elf::ElfFile ──► rpl::RplFile ──► loader::load ──► LoadedModule
//!            (headers,         (decompressed     (place segments,
//!             sections)         sections, file    relocate, resolve
//!                               info, imports,    imports through
//!                               exports)          ImportResolver)
//! ```
//!
//! Parsing is pure; only [`loader::load`] touches guest memory, and it asks
//! its caller where segments go ([`SegmentAllocator`]) and what imports
//! resolve to ([`ImportResolver`]).
mod allocator;
#[cfg(any(test, feature = "builder"))]
pub mod builder;
pub mod elf;
mod error;
pub mod loader;
mod relocation;
pub mod rpl;

pub use allocator::LinearAllocator;
pub use error::LoadError;
pub use loader::{
    ImportKind, ImportResolver, LoadedModule, SegmentAllocator, SegmentKind, TLS_MODULE_INDEX,
    TlsImage, load,
};
pub use rpl::{FileInfo, RplFile};
