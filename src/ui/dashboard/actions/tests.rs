use super::*;
use crate::types::CaptureKind;

fn mic() -> AudioDevice {
    AudioDevice {
        id: "mic-id".into(),
        name: "Mic".into(),
        is_loopback: false,
    }
}

fn app_source(display_name: &str) -> AppAudioSource {
    app_source_with_pids(display_name, vec![1234])
}

fn app_source_with_pids(display_name: &str, process_ids: Vec<u32>) -> AppAudioSource {
    AppAudioSource {
        process_ids,
        exe_name: "game.exe".into(),
        display_name: display_name.into(),
        icon_rgba: None,
    }
}

#[test]
fn build_audio_labels_orders_devices_before_app_sources() {
    let labels = build_audio_labels(&[mic()], &[app_source("Game")]);
    assert_eq!(
        labels,
        vec![
            TrackLabel {
                kind: TrackKind::Microphone,
                name: "Mic".into()
            },
            TrackLabel {
                kind: TrackKind::App,
                name: "Game".into()
            },
        ]
    );
    // The glyph is drawn in the icon column, not as part of the name;
    // in a sentence (a notice) it goes in front.
    assert_eq!(labels[0].to_string(), "🎙 Mic");
    assert_eq!(labels[1].to_string(), "Game");
}

#[test]
fn build_audio_labels_empty_when_nothing_selected() {
    assert!(build_audio_labels(&[], &[]).is_empty());
}

#[test]
fn build_audio_labels_repeats_name_once_per_process_id() {
    // session::start_capture spawns one track per pid, not one per
    // AppAudioSource -- e.g. Discord's main + helper processes, or two
    // independent windows of the same exe.
    let labels = build_audio_labels(
        &[mic()],
        &[app_source_with_pids("Discord", vec![111, 222, 333])],
    );
    assert_eq!(
        labels.iter().map(ToString::to_string).collect::<Vec<_>>(),
        vec!["🎙 Mic", "Discord", "Discord", "Discord"]
    );
}

#[test]
fn build_audio_labels_contributes_nothing_for_a_registered_but_inactive_source() {
    // Empty process_ids (registered but not currently running) spawns no
    // capture task in session::start_capture, so it must contribute no
    // label either -- otherwise a later source's label would land on the
    // wrong track.
    let labels = build_audio_labels(
        &[mic()],
        &[
            app_source_with_pids("Idle App", vec![]),
            app_source_with_pids("Live App", vec![42]),
        ],
    );
    assert_eq!(
        labels.iter().map(ToString::to_string).collect::<Vec<_>>(),
        vec!["🎙 Mic", "Live App"]
    );
}

#[test]
fn the_start_hotkey_over_the_dashboard_does_what_rec_would() {
    let why = "Select a window or display to record first.";
    assert!(matches!(
        hotkey_start(Foreground::Window(window(1, "Game")), Some(why)),
        HotkeyStart::Record(source) if source.window_title == "Game"
    ));
    assert!(matches!(
        hotkey_start(Foreground::Dashboard, None),
        HotkeyStart::RecordSelected
    ));
    assert!(matches!(
        hotkey_start(Foreground::Dashboard, Some(why)),
        HotkeyStart::Explain(said) if said == why
    ));
    assert!(matches!(
        hotkey_start(Foreground::Nothing, None),
        HotkeyStart::Nothing
    ));
}

fn window(hwnd: usize, title: &str) -> CaptureSource {
    CaptureSource {
        kind: CaptureKind::Window,
        process_id: 1,
        window_title: title.into(),
        exe_name: "app.exe".into(),
        hwnd,
        icon_rgba: None,
    }
}

fn device(id: &str) -> AudioDevice {
    AudioDevice {
        id: id.into(),
        name: id.into(),
        is_loopback: false,
    }
}

const SOUND_BLASTER: &str = "{0.0.0.00000000}.{sound-blaster}";
const BT_W3X: &str = "{0.0.0.00000000}.{bt-w3x}";
const YETI: &str = "{0.0.1.00000000}.{yeti}";
const HEADSET_MIC: &str = "{0.0.1.00000000}.{headset}";

