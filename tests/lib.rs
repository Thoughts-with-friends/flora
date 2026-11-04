//! Support code for the developer tools in `examples/` and the ROM smoke tests.
//!
//! ROMs are never referenced by name: their directories come from the git-ignored
//! `shocora.toml`, so the tools work with whatever the developer owns.
use std::path::Path;

use shocora_game_library::{GameEntry, GameLibrary, LEGACY_PATH, legacy_rom_dirs};
use shocora_gui_common::config::AppConfig;

/// Every title in the directories of the repository's `shocora.toml`, or of an old `roms.json`
/// when that has none.
pub fn local_games() -> Vec<GameEntry> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut dirs = AppConfig::load(&root.join("shocora.toml")).rom_dirs;
    if dirs.is_empty() {
        dirs = legacy_rom_dirs(&root.join(LEGACY_PATH)).unwrap_or_default();
    }
    GameLibrary::scan(&dirs).games
}
