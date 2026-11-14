use crate::error::AppError;
use crate::i18n::Lang;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The smallest the dashboard can be made (logical points) -- wider still
/// when what it shows needs it (see `ui::dashboard::min_width`).
pub const MIN_WINDOW_SIZE: (f32, f32) = (620.0, 450.0);

/// Persisted settings (`config.toml`).
///
/// Every struct here is `#[serde(default)]` at the container level: any
/// field missing from the file -- a `config.toml` saved before that setting
/// existed, or a hand-edited one -- falls back to that field's own default
/// instead of failing the whole load, so adding a setting never needs a
/// per-field `#[serde(default = ...)]` again.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// config.toml couldn't be read or moved aside
    /// (`ConfigTrouble::LeftAlone`), so `save` leaves it alone this time
    /// rather than replace it with these defaults. Not saved itself.
    #[serde(skip)]
    pub saves_blocked: bool,
    pub output_dir: PathBuf,
    pub hotkeys: HotkeyConfig,
    pub overlay: OverlayConfig,
    pub encode: EncodeConfig,
    /// Stored as "en" | "ja"; an unknown value loads as English.
    pub language: Lang,
    /// The rolling "Highlight" background buffer — see `crate::highlight`.
    /// Disabled by default.
    pub highlight: HighlightConfig,
    /// Default state of the "App audio only" checkbox on launch (and thus
    /// what a hotkey-started recording uses, since the hotkey starts a
    /// recording with whatever the checkbox is currently set to). Defaults
    /// to true.
    pub default_app_audio_only: bool,
    /// Per-device recording volume, keyed by the device's stable WASAPI
    /// endpoint id (`AudioDevice::id`) -- a linear multiplier applied to that
    /// device's captured samples before encoding (see
    /// `capture::audio::apply_gain`), independent of the device's actual
    /// system volume. A device with no entry here records at 1.0 (100%,
    /// unboosted) -- see `Config::audio_gain`.
    pub audio_device_gain: std::collections::HashMap<String, f32>,
    /// Top-left corner of the window, in screen coordinates, at the last
    /// clean exit (including the exit triggered by a self-update) -- restored
    /// on next launch via `ViewportBuilder::with_position` so the window
    /// reopens where it was left instead of the OS's default placement.
    /// `None` (the default) leaves placement to the OS -- covers both an old
    /// `config.toml` and the first-ever launch. See
    /// `Config::sane_window_position` for why this isn't restored
    /// unconditionally.
    pub window_position: Option<(f32, f32)>,
    /// The dashboard's size (logical points, not maximized) and whether it
    /// was maximized, when PolyRec last closed -- restored on next launch,
    /// like `window_position`. See `Config::sane_window_size`.
    pub window_size: Option<(f32, f32)>,
    pub window_maximized: bool,
    /// Apps pinned to always appear in the Applications audio list, even
    /// while not currently producing sound (or not running at all) --
    /// see `AppAudioSource::process_ids`'s doc comment for how these merge
    /// with live WASAPI sessions.
    pub registered_app_audio: Vec<RegisteredApp>,
    /// Whether the SYSTEM audio list's two checkboxes are ticked: Windows'
    /// default output (its sound, through loopback) and default microphone.
    /// The list only ever shows those two, as whatever devices they are, so
    /// a tick belongs to the role, not the device: switching Windows'
    /// default to another device -- a headset, for a while -- keeps it
    /// ticked. `None` until the user changes that checkbox; see
    /// `records_default_output`/`records_default_input` for what it means
    /// until then.
    pub record_default_output: Option<bool>,
    pub record_default_input: Option<bool>,
    /// What PolyRec 0.5.144 and before saved instead, and still all they
    /// read: the WASAPI endpoint ids of the devices ticked -- so there a
    /// tick didn't follow a changed default. Still written, with the device
    /// each ticked default is now (`record_default`), for an older copy
    /// sharing this config.toml (an installed one beside one built from
    /// source): without it, that copy found no ticks, fell back to speakers
    /// on and microphone off, and saved over the settings above, which it
    /// doesn't know. Read when those are missing -- a config.toml an older
    /// copy wrote last -- to carry its ticks over.
    #[serde(default)]
    pub selected_audio_device_ids: Option<Vec<String>>,
    /// Whether Windows' own colored capture-border indicator shows around the
    /// recorded window/monitor during a manual recording (see
    /// `capture::video::run_video_capture`'s `show_border` param and
    /// `SetIsBorderRequired`) -- doesn't affect Highlight background
    /// buffering, which always suppresses it regardless of this setting so
    /// buffering isn't mistaken for an active recording. Defaults to `true`.
    pub show_recording_border: bool,
    /// Whether the mouse pointer is in the picture, of a recording and of a
    /// Highlight alike. Defaults to `true`, as Windows captures it.
    pub record_cursor: bool,
    /// Whether the window's close button hides PolyRec in the notification
    /// area (tray) instead of quitting it, so the hotkeys keep working --
    /// "Quit PolyRec" in the tray menu quits either way. Defaults to `false`:
    /// the close button quits, as it always has.
    pub close_to_tray: bool,
    /// Whether a finished manual recording or Highlight save is named after
    /// the recorded process (the historical default, `true`) or
    /// `custom_recording_prefix` (see `crate::recording_naming::RecordingNaming`).
    pub use_process_name_for_recording: bool,
    /// User-typed prefix used when `use_process_name_for_recording` is
    /// `false` -- sanitized and given an auto-incrementing `_NNN` sequence
    /// number at finish time (see `recording_naming::resolve_finished_recording_stem`).
    /// Defaults to `""`, which `sanitize_filename_component` turns into
    /// `"recording"`.
    pub custom_recording_prefix: String,
}

