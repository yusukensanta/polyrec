<img src="assets/icon-1024.png" width="120" alt="PolyRec icon" align="left" />

# PolyRec

Multi-track screen recorder for Windows.

<br clear="left" />

## Features

- **Window capture** via Windows.Graphics.Capture — pick any visible window, recording defaults to your display's native resolution regardless of the captured window's own size.
- **Multi-track audio** — record system loopback (Speakers) and microphone as independent tracks in the same file.
- **App audio only** — scope loopback capture to just the selected app's process via the Windows Process Loopback Capture API, instead of the full desktop mix.
- **Pause / resume** without cutting the recording.
- **Global hotkeys** — start/stop, pause, and toggle the on-screen overlay from anywhere (`F9` / `F8` / `F7` by default, configurable).
- **Export** — remux the recording with only the audio tracks you want, no re-encoding.
- **H.264 + AAC** via Media Foundation, `.mp4` output.

## Requirements

- Windows 10 2004+ (build 19041+) — required for the Process Loopback Capture API used by "app audio only".
- A GPU with Direct3D 11 support.

## Build

```powershell
cargo build --release
```

The built binary is at `target/release/polyrec.exe`, with the app icon embedded via `build.rs`.

## Usage

1. Pick a capture source from the list on the left.
2. Choose which audio tracks to include (Speakers / Microphone), and optionally check **App audio only** to isolate the selected app's sound.
3. Press **REC** (or `F9`) to start, `F8` to pause/resume, `F9` again to stop.
4. On stop, export lets you pick which audio tracks to keep in the final file.

Output defaults to your Videos folder; configurable from the app.

## License

Apache-2.0 — see [LICENSE](LICENSE).
