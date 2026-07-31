# PolyRec User Guide

Everything you need to install, verify, and use PolyRec. For build-from-source
and contributing, see [CONTRIBUTING.md](../CONTRIBUTING.md) instead.

## Download / Installation

Grab the latest release from the [Releases page](https://github.com/yusukensanta/polyrec/releases/latest) — two options are published for every version:

- **`polyrec-vX.Y.Z-windows-x64-setup.exe`** — a normal installer. Double-click, click through the wizard, get a Start Menu shortcut and an Add/Remove Programs entry. Recommended if you just want to install it like any other app.
- **`polyrec-vX.Y.Z-windows-x64.zip`** — portable, no installer: unzip and run `polyrec.exe` directly. Nothing is written outside the folder you unzip it to.

Either way, the app checks for newer releases on launch and shows a banner if one's available. Clicking it opens an "Update Now" / "Not Now" confirmation (with a separate link to view release notes without updating) — choosing **Update Now** downloads the release, verifies its SHA256 against the published `SHA256SUMS.txt`, then either swaps the running exe in place (portable) or silently re-launches the installer (installed, triggering the usual UAC prompt). Nothing downloads or installs without that confirmation.

The installer is unsigned (no code-signing certificate), so Windows SmartScreen will show an "Unknown Publisher" warning on first run — click **More info → Run anyway**. See [SECURITY.md](../SECURITY.md) for why, and the verification steps below if you want independent confirmation of what you're running instead of just trusting the warning-click.

### Verifying a download (optional)

Each release includes a `SHA256SUMS.txt` and a build provenance attestation covering both the installer and the zip, so you don't have to just trust that the file on the release page is what CI actually built.

Checksum (PowerShell):

```powershell
Get-FileHash polyrec-vX.Y.Z-windows-x64-setup.exe -Algorithm SHA256
# compare the output against the matching line in SHA256SUMS.txt
```

Build provenance (requires the [GitHub CLI](https://cli.github.com/)) — confirms the file was built by this repo's release workflow from the tagged commit, not just that the bytes match a checksum someone could have regenerated alongside a swapped file:

```powershell
gh attestation verify polyrec-vX.Y.Z-windows-x64-setup.exe -R yusukensanta/polyrec
```

Releases are also immutable once published — assets can't be silently replaced after the fact; any change requires deleting and recreating the release entirely.

## Requirements

- Windows 10 2004+ (build 19041+) — required for the Process Loopback Capture API used by "App audio only".
- A GPU with Direct3D 11 support.

## Features

- **Window or entire-screen capture** via Windows.Graphics.Capture — pick any visible window, or a specific display for a whole-screen recording (every connected monitor is listed separately). Recording defaults to the source's own native resolution; "Match display" and "Custom" are opt-in alternatives in **⚙ Quality**.
- **Multi-track audio** (**🔊 Audio**) — record system loopback (Speakers) and microphone as independent tracks. Loopback is selected by default so the common case is one clean, unambiguous track; check Microphone to add a second. Each checked device gets its own volume slider (0–100%, default 100%) that only affects the recording, not what you actually hear — this selection is remembered across restarts.
- **App audio only** — scope loopback capture to just the selected app's process via the Windows Process Loopback Capture API, instead of the full desktop mix.
- **Per-app audio tracks** (also in **🔊 Audio**) — pick individual running apps with sound (Discord, Spotify, a game, etc.) as their own independent audio tracks. Each gets its own volume slider. Independent of App audio only / the video capture source — pick an app's audio here without it needing to be the window you're recording. Multiple processes belonging to the same app (e.g. Electron helper processes) are collapsed to one track, not duplicated. "+ Add app" pins an app (even one that isn't running yet) so it's remembered and auto-selected across launches.
- **Recording border toggle** — Windows draws its own colored border around the captured window/monitor while recording; turn it on or off from the menu bar to match your preference.
- **Pause / resume** without cutting the recording.
- **Global hotkeys** — start/stop, pause, toggle the on-screen overlay, and save a Highlight clip from anywhere, rebindable from the in-app **⌨ Hotkeys** popup (F9 / F8 / F7 / F10 by default). Pressing start/stop while some other window has focus records *that* window, not whatever's selected in the list.
- **Highlight** — an optional rolling background buffer (30–300s, configurable) that keeps recording even when you're not; hit the save-highlight hotkey to export the last N seconds without having started a manual recording.
- **Quality settings** (**⚙ Quality**) — FPS (30/60), codec (H265 with automatic fallback to H264 if your machine lacks an HEVC encoder, or H264 directly), resolution mode, bitrate (auto-calculated from resolution×fps, or a manual Mbps override), and encoder mode (hardware GPU encoder by default, or software to free up the GPU).
- **Export** — remux the recording with only the audio tracks you want, no re-encoding.
- **Update check** — compares your version against the latest GitHub release on launch and offers an in-app, checksum-verified self-update.
- **English / 日本語** — UI language toggle, persisted in config.

## Usage

1. Pick a capture source from the list on the left.
2. In **🔊 Audio**, choose which audio tracks to include (Speakers / Microphone / running apps), and optionally check **App audio only** to isolate the selected app's sound.
3. Adjust **⚙ Quality** or **⌨ Hotkeys** if you want something other than the defaults.
4. Press **REC** (or the start/stop hotkey) to start, pause hotkey to pause/resume, start/stop hotkey again to stop.
5. On stop, export lets you pick which audio tracks to keep in the final file.

Recordings are saved to `<output folder>/polyrec/<app name>_<finish time>.mp4` — the output folder defaults to your Videos folder and is configurable from the app.

## Building from source

See [CONTRIBUTING.md](../CONTRIBUTING.md) for prerequisites, build/test commands, and project structure.
