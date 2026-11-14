#[test]
fn the_window_comes_back_at_its_size_unless_that_size_is_unusable() {
    let with = |size| Config {
        window_size: Some(size),
        ..Config::default()
    };
    assert_eq!(Config::default().sane_window_size(), None);
    assert_eq!(
        with((900.0, 800.0)).sane_window_size(),
        Some((900.0, 800.0))
    );
    // Smaller than the window may be made, or absurdly large: ignored.
    assert_eq!(with((300.0, 800.0)).sane_window_size(), None);
    assert_eq!(with((900.0, 50_000.0)).sane_window_size(), None);
    // Saved and read back, maximized too; absent in older configs.
    let original = Config {
        window_size: Some((1000.0, 700.0)),
        window_maximized: true,
        ..Config::default()
    };
    let parsed: Config = toml::from_str(&toml::to_string_pretty(&original).unwrap()).unwrap();
    assert_eq!(parsed.window_size, Some((1000.0, 700.0)));
    assert!(parsed.window_maximized);
    let old: Config = toml::from_str("output_dir = \".\"").unwrap();
    assert_eq!((old.window_size, old.window_maximized), (None, false));
}
use super::*;
use tempfile::tempdir;

/// An `[encode]` section parsed on its own -- every field it omits falls
/// back to `EncodeConfig::default()`.
fn encode_toml(text: &str) -> EncodeConfig {
    toml::from_str(text).unwrap()
}

#[test]
fn default_config_has_sane_values() {
    let c = Config::default();
    assert_eq!(c.hotkeys.start_stop, "F9");
    assert_eq!(c.encode.codec, Codec::H265);
    assert_eq!(c.encode.fps, 60);
    assert!(!c.overlay.enabled);
    assert!(!c.highlight.enabled, "Highlight buffering should be opt-in");
    assert_eq!(c.highlight.buffer_seconds, 120);
    assert_eq!(c.hotkeys.save_highlight, "F10");
}

#[test]
fn highlight_settings_round_trip_toml_with_non_default_values() {
    let original = Config {
        highlight: HighlightConfig {
            enabled: true,
            buffer_seconds: 90,
        },
        ..Config::default()
    };
    let text = toml::to_string_pretty(&original).unwrap();
    let parsed: Config = toml::from_str(&text).unwrap();
    assert_eq!(parsed.highlight, original.highlight);
    assert!(parsed.highlight.enabled);
    assert_eq!(parsed.highlight.buffer_seconds, 90);
}

#[test]
fn highlight_and_save_highlight_hotkey_missing_from_toml_fall_back_to_defaults() {
    // Simulates a config.toml saved before Highlight buffering existed.
    let text = r#"
            output_dir = "."
            language = "en"
            [hotkeys]
            start_stop = "F9"
            pause = "F8"
            toggle_overlay = "F7"
            [overlay]
            enabled = false
            opacity = 0.85
            [encode]
            codec = "h265"
            fps = 60
            resolution_mode = "native"
            custom_width = 1920
            custom_height = 1080
            bitrate_mode = "auto"
            manual_bitrate_mbps = 12
        "#;
    let parsed: Config = toml::from_str(text).unwrap();
    assert!(!parsed.highlight.enabled);
    assert_eq!(parsed.highlight.buffer_seconds, 120);
    assert_eq!(parsed.hotkeys.save_highlight, "F10");
    assert_eq!(parsed.audio_gain("any-device-id"), 1.0);
}

#[test]
fn audio_gain_defaults_to_full_volume_for_a_device_with_no_entry() {
    let c = Config::default();
    assert_eq!(c.audio_gain("some-device-id"), 1.0);
}

#[test]
fn audio_device_gain_round_trips_toml_with_a_non_default_value() {
    let mut original = Config::default();
    original.audio_device_gain.insert("mic-123".into(), 1.5);
    let text = toml::to_string_pretty(&original).unwrap();
    let parsed: Config = toml::from_str(&text).unwrap();
    assert_eq!(parsed.audio_gain("mic-123"), 1.5);
    // A device that was never explicitly set still defaults to 1.0 after
    // the round trip, not 0.0 or missing-key panic.
    assert_eq!(parsed.audio_gain("speakers-456"), 1.0);
}

