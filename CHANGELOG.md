# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.4] - 2026-07-08

### Added

- Windows installer (`polyrec-vX.Y.Z-windows-x64-setup.exe`), built via Inno Setup in CI and published alongside the existing portable zip — Start Menu shortcut, Add/Remove Programs entry, English/Japanese installer UI. Covered by the same `SHA256SUMS.txt` and build provenance attestation as the zip.
- `CONTRIBUTING.md`, `SECURITY.md`, issue templates, and `dependabot.yml`.

## [0.2.3] - 2026-07-08

### Added

- `SHA256SUMS.txt` published alongside every release zip.
- Build provenance attestation (`actions/attest-build-provenance`) on the release zip — ties the artifact to the exact repo/workflow/commit that produced it, verifiable with `gh attestation verify`.

### Security

- Enabled GitHub's immutable-releases setting for this repo — published release assets can no longer be replaced in place; any change requires deleting and recreating the release entirely.

## [0.2.2] - 2026-07-08

### Changed

- Refactored `App::update()` (previously ~800 lines) into one `render_*` method per panel/popup (menu bar, source panel, center panel, overlay HUD, Quality/Hotkeys popups, error banner, export dialog), plus a `poll_background_work()` method for non-rendering state polling. Pure reorganization, no behavior change.
- Deduplicated the audio-device checkbox icon (🔊/🎙) selection into a shared `audio_device_icon()` helper.

## [0.2.1] - 2026-07-08

### Added

- Selectable display language (English / Japanese) via a toggle in the menu bar, persisted in `config.toml` and applied immediately without a restart.

### Fixed

- Video encoding was unconditionally forced to software-only — including for real recordings, not just the test suite — missing available GPU hardware encoders (NVENC/QSV/AMF) entirely. This was very likely the cause of recording visibly stealing frame time from whatever else was running (e.g. a game) at 1080p60+.
- The capture pipeline recreated its GPU staging texture on every single frame instead of reusing one, and copied the captured frame buffer an extra time per frame even in the common case where no resolution scaling was needed.

### Security

- Pinned all GitHub Actions in CI workflows to commit SHAs instead of mutable version tags, and bumped each to its latest stable release.

## [0.1.1] - 2026-07-08

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

[Unreleased]: https://github.com/yusukensanta/polyrec/compare/v0.2.4...HEAD
[0.2.4]: https://github.com/yusukensanta/polyrec/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/yusukensanta/polyrec/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/yusukensanta/polyrec/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/yusukensanta/polyrec/compare/v0.1.1...v0.2.1
[0.1.1]: https://github.com/yusukensanta/polyrec/releases/tag/v0.1.1
