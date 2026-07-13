# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [v0.5.27] - 2026-07-13

### Changed

- The Applications audio list is now entirely curated, not auto-populated -- a fresh install shows nothing here but "+ Add app" regardless of how many apps happen to be making sound, and only apps you've explicitly added ever appear. The checkbox itself is the only pin/unpin control: checking an app in persists it across restarts, and unchecking it removes it immediately (no separate button). An added app that isn't currently running still shows, checked, so it starts recording automatically the moment it launches without needing to be re-checked.
- "+ Add app" now opens an in-app search box listing currently running apps by name, instead of going straight to a file browser -- picking one needs no knowledge of where it's installed. A "Browse for .exe instead…" fallback remains for pinning an app that isn't running yet.

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
