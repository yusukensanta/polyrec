# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Starting or stopping a recording by hotkey no longer takes a game in exclusive full-screen (like Fortnite in "Fullscreen" mode) out of focus. The message confirming the hotkey appeared over the game, and any window over a game in exclusive full-screen knocks it out of full-screen, even one that never takes focus, so you had to click back into the game. Over such a game the message, and the overlay if it's on, now show on another monitor, or not at all with only one. Borderless full-screen games are unaffected.

## [v0.5.132] - 2026-11-01

### Fixed

- A hotkey with Alt+Shift (like Alt+Shift+A) no longer switches Windows' input language when pressed, which could pull a full-screen game out of focus. PolyRec keeps the hotkey's key from reaching the game, which left Windows seeing Alt+Shift pressed alone, its own language-switch shortcut; a lone Alt likewise opened the game window's menu. PolyRec now presses an unused key in between, as AutoHotkey does.

## [v0.5.131] - 2026-11-01

### Added

- Moving around PolyRec with Tab (or the arrow keys) now shows a ring around the control in focus, as Windows' own apps do, until the next click. egui's own sign of focus looked like a pressed button and was easy to miss.

## [v0.5.130] - 2026-11-01

### Added

- PolyRec follows Windows' theme: light or dark mode, a high-contrast theme (in Windows' own colours), and your accent colour for what's selected. Every text colour passes WCAG AA contrast in both the light and the dark colours, whatever the accent. Its title bar already followed Windows.
- With Windows' **Animation effects** off, PolyRec doesn't animate either; the recording dot stops pulsing.

## [v0.5.129] - 2026-11-01

### Changed

- Buttons and lists use Windows' own icons (Segoe Fluent Icons, as in Windows 11's apps; Segoe MDL2 Assets on Windows 10) in place of emoji, and the decorative emoji are gone from labels. Screen readers hear just each button's name. Where neither icon font is installed, the emoji stay.

## [v0.5.128] - 2026-11-01

### Changed

- Text is easier to read: body text, buttons and checkboxes are 14 px (they were 12.5) and the small status lines 12 px (they were 11), Windows 11's own sizes. Spacing inside cards and boxes is a little roomier, on Windows' 4 px grid, and a line now marks where the dashboard's content scrolls under the REC row.

### Added

- PolyRec's text follows Windows' **Text size** setting (Settings → Accessibility), and changes as soon as it does.
- The window opens at the size it was left at, and maximized if it was.

## [v0.5.127] - 2026-11-01

### Added

- The selected source's card shows a picture of the window or display, so it's plain what REC will record before recording it. The picture is taken when you pick the source and again when you come back to PolyRec, not continually, so a PolyRec left open costs nothing.

## [v0.5.126] - 2026-11-01

### Added

- The free-space line also says about how long that space lasts at the current quality, before and during a recording. It turns amber below 10 minutes, and while recording a message says so once, so a recording isn't cut short by a full drive without warning.
- Ten seconds into a recording, PolyRec says on screen if a microphone has given no sound at all since the recording started — usually a muted mic — while there's still time to fix it.

## [v0.5.125] - 2026-11-01

### Added

- A red dot on PolyRec's tray icon and taskbar button while it's recording, amber while paused, so you can tell it's recording without opening the window. Windows shows the taskbar dot only when **Show badges on taskbar apps** is on in its taskbar settings; see the user guide.

## [v0.5.124] - 2026-11-01

### Fixed

- The audio level meters move smoothly while recording. They used to show only the sound of the last hundredth of a second before each redraw, so they flickered and missed louder moments, and fell at whatever speed the window happened to redraw. They now show the loudest sound since the last redraw and fall back at a steady pace.
- Icons and names line up in the 🔊 Audio list, the level meters, the export track list and the capture source list. 🔊 and 🎙 are different widths, so Speakers and Microphone started at different places, and apps and displays were laid out differently again.

## [v0.5.123] - 2026-11-01

### Fixed

- Exporting with tracks mixed together no longer runs out of memory on long recordings, which crashed PolyRec. It used to hold every mixed track whole in memory (about 2.7 GB for a 30-minute, three-track mix) and the whole video on top of that. Now it uses about 100 MB whatever the length, and it is faster too: a 30-minute three-track mix took 12 s instead of 37 s. The mixed audio comes out the same as before.

## [v0.5.122] - 2026-11-01

### Added

- **Close: Quit / Close: To tray** in the menu bar chooses what the window's close button does. With **To tray**, closing hides PolyRec in the notification area so the hotkeys keep working, and a short message says it's still running; **Quit PolyRec** in the tray menu quits. The default is unchanged: closing quits.

## [v0.5.121] - 2026-11-01

### Fixed

- A recording started within about 10 seconds of opening the app list in **🔊 Audio** (**+ Add app**) could come out with no video. Listing the installed apps left a background thread set up in a way that video capture, started on the same thread, couldn't use. Highlight buffering could lose its video the same way.

## [v0.5.120] - 2026-11-01

### Fixed

- A recording whose source went away before you pressed Stop is saved again. If the recorded window closed, its display was disconnected, capturing it failed, or an audio device was unplugged, Stop used to never finish saving the file, and quitting PolyRec then lost it. Now the video holds its last picture (or goes black if capturing failed) and the lost audio track goes on as silence, so the file saves as usual. PolyRec says what was lost once the file is saved. Highlight buffering had the same problem and is fixed too.
- An audio track whose capture kept failing (for example while the Windows audio service restarted) no longer stops the whole recording from saving.

## [v0.5.119] - 2026-11-01

### Fixed

- PolyRec no longer disappears when the graphics driver is updated or reset, for example by Windows Update or by a game that crashes the GPU. It now closes the normal way, saving a recording that was running, and starts again by itself, minimized so it doesn't pull you out of a game. Before, it just vanished, and if the reset came while it was drawing it crashed outright, without saving the recording.

