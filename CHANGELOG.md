# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/yusukensanta/polyrec/compare/v0.3.4...HEAD
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
