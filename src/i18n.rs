//! Display-language strings for the dashboard UI. Two languages (English,
//! Japanese) as two `const Strings` instances rather than a runtime lookup
//! table keyed by string — a typo'd or missing key in a HashMap-based
//! approach only shows up at runtime (or not at all, silently falling back);
//! a missing *field* on one of these structs is a compile error instead,
//! since both instances must satisfy the same `Strings` shape.
//!
//! Scope: static UI chrome (labels, headers, tooltips, button text) is fully
//! translated. Underlying technical error text from `AppError`'s `Display`
//! impl (e.g. "Windows API error: ...") is not — those are wrapped into
//! translated sentences (see `dashboard.rs`'s error-message formatting) but
//! the technical detail itself stays in whatever language the OS/API
//! reported it in, same as most apps leave raw exception text untranslated.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Ja,
}

impl Lang {
    pub fn toggle(self) -> Self {
        match self {
            Lang::En => Lang::Ja,
            Lang::Ja => Lang::En,
        }
    }

    /// Config value to persist (`Config::language`) — round-trips through
    /// `Config::lang()`.
    pub fn config_value(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ja => "ja",
        }
    }

    /// Label for the language-toggle button itself — names the *other*
    /// language, matching the convention most apps use for a language
    /// switcher (tapping it says what you'll switch to, not what you're on).
    pub fn toggle_button_label(self) -> &'static str {
        match self {
            Lang::En => "日本語",
            Lang::Ja => "English",
        }
    }

    pub fn strings(self) -> &'static Strings {
        match self {
            Lang::En => &EN,
            Lang::Ja => &JA,
        }
    }
}

pub struct Strings {
    // Menu bar
    pub overlay_on: &'static str,
    pub overlay_off: &'static str,
    pub overlay_toggle_tooltip: &'static str,
    pub language_toggle_tooltip: &'static str,
    pub update_available_suffix: &'static str, // used as "⬆ {version} {suffix}"
    pub update_tooltip: &'static str,
    /// "{prefix}{error}" -- shown in the error banner alongside the existing
    /// tracing::error! log whenever Config::save() fails.
    pub config_save_failed_prefix: &'static str,

    // Self-update confirm/progress popup
    pub update_confirm_title: &'static str,
    /// "{prefix}{version}{suffix}" -- the version is inserted between these.
    pub update_confirm_prefix: &'static str,
    pub update_confirm_suffix: &'static str,
    pub update_confirm_uac_note: &'static str,
    pub update_now_button: &'static str,
    pub update_not_now_button: &'static str,
    pub update_view_release_notes: &'static str,
    pub update_working_message: &'static str,
    pub update_failed_prefix: &'static str,
    pub update_blocked_while_recording: &'static str,
    /// Shown (via the same error-banner mechanism) right before the update
    /// confirm popup, when clicking Update while Highlight buffering (but no
    /// manual recording) was active -- buffering is stopped and
    /// `Config::highlight.enabled` cleared to let the update proceed, since
    /// unlike a manual recording there's no in-progress footage to lose.
    pub update_highlight_disabled_notice: &'static str,