### Added

- PolyRec restarts itself after an unexpected error too, and says so when it comes back. If it fails again within a minute of restarting, it stays closed.
- `crash.log` in `%APPDATA%\PolyRec` records what went wrong whenever PolyRec fails. Before, a failure left nothing behind to say why.

## [v0.5.118] - 2026-11-01

### Fixed

- Starting or stopping a recording no longer takes focus away from the game or app you're in. The overlay and the hotkey confirmation ("● Recording", "⏹ Stopped, saving") could grab focus as they appeared, most noticeably with PolyRec's window minimized, leaving the taskbar or PolyRec in front so you had to switch back to the game before you could play on.

## [v0.5.117] - 2026-10-31

### Fixed

- A video encoder that won't take PolyRec's keyframe spacing now gets a keyframe forced every 2 seconds. Before, PolyRec assumed it had taken the spacing, so at the full frame rate keyframes could end up far apart, which makes seeking and Highlight cuts coarse.
- Quitting no longer leaves PolyRec running in the background if saving the last recording hangs. It now waits for the save for at most 30 seconds. A normal save takes well under a second. Starting PolyRec again while the last copy is still saving now opens a new window. Before, the new launch handed off to the closing copy, so nothing appeared.

## [v0.5.116] - 2026-10-31

### Fixed

- Launching PolyRec again while it's running, say when it's hidden in the tray, now brings the running one up instead of starting a second copy. A second copy registered every hotkey again, added a second tray icon, and saved over the same settings.
- **Start recording** from the tray menu now brings the window up if the recording doesn't start, so you can see why. Before, an error showed only inside the hidden window.
- The start/stop hotkey now shows a "Recording didn't start" toast when it doesn't start one, for example because nothing recordable is in front or the last recording is still being saved. Before, nothing happened.
- While hidden in the tray, the window no longer redraws itself during a recording with the overlay on. Only the overlay is drawn.

## [v0.5.115] - 2026-10-31

### Fixed

- PolyRec no longer crashes when the overlay or a hotkey's toast has to come up while the window is hidden in the tray, for example when starting a recording from the tray with the overlay on, or pressing any hotkey. This affected v0.5.109 to v0.5.114. The cause is a gap in eframe, the UI framework PolyRec uses: it can't open a new window while the app's own window is hidden. Now the overlay or toast comes up a moment later instead.

## [v0.5.114] - 2026-10-31

### Fixed

- **Keep frames on the GPU (experimental)** is sturdier:
  - A frame that can't be copied on the GPU, say because a game is using most of the video memory, is now skipped. It used to end the video for the rest of the recording, with no message.
  - Starting a recording, or Highlight following a new window, no longer freezes the window for up to half a second. The GPU device is set up once per run, in the background. The check of whether the encoder takes frames this way is now part of starting the recording itself, on the recorder's own thread.
  - If the encoder doesn't take frames this way, or stops taking them partway through, PolyRec now reads them back the usual way and the recording goes on. That check used to stop short of writing a frame, and a frame turned down mid-recording was lost.
  - At most 12 frames now wait for the encoder at once, about 180 MB of video memory at 1440p. It used to be up to 68, about 1 GB, if the encoder fell behind.
  - A recording whose frame rate the encoder turns down now steps down to a lower rate it does take. It used to drop back to copying frames through system memory instead.

## [v0.5.113] - 2026-10-31

### Fixed

- A finished recording or Highlight save no longer replaces an existing file with the same name. Two recordings of the same app finishing in the same second, or two saves numbered with a custom prefix at once, used to get the same name, and the second one replaced the first. The second now gets the next free name, such as `app_2026-10-31-12-00-00_2.mp4` or the next number.
- A new recording can't start until the last one has been saved, whether from REC, the tray or the start/stop hotkey. Starting one meanwhile used to lose the save's result, including any notice about it, such as a missing audio track. REC's tooltip says why while it waits, which is usually under a second.

## [v0.5.112] - 2026-10-31

### Fixed

- The last change before a window goes still is now always recorded. Recording at a lower frame rate than the display refreshes (60 fps on a 144 Hz monitor, say), a change that came too soon after the frame before it was dropped. The recording then kept showing the picture before it for as long as the window stayed still. Changes made while a recording is paused now show up as soon as it resumes, too, instead of the picture from before the pause.
- A still window can no longer record black throughout when starting the capture takes a moment. The black frame sent while waiting for the window's first picture made that picture count as too soon, and dropped it.

## [v0.5.111] - 2026-10-31

### Added

- **Keep frames on the GPU (experimental)**, a new checkbox in **⚙ Quality**, off by default. With the hardware encoder, each captured frame goes to the encoder as a GPU texture instead of being copied through system memory, and the encoder does any scaling. Recording a 60 fps display took about 73% less CPU in testing (231 → 62 ms of CPU per second). If the encoder doesn't take textures, the recording falls back to the usual way.

## [v0.5.110] - 2026-10-31

### Fixed

- Stopping a recording, or Highlight buffering, no longer waits on the dashboard's own thread for the recorder to finish. If the recorder ever stalls again, as it did on a still window before v0.5.108, PolyRec stays responsive and the save finishes once the recorder does, instead of the whole app freezing on Stop.

## [v0.5.109] - 2026-10-31

### Added

- A notification-area (tray) icon. Clicking it brings PolyRec up. Its menu hides or shows the window, starts recording the source selected in the list or stops the recording, and quits. Hovering over it shows whether a recording is running. Closing the window still quits.

## [v0.5.108] - 2026-10-31

### Fixed

- Recording a window whose picture doesn't change, such as an idle app, or a minimized window, no longer freezes PolyRec on Stop. Windows hands out no new frames for such a window, so the recording got no video at all: nothing was saved, and the dashboard stopped responding when stopped. The last frame is now sent again four times a second while nothing changes, and a minimized window records as black.

