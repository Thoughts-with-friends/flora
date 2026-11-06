//! Wii U title formats.
//!
//! * [`TitleSource::open`] recognises what the user pointed at: an installed
//!   title directory (`code/`, `content/`, `meta/`) or a bare `.rpx`.
//! * [`meta`] parses `meta.xml`, `app.xml` and `cos.xml`.
//! * [`update`] finds a title's installed update in system storage (`mlc01`).
//! * [`wux`] reads compressed disc images (`.wux`) as a flat WUD stream.
//!   Decrypting the disc partitions needs console keys and is not done here.
mod error;
pub mod meta;
mod source;
pub mod update;
pub mod wux;

pub use error::TitleError;
pub use source::{TitleInfo, TitleSource};
