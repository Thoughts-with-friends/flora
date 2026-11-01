//! Support code for the developer tools in `examples/` and the ROM smoke tests.
//!
//! ROMs are never referenced by name: they come from the git-ignored
//! `roms.json`, so the tools work with whatever the developer owns.
use std::path::{Path, PathBuf};

use flora_game_library::{GameEntry, GameLibrary, RomPaths};

/// Location of `roms.json`, relative to this crate.
pub fn roms_json() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("roms.json")
}

/// Every title listed by `roms.json`, or nothing when the file is missing.
pub fn local_games() -> Vec<GameEntry> {
    RomPaths::load(&roms_json())
        .map_or_else(|_| Vec::new(), |paths| GameLibrary::scan(&paths).games)
}