## [v0.5.107] - 2026-10-31

### Fixed

- With the dashboard minimized, a global hotkey did nothing until the dashboard was restored, unless the overlay HUD happened to be up. Start, pause, stop and Highlight save now work right away, with the overlay HUD and the hotkey toast appearing as usual.

## [v0.5.106] - 2026-10-31

### Fixed

- Recordings of a mostly still screen now have a keyframe at least every two seconds, where the encoder alone placed one every ten seconds or more. Seeking in a player is quicker, and a Highlight save's start lands closer to the requested length. At the full frame rate nothing changes (one keyframe a second, none added).

## [v0.5.105] - 2026-10-31

### Added

- A Recent list under the output folder shows the last three recordings and Highlight saves, with buttons to play one or show it in Explorer.

### Fixed

- After a recording, the REC button no longer overlaps the panel above it. The panel scrolls instead.

## [v0.5.104] - 2026-10-31

### Added

- Live audio level meters while recording, one per track and labelled with its source, so a silent mic or a muted app shows up while there's still time to fix it rather than in the finished file. The meters read on a dBFS scale and turn amber above -12 dB and red near clipping.

## [v0.5.103] - 2026-10-31

### Added

- A short on-screen confirmation after each global hotkey ("● Recording", "⏸ Paused", "▶ Resumed", "⏹ Stopped, saving", the overlay toggle, and the Highlight save), shown for 1.5 s at the top of the monitor you're working on while the dashboard isn't in front. It's click-through.

### Changed

- The overlay HUD and the new confirmations are excluded from screen capture, so a display recording no longer contains the HUD.

## [v0.5.102] - 2026-10-31

### Changed

- Warnings that don't stop anything now appear as a dismissible strip at the top of the Status panel instead of a red "Error" window that had to be clicked away. That covers a recording running below its frame rate (which used to pop up mid-recording), a recording with fewer audio tracks than selected, a disk-full early stop (the file is still saved), Highlight not being on when its hotkey is pressed, and the update notices. Real errors still use the window.
- The frame-rate warning now says what it measured ("Recording at 10 of 60 fps so far") and that a still screen records fewer frames, instead of only blaming the hardware.

## [v0.5.101] - 2026-10-31

### Added

- An Open button next to the output folder opens it in Explorer: the `polyrec` folder recordings land in once it exists, otherwise the chosen folder itself.

### Changed

- The output folder is checked as soon as it's set (and at startup). If its drive or network share isn't available, or the path runs through a file, a warning says so and REC is disabled with the reason. Before, this only surfaced as an error after pressing REC.

## [v0.5.100] - 2026-10-31

### Changed

- While recording, the status line shows the frame rate achieved so far and the file size so far ("2 tracks · 58 fps · 120 MB") instead of a raw frame count. Its tooltip explains that a still screen records fewer frames per second than the setting.

## [v0.5.99] - 2026-10-31

### Added

- The hotkeys for what the recording buttons do right now appear under them. While idle, the hint says the start key records the window in front, which is what the global hotkey does.

### Changed

- REC is disabled with a hint until a source is selected; it used to do nothing when clicked.
- While paused, Stop sits next to Resume, so stopping no longer needs a Resume first.
- The overlay HUD stays up while paused, showing ⏸ and the resume key. It used to disappear, which looked the same as the recording having stopped.

## [v0.5.98] - 2026-10-31

### Fixed

- A recording with desktop or app audio stalled whenever that audio was silent: nothing reached the file, and pressing Stop sat on "Saving recording…" for minutes. Loopback capture delivers no data while nothing plays, which stalled the whole recording behind that track. Those stretches are now recorded as silence.

## [v0.5.97] - 2026-10-31

### Changed

- While recording, the dashboard repaints at 1 fps instead of 30 when it's minimized or another window has focus (typically the game being recorded). The timer still updates every second, and the UI no longer competes with the game for the GPU just to animate the pulsing dot. With the overlay HUD on, it keeps the full rate so the HUD stays on top of fullscreen games.

## [v0.5.96] - 2026-10-31

### Fixed

- The audio track count shown while recording (status panel and overlay) is now the recording's own. It used to follow the live audio checkboxes, so it changed if a box was toggled mid-recording, and it counted an app source as one track however many it actually records (one per running process, none while the app is idle).

## [v0.5.95] - 2026-10-31

### Fixed

- The capture source list now follows window title changes. A browser that switched tabs, or a launcher showing progress, used to keep the title its window had when it first appeared in the list.

## [v0.5.94] - 2026-10-31

### Added

- FPS presets 120, 144 and 240, alongside 30 and 60. Some presets are greyed out, with the reason on hover:
  - a rate above the selected source's display refresh rate;
  - a rate this PC's encoder can't handle for the chosen codec and resolution, e.g. H264 at 240 fps in 1440p.

  A recording set higher than its source allows steps down to the highest rate that works, and the Quality popup says which rate that will be.
- Source cards are now exposed to screen readers as selectable buttons.

### Changed

- Recordings run at the frame rate set in Quality settings. Previously every frame Windows delivered was recorded, up to the display's refresh rate: on a 144 or 240 Hz display, a "60 fps" recording captured, copied and encoded every refresh. Measured on a 1440p 240 Hz display, a 60 fps capture now uses 62% less CPU.
- Quality settings now list Codec, Encoder and Resolution before FPS, since the available frame rates depend on them.

## [v0.5.93] - 2026-10-31

### Changed

- The UI typeface is now IBM Plex Sans, with IBM Plex Sans JP for Japanese, replacing egui's Ubuntu Light and the MS Gothic fallback read from `C:\Windows\Fonts` at startup. Both fonts are bundled in the exe, so the UI looks the same on every machine. They ship unmodified under the SIL Open Font License 1.1, whose text is included as `LICENSE-IBM-Plex.txt`.