/// A user-pinned app for `Config::registered_app_audio` -- picked once via
/// the Audio popup's "+ Add app" file browser.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegisteredApp {
    /// Identity/gain key -- same convention and same field name as
    /// `AppAudioSource::exe_name`, stable across the app's own restarts
    /// (unlike a process id).
    pub exe_name: String,
    /// Full path at the time it was registered -- used only as a
    /// best-effort source for re-extracting the app's icon while it isn't
    /// running (see `sources::extract_exe_icon_rgba`), never as identity.
    /// If the exe has since moved or been uninstalled, icon extraction just
    /// fails gracefully (`None`), same as any other icon lookup here.
    pub exe_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HotkeyConfig {
    pub start_stop: String,
    pub pause: String,
    pub toggle_overlay: String,
    /// Saves the last `highlight.buffer_seconds` of the Highlight buffer to a
    /// file — only takes effect while Highlight buffering is active.
    pub save_highlight: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            start_stop: "F9".into(),
            pause: "F8".into(),
            toggle_overlay: "F7".into(),
            save_highlight: "F10".into(),
        }
    }
}

/// Minimum/maximum for `HighlightConfig::buffer_seconds`, enforced by the
/// settings UI (not by `Config` itself, same convention as `EncodeConfig`'s
/// `manual_bitrate_mbps` clamp living in `bitrate_mode()` rather than here).
pub const HIGHLIGHT_BUFFER_SECONDS_MIN: u32 = 30;
pub const HIGHLIGHT_BUFFER_SECONDS_MAX: u32 = 300;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HighlightConfig {
    pub enabled: bool,
    /// How much of the buffer to save on the Highlight hotkey — clamped to
    /// `[HIGHLIGHT_BUFFER_SECONDS_MIN, HIGHLIGHT_BUFFER_SECONDS_MAX]` by the
    /// settings UI, not enforced here (same convention as the rest of this
    /// file's numeric settings).
    pub buffer_seconds: u32,
}