    // Left panel — source list / audio
    pub capture_source_header: &'static str,
    pub no_windows_found: &'static str,
    pub source_filter_placeholder: &'static str,
    /// Shown when the search box's filter matches nothing, distinct from
    /// `no_windows_found` (no capturable windows exist at all).
    pub no_matching_windows: &'static str,
    /// Header for the physical-device list (Speakers/Microphone) inside the
    /// Audio popup, alongside `applications_header`.
    pub system_audio_header: &'static str,
    pub no_audio_devices: &'static str,
    /// Header for the per-app audio source list (Discord, Spotify, etc.)
    /// inside the Audio popup. See `types::AppAudioSource`.
    pub applications_header: &'static str,
    pub no_app_audio_sources: &'static str,
    pub app_audio_only_label: &'static str,
    pub tooltip_no_loopback_device: &'static str,
    pub tooltip_check_loopback_first: &'static str,
    pub tooltip_app_audio_only: &'static str,
    /// Shown instead of `tooltip_app_audio_only` when the selected capture
    /// source is a `CaptureKind::Monitor` entry (or none is selected) --
    /// there's no owning process to scope loopback to in that case.
    pub tooltip_app_audio_only_needs_window: &'static str,
    /// "+ Add app" button under the Applications list -- the only way an
    /// app gets into that list at all (it doesn't auto-populate from
    /// what's currently making sound, see
    /// `actions::build_app_audio_sources`'s doc comment). Opens the in-app
    /// search picker below, not a file browser directly -- see
    /// `add_app_search_placeholder`.
    pub add_app_button: &'static str,
    /// Hover text on a checked Applications checkbox, explaining that
    /// unchecking it un-registers (removes) the app -- the checkbox is the
    /// only pin/unpin control, there's no separate remove button.
    pub remove_registered_app_tooltip: &'static str,
    /// Placeholder text for the add-app picker's search box, which searches
    /// both currently open windows and installed-but-not-running apps
    /// (resolved from Start Menu shortcuts) -- an app doesn't need to be
    /// open, let alone making sound, to be found and pinned. Picking a
    /// result needs no file browsing. Same convention as
    /// `source_filter_placeholder`.
    pub add_app_search_placeholder: &'static str,
    /// Shown when the add-app search box's filter matches neither an open
    /// window nor an installed app -- distinct from "nothing found at all"
    /// (handled by hiding the list instead).
    pub add_app_no_matches: &'static str,
    /// Shown instead of the search results when neither source has anything
    /// to search through at all (no windows open, no Start Menu shortcuts
    /// resolvable) -- a vanishingly rare edge case.
    pub add_app_none_running: &'static str,
    /// Fallback button in the add-app picker for an app that isn't
    /// currently running (so it can't be found by searching live
    /// sessions) -- opens the native file browser instead, same as
    /// `add_app_button` used to directly.
    pub add_app_browse_button: &'static str,
    /// Shown in the add-app picker while `enumerate_installed_apps`'s
    /// background scan is still running -- open-window candidates already
    /// show above this, so it's specifically flagging that more
    /// (installed-only) results may still arrive, not that nothing's found
    /// yet.
    pub add_app_scanning: &'static str,
    /// Small tag next to a picker candidate that's currently running --
    /// distinguishes it from an installed-but-not-running one, which starts
    /// capturing only once launched.
    pub add_app_running_tag: &'static str,

    // Center panel — status
    pub status_header: &'static str,
    pub state_paused: &'static str,
    pub state_recording: &'static str,
    pub tracks_word: &'static str, // used as "{n} {tracks_word}"
    pub frames_word: &'static str, // used as "{n} {frames_word}"
    pub saving_recording: &'static str,
    pub select_source_prompt: &'static str,
    // Only shown while Idle -- Recording/Paused already show their state via
    // the pulsing dot + colored label at the top of the center panel.
    pub state_prefix: &'static str, // "State: " — followed by session_state_idle
    pub session_state_idle: &'static str,

    // Center panel — output / buttons
    pub output_header: &'static str,
    pub quality_button: &'static str,
    pub quality_tooltip: &'static str,
    pub hotkeys_button: &'static str,
    pub hotkeys_tooltip: &'static str,
    pub audio_button: &'static str,
    pub audio_tooltip: &'static str,
    /// Shown under the settings button row so the current audio selection
    /// is readable without opening the popup -- otherwise moving AUDIO out
    /// of the always-visible source panel would hide that state entirely.
    pub no_audio_selected: &'static str,
    /// "{n} {audio_sources_word}" -- same pattern as `tracks_word`/`frames_word`.
    pub audio_sources_word: &'static str,
    pub browse_button: &'static str,
    /// "{free_space_prefix}{formatted bytes}" -- see format_bytes_free in dashboard.rs.
    pub free_space_prefix: &'static str,
    pub resume_button: &'static str,
    pub stop_button: &'static str,
    pub rec_button: &'static str,
    pub pause_tooltip: &'static str,

    // Overlay HUD
    pub overlay_hud_stop_word: &'static str,

    // Audio popup
    pub audio_title: &'static str,