/// The SYSTEM list as Windows' defaults stand: the default output (its
/// sound, through loopback), then the default microphone.
fn defaults(output: &str, mic: &str) -> [AudioDevice; 2] {
    [
        AudioDevice {
            id: output.into(),
            name: "Speakers".into(),
            is_loopback: true,
        },
        AudioDevice {
            id: mic.into(),
            name: "Microphone".into(),
            is_loopback: false,
        },
    ]
}

/// The list only ever shows Windows' default output and microphone, but
/// ticks were saved against the devices they were: switch a default to
/// another device for a while -- a headset -- and it showed unticked,
/// so it wasn't recorded, with nothing to say so. A tick now stays with
/// its role.
#[test]
fn a_tick_stays_with_its_role_when_windows_default_device_changes() {
    let mut config = Config::default();
    let usual = defaults(SOUND_BLASTER, YETI);
    remember_selected_audio(&mut config, &usual, &[true, true]);
    let switched = defaults(BT_W3X, HEADSET_MIC);
    assert_eq!(resolve_selected_audio(&switched, &config), vec![true, true]);
    assert_eq!(resolve_selected_audio(&usual, &config), vec![true, true]);
}

/// Ticking the switched-to device used to forget the one before it,
/// which then came back unticked.
#[test]
fn ticking_during_a_switch_leaves_the_device_switched_back_to_ticked() {
    let mut config = Config::default();
    let usual = defaults(SOUND_BLASTER, YETI);
    remember_selected_audio(&mut config, &usual, &[true, true]);
    remember_selected_audio(
        &mut config,
        &defaults(SOUND_BLASTER, HEADSET_MIC),
        &[true, true],
    );
    assert_eq!(resolve_selected_audio(&usual, &config), vec![true, true]);
}

/// With no microphone plugged in, only the output is listed; changing its
/// tick leaves the microphone's alone.
#[test]
fn a_role_with_no_device_listed_keeps_its_tick() {
    let mut config = Config::default();
    let both = defaults(SOUND_BLASTER, YETI);
    remember_selected_audio(&mut config, &both, &[true, true]);
    remember_selected_audio(&mut config, &both[..1], &[false]);
    assert_eq!(resolve_selected_audio(&both, &config), vec![false, true]);
}

#[test]
fn reconcile_sources_keeps_the_old_list_when_the_handle_set_is_unchanged() {
    let old = vec![window(1, "a"), window(2, "b")];
    // Reordered and retitled, same handles -- not a change.
    let new = vec![window(2, "b renamed"), window(1, "a")];
    assert!(reconcile_sources(&old, Some(1), new).is_none());
}

#[test]
fn reconcile_sources_re_points_the_selection_at_the_same_handle() {
    let old = vec![window(1, "a"), window(2, "b")];
    let new = vec![window(3, "c"), window(2, "b"), window(1, "a")];
    let (sources, selected) = reconcile_sources(&old, Some(1), new).expect("changed");
    assert_eq!(sources.len(), 3);
    assert_eq!(selected, Some(1));
    assert_eq!(sources[1].hwnd, 2);
}

#[test]
fn reconcile_sources_clears_the_selection_when_its_source_is_gone() {
    let old = vec![window(1, "a"), window(2, "b")];
    let new = vec![window(1, "a")];
    let (_, selected) = reconcile_sources(&old, Some(1), new).expect("changed");
    assert_eq!(selected, None);
}

#[test]
fn reconcile_sources_with_no_selection_stays_unselected() {
    let old = vec![window(1, "a")];
    let new = vec![window(1, "a"), window(2, "b")];
    let (_, selected) = reconcile_sources(&old, None, new).expect("changed");
    assert_eq!(selected, None);
}

#[test]
fn retitle_sources_follows_title_changes_in_place() {
    let mut sources = vec![window(1, "a"), window(2, "b")];
    // Same handles, reordered, one retitled: order stays, title follows.
    retitle_sources(&mut sources, &[window(2, "b renamed"), window(1, "a")]);
    let titles: Vec<_> = sources.iter().map(|s| s.window_title.as_str()).collect();
    assert_eq!(titles, ["a", "b renamed"]);
}