#[test]
fn app_audio_gain_defaults_to_full_volume_for_an_app_with_no_entry() {
    assert_eq!(Config::default().app_audio_gain("Discord.exe"), 1.0);
}

#[test]
fn app_audio_gain_reads_back_a_value_set_via_its_own_key_convention() {
    let mut c = Config::default();
    c.audio_device_gain
        .insert(Config::app_audio_gain_key("Discord.exe"), 1.25);
    assert_eq!(c.app_audio_gain("Discord.exe"), 1.25);
}

#[test]
fn app_audio_gain_key_cannot_collide_with_a_device_id_of_the_same_exe_name() {
    // A device id happening to equal a bare exe name (unlikely in
    // practice -- WASAPI endpoint ids look like
    // "{0.0.0.00000000}.{guid}" -- but not impossible) must not leak
    // into app_audio_gain's answer for that same string, and vice versa.
    let mut c = Config::default();
    c.audio_device_gain.insert("Discord.exe".into(), 2.0);
    assert_eq!(c.audio_gain("Discord.exe"), 2.0);
    assert_eq!(c.app_audio_gain("Discord.exe"), 1.0);
}

#[test]
fn window_position_defaults_to_none() {
    assert_eq!(Config::default().window_position, None);
}

#[test]
fn window_position_round_trips_toml() {
    let original = Config {
        window_position: Some((123.0, -45.0)),
        ..Config::default()
    };
    let text = toml::to_string_pretty(&original).unwrap();
    let parsed: Config = toml::from_str(&text).unwrap();
    assert_eq!(parsed.window_position, Some((123.0, -45.0)));
}

#[test]
fn window_position_missing_from_toml_falls_back_to_none() {
    // Simulates a config.toml saved before this setting existed.
    let text = r#"
            output_dir = "."
            language = "en"
            [hotkeys]
            start_stop = "F9"
            pause = "F8"
            toggle_overlay = "F7"
            [overlay]
            enabled = false
            opacity = 0.85
            [encode]
            codec = "h265"
            fps = 60
            resolution_mode = "native"
            custom_width = 1920
            custom_height = 1080
            bitrate_mode = "auto"
            manual_bitrate_mbps = 12
        "#;
    let parsed: Config = toml::from_str(text).unwrap();
    assert_eq!(parsed.window_position, None);
    assert_eq!(parsed.sane_window_position(), None);
}

#[test]
fn sane_window_position_accepts_typical_multi_monitor_coordinates() {
    // Negative x is normal for a monitor placed to the left of the
    // primary in Windows' virtual desktop coordinate space.
    let c = Config {
        window_position: Some((-1200.0, 300.0)),
        ..Config::default()
    };
    assert_eq!(c.sane_window_position(), Some((-1200.0, 300.0)));
}

#[test]
fn sane_window_position_rejects_wildly_out_of_range_values() {
    let c = Config {
        window_position: Some((-99999.0, 50.0)),
        ..Config::default()
    };
    assert_eq!(c.sane_window_position(), None);

    let c = Config {
        window_position: Some((50.0, 99999.0)),
        ..Config::default()
    };
    assert_eq!(c.sane_window_position(), None);
}

#[test]
fn sane_window_position_is_none_when_unset() {
    assert_eq!(Config::default().sane_window_position(), None);
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

/// One wrong value (or a file from a newer version) made PolyRec start
/// on the defaults and then overwrite the user's config.toml with them
/// at the next save -- every setting gone, with no word of why.
#[test]
fn a_config_that_cant_be_read_is_set_aside_not_overwritten() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let text = "output_dir = 'E:\\\\videos'\n[encode]\nfps = \"sixty\"\n";
    std::fs::write(&path, text).unwrap();

    let (config, aside) = Config::load_or_set_aside_at(&path);

    assert_eq!(config.encode.fps, Config::default().encode.fps);
    let Some(ConfigTrouble::SetAside(aside)) = aside else {
        panic!("not set aside: {aside:?}");
    };
    assert_eq!(aside, dir.path().join("config.toml.bad"));
    assert_eq!(std::fs::read_to_string(&aside).unwrap(), text);
    assert!(!path.exists());
}