    // Quality popup
    pub quality_title: &'static str,
    pub fps_header: &'static str,
    pub codec_header: &'static str,
    pub encoder_mode_header: &'static str,
    pub encoder_mode_hardware: &'static str,
    pub encoder_mode_software: &'static str,
    pub encoder_mode_hardware_tooltip: &'static str,
    pub encoder_mode_software_tooltip: &'static str,
    pub resolution_header: &'static str,
    pub resolution_native: &'static str,
    pub resolution_display: &'static str,
    pub resolution_custom: &'static str,
    pub width_label: &'static str,
    pub height_label: &'static str,
    pub bitrate_header: &'static str,
    pub bitrate_auto: &'static str,
    pub bitrate_manual: &'static str,
    pub mbps_label: &'static str,
    pub close_button: &'static str,

    // Hotkeys popup
    pub hotkeys_title: &'static str,
    pub hotkey_start_stop_header: &'static str,
    pub hotkey_pause_header: &'static str,
    pub hotkey_overlay_header: &'static str,
    pub hotkey_save_highlight_header: &'static str,
    pub hotkey_collision_warning: &'static str,
    pub hotkey_change_button: &'static str,
    pub hotkey_change_tooltip: &'static str,
    pub hotkey_press_any_key_prompt: &'static str,
    pub hotkey_press_esc_to_cancel: &'static str,
    /// Prefix for "'<key>' is already in use or reserved by Windows — try a
    /// different key." — the key name itself is appended at the call site,
    /// same pattern as the other `_prefix` fields in this struct.
    pub hotkey_unavailable_prefix: &'static str,
    pub hotkey_unavailable_suffix: &'static str,

    // Error popup
    pub error_title: &'static str,
    pub disk_full_mid_recording: &'static str,
    pub recording_failed_prefix: &'static str,
    pub recording_ended_unexpectedly_prefix: &'static str,
    pub couldnt_start_recording_prefix: &'static str,
    /// Shown when the finished file has fewer audio tracks than were
    /// selected before recording -- a source that never produced a single
    /// real buffer (muted mic, disconnected device, silent app) gets
    /// silently dropped by Media Foundation's sink writer rather than kept
    /// as an empty track, so without this the user has no way to know one
    /// of their sources didn't make it in. `{actual}`/`{expected}` are
    /// replaced with the real counts at the call site.
    pub audio_tracks_missing_template: &'static str,

    // Export controls (inline in the status panel)
    pub recording_saved_label: &'static str,
    pub audio_tracks_header: &'static str,
    /// Fallback per-track checkbox label when the recorded file has more
    /// tracks than `last_recording_audio_labels` has names for (e.g. a
    /// device disconnected mid-recording) -- used as "{track_label_fallback} {n}".
    pub track_label_fallback: &'static str,
    pub export_button: &'static str,
    pub export_tooltip: &'static str,
    pub open_folder_button: &'static str,
    pub open_folder_tooltip: &'static str,
    pub exporting_header: &'static str,
    pub please_wait: &'static str,
    pub export_complete_header: &'static str,
    pub export_failed_header: &'static str,

    // Highlight buffer -- Quality popup settings section, status indicator
    pub highlight_header: &'static str,
    pub highlight_enabled_label: &'static str,
    pub tooltip_highlight_enabled: &'static str,
    pub highlight_buffer_seconds_label: &'static str,
    pub highlight_status_active: &'static str,
    pub highlight_save_not_active: &'static str,
    pub highlight_saving_label: &'static str,
    pub highlight_saved_prefix: &'static str,
    pub highlight_save_failed_prefix: &'static str,
    pub highlight_disk_full_message: &'static str,
}

