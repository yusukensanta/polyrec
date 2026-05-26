use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub output_dir: PathBuf,
    pub hotkeys: HotkeyConfig,
    pub overlay: OverlayConfig,
    pub encode: EncodeConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HotkeyConfig {
    pub start_stop: String,
    pub pause: String,
    pub toggle_overlay: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OverlayConfig {
    pub enabled: bool,
    pub opacity: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EncodeConfig {
    /// "h265" or "h264"
    pub codec: String,
    pub fps: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            output_dir: dirs::video_dir().unwrap_or_else(|| PathBuf::from(".")),
            hotkeys: HotkeyConfig {
                start_stop: "F9".into(),
                pause: "F8".into(),
                toggle_overlay: "F7".into(),
            },
            overlay: OverlayConfig {
                enabled: false,
                opacity: 0.85,
            },
            encode: EncodeConfig {
                codec: "h265".into(),
                fps: 60,
            },
        }
    }
}

impl Config {
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("PolyRec")
            .join("config.toml")
    }

    pub fn load() -> Result<Self, AppError> {
        let path = Self::config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)?;
        toml::from_str(&text).map_err(|e| AppError::Config(e.to_string()))
    }

    pub fn save(&self) -> Result<(), AppError> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| AppError::Config(e.to_string()))?;
        std::fs::write(&path, text)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn default_config_has_sane_values() {
        let c = Config::default();
        assert_eq!(c.hotkeys.start_stop, "F9");
        assert_eq!(c.encode.codec, "h265");
        assert_eq!(c.encode.fps, 60);
        assert!(!c.overlay.enabled);
    }

    #[test]
    fn round_trip_toml() {
        let original = Config::default();
        let text = toml::to_string_pretty(&original).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(original.hotkeys.start_stop, parsed.hotkeys.start_stop);
        assert_eq!(original.encode.codec, parsed.encode.codec);
        assert_eq!(original.overlay.enabled, parsed.overlay.enabled);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = Config {
            output_dir: dir.path().to_path_buf(),
            ..Config::default()
        };
        let text = toml::to_string_pretty(&original).unwrap();
        std::fs::write(&path, text).unwrap();
        let loaded: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(original.encode.fps, loaded.encode.fps);
    }
}