impl Default for HighlightConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            buffer_seconds: 120,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OverlayConfig {
    pub enabled: bool,
    pub opacity: f32,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            opacity: 0.85,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EncodeConfig {
    pub codec: Codec,
    pub fps: u32,
    pub resolution_mode: ResolutionKind,
    /// Only read when `resolution_mode` is `Custom`.
    pub custom_width: u32,
    pub custom_height: u32,
    pub bitrate_mode: BitrateKind,
    /// Only read when `bitrate_mode` is `Manual`.
    pub manual_bitrate_mbps: u32,
    pub encoder_mode: EncoderMode,
    /// Experimental: keep captured frames on the GPU all the way into the
    /// hardware encoder instead of copying each one through system memory
    /// (see `encode::gpu`). Only with `EncoderMode::Hardware`; a recording
    /// falls back to the usual path if it doesn't work on this machine.
    pub gpu_frames: bool,
}

impl Default for EncodeConfig {
    fn default() -> Self {
        Self {
            codec: Codec::default(),
            fps: 60,
            resolution_mode: ResolutionKind::default(),
            custom_width: 1920,
            custom_height: 1080,
            bitrate_mode: BitrateKind::default(),
            manual_bitrate_mbps: 12,
            encoder_mode: EncoderMode::default(),
            gpu_frames: false,
        }
    }
}

/// Video codec, stored as "h265" | "h264". H265 (the default) falls back to
/// H264 at encode time when no HEVC encoder is available (see
/// `RecordingWriter::new`); an unknown stored value loads as H264.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    #[default]
    H265,
    #[serde(other)]
    H264,
}

/// Stored form of the resolution setting, "native" | "display" | "custom" --
/// see `EncodeConfig::resolution_mode()` for the resolved `ResolutionMode`.
/// An unknown stored value loads as `Native`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResolutionKind {
    /// The display's native resolution — explicit opt-in only, not the default.
    Display,
    /// `custom_width` x `custom_height`.
    Custom,
    /// The captured window's own native size (default — see the resolution-regression fix).
    /// Last because serde's `other` fallback variant has to be.
    #[default]
    #[serde(other)]
    Native,
}

/// Stored form of the bitrate setting, "auto" | "manual" -- see
/// `EncodeConfig::bitrate_mode()` for the resolved `BitrateMode`. An unknown
/// stored value loads as `Auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BitrateKind {
    /// `manual_bitrate_mbps`.
    Manual,
    /// Resolution-aware formula (see `encode::writer::video_bitrate_bps`).
    /// Last because serde's `other` fallback variant has to be.
    #[default]
    #[serde(other)]
    Auto,
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

/// Stored as "hardware" | "software"; an unknown stored value loads as
/// `Hardware` (the default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EncoderMode {
    /// Force Media Foundation's built-in software encoder, bypassing any
    /// hardware transform even if one's available -- frees up the GPU at the
    /// cost of higher CPU use, useful while a demanding game/app is running.
    Software,
    /// Prefer a GPU hardware encoder (NVENC/QSV/AMF) when one's available --
    /// Media Foundation falls back to software transparently if not.
    /// Last because serde's `other` fallback variant has to be.
    #[default]
    #[serde(other)]
    Hardware,
}

impl EncodeConfig {
    pub fn resolution_mode(&self) -> ResolutionMode {
        match self.resolution_mode {
            ResolutionKind::Native => ResolutionMode::Native,
            ResolutionKind::Display => ResolutionMode::Display,
            ResolutionKind::Custom => ResolutionMode::Custom(
                self.custom_width.clamp(2, 7680) & !1,
                self.custom_height.clamp(2, 4320) & !1,
            ),
        }
    }

    pub fn bitrate_mode(&self) -> BitrateMode {
        match self.bitrate_mode {
            BitrateKind::Auto => BitrateMode::Auto,
            BitrateKind::Manual => {
                BitrateMode::Manual(self.manual_bitrate_mbps.clamp(1, 100) * 1_000_000)
            }
        }
    }

    /// Clamped to `[1, 240]` -- the UI only writes one of
    /// `capture::frame_rate::FPS_PRESETS` (30 up to 240), but unlike
    /// `resolution_mode()`/`bitrate_mode()`'s inputs, raw `fps` was never
    /// guarded, so a hand-edited `config.toml` with `fps = 0` divided by
    /// zero on the very first frame of every recording (see
    /// `encode::writer::RecordingWriter::write_video`'s duration_hns
    /// calculation). 240, the highest preset, also keeps a wildly
    /// out-of-range value from producing a degenerate per-frame duration.
    pub fn fps(&self) -> u32 {
        self.fps.clamp(1, 240)
    }
}