pub static EN: Strings = Strings {
    overlay_on: "Overlay: ON",
    overlay_off: "Overlay: OFF",
    overlay_toggle_tooltip: "Show/hide the recording HUD overlay",
    language_toggle_tooltip: "Switch display language",
    update_available_suffix: "available",
    update_tooltip: "Click to update now",
    config_save_failed_prefix: "Failed to save settings: ",

    update_confirm_title: "Update PolyRec",
    update_confirm_prefix: "Update to ",
    update_confirm_suffix: " now? PolyRec will close and restart.",
    update_confirm_uac_note: "Windows may show a permission prompt during the update.",
    update_now_button: "Update Now",
    update_not_now_button: "Not Now",
    update_view_release_notes: "View release notes",
    update_working_message: "Updating… PolyRec will restart shortly.",
    update_failed_prefix: "Update failed: ",
    update_blocked_while_recording: "Can't update while a recording is in progress — stop it first.",
    update_highlight_disabled_notice: "Highlight buffering was stopped and disabled to let the update proceed.",

    capture_source_header: "CAPTURE SOURCE",
    no_windows_found: "No visible windows found.",
    source_filter_placeholder: "Search windows or apps...",
    no_matching_windows: "No windows match your search.",
    system_audio_header: "SYSTEM",
    no_audio_devices: "No audio devices found.",
    applications_header: "APPLICATIONS",
    no_app_audio_sources: "No apps added yet — use + Add app below.",
    app_audio_only_label: "🎯 App audio only (exclude other system sounds)",
    tooltip_no_loopback_device: "No system playback device found — this needs one to exist (being muted doesn't matter, but a device must be present).",
    tooltip_check_loopback_first: "Check the system audio (🔊) box above first.",
    tooltip_app_audio_only: "Records only the selected window's own audio via Windows' Process Loopback API, instead of the full desktop mix. Needs an active system playback device — muting it doesn't stop this from working.",
    tooltip_app_audio_only_needs_window: "Select a specific window (not a monitor) as the capture source to use this — an entire-screen recording has no single app to scope audio to.",
    add_app_button: "+ Add app",
    remove_registered_app_tooltip: "Uncheck to remove this app — while it isn't running there's nothing to record yet, but it'll start automatically the moment it launches.",
    add_app_search_placeholder: "Search apps…",
    add_app_no_matches: "No apps match.",
    add_app_none_running: "No apps found.",
    add_app_browse_button: "Browse for .exe instead…",
    add_app_scanning: "Still searching installed apps…",
    add_app_running_tag: "● Running",

    status_header: "STATUS",
    state_paused: "PAUSED",
    state_recording: "RECORDING",
    tracks_word: "tracks",
    frames_word: "frames",
    saving_recording: "Saving recording…",
    select_source_prompt: "Select a source and press REC to start.",
    state_prefix: "State: ",
    session_state_idle: "Idle",

    output_header: "OUTPUT",
    quality_button: "⚙ Quality",
    quality_tooltip: "FPS, codec, resolution, and bitrate for the next recording",
    hotkeys_button: "⌨ Hotkeys",
    hotkeys_tooltip: "Rebind the start/stop, pause, and overlay-toggle shortcuts",
    audio_button: "🔊 Audio",
    audio_tooltip: "Which audio devices and apps to record, and whether to scope to just the selected window's own audio",
    no_audio_selected: "No audio selected",
    audio_sources_word: "selected",
    browse_button: "Browse…",
    free_space_prefix: "Free: ",
    resume_button: "▶ RESUME",
    stop_button: "⏹ STOP",
    rec_button: "⏺ REC",
    pause_tooltip: "Pause",

    overlay_hud_stop_word: "stop",

    audio_title: "Audio Sources",

    quality_title: "Quality Settings",
    fps_header: "FPS",
    codec_header: "CODEC",
    encoder_mode_header: "ENCODER",
    encoder_mode_hardware: "Hardware",
    encoder_mode_software: "Software",
    encoder_mode_hardware_tooltip: "Use the GPU's encoder (NVENC/QuickSync/AMF) when available -- recommended",
    encoder_mode_software_tooltip: "Encode on the CPU instead -- frees up GPU headroom (useful while a demanding game/app is running), at the cost of higher CPU usage",
    resolution_header: "RESOLUTION",
    resolution_native: "Native (window)",
    resolution_display: "Match display",
    resolution_custom: "Custom",
    width_label: "W:",
    height_label: "H:",
    bitrate_header: "BITRATE",
    bitrate_auto: "Auto",
    bitrate_manual: "Manual",
    mbps_label: "Mbps:",
    close_button: "Close",

    hotkeys_title: "Hotkeys",
    hotkey_start_stop_header: "START / STOP RECORDING",
    hotkey_pause_header: "PAUSE / RESUME",
    hotkey_overlay_header: "TOGGLE OVERLAY",
    hotkey_save_highlight_header: "SAVE HIGHLIGHT",
    hotkey_collision_warning: "⚠ Two actions share the same key — only one will respond.",
    hotkey_change_button: "Change",
    hotkey_change_tooltip: "Press a new key combination for this action",
    hotkey_press_any_key_prompt: "Press any key (Ctrl/Alt/Shift optional)…",
    hotkey_press_esc_to_cancel: "(Esc to cancel)",
    hotkey_unavailable_prefix: "⚠ '",
    hotkey_unavailable_suffix: "' is already in use or reserved by Windows — try a different key.",

    error_title: "Error",
    disk_full_mid_recording: "Recording stopped automatically — your disk ran low on free space (below 500 MB). The recording up to that point was saved.",
    recording_failed_prefix: "Recording failed: ",
    recording_ended_unexpectedly_prefix: "Recording ended unexpectedly: ",
    couldnt_start_recording_prefix: "Couldn't start recording: ",
    audio_tracks_missing_template: "Only {actual} of {expected} selected audio sources made it into this recording — the rest produced no sound (check for a muted mic or a disconnected/silent device) and were left out.",

    recording_saved_label: "Recording saved:",
    audio_tracks_header: "AUDIO TRACKS",
    track_label_fallback: "Track",
    export_button: "Export",
    export_tooltip: "Remux with only the checked audio tracks (no re-encoding)",
    open_folder_button: "Open Folder",
    open_folder_tooltip: "Open the folder containing this recording",
    exporting_header: "EXPORTING…",
    please_wait: "Please wait…",
    export_complete_header: "EXPORT COMPLETE",
    export_failed_header: "EXPORT FAILED",

    highlight_header: "HIGHLIGHT",
    highlight_enabled_label: "✨ Enable Highlight buffer",
    tooltip_highlight_enabled: "Continuously captures the foreground app in the background. Press the Save Highlight hotkey any time to save the last few seconds/minutes to a file — no need to have pressed record beforehand.",
    highlight_buffer_seconds_label: "Buffer length (seconds):",
    highlight_status_active: "● Highlight buffering",
    highlight_save_not_active: "Highlight buffering isn't active — enable it in Quality settings first.",
    highlight_saving_label: "Saving highlight…",
    highlight_saved_prefix: "Highlight saved: ",
    highlight_save_failed_prefix: "Highlight save failed: ",
    highlight_disk_full_message: "Highlight buffering stopped automatically — your disk ran low on free space (below 500 MB).",
};

