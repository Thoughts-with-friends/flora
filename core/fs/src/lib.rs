//! Wii U virtual file system.
//!
//! Guest paths such as `/vol/content/data.bin` are resolved through a mount
//! table: the longest matching mount prefix picks a [`Device`], which serves
//! the rest of the path: a host directory, or an [`Overlay`] of several (an
//! update over its base title).
mod device;
mod error;
mod overlay;
mod path;
mod vfs;

pub use device::{Device, DirEntry, HostDirectory, Metadata};
pub use error::FsError;
pub use overlay::Overlay;
pub use path::GuestPath;
pub use vfs::Vfs;