/// UI-enforced range for `Config::audio_device_gain` values -- 0% (muted) to
/// 100% (unboosted). This is a recorder, not a mixing console: no gain
/// boost above the source's own level, only attenuation.
pub const AUDIO_GAIN_MIN_PERCENT: i32 = 0;
pub const AUDIO_GAIN_MAX_PERCENT: i32 = 100;

/// How a WASAPI endpoint id starts for an output device (`eRender`) and a
/// microphone (`eCapture`): `{0.0.0.00000000}.{…}` and `{0.0.1.00000000}.{…}`.
const OUTPUT_ENDPOINT: &str = "{0.0.0.";
const INPUT_ENDPOINT: &str = "{0.0.1.";

impl Default for Config {
    fn default() -> Self {
        Self {
            saves_blocked: false,
            output_dir: dirs::video_dir().unwrap_or_else(|| PathBuf::from(".")),
            hotkeys: HotkeyConfig::default(),
            overlay: OverlayConfig::default(),
            encode: EncodeConfig::default(),
            language: Lang::default(),
            highlight: HighlightConfig::default(),
            default_app_audio_only: true,
            audio_device_gain: std::collections::HashMap::new(),
            window_position: None,
            window_size: None,
            window_maximized: false,
            registered_app_audio: Vec::new(),
            record_default_output: None,
            record_default_input: None,
            selected_audio_device_ids: None,
            show_recording_border: true,
            record_cursor: true,
            close_to_tray: false,
            use_process_name_for_recording: true,
            custom_recording_prefix: String::new(),
        }
    }
}

impl Config {
    pub fn lang(&self) -> Lang {
        self.language
    }

    /// Whether recordings take Windows' default output's sound: as the user
    /// last ticked it, else as PolyRec 0.5.144 and before had it (any output
    /// ticked), else on -- what anyone recording a game wants.
    pub fn records_default_output(&self) -> bool {
        self.record_default_output
            .unwrap_or_else(|| self.ticked_before(OUTPUT_ENDPOINT).unwrap_or(true))
    }

    /// Whether recordings take Windows' default microphone: as the user last
    /// ticked it, else as PolyRec 0.5.144 and before had it (any microphone
    /// ticked), else off -- a voice in the recording is something to choose.
    pub fn records_default_input(&self) -> bool {
        self.record_default_input
            .unwrap_or_else(|| self.ticked_before(INPUT_ENDPOINT).unwrap_or(false))
    }

    /// Whether a device whose endpoint id starts with `kind` was ticked in
    /// the list PolyRec 0.5.144 and before saved, if it saved one.
    fn ticked_before(&self, kind: &str) -> Option<bool> {
        self.selected_audio_device_ids
            .as_ref()
            .map(|ids| ids.iter().any(|id| id.starts_with(kind)))
    }

    /// Ticks (`on`) or unticks recording Windows' default output (`output`)
    /// or default microphone, which is the device `device_id` now -- in the
    /// device list too, as the device that is that default now, for older
    /// copies of PolyRec (see `selected_audio_device_ids`).
    pub fn record_default(&mut self, output: bool, device_id: &str, on: bool) {
        let kind = if output {
            self.record_default_output = Some(on);
            OUTPUT_ENDPOINT
        } else {
            self.record_default_input = Some(on);
            INPUT_ENDPOINT
        };
        let ids = self.selected_audio_device_ids.get_or_insert_with(Vec::new);
        ids.retain(|id| !id.starts_with(kind));
        if on {
            ids.push(device_id.to_owned());
        }
    }

    /// Recording volume for `device_id` as a linear multiplier -- 1.0 (100%)
    /// if the device has no entry in `audio_device_gain` yet, which is every
    /// device the first time it's seen (and any `config.toml` saved before
    /// this setting existed).
    pub fn audio_gain(&self, device_id: &str) -> f32 {
        self.audio_device_gain
            .get(device_id)
            .copied()
            .unwrap_or(1.0)
    }

    /// Recording volume for a per-app audio source (`AppAudioSource::exe_name`),
    /// same semantics as `audio_gain` -- 1.0 (100%) if not yet set. Shares
    /// `audio_device_gain`'s map rather than a separate field, distinguished
    /// by an `"app:"` key prefix so an app's exe name can never collide with
    /// a WASAPI endpoint id (which `audio_gain` keys on directly).
    pub fn app_audio_gain(&self, exe_name: &str) -> f32 {
        self.audio_gain(&Self::app_audio_gain_key(exe_name))
    }