#[test]
fn retitle_sources_leaves_windows_missing_from_the_new_list_alone() {
    let mut sources = vec![window(1, "a"), window(2, "b")];
    retitle_sources(&mut sources, &[window(1, "a2"), window(3, "c")]);
    let titles: Vec<_> = sources.iter().map(|s| s.window_title.as_str()).collect();
    assert_eq!(titles, ["a2", "b"]);
}

#[test]
fn audio_device_ids_changed_ignores_order_but_not_membership() {
    let old = vec![device("a"), device("b")];
    assert!(!audio_device_ids_changed(&old, &[device("b"), device("a")]));
    assert!(audio_device_ids_changed(&old, &[device("a")]));
    assert!(audio_device_ids_changed(&old, &[device("a"), device("c")]));
}

/// Buffering window 1, window 1 in front, nothing in the way.
fn buffering() -> HighlightView {
    HighlightView {
        enabled: true,
        recording: false,
        saving: false,
        following: Some(1),
        front: Some(1),
        may_start: true,
    }
}

/// A save reads the buffer's segment files. Switching windows
/// discarded them under it (and the start after cleared the folder);
/// so did turning Highlight off.
#[test]
fn a_save_in_flight_keeps_the_buffer_it_reads() {
    let saving = HighlightView {
        saving: true,
        ..buffering()
    };
    for view in [
        HighlightView {
            front: Some(2),
            ..saving
        },
        HighlightView {
            enabled: false,
            ..saving
        },
    ] {
        assert_eq!(highlight_step(&view), HighlightStep::Leave);
    }
    let nothing_running = HighlightView {
        following: None,
        ..saving
    };
    assert_eq!(highlight_step(&nothing_running), HighlightStep::Leave);
}

/// Recording and buffering never run at once; pausing leaves the files.
#[test]
fn a_recording_pauses_buffering() {
    let recording = HighlightView {
        recording: true,
        ..buffering()
    };
    assert_eq!(highlight_step(&recording), HighlightStep::Pause);
    let saving_too = HighlightView {
        saving: true,
        ..recording
    };
    assert_eq!(highlight_step(&saving_too), HighlightStep::Pause);
}

#[test]
fn turning_highlight_off_discards_the_buffer() {
    let off = HighlightView {
        enabled: false,
        ..buffering()
    };
    assert_eq!(highlight_step(&off), HighlightStep::Discard);
}

#[test]
fn buffering_follows_the_window_in_front() {
    assert_eq!(highlight_step(&buffering()), HighlightStep::Leave);
    let switched = HighlightView {
        front: Some(2),
        ..buffering()
    };
    assert_eq!(highlight_step(&switched), HighlightStep::Start(2));
    let not_running = HighlightView {
        following: None,
        ..buffering()
    };
    assert_eq!(highlight_step(&not_running), HighlightStep::Start(1));
    let dashboard_in_front = HighlightView {
        front: None,
        ..buffering()
    };
    assert_eq!(highlight_step(&dashboard_in_front), HighlightStep::Leave);
}

/// No room on the disk, or a failure to wait out: it isn't started --
/// and a buffer for the window that was in front goes.
#[test]
fn a_window_that_may_not_start_isnt_started() {
    let not_running = HighlightView {
        following: None,
        may_start: false,
        ..buffering()
    };
    assert_eq!(highlight_step(&not_running), HighlightStep::Leave);
    let switched = HighlightView {
        front: Some(2),
        may_start: false,
        ..buffering()
    };
    assert_eq!(highlight_step(&switched), HighlightStep::Discard);
}

#[test]
fn refresh_snapshot_is_produced_on_the_worker_thread() {
    let ctx = egui::Context::default();
    let rx = spawn_refresh_worker(&ctx).expect("worker spawned");
    let snapshot = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("worker sent a snapshot");
    // Every connected display is always a capture source.
    assert!(
        snapshot
            .sources
            .iter()
            .any(|s| matches!(s.kind, CaptureKind::Monitor))
    );
}