pub static JA: Strings = Strings {
    overlay_on: "オーバーレイ: オン",
    overlay_off: "オーバーレイ: オフ",
    overlay_toggle_tooltip: "録画中のHUDオーバーレイの表示/非表示を切り替え",
    language_toggle_tooltip: "表示言語を切り替え",
    update_available_suffix: "が利用可能",
    update_tooltip: "クリックして今すぐ更新",
    config_save_failed_prefix: "設定の保存に失敗しました: ",

    update_confirm_title: "PolyRecを更新",
    update_confirm_prefix: "",
    update_confirm_suffix: " に今すぐ更新しますか？PolyRecは一度終了して再起動します。",
    update_confirm_uac_note: "更新中にWindowsの権限確認ダイアログが表示される場合があります。",
    update_now_button: "今すぐ更新",
    update_not_now_button: "後で",
    update_view_release_notes: "リリースノートを見る",
    update_working_message: "更新中…まもなくPolyRecが再起動します。",
    update_failed_prefix: "更新に失敗しました: ",
    update_blocked_while_recording: "録画中は更新できません。先に停止してください。",
    update_highlight_disabled_notice: "更新のため、ハイライトバッファを停止して無効にしました。",

    capture_source_header: "キャプチャ ソース",
    no_windows_found: "表示中のウィンドウが見つかりません。",
    source_filter_placeholder: "ウィンドウやアプリを検索...",
    no_matching_windows: "検索に一致するウィンドウがありません。",
    system_audio_header: "システム",
    no_audio_devices: "オーディオデバイスが見つかりません。",
    applications_header: "アプリケーション",
    no_app_audio_sources: "アプリはまだ追加されていません。下の「+ アプリを追加」から追加してください。",
    app_audio_only_label: "🎯 このアプリの音声のみ（他のシステム音を除外）",
    tooltip_no_loopback_device: "システム再生デバイスが見つかりません。ミュートは問題ありませんが、デバイス自体は存在している必要があります。",
    tooltip_check_loopback_first: "まず上のシステム音声（🔊）にチェックを入れてください。",
    tooltip_app_audio_only: "デスクトップ全体の音声ではなく、Windows の Process Loopback API を使って選択したウィンドウ自体の音声のみを録音します。有効な再生デバイスが必要です（ミュートしていても動作します）。",
    tooltip_app_audio_only_needs_window: "この機能を使うには、モニターではなく特定のウィンドウをキャプチャソースとして選択してください。画面全体の録画には音声を絞り込む対象のアプリがありません。",
    add_app_button: "+ アプリを追加",
    remove_registered_app_tooltip: "チェックを外すとこのアプリを削除します。実行されていない間は録音対象がありませんが、起動すると自動的に録音が始まります。",
    add_app_search_placeholder: "アプリを検索…",
    add_app_no_matches: "一致するアプリがありません。",
    add_app_none_running: "アプリが見つかりません。",
    add_app_browse_button: "代わりに .exe を参照…",
    add_app_scanning: "インストール済みアプリを検索中…",
    add_app_running_tag: "● 実行中",

    status_header: "ステータス",
    state_paused: "一時停止中",
    state_recording: "録画中",
    tracks_word: "トラック",
    frames_word: "フレーム",
    saving_recording: "録画を保存中…",
    select_source_prompt: "ソースを選択して REC を押すと開始します。",
    state_prefix: "状態: ",
    session_state_idle: "待機中",

    output_header: "出力",
    quality_button: "⚙ 画質",
    quality_tooltip: "次回の録画の FPS・コーデック・解像度・ビットレート",
    hotkeys_button: "⌨ ショートカット",
    hotkeys_tooltip: "開始/停止・一時停止・オーバーレイ切替のショートカットを再設定",
    audio_button: "🔊 オーディオ",
    audio_tooltip: "録音するオーディオデバイスとアプリ、選択したウィンドウ自体の音声のみに絞るかどうか",
    no_audio_selected: "オーディオ未選択",
    audio_sources_word: "選択中",
    browse_button: "参照…",
    free_space_prefix: "空き容量: ",
    resume_button: "▶ 再開",
    stop_button: "⏹ 停止",
    rec_button: "⏺ 録画",
    pause_tooltip: "一時停止",

    overlay_hud_stop_word: "停止",

    audio_title: "オーディオソース",

    quality_title: "画質設定",
    fps_header: "FPS",
    codec_header: "コーデック",
    encoder_mode_header: "エンコーダー",
    encoder_mode_hardware: "ハードウェア",
    encoder_mode_software: "ソフトウェア",
    encoder_mode_hardware_tooltip: "GPUのエンコーダー（NVENC/QuickSync/AMF）が使用可能な場合はそれを使用します（推奨）",
    encoder_mode_software_tooltip: "代わりにCPUでエンコードします -- GPUの負荷を空けられます（負荷の高いゲームやアプリの実行中に便利）が、CPU使用率は高くなります",
    resolution_header: "解像度",
    resolution_native: "ネイティブ（ウィンドウ）",
    resolution_display: "ディスプレイに合わせる",
    resolution_custom: "カスタム",
    width_label: "幅:",
    height_label: "高さ:",
    bitrate_header: "ビットレート",
    bitrate_auto: "自動",
    bitrate_manual: "手動",
    mbps_label: "Mbps:",
    close_button: "閉じる",

    hotkeys_title: "ショートカット",
    hotkey_start_stop_header: "録画の開始/停止",
    hotkey_pause_header: "一時停止/再開",
    hotkey_overlay_header: "オーバーレイ切替",
    hotkey_save_highlight_header: "ハイライトを保存",
    hotkey_collision_warning: "⚠ 2つの操作が同じキーに割り当てられています。片方のみ動作します。",
    hotkey_change_button: "変更",
    hotkey_change_tooltip: "この操作の新しいキーの組み合わせを押してください",
    hotkey_press_any_key_prompt: "何かキーを押してください（Ctrl/Alt/Shiftとの組み合わせも可）…",
    hotkey_press_esc_to_cancel: "(Escでキャンセル)",
    hotkey_unavailable_prefix: "⚠「",
    hotkey_unavailable_suffix: "」はすでに使用されているか、Windowsに予約されています。別のキーをお試しください。",

    error_title: "エラー",
    disk_full_mid_recording: "空き容量が不足したため録画を自動停止しました（500MB未満）。その時点までの録画は保存されています。",
    recording_failed_prefix: "録画に失敗しました: ",
    recording_ended_unexpectedly_prefix: "録画が予期せず終了しました: ",
    couldnt_start_recording_prefix: "録画を開始できませんでした: ",
    audio_tracks_missing_template: "選択した音声ソースのうち {actual}/{expected} のみがこの録画に含まれました。残りは音声が検出されず（マイクのミュートやデバイスの切断・無音をご確認ください）除外されました。",

    recording_saved_label: "録画を保存しました:",
    audio_tracks_header: "オーディオ トラック",
    track_label_fallback: "トラック",
    export_button: "エクスポート",
    export_tooltip: "チェックした音声トラックのみで再多重化します（再エンコードなし）",
    open_folder_button: "フォルダを開く",
    open_folder_tooltip: "この録画が入っているフォルダを開く",
    exporting_header: "エクスポート中…",
    please_wait: "お待ちください…",
    export_complete_header: "エクスポート完了",
    export_failed_header: "エクスポート失敗",

    highlight_header: "ハイライト",
    highlight_enabled_label: "✨ ハイライトバッファを有効にする",
    tooltip_highlight_enabled: "フォアグラウンドのアプリをバックグラウンドで常時録画します。ハイライト保存のショートカットを押すと、事前に録画を開始していなくても直近の数秒〜数分をファイルに保存できます。",
    highlight_buffer_seconds_label: "バッファ長（秒）:",
    highlight_status_active: "● ハイライト録画中",
    highlight_save_not_active: "ハイライトバッファが有効になっていません。まず画質設定で有効にしてください。",
    highlight_saving_label: "ハイライトを保存中…",
    highlight_saved_prefix: "ハイライトを保存しました: ",
    highlight_save_failed_prefix: "ハイライトの保存に失敗しました: ",
    highlight_disk_full_message: "空き容量が不足したため、ハイライトバッファを自動停止しました（500MB未満）。",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_swaps_language() {
        assert_eq!(Lang::En.toggle(), Lang::Ja);
        assert_eq!(Lang::Ja.toggle(), Lang::En);
    }

    #[test]
    fn config_value_round_trips_through_toggle() {
        assert_eq!(Lang::En.config_value(), "en");
        assert_eq!(Lang::Ja.config_value(), "ja");
    }

    #[test]
    fn toggle_button_label_names_the_other_language() {
        assert_eq!(Lang::En.toggle_button_label(), "日本語");
        assert_eq!(Lang::Ja.toggle_button_label(), "English");
    }

    #[test]
    fn no_string_field_is_empty_in_either_language() {
        // A blank field would silently render as invisible UI text -- catches
        // a copy-paste-and-forgot-to-fill-in mistake at test time instead.
        for strings in [&EN, &JA] {
            assert!(!strings.quality_title.is_empty());
            assert!(!strings.close_button.is_empty());
            assert!(!strings.hotkey_collision_warning.is_empty());
            assert!(!strings.export_failed_header.is_empty());
            assert!(!strings.highlight_header.is_empty());
            assert!(!strings.highlight_enabled_label.is_empty());
            assert!(!strings.update_confirm_title.is_empty());
            assert!(!strings.update_now_button.is_empty());
            assert!(!strings.update_working_message.is_empty());
            assert!(!strings.update_blocked_while_recording.is_empty());
            assert!(!strings.hotkey_save_highlight_header.is_empty());
        }
    }
}