## [v0.5.78] - 2026-10-29

### Fixed

- Windows and app-audio sources belonging to elevated or protected processes (e.g. a game running under anti-cheat, Task Manager, the Windows audio engine) showed up as "Unknown" with no icon -- their exe path is now resolved with the minimal query rights Windows still grants for them.
- Each recording leaked a small allocation per audio device track (the WASAPI mix format was never freed).

## [v0.5.42] - 2026-07-20

### Changed

- Export track checkboxes are now grouped by app/device instead of one checkbox per underlying track -- a multi-process app (e.g. Discord with more than one live audio session at once) previously showed as several identically-named checkboxes for what's really one audio source. Same-app tracks are now automatically merged into a single output track when exported, while other selected sources stay independently separate -- distinct from (and unaffected by) the existing "Mix into one track" checkbox, which still mixes everything checked together when turned on.

## [v0.5.41] - 2026-07-17

### Fixed

- "Mix into one track" export (added in v0.5.40) could take as long as the original recording itself, even for short clips -- its sink writer left Media Foundation's real-time write throttling enabled, which paces `WriteSample` to 1x playback speed whenever an actual encoder is involved (as opposed to a plain track-selection export, which never transcodes anything). A ~5.5 minute recording that mixed and decoded in 1.3s then took 15+ minutes to actually write. Disabling the throttle brings that down to a couple of seconds.

## [v0.5.40] - 2026-07-17

### Added

- Export now has a "Mix into one track" checkbox -- combines the checked audio tracks into a single track instead of keeping each separate, for platforms like YouTube that only ever play one audio track from an uploaded file and silently ignore the rest.
- The export Save dialog now opens pre-filled at `<output dir>/polyrec/exported/<app name>_<timestamp>.mp4` (auto-created) instead of an unrelated default location and a generic "export.mp4" name -- still fully overridable in the dialog.

## [v0.5.39] - 2026-07-17

### Added

- A warning now shows after a recording finishes if fewer audio tracks made it into the file than were selected (e.g. a muted mic, a disconnected device, or an app that produced no sound) -- previously a source that never produced a single real audio buffer was silently dropped from the finished file with no indication anything was missing.

## [v0.5.38] - 2026-07-16

### Changed

