//! Front-end settings, stored as TOML.
use std::path::{Path, PathBuf};

use shocora_render::{GraphicsApi, UpscaleConfig};

use crate::input::{KeyBindings, PadBindings};

/// Persisted front-end settings.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Graphics API used at the next start.
    pub graphics_api: GraphicsApi,
    /// Last booted title or RPX.
    pub last_title: Option<PathBuf>,
    /// Root of per-title save data.
    pub save_dir: PathBuf,
    /// Folder holding the system fonts (CafeStd.ttf, ...), when not found automatically.
    pub fonts_dir: Option<PathBuf>,
    pub key_bindings: KeyBindings,
    pub pad_bindings: PadBindings,
    /// Screen post-processing.
    pub upscale: UpscaleConfig,
    /// Vertical blanks per flip, whatever the title asks for. `1` is 60 fps, and
    /// double speed in a title made for 30 whose logic counts frames.
    pub swap_interval: Option<u32>,
    /// Backdrop of the start screen, as RGBA: alpha lets the picture behind it show.
    pub launcher_background: [u8; 4],
    /// Picture behind the start screen's color.
    pub launcher_image: Option<PathBuf>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            graphics_api: GraphicsApi::default(),
            last_title: None,
            save_dir: PathBuf::from("save"),
            fonts_dir: None,
            key_bindings: KeyBindings::default(),
            pad_bindings: PadBindings::default(),
            upscale: UpscaleConfig::default(),
            swap_interval: None,
            launcher_background: [231, 234, 237, 255],
            launcher_image: None,
        }
    }
}

impl AppConfig {
    /// Default location: `shocora.toml` in the working directory.
    pub fn default_path() -> PathBuf {
        PathBuf::from("shocora.toml")
    }

    /// Loads `path`, falling back to defaults when missing or invalid.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                log::warn!("{}: {e}; using defaults", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// # Errors
    /// Fails if the file cannot be written.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    pub fn emulator_config(&self) -> wiiu_emu::Config {
        wiiu_emu::Config {
            save_dir: self.save_dir.clone(),
            fonts_dir: self.fonts_dir.clone(),
            swap_interval: self.swap_interval,
            ..wiiu_emu::Config::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let config = AppConfig { graphics_api: GraphicsApi::OpenGl, ..AppConfig::default() };
        let text = toml::to_string_pretty(&config).unwrap();
        assert!(text.contains("graphics_api = \"opengl\""));
        assert_eq!(toml::from_str::<AppConfig>(&text).unwrap(), config);
        // Missing fields fall back to defaults.
        assert_eq!(
            toml::from_str::<AppConfig>("graphics_api = \"vulkan\"").unwrap().save_dir,
            config.save_dir
        );
    }
}
