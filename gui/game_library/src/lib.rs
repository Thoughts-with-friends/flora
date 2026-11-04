//! Game library.
//!
//! ROM directories are listed in `shocora.toml`. Each may contain extracted titles
//! (`<name>/code`, `content`, `meta`) and bare `.rpx` files; [`GameLibrary::scan`] finds them
//! all.
use std::path::{Path, PathBuf};

use wiiu_title::{TitleInfo, TitleSource};

/// Where the ROM directories were listed before `shocora.toml` took them over.
pub const LEGACY_PATH: &str = "roms.json";

/// Contents of the old `roms.json`.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct LegacyRomPaths {
    rom_dirs: Vec<PathBuf>,
}

/// The directories an old `roms.json` lists, if there is a readable one.
pub fn legacy_rom_dirs(path: &Path) -> Option<Vec<PathBuf>> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<LegacyRomPaths>(&text) {
        Ok(paths) => Some(paths.rom_dirs),
        Err(error) => {
            log::warn!("{}: {error}", path.display());
            None
        }
    }
}

/// One bootable title found in a ROM directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameEntry {
    /// Name to display (Japanese name if the title has one).
    pub name: String,
    pub title_id: Option<u64>,
    /// Path to hand to `Emulator::load_title`.
    pub path: PathBuf,
    pub rpx_path: PathBuf,
}

impl GameEntry {
    fn from_info(path: PathBuf, info: &TitleInfo) -> Self {
        Self {
            name: info.display_name(),
            title_id: info.meta.title_id,
            path,
            rpx_path: info.rpx_path.clone(),
        }
    }
}

/// Titles found in all ROM directories.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GameLibrary {
    pub games: Vec<GameEntry>,
    /// Paths that looked like titles but failed to open, with the reason.
    pub problems: Vec<String>,
}

impl GameLibrary {
    /// Scans every directory in `dirs` (one level deep).
    pub fn scan(dirs: &[PathBuf]) -> Self {
        let mut library = Self::default();
        for dir in dirs {
            library.scan_dir(dir);
        }
        library.games.sort_by(|a, b| a.name.cmp(&b.name));
        library
    }

    fn scan_dir(&mut self, dir: &Path) {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                self.problems.push(format!("{}: {error}", dir.display()));
                return;
            }
        };
        let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            let candidate = if path.is_dir() {
                path.join("code").is_dir()
            } else {
                path.extension().is_some_and(|e| e.eq_ignore_ascii_case("rpx"))
            };
            if !candidate {
                continue;
            }
            match TitleSource::open(&path) {
                Ok(info) => self.games.push(GameEntry::from_info(path, &info)),
                Err(error) => {
                    log::warn!("{error}");
                    self.problems.push(error.to_string());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn scans_titles_and_rpx_files() {
        let root = std::env::temp_dir().join(format!("shocora-library-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Game B/code")).unwrap();
        fs::create_dir_all(root.join("Game B/meta")).unwrap();
        fs::write(root.join("Game B/code/main.rpx"), b"").unwrap();
        fs::write(
            root.join("Game B/meta/meta.xml"),
            "<menu><title_id>00050000DEAD0000</title_id><longname_ja>ゲーム</longname_ja></menu>",
        )
        .unwrap();
        fs::write(root.join("homebrew.rpx"), b"").unwrap();
        fs::create_dir_all(root.join("not a title")).unwrap();
        fs::create_dir_all(root.join("broken/code")).unwrap();

        let library = GameLibrary::scan(&[root.clone(), root.join("missing")]);

        let names: Vec<_> = library.games.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["homebrew", "ゲーム"]);
        assert_eq!(library.games[1].title_id, Some(0x0005_0000_DEAD_0000));
        assert_eq!(library.problems.len(), 2); // "broken" has no RPX, "missing" does not exist
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_old_rom_list_is_still_understood() {
        let path = std::env::temp_dir().join(format!("shocora-roms-{}.json", std::process::id()));
        fs::write(&path, r#"{ "rom_dirs": ["D:\\Roms"] }"#).unwrap();
        assert_eq!(legacy_rom_dirs(&path), Some(vec![PathBuf::from("D:\\Roms")]));
        fs::write(&path, "not json").unwrap();
        assert_eq!(legacy_rom_dirs(&path), None);
        let _ = fs::remove_file(&path);
        assert_eq!(legacy_rom_dirs(&path), None);
    }
}