/// Something else holding config.toml at startup (a backup or sync
/// tool, say) made reading it fail, and that was taken for a bad file:
/// a perfectly good config was moved aside and replaced with defaults.
#[test]
fn a_config_that_cant_be_read_at_all_is_left_where_it_is() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    // Reading a folder fails, as a file held open by another program does.
    std::fs::create_dir(&path).unwrap();

    let (config, trouble) = Config::load_or_set_aside_at(&path);

    assert_eq!(trouble, Some(ConfigTrouble::LeftAlone(path.clone())));
    assert!(config.saves_blocked);
    assert!(path.is_dir());
    assert!(!dir.path().join("config.toml.bad").exists());
}

/// The second time a config couldn't be understood, it was moved over
/// the first one set aside -- the one with the user's real settings.
#[test]
fn a_second_bad_config_keeps_the_first_one_set_aside() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "fps = [").unwrap();
    let (_, first) = Config::load_or_set_aside_at(&path);
    std::fs::write(&path, "language = {").unwrap();
    let (_, second) = Config::load_or_set_aside_at(&path);

    let first_path = dir.path().join("config.toml.bad");
    let second_path = dir.path().join("config.toml.bad.2");
    assert_eq!(first, Some(ConfigTrouble::SetAside(first_path.clone())));
    assert_eq!(second, Some(ConfigTrouble::SetAside(second_path.clone())));
    assert_eq!(std::fs::read_to_string(first_path).unwrap(), "fps = [");
    assert_eq!(
        std::fs::read_to_string(second_path).unwrap(),
        "language = {"
    );
}

/// A bad config that couldn't be moved aside (another program has it
/// open) was still saved over with the defaults at the first change.
#[test]
fn a_bad_config_that_cant_be_moved_aside_is_never_saved_over() {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "fps = [").unwrap();
    // Readable, but not movable while this is open.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&path)
        .unwrap();

    let (config, trouble) = Config::load_or_set_aside_at(&path);
    drop(held);

    assert_eq!(trouble, Some(ConfigTrouble::LeftAlone(path.clone())));
    assert!(config.saves_blocked);
    config.save_to(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "fps = [");
}

#[test]
fn the_saved_language_is_read_without_the_rest() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    assert_eq!(Config::saved_language_at(&path), Lang::En);
    // A wrong value elsewhere doesn't hide the language...
    std::fs::write(&path, "language = \"ja\"\n[encode]\nfps = \"sixty\"\n").unwrap();
    assert_eq!(Config::saved_language_at(&path), Lang::Ja);
    // ...but a file that isn't TOML at all reads as the default.
    std::fs::write(&path, "language = \"ja\"\nfps = [").unwrap();
    assert_eq!(Config::saved_language_at(&path), Lang::En);
    std::fs::write(&path, "output_dir = 'E:\\\\videos'\nlanguage = \"ja\"\n").unwrap();
    assert_eq!(Config::saved_language_at(&path), Lang::Ja);
    assert!(path.exists());
}

#[test]
fn a_save_leaves_only_config_toml_behind() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let mine = Config {
        output_dir: dir.path().join("videos"),
        ..Config::default()
    };
    mine.save_to(&path).unwrap();
    mine.save_to(&path).unwrap();

    let (loaded, trouble) = Config::load_or_set_aside_at(&path);
    assert_eq!(trouble, None);
    assert_eq!(loaded, mine);
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["config.toml"]);
}

#[test]
fn a_readable_config_is_loaded_where_it_is() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let mine = Config {
        output_dir: dir.path().to_path_buf(),
        ..Config::default()
    };
    std::fs::write(&path, toml::to_string_pretty(&mine).unwrap()).unwrap();

    let (config, aside) = Config::load_or_set_aside_at(&path);

    assert_eq!(config.output_dir, mine.output_dir);
    assert_eq!(aside, None);
    assert!(path.exists());
}

