# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