    /// The `audio_device_gain` key `app_audio_gain` reads from and the
    /// per-app volume slider UI writes to -- kept as one function so the
    /// `"app:"` prefix convention lives in exactly one place.
    pub fn app_audio_gain_key(exe_name: &str) -> String {
        format!("app:{exe_name}")
    }

    /// Adds `exe_name` to the Applications audio list -- the only way an
    /// app ends up there at all, via "+ Add app"'s in-app search picker
    /// (either a running window or an installed-but-not-running app found
    /// via Start Menu shortcut) or its native-file-browser fallback. The
    /// Applications list itself only ever contains registered apps (see
    /// `actions::build_app_audio_sources`'s doc comment) -- there's no
    /// separate ephemeral "checked but not remembered" state, checking one
    /// of these rows and registering it are the same thing. No-op (not an
    /// error) if it's already registered.
    pub fn register_app_audio(&mut self, exe_name: String, exe_path: String) {
        if self
            .registered_app_audio
            .iter()
            .any(|a| a.exe_name == exe_name)
        {
            return;
        }
        self.registered_app_audio
            .push(RegisteredApp { exe_name, exe_path });
    }

    /// Removes `exe_name` -- called when its Applications checkbox is
    /// unchecked (the checkbox is the only pin/unpin control, no separate
    /// remove button). If it's still actively producing audio when this is
    /// called, that doesn't stop an already-running capture -- this only
    /// affects future refreshes/sessions, and the row itself disappears
    /// from the Applications list immediately either way.
    pub fn unregister_app_audio(&mut self, exe_name: &str) {
        self.registered_app_audio.retain(|a| a.exe_name != exe_name);
    }

    /// `window_size` if it's still worth restoring: no smaller than the
    /// window may be made (`MIN_WINDOW_SIZE`), and not absurdly large.
    pub fn sane_window_size(&self) -> Option<(f32, f32)> {
        self.window_size.filter(|&(w, h)| {
            (MIN_WINDOW_SIZE.0..20_000.0).contains(&w) && (MIN_WINDOW_SIZE.1..20_000.0).contains(&h)
        })
    }

