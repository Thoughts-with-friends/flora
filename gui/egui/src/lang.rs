//! The window's texts in the language chosen.
//!
//! `lang/translation.<code>.json` holds them as a flat `key: text` table. They are built into
//! the program; the same file in a `lang` folder in the working directory takes precedence, so
//! texts can be changed without building. A text missing from a language shows in English.
use std::{
    collections::HashMap,
    fmt::Display,
    path::Path,
    sync::{LazyLock, PoisonError, RwLock},
};

/// `(code, name)` of the languages on offer, each in its own language.
pub const LANGUAGES: [(&str, &str); 2] = [("jpn", "日本語"), ("eng", "English")];

const BUILT_IN_ENGLISH: &str = include_str!("../../../lang/translation.eng.json");
const BUILT_IN_JAPANESE: &str = include_str!("../../../lang/translation.jpn.json");
/// Where translations found next to the program are.
const FOLDER: &str = "lang";

type Table = HashMap<String, &'static str>;

/// Parses a translation. The texts live for as long as the program does: `tr` hands them out
/// without copying, once per widget and frame, and languages change too rarely for it to count.
fn parse(json: &str) -> Result<Table, serde_json::Error> {
    let table: HashMap<String, String> = serde_json::from_str(json)?;
    Ok(table.into_iter().map(|(key, text)| (key, &*text.leak())).collect())
}

fn built_in(code: &str) -> Table {
    let json = if code == "eng" { BUILT_IN_ENGLISH } else { BUILT_IN_JAPANESE };
    parse(json).unwrap_or_else(|error| {
        log::error!("built-in translation {code}: {error}");
        Table::new()
    })
}

/// The language `code`, from `folder` if it is there and makes sense.
fn load(code: &str, folder: &Path) -> Table {
    let path = folder.join(format!("translation.{code}.json"));
    let Ok(json) = std::fs::read_to_string(&path) else { return built_in(code) };
    parse(&json).unwrap_or_else(|error| {
        log::warn!("{}: {error}", path.display());
        built_in(code)
    })
}

static ENGLISH: LazyLock<Table> = LazyLock::new(|| built_in("eng"));
static CURRENT: LazyLock<RwLock<Table>> = LazyLock::new(|| RwLock::new(built_in("jpn")));

/// Shows the window in the language `code` from now on.
pub fn set_language(code: &str) {
    let table = load(code, Path::new(FOLDER));
    *CURRENT.write().unwrap_or_else(PoisonError::into_inner) = table;
}

/// The text `key` stands for.
pub fn tr(key: &'static str) -> &'static str {
    let current = CURRENT.read().unwrap_or_else(PoisonError::into_inner);
    current.get(key).or_else(|| ENGLISH.get(key)).copied().unwrap_or(key)
}

/// The text `key` stands for, with its `{name}` places filled in.
pub fn trf(key: &'static str, values: &[(&str, &dyn Display)]) -> String {
    values.iter().fold(tr(key).to_owned(), |text, (name, value)| {
        text.replace(&format!("{{{name}}}"), &value.to_string())
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// The `{name}` places of a text.
    fn places(text: &str) -> BTreeSet<&str> {
        text.split('{').skip(1).filter_map(|rest| rest.split_once('}')).map(|(n, _)| n).collect()
    }

    #[test]
    fn both_languages_have_the_same_texts_and_places() {
        let (english, japanese) = (built_in("eng"), built_in("jpn"));
        assert!(!english.is_empty());
        let keys = |table: &Table| table.keys().cloned().collect::<BTreeSet<_>>();
        assert_eq!(keys(&english), keys(&japanese));
        for (key, text) in &english {
            assert_eq!(places(text), places(japanese[key]), "{key}");
        }
    }

    /// Every `tr("key")` and `trf("key"` in the sources, and whether the files agree.
    #[test]
    fn the_sources_and_the_files_use_the_same_keys() {
        let mut used = BTreeSet::new();
        for source in sources(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")) {
            for call in ["tr(", "trf("] {
                for piece in source.split(call).skip(1) {
                    // A long call has its key on the next line.
                    let Some(rest) = piece.trim_start().strip_prefix('"') else { continue };
                    let key = rest.split('"').next().unwrap_or_default();
                    used.insert(key.to_owned());
                }
            }
        }
        let english = built_in("eng");
        let known = english.keys().cloned().collect::<BTreeSet<_>>();
        assert!(used.len() > 50, "only {} keys found", used.len());
        assert_eq!(used.difference(&known).collect::<Vec<_>>(), Vec::<&String>::new(), "no text");
        assert_eq!(known.difference(&used).collect::<Vec<_>>(), Vec::<&String>::new(), "unused");
    }

    /// Every source file under `dir` but this one, whose tests ask for keys that do not exist.
    fn sources(dir: &std::path::Path) -> Vec<String> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap().map(Result::unwrap) {
            let path = entry.path();
            if path.is_dir() {
                found.extend(sources(&path));
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.file_name().is_some_and(|n| n != "lang.rs")
            {
                found.push(std::fs::read_to_string(&path).unwrap());
            }
        }
        found
    }

    #[test]
    fn places_are_filled_in() {
        set_language("eng");
        let text = trf("graphics.memory_ran_out", &[("factor", &8), ("lower", &6)]);
        assert!(text.contains("8x") && text.contains("6x"), "{text}");
        assert_eq!(tr("no.such.key"), "no.such.key");
    }

    #[test]
    fn a_folder_takes_precedence_and_nonsense_does_not() {
        let folder = std::env::temp_dir().join(format!("shocora-lang-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("translation.eng.json");
        std::fs::write(&path, r#"{"menu.file": "Fichier"}"#).unwrap();
        assert_eq!(load("eng", &folder)["menu.file"], "Fichier");
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(load("eng", &folder)["menu.file"], "File");
        std::fs::remove_dir_all(&folder).unwrap();
        assert_eq!(load("jpn", &folder)["menu.file"], "ファイル");
    }
}
