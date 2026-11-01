//! Game library.
//!
//! ROM directories are listed in `roms.json` (machine-specific, git-ignored;
//! see `roms.example.json`):
//!
//! ```json
//! { "rom_dirs": ["D:\\GAME\\Emulator\\WiiU\\Roms"] }
//! ```
//!
//! Each directory may contain extracted titles (`<name>/code`, `content`,
//! `meta`) and bare `.rpx` files; [`GameLibrary::scan`] finds them all.
use std::path::{Path, PathBuf};

use wiiu_title::{TitleInfo, TitleSource};

/// Default location of the ROM list: `roms.json` in the working directory.
pub const DEFAULT_PATH: &str = "roms.json";

/// Contents of `roms.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RomPaths {
    /// Directories scanned for titles.
    pub rom_dirs: Vec<PathBuf>,
}

/// Why `roms.json` could not be read.
#[derive(Debug)]
pub enum RomPathsError {
    Missing(PathBuf),
    Io(PathBuf, std::io::Error),
    Json(PathBuf, serde_json::Error),
}

impl std::fmt::Display for RomPathsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(path) => {
                write!(f, "{} not found (copy roms.example.json)", path.display())
            }
            Self::Io(path, e) => write!(f, "{}: {e}", path.display()),
            Self::Json(path, e) => write!(f, "{}: {e}", path.display()),
        }
    }
}

impl std::error::Error for RomPathsError {}

impl RomPaths {
    /// Reads `path`.
    ///
    /// # Errors
    /// Fails if the file is missing, unreadable or not valid JSON.
    pub fn load(path: &Path) -> Result<Self, RomPathsError> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                RomPathsError::Missing(path.to_owned())
            } else {
                RomPathsError::Io(path.to_owned(), e)
            }
        })?;
        serde_json::from_str(&text).map_err(|e| RomPathsError::Json(path.to_owned(), e))
    }

    /// Writes `path` as pretty JSON.
    ///
    /// # Errors
    /// Fails if the file cannot be written.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, format!("{text}\n"))
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
    /// Scans every directory in `paths` (one level deep).
    pub fn scan(paths: &RomPaths) -> Self {
        let mut library = Self::default();
        for dir in &paths.rom_dirs {
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
        let root = std::env::temp_dir().join(format!("flora-library-{}", std::process::id()));
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

        let json_path = root.join("roms.json");
        RomPaths { rom_dirs: vec![root.clone(), root.join("missing")] }.save(&json_path).unwrap();
        let library = GameLibrary::scan(&RomPaths::load(&json_path).unwrap());

        let names: Vec<_> = library.games.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["homebrew", "ゲーム"]);
        assert_eq!(library.games[1].title_id, Some(0x0005_0000_DEAD_0000));
        assert_eq!(library.problems.len(), 2); // "broken" has no RPX, "missing" does not exist
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_file_is_reported() {
        assert!(matches!(
            RomPaths::load(Path::new("definitely-missing.json")),
            Err(RomPathsError::Missing(_))
        ));
    }
}
