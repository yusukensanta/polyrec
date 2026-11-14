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

/// Display language, stored in `config.toml` as "en" | "ja"; an unknown
/// stored value loads as English.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Ja,
    /// Last because serde's `other` fallback variant has to be.
    #[default]
    #[serde(other)]
    En,
}

impl Lang {
    pub fn toggle(self) -> Self {
        match self {
            Lang::En => Lang::Ja,
            Lang::Ja => Lang::En,
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

/// Every string of the UI, once: its name, then the English and the
/// Japanese text. Gives `Strings` (a field per name), `EN` and `JA`. A
/// string used to be written in three places -- the field, and its text in
/// each language two hundred lines apart -- which is how a translation
/// drifts from its original.
macro_rules! strings {
    ($( $(#[$doc:meta])* $name:ident: $en:literal, $ja:literal; )*) => {
        pub struct Strings {
            $( $(#[$doc])* pub $name: &'static str, )*
        }

        pub static EN: Strings = Strings {
            $( $name: $en, )*
        };

        pub static JA: Strings = Strings {
            $( $name: $ja, )*
        };

        /// Every string as (name, English, Japanese), for the tests that
        /// look at all of them.
        #[cfg(test)]
        const ALL: &[(&str, &str, &str)] = &[
            $( (stringify!($name), $en, $ja), )*
        ];
    };
}

strings! {
    // Menu bar
    overlay_on:
        "Overlay: ON",
        "オーバーレイ: オン";
    overlay_off:
        "Overlay: OFF",
        "オーバーレイ: オフ";
    overlay_toggle_tooltip:
        "Show/hide the recording HUD overlay",
        "録画中のHUDオーバーレイの表示/非表示を切り替え";
    border_on:
        "Border: ON",
        "枠線: オン";
    border_off:
        "Border: OFF",
        "枠線: オフ";
    border_toggle_tooltip:
        "Show/hide Windows' colored capture border around the recorded window/monitor",
        "録画中のウィンドウ/モニターに表示されるWindowsのキャプチャ枠線の表示/非表示を切り替え";
    /// Menu-bar toggle for `Config::close_to_tray`.
    close_quits:
        "Close: Quit",
        "閉じる: 終了";
    close_hides:
        "Close: To tray",
        "閉じる: トレイへ";
    close_toggle_tooltip:
        "What the window's close button does: quit PolyRec, or hide it in the notification area (tray) so the hotkeys keep working. \"Quit PolyRec\" in the tray menu quits either way.",
        "ウィンドウの閉じるボタンの動作: PolyRec を終了するか、通知領域（トレイ）に隠してホットキーを使えるままにします。トレイメニューの「PolyRec を終了」ではどちらの場合も終了します。";
    /// Menu-bar toggle for starting with Windows (`crate::autostart`).
    startup_on:
        "Startup: ON",
        "自動起動: オン";
    startup_off:
        "Startup: OFF",
        "自動起動: オフ";
    startup_toggle_tooltip:
        "Start PolyRec when you sign in to Windows, hidden in the notification area (tray), so the hotkeys and Highlight are ready without opening it.",
        "Windows にサインインしたときに PolyRec を通知領域（トレイ）に隠した状態で起動します。開かなくてもホットキーとハイライトが使えます。";
    startup_failed_prefix:
        "Couldn't change whether PolyRec starts with Windows: ",
        "Windows と同時に起動する設定を変更できませんでした: ";
    /// Quality popup checkbox for `Config::record_cursor`.
    record_cursor_label:
        "Record the mouse pointer",
        "マウスポインターを録画する";
    record_cursor_tooltip:
        "Whether the mouse pointer is in recordings and highlights. Takes effect from the next recording.",
        "録画とハイライトにマウスポインターを入れるかどうか。次の録画から反映されます。";
    /// Toast when the close button has hidden PolyRec in the tray.
    toast_still_in_tray:
        "PolyRec is still running in the tray",
        "PolyRec はトレイで動作しています";
    language_toggle_tooltip:
        "Switch display language",
        "表示言語を切り替え";
    // used as "{version} {suffix}", after the update icon
    update_available_suffix:
        "available",
        "が利用可能";
    update_tooltip:
        "Click to update now",
        "クリックして今すぐ更新";
    /// "{prefix}{error}" -- shown in the error banner alongside the existing
    /// tracing::error! log whenever Config::save() fails.
    config_save_failed_prefix:
        "Failed to save settings: ",
        "設定の保存に失敗しました: ";

    // Self-update confirm/progress popup
    update_confirm_title:
        "Update PolyRec",
        "PolyRecを更新";
    /// "{prefix}{version}{suffix}" -- the version is inserted between these.
    update_confirm_prefix:
        "Update to ",
        "";
    update_confirm_suffix:
        " now? PolyRec will close and restart.",
        " に今すぐ更新しますか？PolyRecは一度終了して再起動します。";
    update_confirm_uac_note:
        "Windows may show a permission prompt during the update.",
        "更新中にWindowsの権限確認ダイアログが表示される場合があります。";
    update_now_button:
        "Update Now",
        "今すぐ更新";
    update_not_now_button:
        "Not Now",
        "後で";
    update_view_release_notes:
        "View release notes",
        "リリースノートを見る";
    update_working_message:
        "Updating… PolyRec will restart shortly.",
        "更新中…まもなくPolyRecが再起動します。";
    update_failed_prefix:
        "Update failed: ",
        "更新に失敗しました: ";
    update_blocked_while_recording:
        "Can't update while a recording is in progress or being saved — wait until it's saved first.",
        "録画中、または保存中は更新できません。保存が終わってからにしてください。";

    // Left panel — source list / audio
    capture_source_header:
        "CAPTURE SOURCE",
        "キャプチャ ソース";
    no_windows_found:
        "No visible windows found.",
        "表示中のウィンドウが見つかりません。";
    source_filter_placeholder:
        "Search windows or apps...",
        "ウィンドウやアプリを検索...";
    /// Shown when the search box's filter matches nothing, distinct from
    /// `no_windows_found` (no capturable windows exist at all).
    no_matching_windows:
        "No windows match your search.",
        "検索に一致するウィンドウがありません。";
    /// Header for the physical-device list (Speakers/Microphone) inside the
    /// Audio popup, alongside `applications_header`.
    system_audio_header:
        "SYSTEM",
        "システム";
    no_audio_devices:
        "No audio devices found.",
        "オーディオデバイスが見つかりません。";
    /// Header for the per-app audio source list (Discord, Spotify, etc.)
    /// inside the Audio popup. See `types::AppAudioSource`.
    applications_header:
        "APPLICATIONS",
        "アプリケーション";
    no_app_audio_sources:
        "No apps added yet — use + Add app below.",
        "アプリはまだ追加されていません。下の「+ アプリを追加」から追加してください。";
    app_audio_only_label:
        "App audio only (exclude other system sounds)",
        "このアプリの音声のみ（他のシステム音を除外）";
    tooltip_no_loopback_device:
        "No system playback device found — this needs one to exist (being muted doesn't matter, but a device must be present).",
        "システム再生デバイスが見つかりません。ミュートは問題ありませんが、デバイス自体は存在している必要があります。";
    tooltip_check_loopback_first:
        "Check a system audio (speakers) box above first.",
        "まず上のシステム音声（スピーカー）にチェックを入れてください。";
    tooltip_app_audio_only:
        "Records only the selected window's own audio via Windows' Process Loopback API, instead of the full desktop mix. Needs an active system playback device — muting it doesn't stop this from working.",
        "デスクトップ全体の音声ではなく、Windows の Process Loopback API を使って選択したウィンドウ自体の音声のみを録音します。有効な再生デバイスが必要です（ミュートしていても動作します）。";
    /// Shown instead of `tooltip_app_audio_only` when the selected capture
    /// source is a `CaptureKind::Monitor` entry (or none is selected) --
    /// there's no owning process to scope loopback to in that case.
    tooltip_app_audio_only_needs_window:
        "Select a specific window (not a monitor) as the capture source to use this — an entire-screen recording has no single app to scope audio to.",
        "この機能を使うには、モニターではなく特定のウィンドウをキャプチャソースとして選択してください。画面全体の録画には音声を絞り込む対象のアプリがありません。";
    /// "+ Add app" button under the Applications list -- the only way an
    /// app gets into that list at all (it doesn't auto-populate from
    /// what's currently making sound, see
    /// `actions::build_app_audio_sources`'s doc comment). Opens the in-app
    /// search picker below, not a file browser directly -- see
    /// `add_app_search_placeholder`.
    add_app_button:
        "+ Add app",
        "+ アプリを追加";
    /// Hover text on a checked Applications checkbox, explaining that
    /// unchecking it un-registers (removes) the app -- the checkbox is the
    /// only pin/unpin control, there's no separate remove button.
    remove_registered_app_tooltip:
        "Uncheck to remove this app — while it isn't running there's nothing to record yet, but it'll start automatically the moment it launches.",
        "チェックを外すとこのアプリを削除します。実行されていない間は録音対象がありませんが、起動すると自動的に録音が始まります。";
    /// Placeholder text for the add-app picker's search box, which searches
    /// both currently open windows and installed-but-not-running apps
    /// (resolved from Start Menu shortcuts) -- an app doesn't need to be
    /// open, let alone making sound, to be found and pinned. Picking a
    /// result needs no file browsing. Same convention as
    /// `source_filter_placeholder`.
    add_app_search_placeholder:
        "Search apps…",
        "アプリを検索…";
    /// Shown when the add-app search box's filter matches neither an open
    /// window nor an installed app -- distinct from "nothing found at all"
    /// (handled by hiding the list instead).
    add_app_no_matches:
        "No apps match.",
        "一致するアプリがありません。";
    /// Shown instead of the search results when neither source has anything
    /// to search through at all (no windows open, no Start Menu shortcuts
    /// resolvable) -- a vanishingly rare edge case.
    add_app_none_running:
        "No apps found.",
        "アプリが見つかりません。";
    /// Fallback button in the add-app picker for an app that isn't
    /// currently running (so it can't be found by searching live
    /// sessions) -- opens the native file browser instead, same as
    /// `add_app_button` used to directly.
    add_app_browse_button:
        "Browse for .exe instead…",
        "代わりに .exe を参照…";
    /// Shown in the add-app picker while `enumerate_installed_apps`'s
    /// background scan is still running -- open-window candidates already
    /// show above this, so it's specifically flagging that more
    /// (installed-only) results may still arrive, not that nothing's found
    /// yet.
    add_app_scanning:
        "Still searching installed apps…",
        "インストール済みアプリを検索中…";
    /// Small tag next to a picker candidate that's currently running --
    /// distinguishes it from an installed-but-not-running one, which starts
    /// capturing only once launched.
    add_app_running_tag:
        "● Running",
        "● 実行中";

    // Center panel — status
    status_header:
        "STATUS",
        "ステータス";
    state_paused:
        "PAUSED",
        "一時停止中";
    state_recording:
        "RECORDING",
        "録画中";
    // used as "{n} {tracks_word}"
    tracks_word:
        "tracks",
        "トラック";
    /// Hover text on the recording stats line (tracks · fps · size).
    recording_stats_tooltip:
        "Audio tracks, the video's frame rate, and the file size so far. The video always runs at exactly this frame rate: when no new picture comes in time for a frame, the one before is repeated.",
        "音声トラック数、動画のフレームレート、ここまでのファイルサイズです。動画は常にこのフレームレートちょうどで記録されます。フレームに間に合う新しい画像がないときは、直前の画像を繰り返します。";
    /// Added to `recording_stats_tooltip` once a recording has run a second:
    /// `{new}` of the video's `{fps}` frames a second were new pictures.
    recording_new_pictures_template:
        "So far, {new} of its {fps} frames a second were new pictures; the rest repeat the one before. A still screen, or a game running below {fps} fps, gives fewer.",
        "ここまで、毎秒 {fps} フレームのうち {new} フレームが新しい画像で、残りは直前の画像の繰り返しです。静止した画面や、ゲームが {fps} fps を下回っているときは少なくなります。";
    saving_recording:
        "Saving recording…",
        "録画を保存中…";
    select_source_prompt:
        "Select a source and press REC to start.",
        "ソースを選択して「録画」を押すと開始します。";
    /// In the selected source's card when it has no picture to show.
    preview_unavailable:
        "No preview while it's minimized",
        "最小化中はプレビューできません";
    // Only shown while Idle -- Recording/Paused already show their state via
    // the pulsing dot + colored label at the top of the center panel.
    // "State: " — followed by session_state_idle
    state_prefix:
        "State: ",
        "状態: ";
    session_state_idle:
        "Idle",
        "待機中";

    // Center panel — output / buttons
    output_header:
        "OUTPUT",
        "出力";
    quality_button:
        "Quality",
        "画質";
    quality_tooltip:
        "FPS, codec, resolution, and bitrate for the next recording",
        "次回の録画の FPS・コーデック・解像度・ビットレート";
    hotkeys_button:
        "Hotkeys",
        "ショートカット";
    hotkeys_tooltip:
        "Rebind the start/stop, pause, overlay-toggle, and Save Highlight shortcuts",
        "開始/停止・一時停止・オーバーレイ切替・ハイライト保存のショートカットを再設定";
    audio_button:
        "Audio",
        "オーディオ";
    audio_tooltip:
        "Which audio devices and apps to record, and whether to scope to just the selected window's own audio",
        "録音するオーディオデバイスとアプリ、選択したウィンドウ自体の音声のみに絞るかどうか";
    /// Shown under the settings button row so the current audio selection
    /// is readable without opening the popup -- otherwise moving AUDIO out
    /// of the always-visible source panel would hide that state entirely.
    no_audio_selected:
        "No audio selected",
        "オーディオ未選択";
    /// "{n} {audio_sources_word}" -- same pattern as `tracks_word`.
    audio_sources_word:
        "selected",
        "選択中";
    browse_button:
        "Browse…",
        "参照…";
    open_output_dir_button:
        "Open",
        "開く";
    open_output_dir_tooltip:
        "Open the folder recordings are saved in",
        "録画の保存先フォルダーを開く";
    recent_header:
        "RECENT",
        "最近の録画";
    recent_play_tooltip:
        "Play",
        "再生";
    recent_show_tooltip:
        "Show in folder",
        "フォルダーで表示";
    /// Under the output folder (and on a disabled REC) when its drive or
    /// network share isn't there.
    output_dir_unavailable:
        "This drive or network share isn't available, so recordings can't be saved here.",
        "このドライブまたはネットワーク共有が見つからないため、ここには録画を保存できません。";
    /// Under the output folder (and on a disabled REC) when the path leads
    /// through a file.
    output_dir_not_a_folder:
        "Part of this path is a file, not a folder, so recordings can't be saved here.",
        "このパスの途中がフォルダーではなくファイルのため、ここには録画を保存できません。";
    /// "{free_space_prefix}{formatted bytes}" -- see format_bytes in dashboard/widgets.rs.
    free_space_prefix:
        "Free: ",
        "空き容量: ";
    /// How long the free space lasts at the current quality, after the free
    /// space: "{h}" hours and "{m}" minutes.
    time_left_hours_template:
        "about {h} h of recording",
        "録画 約{h}時間分";
    time_left_hours_minutes_template:
        "about {h} h {m} min of recording",
        "録画 約{h}時間{m}分";
    time_left_minutes_template:
        "about {m} min of recording",
        "録画 約{m}分";
    time_left_under_a_minute:
        "under a minute of recording",
        "録画 1分未満";
    /// Toast while recording when the drive is nearly full; "{left}" is one
    /// of the time-left texts.
    toast_low_space_template:
        "Recording drive almost full: {left} left",
        "録画先の空き容量が残りわずかです（{left}）";
    /// Notice (and toast) when a microphone has given no sound at all 10 s
    /// into a recording; "{mics}" lists them.
    notice_silent_microphones_template:
        "No sound from {mics} since recording started. Check it isn't muted.",
        "{mics} から録画開始後まったく音が入っていません。ミュートになっていないか確認してください。";
    resume_button:
        "RESUME",
        "再開";
    stop_button:
        "STOP",
        "停止";
    rec_button:
        "REC",
        "録画";
    pause_tooltip:
        "Pause",
        "一時停止";
    /// Checkbox above the output-dir row -- checked (default) names each
    /// finished recording/Highlight save after the recorded process, same as
    /// before this setting existed; unchecked reveals a text field for
    /// `custom_recording_prefix` instead (see `recording_naming::RecordingNaming`).
    use_process_name_label:
        "Use process name automatically",
        "プロセス名を自動的に使用";
    use_process_name_tooltip:
        "Names each finished recording after the app being recorded (e.g. \"vivaldi_2026-09-19-14-30-00.mp4\"). Uncheck to use your own prefix instead.",
        "録画対象のアプリ名でファイル名を付けます（例:「vivaldi_2026-09-19-14-30-00.mp4」）。チェックを外すと、独自の接頭辞を使用できます。";
    /// Hint text for the custom-prefix field, shown only while
    /// `use_process_name_label`'s checkbox is unchecked.
    custom_recording_prefix_placeholder:
        "Recording prefix (e.g. MyStream) — saved as MyStream_001.mp4, MyStream_002.mp4, …",
        "録画の接頭辞（例: MyStream）— MyStream_001.mp4, MyStream_002.mp4 … として保存されます";

    // Hotkey hints, under the action buttons and on the overlay HUD; "{key}"
    // is the bound key.
    hotkey_hint_record_front:
        "{key}: record the window in front",
        "{key}: 前面のウィンドウを録画";
    hotkey_hint_stop:
        "{key} stop",
        "{key} 停止";
    hotkey_hint_pause:
        "{key} pause",
        "{key} 一時停止";
    hotkey_hint_resume:
        "{key} resume",
        "{key} 再開";
    /// Hover text on REC while no source is selected.
    rec_needs_source_tooltip:
        "Select a window or display on the left first.",
        "まず左のリストからウィンドウかディスプレイを選んでください。";
    /// Hover text on REC while the last recording is still being saved.
    rec_wait_for_save_tooltip:
        "Wait until the last recording has been saved.",
        "前の録画の保存が終わるまでお待ちください。";
    rec_wait_for_update_tooltip:
        "PolyRec is updating and will restart — wait until it has.",
        "PolyRec を更新中です。再起動するまでお待ちください。";
    // Hotkey toasts (see `ui::dashboard::toast`)
    toast_recording:
        "● Recording",
        "● 録画中";
    toast_stopped:
        "⏹ Stopped, saving",
        "⏹ 停止、保存中";
    toast_paused:
        "⏸ Paused",
        "⏸ 一時停止";
    toast_resumed:
        "▶ Resumed",
        "▶ 再開";
    toast_highlight_off:
        "Highlight isn't on",
        "ハイライトは無効です";
    /// The start/stop hotkey didn't start a recording (the dashboard says
    /// why).
    toast_not_started:
        "⚠ Recording didn't start",
        "⚠ 録画を開始できませんでした";
    /// Shown by a copy started in place of one that closed on its own (see
    /// `restart`): its graphics device was lost, or it panicked. `{path}`
    /// is crash.log's.
    notice_restarted_graphics:
        "PolyRec restarted itself after a problem with the graphics driver (for example a driver update or reset). A recording that was running was stopped and saved.",
        "グラフィックドライバーの問題（ドライバーの更新やリセットなど）のため、PolyRec を自動で再起動しました。録画中だった録画は停止して保存しました。";
    notice_restarted_error:
        "PolyRec restarted itself after an unexpected error. A recording that was running was stopped and saved. What went wrong is in {path}.",
        "予期しないエラーのため、PolyRec を自動で再起動しました。録画中だった録画は停止して保存しました。エラーの内容は {path} に記録されています。";
    /// Shown when config.toml couldn't be read and was moved aside (see
    /// `Config::load_or_set_aside`). `{path}` is where it went.
    notice_config_set_aside:
        "Your settings file couldn't be read, so PolyRec started with its default settings. The old file is kept as {path}.",
        "設定ファイルを読み込めなかったため、既定の設定で起動しました。元のファイルは {path} に残してあります。";
    /// Shown when config.toml couldn't be read, or moved aside
    /// (`ConfigTrouble::LeftAlone`). `{path}` is where it is.
    notice_config_left_alone:
        "Your settings file couldn't be read, so PolyRec started with its default settings. It won't save over the file this time, so nothing in it is lost. Another program may have had it open: close PolyRec and start it again to read {path}.",
        "設定ファイルを読み込めなかったため、既定の設定で起動しました。今回はこのファイルに上書き保存しないので、内容は失われません。他のプログラムが開いていた可能性があります。PolyRec を起動し直すと {path} を読み込みます。";
    /// `{files}`: what the recordings cut off last time are saved as.
    notice_recovered_template:
        "PolyRec closed partway through a recording last time. What was recorded up to then is saved as {files}.",
        "前回、録画の途中で PolyRec が終了しました。そこまでの録画を {files} として保存しました。";
    /// A message box, for a launch that finds PolyRec running but can't
    /// bring it up (`single_instance::NotClaimed::Unreachable`).
    already_running_unreachable:
        "PolyRec is already running, but it can't be brought up from here. It may have been started as administrator. Open it from the notification area, or close it and start PolyRec again.",
        "PolyRec はすでに起動していますが、ここから表示できません。管理者として起動されている可能性があります。通知領域から開くか、終了してから PolyRec を起動し直してください。";
    /// An export's failure when its destination is the recording itself.
    export_over_itself:
        "That's the recording being exported. Choose another name to save the export as.",
        "エクスポート元の録画と同じファイルです。別の名前を指定してください。";
    /// Shown once a recording that lost its video source or an audio
    /// track's device/app before it was stopped is saved -- see
    /// `capture::LostSources`. `{tracks}` names the audio tracks.
    notice_video_source_lost:
        "The video stopped before you did: the recorded window closed, its display was disconnected, or capturing it failed. From there on, the video doesn't change.",
        "停止する前に映像が止まりました（録画中のウィンドウが閉じられた、ディスプレイが切断された、またはキャプチャに失敗しました）。それ以降の映像は変化しません。";
    notice_audio_source_lost_template:
        "Audio from {tracks} stopped partway through (a device was unplugged, or an app closed). From there on, that audio is silent.",
        "{tracks} の音声が途中で止まりました（デバイスが外されたか、アプリが終了しました）。それ以降の音声は無音です。";
    /// The same for tracks whose device came back (`LostSources::audio_interrupted`).
    notice_audio_source_interrupted_template:
        "Audio from {tracks} went silent for a while partway through (its device was unplugged or turned off) and carried on once a device was back.",
        "{tracks} の音声が途中でしばらく無音になりました（デバイスが外されたか、オフになりました）。デバイスが戻ってからは録音を続けています。";

    // Notification-area icon (see `ui::dashboard::tray`)
    tray_show:
        "Show PolyRec",
        "PolyRec を表示";
    tray_hide:
        "Hide PolyRec",
        "PolyRec を隠す";
    /// Start item while nothing can be recorded yet (disabled).
    tray_start:
        "Start recording",
        "録画開始";
    /// Start item with a source selected; "{source}" is its title.
    tray_start_source_template:
        "Start recording: {source}",
        "録画開始: {source}";
    tray_stop:
        "Stop recording",
        "録画停止";
    tray_quit:
        "Quit PolyRec",
        "PolyRec を終了";

    // Audio popup
    audio_title:
        "Audio Sources",
        "オーディオソース";

    // Quality popup
    quality_title:
        "Quality Settings",
        "画質設定";
    fps_header:
        "FPS",
        "FPS";
    /// Caption under the fps presets: "{hz}" is the selected source's
    /// display refresh rate.
    fps_display_caption_template:
        "The selected source's display refreshes at {hz} Hz.",
        "選択中のソースのディスプレイのリフレッシュレートは {hz} Hz です。";
    /// Hover text on an fps preset the selected source's display can't
    /// reach; "{hz}" is its refresh rate and "{fps}" the standard rate a
    /// recording from it runs at instead (not always a preset: 50 isn't).
    fps_above_display_template:
        "Above this display's {hz} Hz refresh rate. It can't deliver more frames than it draws, so a recording from it runs at {fps} fps instead.",
        "このディスプレイのリフレッシュレート（{hz} Hz）を超えています。描画される以上のフレームは取得できないため、このソースの録画は代わりに {fps} fps になります。";
    /// Hover text on an fps preset the encoder rejects at the source's
    /// output size; fills in "{codec}", "{fps}", "{w}" and "{h}".
    fps_encoder_limit_template:
        "This PC's {codec} encoder can't record {fps} fps at {w}×{h}. Try H265 or a lower resolution.",
        "この PC の {codec} エンコーダーは {w}×{h} で {fps} fps を録画できません。H265 にするか解像度を下げてください。";
    /// Caption when the chosen fps can't be used for the selected source;
    /// "{fps}" is what a recording from it will run at instead.
    fps_recorded_template:
        "This source will record at {fps} fps.",
        "このソースは {fps} fps で録画されます。";
    codec_header:
        "CODEC",
        "コーデック";
    encoder_mode_header:
        "ENCODER",
        "エンコーダー";
    encoder_mode_hardware:
        "Hardware",
        "ハードウェア";
    encoder_mode_software:
        "Software",
        "ソフトウェア";
    encoder_mode_hardware_tooltip:
        "Use the GPU's encoder (NVENC/QuickSync/AMF) when available -- recommended",
        "GPUのエンコーダー（NVENC/QuickSync/AMF）が使用可能な場合はそれを使用します（推奨）";
    encoder_mode_software_tooltip:
        "Encode on the CPU instead -- frees up GPU headroom (useful while a demanding game/app is running), at the cost of higher CPU usage",
        "代わりにCPUでエンコードします -- GPUの負荷を空けられます（負荷の高いゲームやアプリの実行中に便利）が、CPU使用率は高くなります";
    /// Checkbox under the encoder choice -- see `EncodeConfig::gpu_frames`.
    gpu_frames_label:
        "Keep frames on the GPU (experimental)",
        "フレームをGPU上で処理（試験的）";
    gpu_frames_tooltip:
        "Hands each captured frame to the encoder without copying it through system memory -- far less CPU while recording. Falls back to the usual way if your GPU's encoder doesn't support it.",
        "キャプチャした各フレームをシステムメモリにコピーせずにエンコーダーへ渡します。録画中のCPU使用率が大きく下がります。GPUのエンコーダーが対応していない場合は通常の方法で録画します。";
    /// Hover text on the checkbox while the software encoder is chosen.
    gpu_frames_needs_hardware:
        "Needs the hardware encoder.",
        "ハードウェアエンコーダーが必要です。";
    resolution_header:
        "RESOLUTION",
        "解像度";
    resolution_native:
        "Native (window)",
        "ネイティブ（ウィンドウ）";
    resolution_display:
        "Match display",
        "ディスプレイに合わせる";
    resolution_custom:
        "Custom",
        "カスタム";
    width_label:
        "W:",
        "幅:";
    height_label:
        "H:",
        "高さ:";
    bitrate_header:
        "BITRATE",
        "ビットレート";
    bitrate_auto:
        "Auto",
        "自動";
    bitrate_manual:
        "Manual",
        "手動";
    mbps_label:
        "Mbps:",
        "Mbps:";
    close_button:
        "Close",
        "閉じる";

    // Hotkeys popup
    hotkeys_title:
        "Hotkeys",
        "ショートカット";
    hotkey_start_stop_header:
        "START / STOP RECORDING",
        "録画の開始/停止";
    hotkey_pause_header:
        "PAUSE / RESUME",
        "一時停止/再開";
    hotkey_overlay_header:
        "TOGGLE OVERLAY",
        "オーバーレイ切替";
    hotkey_save_highlight_header:
        "SAVE HIGHLIGHT",
        "ハイライトを保存";
    hotkey_collision_warning:
        "⚠ Two actions share the same key — only one will respond.",
        "⚠ 2つの操作が同じキーに割り当てられています。片方のみ動作します。";
    hotkey_change_button:
        "Change",
        "変更";
    hotkey_change_tooltip:
        "Press a new key combination for this action",
        "この操作の新しいキーの組み合わせを押してください";
    hotkey_press_any_key_prompt:
        "Press any key (Ctrl/Alt/Shift optional)…",
        "何かキーを押してください（Ctrl/Alt/Shiftとの組み合わせも可）…";
    hotkey_press_esc_to_cancel:
        "(Esc to cancel)",
        "(Escでキャンセル)";
    /// Prefix for "'<key>' is already in use or reserved by Windows — try a
    /// different key." — the key name itself is appended at the call site,
    /// same pattern as the other `_prefix` fields in this struct.
    hotkey_unavailable_prefix:
        "⚠ '",
        "⚠「";
    hotkey_unavailable_suffix:
        "' is already in use or reserved by Windows — try a different key.",
        "」はすでに使用されているか、Windowsに予約されています。別のキーをお試しください。";

    // Error popup
    error_title:
        "Error",
        "エラー";
    disk_full_mid_recording:
        "Recording stopped automatically — your disk ran low on free space (below 500 MB). The recording up to that point was saved.",
        "空き容量が不足したため録画を自動停止しました（500MB未満）。その時点までの録画は保存されています。";
    /// A recording the recorder stopped because writing it kept failing.
    /// `{error}` is the last failure.
    notice_recording_gave_up:
        "Recording stopped automatically — PolyRec couldn't keep writing it ({error}). The recording up to that point was saved.",
        "録画を書き込めなくなったため自動停止しました（{error}）。その時点までの録画は保存されています。";
    recording_failed_prefix:
        "Recording failed: ",
        "録画に失敗しました: ";
    recording_ended_unexpectedly_prefix:
        "Recording ended unexpectedly: ",
        "録画が予期せず終了しました: ";
    couldnt_start_recording_prefix:
        "Couldn't start recording: ",
        "録画を開始できませんでした: ";
    /// Notice shown once per recording if the rate of new frames captured
    /// falls persistently short of the target; fills in "{achieved}" and
    /// "{target}". The file still runs at the target, repeating frames (see
    /// `encode::cfr`). It names both causes, because the app can't tell them
    /// apart: a still screen (Windows only delivers changed frames) or a PC
    /// struggling to keep up.
    fps_shortfall_template:
        "Only {achieved} of the {target} frames a second so far were new pictures; the rest repeat the one before. A still screen gives fewer new pictures; if the picture was moving, this PC may be struggling to keep up.",
        "ここまで毎秒 {target} フレームのうち新しい画像は {achieved} フレームで、残りは直前の画像の繰り返しです。静止した画面では新しい画像が少なくなります。画面が動いていた場合は、この PC の処理が追いついていない可能性があります。";
    /// Shown when the finished file has fewer audio tracks than were
    /// selected before recording -- a source that never produced a single
    /// real buffer (muted mic, disconnected device, silent app) gets
    /// silently dropped by Media Foundation's sink writer rather than kept
    /// as an empty track, so without this the user has no way to know one
    /// of their sources didn't make it in. `{actual}`/`{expected}` are
    /// replaced with the real counts at the call site.
    audio_tracks_missing_template:
        "Only {actual} of {expected} selected audio sources made it into this recording — the rest produced no sound (check for a muted mic or a disconnected/silent device) and were left out.",
        "選択した音声ソースのうち {actual}/{expected} のみがこの録画に含まれました。残りは音声が検出されず（マイクのミュートやデバイスの切断・無音をご確認ください）除外されました。";

    // Export controls (inline in the status panel)
    recording_saved_label:
        "Recording saved:",
        "録画を保存しました:";
    audio_tracks_header:
        "AUDIO TRACKS",
        "オーディオ トラック";
    /// Fallback per-track checkbox label when the recorded file has more
    /// tracks than `last_recording_audio_labels` has names for (e.g. a
    /// device disconnected mid-recording) -- used as "{track_label_fallback} {n}".
    track_label_fallback:
        "Track",
        "トラック";
    /// Checkbox: decode the checked tracks above and sum them into a single
    /// output track instead of keeping each as its own stream -- useful for
    /// platforms (e.g. YouTube) that only ever play one audio track from an
    /// uploaded file. Unchecked by default.
    export_mix_tracks_label:
        "Mix into one track",
        "1つのトラックに統合";
    export_mix_tracks_tooltip:
        "Combine the checked tracks above into a single audio track -- useful for sites like YouTube that only play one audio track from an uploaded file.",
        "上でチェックしたトラックを1つの音声トラックに統合します -- アップロードしたファイルの音声トラックを1つしか再生しないYouTubeなどのサイト向けです。";
    export_button:
        "Export",
        "エクスポート";
    export_tooltip:
        "Remux with only the checked audio tracks (no re-encoding)",
        "チェックした音声トラックのみで再多重化します（再エンコードなし）";
    /// The file type an export is saved as, in its Save dialog.
    mp4_file_type:
        "MP4 video",
        "MP4 動画";
    /// The file type Browse... picks an app by, in the Audio pop-up.
    exe_file_type:
        "Program",
        "プログラム";
    open_folder_button:
        "Open Folder",
        "フォルダを開く";
    open_folder_tooltip:
        "Open the folder containing this recording",
        "この録画が入っているフォルダを開く";
    exporting_header:
        "EXPORTING…",
        "エクスポート中…";
    please_wait:
        "Please wait…",
        "お待ちください…";
    export_complete_header:
        "EXPORT COMPLETE",
        "エクスポート完了";
    export_failed_header:
        "EXPORT FAILED",
        "エクスポート失敗";

    // Highlight buffer -- Quality popup settings section, status indicator
    highlight_header:
        "HIGHLIGHT",
        "ハイライト";
    highlight_enabled_label:
        "Enable Highlight buffer",
        "ハイライトバッファを有効にする";
    tooltip_highlight_enabled:
        "Continuously captures the foreground app in the background. Press the Save Highlight hotkey any time to save the last few seconds/minutes to a file — no need to have pressed record beforehand.",
        "フォアグラウンドのアプリをバックグラウンドで常時録画します。ハイライト保存のショートカットを押すと、事前に録画を開始していなくても直近の数秒〜数分をファイルに保存できます。";
    highlight_buffer_seconds_label:
        "Buffer length (seconds):",
        "バッファ長（秒）:";
    highlight_status_active:
        "● Highlight buffering",
        "● ハイライト録画中";
    highlight_save_not_active:
        "Highlight buffering isn't active — enable it in Quality settings first.",
        "ハイライトバッファが有効になっていません。まず画質設定で有効にしてください。";
    highlight_saving_label:
        "Saving highlight…",
        "ハイライトを保存中…";
    highlight_saved_prefix:
        "Highlight saved: ",
        "ハイライトを保存しました: ";
    highlight_save_failed_prefix:
        "Highlight save failed: ",
        "ハイライトの保存に失敗しました: ";
    highlight_disk_full_message:
        "Highlight buffering stopped automatically — your disk ran low on free space (below 500 MB).",
        "空き容量が不足したため、ハイライトバッファを自動停止しました（500MB未満）。";
    /// `{why}`: the error.
    highlight_failed_template:
        "Highlight buffering stopped: {why}. It will try again shortly.",
        "ハイライトバッファが停止しました: {why}。しばらくしてから再開を試みます。";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_swaps_language() {
        assert_eq!(Lang::En.toggle(), Lang::Ja);
        assert_eq!(Lang::Ja.toggle(), Lang::En);
    }

    #[test]
    fn lang_round_trips_through_its_stored_string() {
        for (lang, stored) in [(Lang::En, "\"en\""), (Lang::Ja, "\"ja\"")] {
            assert_eq!(stored_value(lang), stored);
        }
    }

    /// `Lang` serialized as a bare TOML value (`toml` only serializes whole
    /// documents, so wrap it in a one-field table and strip the key).
    fn stored_value(lang: Lang) -> String {
        #[derive(serde::Serialize)]
        struct One {
            v: Lang,
        }
        toml::to_string(&One { v: lang })
            .unwrap()
            .trim()
            .trim_start_matches("v = ")
            .to_string()
    }

    #[test]
    fn toggle_button_label_names_the_other_language() {
        assert_eq!(Lang::En.toggle_button_label(), "日本語");
        assert_eq!(Lang::Ja.toggle_button_label(), "English");
    }

    /// A blank string would silently render as invisible UI text. The one
    /// meant to be blank: Japanese puts all of the update question after
    /// the version, so its half before it is empty.
    #[test]
    fn no_string_is_empty_in_either_language() {
        for (name, en, ja) in ALL {
            assert!(!en.is_empty(), "{name} (English)");
            assert!(
                !ja.is_empty() || *name == "update_confirm_prefix",
                "{name} (Japanese)"
            );
        }
    }

    /// The `{placeholder}`s a string is filled in through are part of its
    /// contract with the code: one missing from a translation leaves the
    /// value out, one misspelt shows up in the UI as it stands.
    #[test]
    fn both_languages_take_the_same_placeholders() {
        fn placeholders(text: &str) -> Vec<&str> {
            let mut found: Vec<&str> = text
                .split('{')
                .skip(1)
                .filter_map(|rest| rest.split_once('}').map(|(name, _)| name))
                .collect();
            found.sort_unstable();
            found.dedup();
            found
        }
        for (name, en, ja) in ALL {
            assert_eq!(placeholders(en), placeholders(ja), "{name}");
        }
    }
}
