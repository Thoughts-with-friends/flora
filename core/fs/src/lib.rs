//! Wii U virtual file system.
//!
//! Guest paths such as `/vol/content/data.bin` are resolved through a mount
//! table: the longest matching mount prefix picks a [`Device`], which serves
//! the rest of the path. Only host directories are implemented for now.
mod device;
mod error;
mod path;
mod vfs;

pub use device::{Device, DirEntry, HostDirectory, Metadata};
pub use error::FsError;
pub use path::GuestPath;
pub use vfs::Vfs;