- A recording with multiple audio tracks (e.g. mic + system audio) could sound completely silent in a player that only plays the first audio track by default -- Media Foundation doesn't guarantee the finished file's physical track order matches recording order, so which track ended up "first" was unpredictable. Selected devices are now ordered with system/loopback audio (what most people actually want to hear by default, e.g. game sound) ahead of the mic before recording starts -- app-audio sources already tend to land after device tracks in practice (activating one needs an async setup step a device capture doesn't), so this is the one remaining lever that reliably helps. Not a hard guarantee for every case, but meaningfully improves the odds the default track is the one you want.

## [v0.5.37] - 2026-07-15

### Fixed

- The export dialog's per-track labels (added in v0.5.34) desynced from the actual tracks whenever an app-audio source had more than one process -- e.g. Discord's main + helper processes, or two windows of the same exe -- since one recording track gets created per process id, not one per app-audio source. Labeling built only one label per source regardless, so every label after the first multi-process source landed on the wrong track: a real app-audio track could show a generic "Track N" fallback (looking like it wasn't captured at all) while its actual name ended up on an unrelated track. Labels now repeat once per process id, matching how the tracks are actually created.

## [v0.5.36] - 2026-07-15

### Fixed

- The "N tracks" counter shown while recording (and in the overlay HUD) only ever counted checked system audio devices -- checked app-audio sources (e.g. a game captured via per-app loopback) never added to it, so recording with an app-audio source selected always undercounted. Both now count devices and app-audio sources together, matching the audio popup's own "{n} selected" summary, which already did this correctly.

## [v0.5.35] - 2026-07-15

### Changed

- Clicking an available update no longer refuses outright just because Highlight buffering was running -- unlike a manual recording (still blocked, in-progress footage would be lost), buffering is just a rotating background buffer nothing was explicitly started for. It's now stopped and disabled automatically, with a notice popup explaining why, before the update confirmation dialog opens.

## [v0.5.34] - 2026-07-15

### Fixed

- The post-recording export track-selection checkboxes listed whatever's in the live system audio-device list, not what the recording actually used -- app-audio sources (e.g. a game captured via per-app loopback) never appeared at all, since they aren't system devices, and with 2+ devices installed the checkbox shown for a given track could easily be the wrong device's name. Unchecking a track to exclude it from export could silently do nothing meaningful, or exclude the wrong track. The export dialog now labels each checkbox from what was actually selected for that specific recording (devices, then app-audio sources, matching track order), falling back to a generic "Track N" label only if the file ends up with more tracks than were selected (e.g. a device disconnecting mid-recording).
- Windows draws its own colored border around a capture target while its window has focus. Highlight buffering runs a capture session continuously in the background whenever enabled, with no recording actually started, so seeing this border made it look like a manual recording was in progress. Manual recordings still show it; highlight buffering now suppresses it (Windows 11 24H2+; on older Windows there's no way to suppress it, so the border still shows during highlight buffering there).

## [v0.5.33] - 2026-07-14

### Fixed

- A hand-edited `config.toml` with `fps = 0` crashed on the very first frame of every recording (division by zero computing per-frame duration). The UI itself only ever writes 30 or 60, so this only hit users who edited the file directly, but recovery meant deleting the config -- `fps` is now clamped to `[1, 240]` the same way `resolution_mode`/`bitrate_mode`/`encoder_mode` already guard their own raw config inputs.

## [v0.5.32] - 2026-07-14

### Fixed

- The **⚙ Quality** popup's scroll region was capped too low (400px) -- even the default configuration (Native resolution, Auto bitrate) already clipped the Auto/Manual bitrate choice and the entire Highlight section, with no visible scrollbar to hint more was there. Raised to 520px so the common cases fit without scrolling; only the rare combination of Custom resolution + Manual bitrate + Highlight all expanded at once still needs it.

## [v0.5.31] - 2026-07-14

### Changed

- The "+ Add app" search picker no longer scans the Start Menu on the UI thread -- opening it now shows currently-open windows immediately, with installed-but-not-running apps appearing a moment later (with a "Still searching installed apps…" note in between) instead of the whole popup briefly freezing while every shortcut resolves.
- Picker rows now show each app's icon, and a green "● Running" tag on candidates that are currently open -- easier to scan a long list, and clear at a glance which ones will start capturing immediately versus once launched.

## [v0.5.29] - 2026-07-13

### Fixed

- The "+ Add app" search picker only searched apps currently holding a live WASAPI audio session -- an app that wasn't open, or was open but not actively producing sound at that moment (e.g. a voice chat app nobody's talking in yet), wouldn't show up at all. It now also searches installed apps by resolving every Start Menu shortcut to its target exe, so an app can be found and pinned without needing to launch it first just to make it discoverable. (Electron/Squirrel apps like Discord and Slack point their shortcut at an updater stub rather than the real exe -- resolved through to the actual versioned exe rather than pinning the stub, which would've silently done nothing.)

## [v0.5.28] - 2026-07-13

### Changed

- The Applications audio list is now entirely curated, not auto-populated -- a fresh install shows nothing here but "+ Add app" regardless of how many apps happen to be making sound, and only apps you've explicitly added ever appear. The checkbox itself is the only pin/unpin control: checking an app in persists it across restarts, and unchecking it removes it immediately -- replaces v0.5.27's separate "📌 Pin"/"×" buttons and its auto-persist-on-check behavior with this single control.
- "+ Add app" now opens an in-app search box listing currently running apps by name, instead of going straight to a file browser -- picking one needs no knowledge of where it's installed. A "Browse for .exe instead…" fallback remains for pinning an app that isn't running yet.

## [v0.5.27] - 2026-07-13

### Changed

- Checking any app in the Applications audio list pinned it too, not just apps added via "+ Add app" -- it stayed selected across restarts even after closing, the same way an explicitly-browsed-to app already did. Unchecking alone didn't un-pin; only a new "×" button removed it.

## [v0.5.26] - 2026-07-13

### Added

- Apps can now be pinned to the Applications audio list via a new "+ Add app" button in **🔊 Audio** -- pick any `.exe`, even one that isn't running yet, and it shows greyed with its default volume until it launches, at which point it starts capturing automatically without needing to be re-checked. A small "×" on a pinned row un-pins it.

### Changed

- Two independent instances of the same app (not parent/child -- e.g. two separate game or app windows) now share a single checkbox and volume slider in the Applications list instead of showing as two separate entries. Each instance is still captured as its own audio track under the hood (Windows' Process Loopback Capture only targets one process per stream), so the recording gets one track per running instance, just no longer split across separate UI rows to manage.

## [v0.5.25] - 2026-07-13

### Added

- "Entire screen" capture: every connected display now appears as its own selectable capture source (e.g. "🖥 Display 1 (Primary) (2560×1440)"), listed above windows in the source panel. Captured via `Windows.Graphics.Capture`'s per-monitor capture item, same backend as window capture.

### Changed

- "App audio only" is disabled (with an explanatory tooltip) when a monitor/display source is selected -- a whole-screen recording has no single owning process to scope loopback audio to, unlike a window source.

## [v0.5.24] - 2026-07-13

### Changed

- Audio device/app selection moved out of the always-visible source panel into a new **🔊 Audio** popup, matching the existing Quality/Hotkeys pattern -- it's set-once-per-session like those, not something referenced on every recording the way the source list is. The source panel is now just the capture-source list, full height. A "{n} selected" (or "No audio selected") caption under the settings buttons shows the current selection at a glance without opening the popup.

## [v0.5.23] - 2026-07-13

### Changed

- AUDIO (SYSTEM + APPLICATIONS) is now pinned to the bottom of the source panel, always fully visible and never scrolled -- readable at a glance without hunting for it, since the set of system devices and currently audio-active apps is normally small. Only the capture-source list scrolls now, restoring a single scroll region (multiple independently-scrollable regions in one panel is a documented accessibility problem: keyboard/switch users can't reliably tell which region has scroll focus, and screen magnifier users can miss content cropped by an inner region's own boundary) while still keeping audio controls from being pushed out of view by a long source list.
- Split the dashboard UI code out of one 2450-line `mod.rs` into focused modules (per-panel render functions, shared widgets, theme tokens, background polling, and user actions) -- no behavior change, just following the same pattern the Hotkeys popup already used.

## [v0.5.22] - 2026-07-12

### Changed

- Reorganized the source panel's AUDIO section: SYSTEM (Speakers/Microphone) and APPLICATIONS are now both subsections under one AUDIO heading, in a smaller font than top-level section headers, since both are audio *inputs* just scoped differently. SYSTEM always shows every device at once (sized to fit all of them, since the device list is effectively fixed for the process's lifetime) rather than the fixed small cap it had before.
- Each panel section (capture sources, SYSTEM, APPLICATIONS) now scrolls independently instead of the whole panel sharing one scroll area, so every section header stays reachable without having to scroll past a long source list first.
- A checkbox's volume slider now appears on its own row underneath, indented, instead of stacking sideways -- unchanged from the original per-device slider design, just also applied to the new APPLICATIONS list. Toggling a checkbox never resizes the panel: each section's scroll area reserves a fixed height regardless of how many rows are currently expanded.
- Volume sliders are capped at 100% (down from 200%) -- this is a recorder, not a mixing console, so only attenuation is offered, not boost.

## [v0.5.21] - 2026-07-12

### Added

- Individual running applications with sound (Discord, Spotify, a game, etc.) can now be selected as their own independent audio recording track, in a new "APPLICATIONS" section below AUDIO in the source panel. Each entry gets the same per-source volume slider as physical audio devices. Captured via the same Process Loopback Capture mechanism "App audio only" already uses, just targeting an arbitrary app instead of requiring it to also be the video capture source -- the two are independent and can be combined.

### Fixed

- The source panel's three lists (capture sources, audio devices, per-app audio) each had their own capped, independently-scrolling area; nesting them meant the mouse wheel always scrolled whichever one the cursor was over, so a section pushed past the window's bottom edge (most easily hit with the new APPLICATIONS list on a machine with several sound-producing apps open) had no way to be reached, and could visually overlap the section after it. Replaced with a single scroll area for the whole panel.

## [v0.5.20] - 2026-07-11

### Fixed

- With two audio devices checked, the second device's volume slider could be clipped by the AUDIO section's own internal scroll boundary (capped at 140px, tuned before sliders existed -- two checked devices with sliders need ~160px) while its checkbox label stayed visible above the cut. Cap raised to 180px; default window height raised 680 -> 720 to keep "App audio only" comfortably clear of the bottom edge with the larger AUDIO section.

## [v0.5.19] - 2026-07-11

### Added

- The window reopens wherever it was last left instead of the OS's default placement -- saved on close (including the close a self-update triggers, so it survives updating too), restored on next launch.

## [v0.5.18] - 2026-07-11

### Fixed

- The left panel's "App audio only" checkbox (and, with both audio devices checked, the Microphone volume slider above it) could be cut off below the default window's bottom edge -- each checked device's volume slider (v0.5.16) grew the AUDIO section, and its internal scroll area reserves its full capped height once content exceeds it rather than shrinking to fit. Default window height increased 600 -> 680.

## [v0.5.17] - 2026-07-11

### Fixed

- The per-device volume slider (added in v0.5.16) only persisted a change to config.toml when an actual pointer-drag ended, so a value change from anywhere else (keyboard, assistive tech) could silently never be saved -- found via a live restart-and-check. Now saves on any confirmed value change.

## [v0.5.16] - 2026-07-11

### Added

- Per-audio-device recording volume (0-200%, default 100%) -- a slider next to each checked device in the source panel. Applied to that device's captured samples before encoding, independent of the device's actual system volume, so it doesn't change what you hear live, only what's recorded. Persisted per device across restarts. Set before pressing REC, like Quality/Hotkeys; not adjustable live mid-recording.

## [v0.5.15] - 2026-07-11

### Fixed

- Closing the window being recorded mid-recording used to spin the video capture loop forever (retrying ~1000 times/sec with no logging) instead of stopping -- audio kept recording fine, but video silently died. Now detected via Windows.Graphics.Capture's own "item closed" signal and stops that capture cleanly with a single warning log.

## [v0.5.13] - 2026-07-11

### Fixed

- The Stop/Pause buttons shown while recording still weren't actually centered under REC despite v0.5.12's fix -- that attempt centered the row within an oversized fallback area (the layout's full remaining space, in a bottom-up panel) rather than a properly reserved slot, which also nudged REC itself up from its usual spot. Both REC and Stop/Pause now reserve an exact-sized, correctly stacked slot before centering within it.

## [v0.5.12] - 2026-07-11

### Fixed

- The Stop/Pause buttons shown while recording were left-aligned instead of sharing the REC button's centered position, and the extra "State: Idle" hint line (shown only when idle) shifted the whole action row up or down relative to Recording/Paused -- both now sit at the same centered position regardless of state.

### Changed

- Recording start/stop/finalize now log at info level (source, audio track count, output path) instead of being silent on success -- previously only failures were logged, so there was no way to reconstruct a recording session's timeline from the log alone.

## [v0.5.11] - 2026-07-11

### Fixed

- Unplugging or disabling an audio device (microphone/speakers) mid-recording used to spin retrying forever with a warning logged every 10ms while that track silently went dead; it's now detected and stops that track cleanly with a single error log, leaving other tracks/video unaffected.

## [v0.5.9] - 2026-07-10

### Fixed

- A rare failure building a capture worker's per-thread Tokio runtime (video/audio, manual recording or Highlight) now logs and stops that worker cleanly instead of panicking its OS thread.

## [v0.5.1] - 2026-07-10

### Security

- Release binaries (portable exe and installer) are now Authenticode-signed in CI via SignPath Foundation's free open-source code signing program. SmartScreen reputation still builds up gradually rather than disappearing instantly (OV-tier certificate, not EV).

## [v0.5.0] - 2026-07-10

### Added

- Clicking the "update available" menu bar button now performs the update directly instead of just opening the release page: downloads and verifies the new version, then either swaps the running app in place (portable zip) or silently re-runs the installer (installed copies), restarting automatically. Blocked while a recording or Highlight buffering is active.

## [v0.4.1] - 2026-07-10

### Added

- Hotkey-started recordings now default to "App audio only" checked, persisted as a setting (changeable via the checkbox itself) instead of always starting unchecked on every launch.

### Fixed

- Pausing the Highlight buffer for a manual recording, then resuming afterward, left the paused session's segment files orphaned on disk forever -- untracked by the resumed session, never cleaned up. Starting (or resuming) Highlight buffering now clears any leftover files from a prior session first.

## [v0.4.0] - 2026-07-10

### Added

- Highlight buffer: continuously captures the foreground app in the background, and a hotkey (default F10) saves the last N seconds (configurable 30-300s, default 120s) to a file on demand -- no need to have pressed record beforehand. Enable it in Quality settings. Follows foreground-window switches automatically, pauses whenever a manual recording is active, and stops itself gracefully if free disk space runs low.

## [v0.3.8] - 2026-07-10

### Fixed

- Stopping a recording relied on aborting an internal channel-pump task as an indirect way of making the capture threads notice and exit -- worked today only as a side effect, with no direct signal reaching the capture loops themselves. Capture threads now check an explicit stop flag every loop iteration, the same pattern already used for pause.

## [v0.3.7] - 2026-07-09

### Added

- Free disk space on the selected output drive is now shown below the output-directory field, refreshed every few seconds (or immediately when the drive changes). Turns a warning color when below the 500 MB threshold the recording pipeline itself refuses to start/continue under.

## [v0.3.6] - 2026-07-09

### Note

- v0.3.4 and v0.3.5 both have no published release binary. v0.3.4's release was created with the wrong target commit; v0.3.5's release upload failed after being manually pre-created instead of left for the workflow to create. GitHub's immutable-releases setting permanently blocks reusing a tag name for a release once one has existed for it, even after deleting the broken release -- confirmed by testing on v0.3.5. Both git tags are otherwise correct; this release is created by a plain tag push only (no `gh release create` beforehand), letting `action-gh-release` create the release and upload assets atomically, same as every other working release in this project's history.

## [v0.3.5] - 2026-07-09

### Changed

- CHANGELOG.md version headings and their link-reference definitions now use `vX.Y.Z` (matching the actual git tags) instead of bare `X.Y.Z`.

### Note

- v0.3.4 has no published release binary -- its release was accidentally created with the wrong target commit, and GitHub's immutable-releases setting permanently blocks reusing a tag name for a release once one has existed for it, even after deletion. The `v0.3.4` git tag itself is correct; skip straight to v0.3.5 for a working release.

## [v0.3.4] - 2026-07-09

### Fixed

- A `config.toml` that exists but fails to parse now logs a warning before falling back to defaults, instead of silently resetting settings with no trace of why.

## [v0.3.3] - 2026-07-09

### Fixed

- The menu bar's Refresh button no longer resets the export track-selection checkboxes to match the live audio device count -- that state is tied to the last finished recording's actually-probed track count, not the current device list.

### Added

- Hover feedback on capture source cards in the source list (previously only the selected card had any visual distinction).

## [v0.3.2] - 2026-07-09

### Changed

- The export track-selection UI is now shown inline in the status panel instead of a popup window -- the popup used to sit visually on top of the REC button, blocking a new recording from being started while it was up. Starting a new recording now simply replaces the export UI with the live recording status, same as always.
- Export track checkboxes now reflect the audio tracks actually present in the finished recording file (probed directly from it), not just whatever was selected before recording started -- catches the case where a selected device didn't end up producing a track.
- The export button (and track checkboxes) are hidden entirely when a recording has fewer than 2 audio tracks, since there's nothing a track-selection export could meaningfully remove in that case. "Open Folder" remains available either way.

## [v0.3.1] - 2026-07-09

### Fixed

- The recording overlay HUD sometimes didn't appear over fullscreen games, or took several tries. Two compounding bugs: (1) hotkey events queued from the low-level keyboard hook's background thread never woke egui's own idle event loop, so the recording state (and the overlay it gates) sometimes didn't update until something else happened to trigger a repaint; (2) the overlay window only asserted itself as topmost once, at creation, and fullscreen games commonly re-assert their own topmost z-order afterward, silently demoting the overlay behind them.

## [v0.3.0] - 2026-07-09

### Fixed

- Global hotkeys (start/stop, pause, toggle overlay) now use a low-level keyboard hook (`WH_KEYBOARD_LL`) instead of `RegisterHotKey` — fixes hotkeys not firing while a game is running in exclusive fullscreen mode, a known limitation of `RegisterHotKey`-based global hotkeys that other recording tools work around the same way.

## [v0.2.8] - 2026-07-08

### Added

- `cargo audit` now runs in CI on every PR/push, catching known-vulnerable dependencies automatically.

### Changed

- Migrated to Rust edition 2024.
- Split `dashboard.rs` (1471 lines) into `dashboard/mod.rs` + `dashboard/hotkeys_popup.rs` for maintainability; no behavior change.

## [v0.2.7] - 2026-07-08

### Fixed

- The recording overlay's stop-hotkey hint showed a hardcoded "F9" regardless of the actually configured start/stop binding; now reflects the real key (or key combination).

## [v0.2.6] - 2026-07-08

### Added

- Hotkeys are now bound by pressing the actual key combination you want (optionally holding Ctrl/Alt/Shift), instead of picking from a fixed grid of F1-F12 buttons — supports any letter, digit, F-key (F1-F24), or common navigation key, alone or combined with modifiers.
- If a captured key combination is already in use by another running application or reserved by Windows, `RegisterHotKey` is tested immediately and a warning is shown inline instead of silently failing later.

## [v0.2.5] - 2026-07-08

### Fixed

- Migrated windows-rs 0.58 → 0.62.2 and egui/eframe 0.29 → 0.35.0 (PR #12) — both had accumulated real breaking API changes; see the PR for the full list (PROPVARIANT restructuring, HGDIOBJ conversions, eframe's `App::ui` replacing `update`, egui's `CornerRadius`/`Panel` changes).

## [v0.2.4] - 2026-07-08

### Added

- Windows installer (`polyrec-vX.Y.Z-windows-x64-setup.exe`), built via Inno Setup in CI and published alongside the existing portable zip — Start Menu shortcut, Add/Remove Programs entry, English/Japanese installer UI. Covered by the same `SHA256SUMS.txt` and build provenance attestation as the zip.
- `CONTRIBUTING.md`, `SECURITY.md`, issue templates, and `dependabot.yml`.

## [v0.2.3] - 2026-07-08

### Added

- `SHA256SUMS.txt` published alongside every release zip.
- Build provenance attestation (`actions/attest-build-provenance`) on the release zip — ties the artifact to the exact repo/workflow/commit that produced it, verifiable with `gh attestation verify`.

### Security

- Enabled GitHub's immutable-releases setting for this repo — published release assets can no longer be replaced in place; any change requires deleting and recreating the release entirely.

## [v0.2.2] - 2026-07-08

### Changed

- Refactored `App::update()` (previously ~800 lines) into one `render_*` method per panel/popup (menu bar, source panel, center panel, overlay HUD, Quality/Hotkeys popups, error banner, export dialog), plus a `poll_background_work()` method for non-rendering state polling. Pure reorganization, no behavior change.
- Deduplicated the audio-device checkbox icon (🔊/🎙) selection into a shared `audio_device_icon()` helper.

## [v0.2.1] - 2026-07-08

### Added

- Selectable display language (English / Japanese) via a toggle in the menu bar, persisted in `config.toml` and applied immediately without a restart.

### Fixed

- Video encoding was unconditionally forced to software-only — including for real recordings, not just the test suite — missing available GPU hardware encoders (NVENC/QSV/AMF) entirely. This was very likely the cause of recording visibly stealing frame time from whatever else was running (e.g. a game) at 1080p60+.
- The capture pipeline recreated its GPU staging texture on every single frame instead of reusing one, and copied the captured frame buffer an extra time per frame even in the common case where no resolution scaling was needed.

### Security

- Pinned all GitHub Actions in CI workflows to commit SHAs instead of mutable version tags, and bumped each to its latest stable release.

## [v0.1.1] - 2026-07-08

First tagged release. Covers the initial feature set plus disk-space handling:

### Added

- Window capture via Windows.Graphics.Capture, defaulting to the captured window's own native resolution.
- Multi-track audio: independent system-loopback and microphone tracks, with "app audio only" scoping via the Process Loopback Capture API.
- Pause / resume without cutting the recording.
- Global hotkeys for start/stop, pause, and overlay toggle, rebindable in-app.
- Quality settings: FPS, codec (H265 with automatic H264 fallback), resolution mode, and bitrate.
- Post-recording export: remux with only the selected audio tracks, no re-encoding.
- GitHub Releases-based update check on launch.
- Disk-space monitoring: refuses to start a recording with less than 500 MB free, and stops an in-progress recording gracefully (finalizing normally) if free space drops below that threshold mid-recording.
- Release CI/CD: tag-triggered build and publish to GitHub Releases.

### Changed

- Compact window layout and larger section-header type.

### Fixed

- Window titlebar icon not matching the desktop shortcut icon.
- Silent audio caused by MP4 container physical track order not following `AddStream()` call order.
- Video quality regression from forced upscaling to display resolution.
- CJK text (window titles, exe names) rendering as tofu boxes without a fallback font.

### Security

- Fixed a signed/unsigned integer cast bug in exe icon extraction (a negative `bmHeight` for top-down DIBs could reinterpret as a huge buffer size).
- Fixed an NTLM-hash-leak vector: a network-sourced update URL passed to `explorer.exe` is now validated to start with `https://github.com/` before opening, closing off a UNC-path (`\\host\share`) SMB-credential-leak technique.
- Removed an unused dependency carrying a known RustSec advisory.

[Unreleased]: https://github.com/yusukensanta/polyrec/compare/v0.5.1...HEAD
[v0.5.1]: https://github.com/yusukensanta/polyrec/compare/v0.5.0...v0.5.1
[v0.5.0]: https://github.com/yusukensanta/polyrec/compare/v0.4.1...v0.5.0
[v0.4.1]: https://github.com/yusukensanta/polyrec/compare/v0.4.0...v0.4.1
[v0.4.0]: https://github.com/yusukensanta/polyrec/compare/v0.3.8...v0.4.0
[v0.3.8]: https://github.com/yusukensanta/polyrec/compare/v0.3.7...v0.3.8
[v0.3.7]: https://github.com/yusukensanta/polyrec/compare/v0.3.6...v0.3.7
[v0.3.6]: https://github.com/yusukensanta/polyrec/compare/v0.3.5...v0.3.6
[v0.3.5]: https://github.com/yusukensanta/polyrec/compare/v0.3.4...v0.3.5
[v0.3.4]: https://github.com/yusukensanta/polyrec/compare/v0.3.3...v0.3.4
[v0.3.3]: https://github.com/yusukensanta/polyrec/compare/v0.3.2...v0.3.3
[v0.3.2]: https://github.com/yusukensanta/polyrec/compare/v0.3.1...v0.3.2
[v0.3.1]: https://github.com/yusukensanta/polyrec/compare/v0.3.0...v0.3.1
[v0.3.0]: https://github.com/yusukensanta/polyrec/compare/v0.2.8...v0.3.0
[v0.2.8]: https://github.com/yusukensanta/polyrec/compare/v0.2.7...v0.2.8
[v0.2.7]: https://github.com/yusukensanta/polyrec/compare/v0.2.6...v0.2.7
[v0.2.6]: https://github.com/yusukensanta/polyrec/compare/v0.2.5...v0.2.6
[v0.2.5]: https://github.com/yusukensanta/polyrec/compare/v0.2.4...v0.2.5
[v0.2.4]: https://github.com/yusukensanta/polyrec/compare/v0.2.3...v0.2.4
[v0.2.3]: https://github.com/yusukensanta/polyrec/compare/v0.2.2...v0.2.3
[v0.2.2]: https://github.com/yusukensanta/polyrec/compare/v0.2.1...v0.2.2
[v0.2.1]: https://github.com/yusukensanta/polyrec/compare/v0.1.1...v0.2.1
[v0.1.1]: https://github.com/yusukensanta/polyrec/releases/tag/v0.1.1
