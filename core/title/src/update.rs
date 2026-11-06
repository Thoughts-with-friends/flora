//! Updates installed in Wii U system storage, and titles that refuse to start
//! without one.
//!
//! ```text
//!  <mlc>/usr/title/00050000/<id>/   base title (not used here: booted from where it is)
//!  <mlc>/usr/title/0005000e/<id>/   its update: code/, content/, meta/ that lie over the base
//! ```
//!
//! `<mlc>` is the console's internal storage (`mlc01`) as Cemu lays it out.
use std::path::{Path, PathBuf};

/// High half of the title ID of every update.
const UPDATE_TITLE_TYPE: u64 = 0x0005_000E;

/// Title ID of the update of `title_id`.
pub const fn update_title_id(title_id: u64) -> u64 {
    (UPDATE_TITLE_TYPE << 32) | (title_id & 0xFFFF_FFFF)
}

/// Where Cemu keeps its system storage on this machine, if the platform has a known place.
pub fn default_mlc_dirs() -> Vec<PathBuf> {
    let env = |name| std::env::var_os(name).map(PathBuf::from);
    let dir = if cfg!(windows) {
        env("APPDATA").map(|d| d.join("Cemu"))
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|d| d.join("Library/Application Support/Cemu"))
    } else {
        env("XDG_DATA_HOME")
            .or_else(|| env("HOME").map(|d| d.join(".local/share")))
            .map(|d| d.join("Cemu"))
    };
    dir.map(|d| d.join("mlc01")).into_iter().collect()
}

/// The installed update of `title_id`: the first `<mlc>/usr/title/0005000e/<id>` with a
/// `code/` directory, searching `mlc_dirs` in order.
pub fn find_update(title_id: u64, mlc_dirs: &[PathBuf]) -> Option<PathBuf> {
    let id = update_title_id(title_id);
    let (high, low) = (id >> 32, id & 0xFFFF_FFFF);
    // Cemu writes lowercase; a copy made by hand may not be.
    let names = [
        (format!("{high:08x}"), format!("{low:08x}")),
        (format!("{high:08X}"), format!("{low:08X}")),
    ];
    mlc_dirs.iter().find_map(|mlc| {
        names.iter().map(|(h, l)| mlc.join("usr/title").join(h).join(l)).find(|dir| is_title(dir))
    })
}

fn is_title(dir: &Path) -> bool {
    dir.join("code").is_dir()
}

/// A file a title's base version checks before starting, and what it holds once updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionGate {
    /// Path inside `/vol/content`.
    pub file: &'static str,
    /// Contents that let the title start.
    pub contents: &'static [u8],
}

/// *Breath of the Wild* (JPN, USA, EUR): the disc's `System/Version.txt` says `0.9.0`,
/// and below 1.0 the title only says that it must be updated. The last update (v208)
/// says `1.5.0`.
const BREATH_OF_THE_WILD: (&[u64], VersionGate) = (
    &[0x0005_0000_101C_9300, 0x0005_0000_101C_9400, 0x0005_0000_101C_9500],
    VersionGate { file: "/System/Version.txt", contents: b"1.5.0" },
);

const GATES: &[(&[u64], VersionGate)] = &[BREATH_OF_THE_WILD];

/// The gate of `title_id`, for titles known to refuse to start without an update.
pub fn version_gate(title_id: u64) -> Option<VersionGate> {
    GATES.iter().find(|(ids, _)| ids.contains(&title_id)).map(|&(_, gate)| gate)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn finds_updates_in_mlc_layout() {
        let mlc =
            std::env::temp_dir().join(format!("shocora-mlc-{}", std::process::id())).join("mlc01");
        let _ = fs::remove_dir_all(&mlc);
        let update = mlc.join("usr/title/0005000e/101c9300");
        fs::create_dir_all(update.join("code")).unwrap();

        let base = 0x0005_0000_101C_9300;
        assert_eq!(update_title_id(base), 0x0005_000E_101C_9300);
        assert_eq!(find_update(base, &[PathBuf::from("missing"), mlc.clone()]), Some(update));
        assert_eq!(find_update(0x0005_0000_1010_EC00, std::slice::from_ref(&mlc)), None);
        let _ = fs::remove_dir_all(mlc.parent().unwrap());
    }

    #[test]
    fn knows_gated_titles() {
        assert_eq!(version_gate(0x0005_0000_101C_9300).unwrap().contents, b"1.5.0");
        assert_eq!(version_gate(0x0005_0000_1010_EC00), None);
    }
}
