use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub output_dir: PathBuf,
    pub hotkeys: HotkeyConfig,
    pub overlay: OverlayConfig,
    pub encode: EncodeConfig,
    /// "en" | "ja" — see `crate::i18n::Lang`. Unknown values fall back to
    /// English, same convention as the other mode fields in this file.
    pub language: String,
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
    /// "native" (window's own size, default) | "display" | "custom"
    pub resolution_mode: String,
    /// Only read when `resolution_mode == "custom"`.
    pub custom_width: u32,
    pub custom_height: u32,
    /// "auto" (resolution-aware formula, default) | "manual"
    pub bitrate_mode: String,
    /// Only read when `bitrate_mode == "manual"`.
    pub manual_bitrate_mbps: u32,
}

/// Resolved form of `EncodeConfig::resolution_mode` — see `EncodeConfig::resolution_mode()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionMode {
    /// The captured window's own native size (default — see the resolution-regression fix).
    Native,
    /// The display's native resolution — explicit opt-in only, not the default.
    Display,
    /// An explicit width/height, already clamped to even and within [2, 7680]x[2, 4320].
    Custom(u32, u32),
}

/// Resolved form of `EncodeConfig::bitrate_mode` — see `EncodeConfig::bitrate_mode()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitrateMode {
    /// Resolution-aware formula (see `encode::writer::video_bitrate_bps`).
    Auto,
    /// Explicit bits-per-second, already clamped from a 1-100 Mbps user input.
    Manual(u32),
}

impl EncodeConfig {
    pub fn resolution_mode(&self) -> ResolutionMode {
        match self.resolution_mode.as_str() {
            "display" => ResolutionMode::Display,
            "custom" => ResolutionMode::Custom(
                self.custom_width.clamp(2, 7680) & !1,
                self.custom_height.clamp(2, 4320) & !1,
            ),
            _ => ResolutionMode::Native,
        }
    }

    pub fn bitrate_mode(&self) -> BitrateMode {
        match self.bitrate_mode.as_str() {
            "manual" => BitrateMode::Manual(self.manual_bitrate_mbps.clamp(1, 100) * 1_000_000),
            _ => BitrateMode::Auto,
        }
    }
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
                resolution_mode: "native".into(),
                custom_width: 1920,
                custom_height: 1080,
                bitrate_mode: "auto".into(),
                manual_bitrate_mbps: 12,
            },
            language: "en".into(),
        }
    }
}

impl Config {
    pub fn lang(&self) -> crate::i18n::Lang {
        match self.language.as_str() {
            "ja" => crate::i18n::Lang::Ja,
            _ => crate::i18n::Lang::En,
        }
    }

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
    fn output_dir_survives_round_trip() {
        let dir = tempdir().unwrap();
        let expected = dir.path().join("recordings");
        let cfg = Config {
            output_dir: expected.clone(),
            ..Config::default()
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        let loaded: Config = toml::from_str(&text).unwrap();
        assert_eq!(loaded.output_dir, expected);
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

    #[test]
    fn resolution_mode_parses_known_strings() {
        let mut c = Config::default();
        c.encode.resolution_mode = "native".into();
        assert!(matches!(c.encode.resolution_mode(), ResolutionMode::Native));
        c.encode.resolution_mode = "display".into();
        assert!(matches!(c.encode.resolution_mode(), ResolutionMode::Display));
        c.encode.resolution_mode = "custom".into();
        c.encode.custom_width = 2560;
        c.encode.custom_height = 1440;
        assert!(matches!(c.encode.resolution_mode(), ResolutionMode::Custom(2560, 1440)));
    }

    #[test]
    fn resolution_mode_unknown_string_falls_back_to_native() {
        let mut c = Config::default();
        c.encode.resolution_mode = "not-a-real-mode".into();
        assert!(matches!(c.encode.resolution_mode(), ResolutionMode::Native));
    }

    #[test]
    fn resolution_mode_custom_clamps_to_even_and_bounds() {
        let mut c = Config::default();
        c.encode.resolution_mode = "custom".into();
        c.encode.custom_width = 1;
        c.encode.custom_height = 100_000;
        match c.encode.resolution_mode() {
            ResolutionMode::Custom(w, h) => {
                assert_eq!(w, 2, "width below minimum should clamp to 2");
                assert_eq!(h, 4320, "height above maximum should clamp to 4320");
                assert_eq!(w % 2, 0);
                assert_eq!(h % 2, 0);
            }
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn bitrate_mode_parses_known_strings() {
        let mut c = Config::default();
        c.encode.bitrate_mode = "auto".into();
        assert!(matches!(c.encode.bitrate_mode(), BitrateMode::Auto));
        c.encode.bitrate_mode = "manual".into();
        c.encode.manual_bitrate_mbps = 20;
        assert!(matches!(c.encode.bitrate_mode(), BitrateMode::Manual(20_000_000)));
    }

    #[test]
    fn bitrate_mode_unknown_string_falls_back_to_auto() {
        let mut c = Config::default();
        c.encode.bitrate_mode = "not-a-real-mode".into();
        assert!(matches!(c.encode.bitrate_mode(), BitrateMode::Auto));
    }

    #[test]
    fn bitrate_mode_manual_clamps_to_1_100_mbps() {
        let mut c = Config::default();
        c.encode.bitrate_mode = "manual".into();
        c.encode.manual_bitrate_mbps = 0;
        assert!(matches!(c.encode.bitrate_mode(), BitrateMode::Manual(1_000_000)));
        c.encode.manual_bitrate_mbps = 500;
        assert!(matches!(c.encode.bitrate_mode(), BitrateMode::Manual(100_000_000)));
    }

    #[test]
    fn default_language_is_english() {
        assert_eq!(Config::default().language, "en");
        assert!(matches!(Config::default().lang(), crate::i18n::Lang::En));
    }

    #[test]
    fn lang_recognizes_japanese() {
        let c = Config {
            language: "ja".into(),
            ..Config::default()
        };
        assert!(matches!(c.lang(), crate::i18n::Lang::Ja));
    }

    #[test]
    fn lang_unknown_string_falls_back_to_english() {
        let c = Config {
            language: "fr".into(),
            ..Config::default()
        };
        assert!(matches!(c.lang(), crate::i18n::Lang::En));
    }

    #[test]
    fn encode_config_new_fields_round_trip_toml() {
        let mut original = Config::default();
        original.encode.resolution_mode = "custom".into();
        original.encode.custom_width = 1280;
        original.encode.custom_height = 720;
        original.encode.bitrate_mode = "manual".into();
        original.encode.manual_bitrate_mbps = 15;
        let text = toml::to_string_pretty(&original).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.encode, original.encode);
    }
}
