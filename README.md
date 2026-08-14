<img src="assets/icon-1024.png" width="120" alt="PolyRec icon" align="left" />

<!-- portfolio-badge -->
[![Portfolio Docs](https://img.shields.io/badge/docs-yusukensanta.github.io-blue?style=flat-square)](https://yusukensanta.github.io/projects/polyrec/)
<!-- portfolio-badge -->

# PolyRec

Multi-track screen recorder for Windows.

<br clear="left" />

## Why PolyRec

Most Windows screen recorders hand you one flattened audio track and lock in
your choices before you hit record. PolyRec doesn't:

- **Every audio source is its own track — decided at export, not record time.** System sound, mic, and each running app (Discord, a game, Spotify, ...) record independently. Change your mind about what to keep in the final file *after* you've already recorded.
- **Isolate one app's sound with zero setup.** "App audio only" scopes capture to a single process via Windows' own Process Loopback Capture API — no virtual audio cables, no mixer config.
- **Never miss the moment.** Highlight keeps a rolling background buffer running even when you haven't pressed record — hit one hotkey to save the last N seconds after something happens.
- **Updates you can actually verify.** The in-app self-updater checks a SHA256 checksum *and* a build-provenance attestation before installing anything — not just "trust the download."

Grab a build from the [Releases page](https://github.com/yusukensanta/polyrec/releases/latest) and see the [User Guide](docs/USER_GUIDE.md) for installation, the full feature list, and how to use it.

## Installation

Grab the latest release from the [Releases page](https://github.com/yusukensanta/polyrec/releases/latest) — two options are published for every version:

- **`polyrec-vX.Y.Z-windows-x64-setup.exe`** — a normal installer. Double-click, click through the wizard, get a Start Menu shortcut and an Add/Remove Programs entry.
- **`polyrec-vX.Y.Z-windows-x64.zip`** — portable, no installer: unzip and run `polyrec.exe` directly.

The installer is unsigned (no code-signing certificate), so Windows SmartScreen shows an "Unknown Publisher" warning on first run — click **More info → Run anyway**. Each release also ships a `SHA256SUMS.txt` and a build provenance attestation, so you can verify the download instead of just trusting the warning-click. See the [User Guide](docs/USER_GUIDE.md#verifying-a-download-optional) for verification steps.

**Requirements:** Windows 10 2004+ (build 19041+), and a GPU with Direct3D 11 support.

## Documentation

- **[User Guide](docs/USER_GUIDE.md)** — installation, verifying a download, full feature list, requirements, and usage walkthrough.
- **[CONTRIBUTING.md](CONTRIBUTING.md)** — build from source, project structure, and the PR/release process.
- **[SECURITY.md](SECURITY.md)** — supported versions and how to report a vulnerability.
- **[CHANGELOG.md](CHANGELOG.md)** — notable changes per release.

## License

Apache-2.0 — see [LICENSE](LICENSE).