    /// `window_position` if it's still worth restoring, `None` otherwise.
    ///
    /// This is a loose sanity floor, not real multi-monitor validation --
    /// eframe/winit don't expose the connected-monitor list before a window
    /// exists to create one, so there's no reliable way to check "is this
    /// position still on a screen we actually have" ahead of
    /// `ViewportBuilder::with_position`. This only catches obviously-invalid
    /// values (corrupted config.toml, or a coordinate system change) --
    /// restoring a position from a monitor that's since been unplugged is an
    /// accepted, undetected edge case; Windows' own window-placement
    /// recovery (right-click taskbar icon -> Move, or Win+Shift+Arrow) is the
    /// fallback for it, same as for any other app.
    pub fn sane_window_position(&self) -> Option<(f32, f32)> {
        // Symmetric and generous in both directions -- a monitor placed to
        // the left of (or above) the primary is common and puts the window
        // at a meaningfully negative x (or y), not just slightly negative
        // (e.g. a 4K monitor to the left alone accounts for -3840).
        self.window_position.filter(|&(x, y)| {
            (-20_000.0..20_000.0).contains(&x) && (-20_000.0..20_000.0).contains(&y)
        })
    }

    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("PolyRec")
            .join("config.toml")
    }

    /// The settings in config.toml, or the defaults if there's none yet. One
    /// PolyRec can't understand -- a hand edit gone wrong, or one from a newer
    /// version -- is moved aside (`set_aside_path`) and the defaults used
    /// instead: left where it was, the defaults would overwrite it at the next
    /// save. One that can't be read at all, or moved aside, is left as it is,
    /// and not saved over this time (`saves_blocked`). Also says which, if
    /// either.
    pub fn load_or_set_aside() -> (Self, Option<ConfigTrouble>) {
        Self::load_or_set_aside_at(&Self::config_path())
    }

    fn load_or_set_aside_at(path: &Path) -> (Self, Option<ConfigTrouble>) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            // Not a bad file, just one that couldn't be read right now --
            // held open by a backup or sync tool, say. It used to be moved
            // aside like a bad one.
            Err(e) => {
                tracing::warn!(
                    "couldn't read {} ({e}); started with the defaults and left it as it is",
                    path.display()
                );
                return Self::left_alone(path);
            }
        };
        let e = match toml::from_str(&text) {
            Ok(config) => return (config, None),
            Err(e) => e,
        };
        let aside = set_aside_path(path);
        match std::fs::rename(path, &aside) {
            Ok(()) => {
                tracing::warn!(
                    "couldn't read {} ({e}); moved it to {} and started with the defaults",
                    path.display(),
                    aside.display()
                );
                (Self::default(), Some(ConfigTrouble::SetAside(aside)))
            }
            Err(move_err) => {
                tracing::warn!(
                    "couldn't read {} ({e}) or move it aside ({move_err}); started with the defaults and left it as it is",
                    path.display()
                );
                Self::left_alone(path)
            }
        }
    }

    /// The language in config.toml, read without loading the rest -- or
    /// setting anything aside: for what's said before PolyRec is running
    /// (`single_instance::say_unreachable`).
    pub fn saved_language() -> Lang {
        Self::saved_language_at(&Self::config_path())
    }

    fn saved_language_at(path: &Path) -> Lang {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct LanguageOnly {
            language: Lang,
        }
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str::<LanguageOnly>(&text).ok())
            .unwrap_or_default()
            .language
    }

    /// The defaults, for a config.toml at `path` that's left as it is.
    fn left_alone(path: &Path) -> (Self, Option<ConfigTrouble>) {
        let config = Self {
            saves_blocked: true,
            ..Self::default()
        };
        (config, Some(ConfigTrouble::LeftAlone(path.to_path_buf())))
    }

    pub fn save(&self) -> Result<(), AppError> {
        self.save_to(&Self::config_path())
    }

    fn save_to(&self, path: &Path) -> Result<(), AppError> {
        if self.saves_blocked {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| AppError::Config(e.to_string()))?;
        // Write-to-temp-then-rename, the same technique the recording path
        // already uses for its own output file -- a crash or power loss
        // mid-write previously left a truncated/corrupt config.toml with no
        // recovery path (`load()` just bubbles the resulting `toml::from_str`
        // parse error up). The temp file lives in the same directory, so the
        // rename is a same-volume move: config.toml is always either the old
        // complete file or the new complete file, never a partial write.
        // The temp file is this save's own -- two copies of PolyRec sharing
        // these settings (an installed one and one built from source)
        // saving at once used to write the one config.toml.tmp together --
        // and is on disk before it replaces config.toml, or a power cut
        // could leave the rename done and the contents not.
        static SAVES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let tmp_path = path.with_file_name(format!(
            "config.toml.{}-{}.tmp",
            std::process::id(),
            SAVES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let saved = write_synced(&tmp_path, text.as_bytes())
            .and_then(|()| std::fs::rename(&tmp_path, path));
        if saved.is_err() {
            let _ = std::fs::remove_file(&tmp_path);
        }
        Ok(saved?)
    }
}

/// Writes `bytes` to a new file at `path`, and waits until they're on disk.
fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create_new(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Where a config.toml at `path` that couldn't be understood is moved:
/// config.toml.bad, or config.toml.bad.2, .3 and so on -- never over one set
/// aside before, which holds the settings as they were before the first.
fn set_aside_path(path: &Path) -> PathBuf {
    std::iter::once(path.with_file_name("config.toml.bad"))
        .chain((2u32..).map(|n| path.with_file_name(format!("config.toml.bad.{n}"))))
        .find(|candidate| !candidate.exists())
        .expect("a free name")
}

/// Why the settings PolyRec started with aren't the ones in config.toml.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigTrouble {
    /// It couldn't be understood, and was moved here.
    SetAside(PathBuf),
    /// It couldn't be read, or moved aside: it's left as it was (at this
    /// path), and nothing is saved over it this time
    /// (`Config::saves_blocked`).
    LeftAlone(PathBuf),
}

#[cfg(test)]
mod tests;