#[test]
fn no_config_yet_means_the_defaults_and_nothing_set_aside() {
    let dir = tempdir().unwrap();
    let (config, aside) = Config::load_or_set_aside_at(&dir.path().join("config.toml"));
    assert_eq!(config.encode.fps, Config::default().encode.fps);
    assert_eq!(aside, None);
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
    assert_eq!(
        encode_toml(r#"resolution_mode = "native""#).resolution_mode(),
        ResolutionMode::Native
    );
    assert_eq!(
        encode_toml(r#"resolution_mode = "display""#).resolution_mode(),
        ResolutionMode::Display
    );
    let custom = encode_toml(
        r#"
            resolution_mode = "custom"
            custom_width = 2560
            custom_height = 1440
            "#,
    );
    assert_eq!(custom.resolution_mode(), ResolutionMode::Custom(2560, 1440));
}

#[test]
fn resolution_mode_unknown_string_falls_back_to_native() {
    let c = encode_toml(r#"resolution_mode = "not-a-real-mode""#);
    assert_eq!(c.resolution_mode(), ResolutionMode::Native);
}

#[test]
fn resolution_mode_custom_clamps_to_even_and_bounds() {
    let mut c = Config::default();
    c.encode.resolution_mode = ResolutionKind::Custom;
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
    assert_eq!(
        encode_toml(r#"bitrate_mode = "auto""#).bitrate_mode(),
        BitrateMode::Auto
    );
    let manual = encode_toml(
        r#"
            bitrate_mode = "manual"
            manual_bitrate_mbps = 20
            "#,
    );
    assert_eq!(manual.bitrate_mode(), BitrateMode::Manual(20_000_000));
}

#[test]
fn bitrate_mode_unknown_string_falls_back_to_auto() {
    let c = encode_toml(r#"bitrate_mode = "not-a-real-mode""#);
    assert_eq!(c.bitrate_mode(), BitrateMode::Auto);
}

#[test]
fn bitrate_mode_manual_clamps_to_1_100_mbps() {
    let mut c = Config::default();
    c.encode.bitrate_mode = BitrateKind::Manual;
    c.encode.manual_bitrate_mbps = 0;
    assert!(matches!(
        c.encode.bitrate_mode(),
        BitrateMode::Manual(1_000_000)
    ));
    c.encode.manual_bitrate_mbps = 500;
    assert!(matches!(
        c.encode.bitrate_mode(),
        BitrateMode::Manual(100_000_000)
    ));
}

#[test]
fn encoder_mode_parses_known_strings() {
    assert_eq!(
        encode_toml(r#"encoder_mode = "hardware""#).encoder_mode,
        EncoderMode::Hardware
    );
    assert_eq!(
        encode_toml(r#"encoder_mode = "software""#).encoder_mode,
        EncoderMode::Software
    );
}

#[test]
fn encoder_mode_unknown_string_falls_back_to_hardware() {
    let c = encode_toml(r#"encoder_mode = "not-a-real-mode""#);
    assert_eq!(c.encoder_mode, EncoderMode::Hardware);
}

#[test]
fn codec_parses_known_strings_and_unknown_falls_back_to_h264() {
    assert_eq!(encode_toml(r#"codec = "h265""#).codec, Codec::H265);
    assert_eq!(encode_toml(r#"codec = "h264""#).codec, Codec::H264);
    assert_eq!(encode_toml(r#"codec = "vp9""#).codec, Codec::H264);
}

#[test]
fn mode_enums_serialize_to_the_same_strings_older_versions_wrote() {
    let mut c = Config::default();
    c.encode.resolution_mode = ResolutionKind::Display;
    c.encode.bitrate_mode = BitrateKind::Manual;
    c.encode.encoder_mode = EncoderMode::Software;
    c.language = Lang::Ja;
    let text = toml::to_string_pretty(&c).unwrap();
    for expected in [
        r#"codec = "h265""#,
        r#"resolution_mode = "display""#,
        r#"bitrate_mode = "manual""#,
        r#"encoder_mode = "software""#,
        r#"language = "ja""#,
    ] {
        assert!(text.contains(expected), "missing {expected} in:\n{text}");
    }
}

#[test]
fn a_partially_missing_section_keeps_its_other_fields_instead_of_failing_the_load() {
    // Before the container-level #[serde(default)], a section missing any
    // non-defaulted field (here: hotkeys.pause/toggle_overlay, and every
    // encode field but fps) failed the whole parse, resetting every
    // setting back to its default.
    let text = r#"
            [hotkeys]
            start_stop = "F2"
            [encode]
            fps = 30
        "#;
    let parsed: Config = toml::from_str(text).unwrap();
    assert_eq!(parsed.hotkeys.start_stop, "F2");
    assert_eq!(parsed.hotkeys.pause, "F8");
    assert_eq!(parsed.encode.fps, 30);
    assert_eq!(parsed.encode.codec, Codec::H265);
    assert_eq!(parsed.encode.custom_width, 1920);
}

#[test]
fn fps_clamps_to_1_240() {
    let mut c = Config::default();
    c.encode.fps = 0;
    assert_eq!(c.encode.fps(), 1);
    c.encode.fps = 500;
    assert_eq!(c.encode.fps(), 240);
    c.encode.fps = 30;
    assert_eq!(c.encode.fps(), 30);
}

#[test]
fn default_app_audio_only_is_true() {
    assert!(Config::default().default_app_audio_only);
}

#[test]
fn default_app_audio_only_missing_from_toml_falls_back_to_true() {
    // Simulates a config.toml saved before this field existed.
    let text = r#"
            output_dir = "."
            language = "en"
            [hotkeys]
            start_stop = "F9"
            pause = "F8"
            toggle_overlay = "F7"
            [overlay]
            enabled = false
            opacity = 0.85
            [encode]
            codec = "h265"
            fps = 60
            resolution_mode = "native"
            custom_width = 1920
            custom_height = 1080
            bitrate_mode = "auto"
            manual_bitrate_mbps = 12
        "#;
    let parsed: Config = toml::from_str(text).unwrap();
    assert!(parsed.default_app_audio_only);
    // Also confirms encoder_mode -- likewise absent from this pre-existing
    // TOML -- falls back to "hardware" instead of failing to parse.
    assert_eq!(parsed.encode.encoder_mode, EncoderMode::Hardware);
}

#[test]
fn gpu_frames_is_off_unless_turned_on_and_survives_a_save() {
    // Absent from every config written before the setting existed.
    assert!(!Config::default().encode.gpu_frames);
    let parsed: Config = toml::from_str(
        "[encode]
codec = \"h264\"
",
    )
    .unwrap();
    assert!(!parsed.encode.gpu_frames);

    let mut c = Config::default();
    c.encode.gpu_frames = true;
    let text = toml::to_string(&c).unwrap();
    let parsed: Config = toml::from_str(&text).unwrap();
    assert!(parsed.encode.gpu_frames);
}

#[test]
fn default_language_is_english() {
    assert_eq!(Config::default().language, Lang::En);
    assert_eq!(Config::default().lang(), Lang::En);
}

#[test]
fn lang_recognizes_japanese() {
    let c: Config = toml::from_str(r#"language = "ja""#).unwrap();
    assert_eq!(c.lang(), Lang::Ja);
}

#[test]
fn lang_unknown_string_falls_back_to_english() {
    let c: Config = toml::from_str(r#"language = "fr""#).unwrap();
    assert_eq!(c.lang(), Lang::En);
}

#[test]
fn registered_app_audio_defaults_to_empty() {
    assert!(Config::default().registered_app_audio.is_empty());
}

#[test]
fn registered_app_audio_missing_from_toml_falls_back_to_empty() {
    // Simulates a config.toml saved before this field existed.
    let text = r#"
            output_dir = "."
            language = "en"
            [hotkeys]
            start_stop = "F9"
            pause = "F8"
            toggle_overlay = "F7"
            [overlay]
            enabled = false
            opacity = 0.85
            [encode]
            codec = "h265"
            fps = 60
            resolution_mode = "native"
            custom_width = 1920
            custom_height = 1080
            bitrate_mode = "auto"
            manual_bitrate_mbps = 12
        "#;
    let parsed: Config = toml::from_str(text).unwrap();
    assert!(parsed.registered_app_audio.is_empty());
}

#[test]
fn register_app_audio_adds_a_new_entry() {
    let mut c = Config::default();
    c.register_app_audio("Discord.exe".into(), "C:/Discord/Discord.exe".into());
    assert_eq!(c.registered_app_audio.len(), 1);
    assert_eq!(c.registered_app_audio[0].exe_name, "Discord.exe");
    assert_eq!(c.registered_app_audio[0].exe_path, "C:/Discord/Discord.exe");
}

#[test]
fn register_app_audio_is_a_noop_for_an_exe_name_already_registered() {
    let mut c = Config::default();
    c.register_app_audio("Discord.exe".into(), "C:/Discord/Discord.exe".into());
    c.register_app_audio("Discord.exe".into(), "D:/Other/Discord.exe".into());
    assert_eq!(c.registered_app_audio.len(), 1);
    // First registration's path wins -- silently overwriting on a
    // duplicate call would be surprising for what's meant to be a
    // one-time pin.
    assert_eq!(c.registered_app_audio[0].exe_path, "C:/Discord/Discord.exe");
}

#[test]
fn unregister_app_audio_removes_the_matching_entry() {
    let mut c = Config::default();
    c.register_app_audio("Discord.exe".into(), "C:/Discord/Discord.exe".into());
    c.register_app_audio("Spotify.exe".into(), "C:/Spotify/Spotify.exe".into());
    c.unregister_app_audio("Discord.exe");
    assert_eq!(c.registered_app_audio.len(), 1);
    assert_eq!(c.registered_app_audio[0].exe_name, "Spotify.exe");
}

#[test]
fn unregister_app_audio_is_a_noop_for_an_exe_name_not_registered() {
    let mut c = Config::default();
    c.register_app_audio("Discord.exe".into(), "C:/Discord/Discord.exe".into());
    c.unregister_app_audio("NotRegistered.exe");
    assert_eq!(c.registered_app_audio.len(), 1);
}

#[test]
fn registered_app_audio_round_trips_toml() {
    let mut original = Config::default();
    original.register_app_audio("Discord.exe".into(), "C:/Discord/Discord.exe".into());
    let text = toml::to_string_pretty(&original).unwrap();
    let parsed: Config = toml::from_str(&text).unwrap();
    assert_eq!(parsed.registered_app_audio, original.registered_app_audio);
}

#[test]
fn selected_audio_device_ids_defaults_to_none() {
    assert_eq!(Config::default().selected_audio_device_ids, None);
}

#[test]
fn selected_audio_device_ids_missing_from_toml_falls_back_to_none() {
    // Simulates a config.toml saved before this field existed.
    let text = r#"
            output_dir = "."
            language = "en"
            [hotkeys]
            start_stop = "F9"
            pause = "F8"
            toggle_overlay = "F7"
            [overlay]
            enabled = false
            opacity = 0.85
            [encode]
            codec = "h265"
            fps = 60
            resolution_mode = "native"
            custom_width = 1920
            custom_height = 1080
            bitrate_mode = "auto"
            manual_bitrate_mbps = 12
        "#;
    let parsed: Config = toml::from_str(text).unwrap();
    assert_eq!(parsed.selected_audio_device_ids, None);
}

const SPEAKERS: &str = "{0.0.0.00000000}.{2cdd9143-16c3-42ba-b371-58651e4f0f4a}";
const MIC: &str = "{0.0.1.00000000}.{1bdba34e-35f7-4c39-9c98-14eef5a49919}";

/// What PolyRec 0.5.144 and before saved: the ids of the devices ticked.
fn ticked_before(ids: &[&str]) -> Config {
    Config {
        selected_audio_device_ids: Some(ids.iter().map(|&id| id.into()).collect()),
        ..Config::default()
    }
}

#[test]
fn with_nothing_ticked_yet_the_default_output_is_recorded_and_the_mic_isnt() {
    let config = Config::default();
    assert!(config.records_default_output());
    assert!(!config.records_default_input());
}

/// The devices someone had ticked are carried over by what they were --
/// an output, a microphone -- whatever devices those are now.
#[test]
fn ticks_saved_by_device_before_carry_over_by_role() {
    let both = ticked_before(&[SPEAKERS, MIC]);
    assert!(both.records_default_output() && both.records_default_input());
    let mic_only = ticked_before(&[MIC]);
    assert!(!mic_only.records_default_output() && mic_only.records_default_input());
    // Everything unticked on purpose stays so; it isn't "never saved".
    let none = ticked_before(&[]);
    assert!(!none.records_default_output() && !none.records_default_input());
}

#[test]
fn a_tick_changed_since_wins_over_the_old_list() {
    let config = Config {
        record_default_input: Some(false),
        ..ticked_before(&[SPEAKERS, MIC])
    };
    assert!(config.records_default_output());
    assert!(!config.records_default_input());
}

const HEADSET_MIC: &str = "{0.0.1.00000000}.{headset}";

/// PolyRec 0.5.144 and before read only the device list. With it gone
/// (0.5.145), an older copy sharing this config.toml -- an installed
/// one beside one built from source -- found no ticks and fell back to
/// speakers on, mic off.
#[test]
fn older_copies_still_find_the_ticks_in_the_device_list_they_read() {
    let mut config = Config::default();
    config.record_default(true, SPEAKERS, true);
    config.record_default(false, MIC, true);
    let saved = toml::to_string_pretty(&config).unwrap();
    assert!(saved.contains("selected_audio_device_ids"), "{saved}");
    let ids = config.selected_audio_device_ids.unwrap_or_default();
    assert!(ids.contains(&SPEAKERS.to_string()) && ids.contains(&MIC.to_string()));
}

/// An older copy saving config.toml drops the settings it doesn't know
/// -- the ticks by role -- and keeps the device list. The microphone
/// tick came back off.
#[test]
fn the_ticks_survive_an_older_copy_saving_over_them() {
    let mut config = Config::default();
    config.record_default(true, SPEAKERS, false);
    config.record_default(false, MIC, true);
    let mut as_an_older_copy_saves_it: Config =
        toml::from_str(&toml::to_string_pretty(&config).unwrap()).unwrap();
    as_an_older_copy_saves_it.record_default_output = None;
    as_an_older_copy_saves_it.record_default_input = None;
    let parsed: Config =
        toml::from_str(&toml::to_string_pretty(&as_an_older_copy_saves_it).unwrap()).unwrap();
    assert!(!parsed.records_default_output());
    assert!(parsed.records_default_input());
}

/// The device list holds the current default microphone, not every one
/// it has been.
#[test]
fn a_switched_microphone_replaces_the_one_before_it_in_the_device_list() {
    let mut config = Config::default();
    config.record_default(true, SPEAKERS, true);
    config.record_default(false, MIC, true);
    config.record_default(false, HEADSET_MIC, true);
    let ids = config.selected_audio_device_ids.unwrap_or_default();
    assert_eq!(ids, vec![SPEAKERS.to_string(), HEADSET_MIC.to_string()]);
}

#[test]
fn the_ticks_survive_a_save() {
    let original = Config {
        record_default_output: Some(false),
        record_default_input: Some(true),
        ..Config::default()
    };
    let text = toml::to_string_pretty(&original).unwrap();
    let parsed: Config = toml::from_str(&text).unwrap();
    assert_eq!(parsed.record_default_output, Some(false));
    assert_eq!(parsed.record_default_input, Some(true));
}

#[test]
fn the_cursor_is_recorded_unless_turned_off_and_the_setting_survives_a_save() {
    // Also absent from every config written before the setting existed.
    assert!(Config::default().record_cursor);
    let parsed: Config = toml::from_str("output_dir = \".\"").unwrap();
    assert!(parsed.record_cursor);

    let original = Config {
        record_cursor: false,
        ..Config::default()
    };
    let text = toml::to_string_pretty(&original).unwrap();
    let parsed: Config = toml::from_str(&text).unwrap();
    assert!(!parsed.record_cursor);
}

#[test]
fn the_close_button_quits_unless_set_to_hide_and_the_setting_survives_a_save() {
    // Also absent from every config written before the setting existed.
    assert!(!Config::default().close_to_tray);
    let parsed: Config = toml::from_str("output_dir = \".\"").unwrap();
    assert!(!parsed.close_to_tray);

    let original = Config {
        close_to_tray: true,
        ..Config::default()
    };
    let text = toml::to_string_pretty(&original).unwrap();
    let parsed: Config = toml::from_str(&text).unwrap();
    assert!(parsed.close_to_tray);
}

#[test]
fn show_recording_border_is_true_by_default() {
    assert!(Config::default().show_recording_border);
}

#[test]
fn show_recording_border_missing_from_toml_falls_back_to_true() {
    // Simulates a config.toml saved before this field existed.
    let text = r#"
            output_dir = "."
            language = "en"
            [hotkeys]
            start_stop = "F9"
            pause = "F8"
            toggle_overlay = "F7"
            [overlay]
            enabled = false
            opacity = 0.85
            [encode]
            codec = "h265"
            fps = 60
            resolution_mode = "native"
            custom_width = 1920
            custom_height = 1080
            bitrate_mode = "auto"
            manual_bitrate_mbps = 12
        "#;
    let parsed: Config = toml::from_str(text).unwrap();
    assert!(parsed.show_recording_border);
}

#[test]
fn show_recording_border_round_trips_toml_when_disabled() {
    let original = Config {
        show_recording_border: false,
        ..Config::default()
    };
    let text = toml::to_string_pretty(&original).unwrap();
    let parsed: Config = toml::from_str(&text).unwrap();
    assert!(!parsed.show_recording_border);
}

#[test]
fn use_process_name_for_recording_is_true_by_default() {
    assert!(Config::default().use_process_name_for_recording);
}

#[test]
fn custom_recording_prefix_is_empty_by_default() {
    assert_eq!(Config::default().custom_recording_prefix, "");
}

#[test]
fn recording_naming_fields_missing_from_toml_fall_back_to_defaults() {
    // Simulates a config.toml saved before these fields existed.
    let text = r#"
            output_dir = "."
            language = "en"
            [hotkeys]
            start_stop = "F9"
            pause = "F8"
            toggle_overlay = "F7"
            [overlay]
            enabled = false
            opacity = 0.85
            [encode]
            codec = "h265"
            fps = 60
            resolution_mode = "native"
            custom_width = 1920
            custom_height = 1080
            bitrate_mode = "auto"
            manual_bitrate_mbps = 12
        "#;
    let parsed: Config = toml::from_str(text).unwrap();
    assert!(parsed.use_process_name_for_recording);
    assert_eq!(parsed.custom_recording_prefix, "");
}

#[test]
fn recording_naming_fields_round_trip_toml_with_non_default_values() {
    let original = Config {
        use_process_name_for_recording: false,
        custom_recording_prefix: "MyStream".into(),
        ..Config::default()
    };
    let text = toml::to_string_pretty(&original).unwrap();
    let parsed: Config = toml::from_str(&text).unwrap();
    assert!(!parsed.use_process_name_for_recording);
    assert_eq!(parsed.custom_recording_prefix, "MyStream");
}

#[test]
fn encode_config_new_fields_round_trip_toml() {
    let mut original = Config::default();
    original.encode.resolution_mode = ResolutionKind::Custom;
    original.encode.custom_width = 1280;
    original.encode.custom_height = 720;
    original.encode.bitrate_mode = BitrateKind::Manual;
    original.encode.manual_bitrate_mbps = 15;
    let text = toml::to_string_pretty(&original).unwrap();
    let parsed: Config = toml::from_str(&text).unwrap();
    assert_eq!(parsed.encode, original.encode);
}
