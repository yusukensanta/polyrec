# Contributing to PolyRec

Thank you for your interest in contributing to PolyRec! This guide will help you get started.

## Quick Start

### 1. Fork and Clone

```powershell
git clone https://github.com/yusukensanta/polyrec.git
cd polyrec
```

### 2. Prerequisites

- Windows 10 2004+ (build 19041+) — required for the Process Loopback Capture API.
- A GPU with Direct3D 11 support.
- Rust via [rustup](https://rustup.rs/). `rust-toolchain.toml` pins the version (1.99.1), and rustup installs it on the first build. A `RUSTUP_TOOLCHAIN` environment variable overrides that pin; Cargo then stops with "requires rustc 1.99" if it names an older one.

### 3. Build and Test

```powershell
cargo build              # Debug build
cargo build --release    # Release build (target/release/polyrec.exe)
cargo test               # Run the test suite
cargo clippy --all-targets
```

**Note:** several tests exercise real Windows APIs (window enumeration, D3D11 device creation, WASAPI capture) rather than mocks, so they need to run on an actual Windows desktop session, not headless CI without a display. The release CI runs these on a `windows-latest` GitHub-hosted runner.

## Development Workflow

### Making Changes

1. **Create a branch**
   ```powershell
   git switch -c feat/my-feature
   ```

2. **Make your changes**
   - Write code
   - Add tests where the change is testable (pure logic, config parsing, etc. — not everything here is; see Testing below)
   - Update `CHANGELOG.md` under `[Unreleased]`

3. **Verify**
   ```powershell
   cargo build --release
   cargo clippy --all-targets
   cargo test
   ```

4. **Commit your changes**
   ```powershell
   git add .
   git commit -m "feat: add awesome feature"
   ```

   We follow [Conventional Commits](https://www.conventionalcommits.org/):
   - `feat:` — new feature
   - `fix:` — bug fix
   - `perf:` — performance improvement
   - `docs:` — documentation changes
   - `test:` — adding or fixing tests
   - `refactor:` — code change with no behavior change
   - `ci:` — CI/CD pipeline changes
   - `chore:` — maintenance tasks (version bumps, dependency updates)

5. **Push and open a Pull Request**
   ```powershell
   git push origin feat/my-feature
   ```

### Running the Application Locally

```powershell
cargo run
```

## Code Quality

### Linting

```powershell
cargo clippy --all-targets
```

CI treats new clippy warnings as something to fix before merge — the codebase currently builds clippy-clean.

### Testing

```powershell
cargo test               # Fast tests
cargo test -- --ignored  # Also run tests that need a real desktop/hardware (D3D11, live capture)
# The ignored tests capture real windows and audio devices (they play nothing)
# and take a few minutes; run one by name with
# `-- --ignored --exact session::tests::full_capture_produces_nonempty_file`.
```

## Project Structure

```
polyrec/
├── src/
│   ├── main.rs                  # Entry point, Media Foundation lifetime, module declarations
│   ├── config.rs                # Config structs + typed setting enums, TOML load/save
│   ├── i18n.rs                  # Display-language string tables (EN/JA)
│   ├── error.rs                 # AppError (thiserror) + ResultExt (`.encode_ctx(..)` etc.)
│   ├── types.rs                 # Shared types: CaptureSource, AudioDevice, VideoFrame, ...
│   ├── sources.rs               # Window/monitor/process enumeration, exe names/icons
│   ├── disk_space.rs            # Free-disk-space check + periodic guard
│   ├── recording_naming.rs      # Finished-file naming (process name / custom prefix + sequence)
│   ├── update_check.rs          # GitHub Releases version check
│   ├── self_update.rs           # Verified download + in-place update / installer launch
│   ├── hotkeys/mod.rs           # Global hotkeys (RegisterHotKey + low-level keyboard hook)
│   ├── capture/
│   │   ├── mod.rs               # CaptureControl: shared clock + pause/stop signals
│   │   ├── video.rs             # Windows.Graphics.Capture pipeline
│   │   ├── audio.rs             # WASAPI device/loopback + per-process loopback capture
│   │   └── device.rs            # D3D11 device creation
│   ├── encode/
│   │   ├── mf.rs                # MfSession: RAII MFStartup/MFShutdown
│   │   ├── writer.rs            # IMFSinkWriter setup (EncoderParams), media types
│   │   ├── actor.rs             # Recording actor + capture->encoder pump tasks
│   │   ├── remux.rs             # Post-recording track-selective / mixing export
│   │   └── highlight_export.rs  # Concatenate + trim Highlight segments into one file
│   ├── highlight/mod.rs         # Rolling Highlight segment-rotation actor
│   ├── session/
│   │   ├── mod.rs               # SessionManager: recording + Highlight lifecycle
│   │   ├── pipeline.rs          # CapturePipeline: spawns/stops video + audio capture threads
│   │   ├── state.rs             # Idle/Recording/Paused state machine
│   │   └── clock.rs             # Pause-aware recording clock (QPC-based)
│   └── ui/dashboard/            # egui dashboard
│       ├── mod.rs               # App struct + eframe entry point
│       ├── state.rs             # Feature-scoped App sub-state (export, add-app picker, ...)
│       ├── actions.rs           # Start/stop/refresh actions behind the UI
│       ├── background.rs        # Per-frame polling of background jobs
│       ├── panel_*.rs           # Menu bar, source list, center panel
│       ├── *_popup.rs, popups.rs# Quality / audio / hotkeys popups, error/update dialogs
│       ├── overlay.rs           # Recording HUD overlay viewport
│       └── theme.rs, widgets.rs, util.rs
├── assets/                      # App icon
├── installer/                   # Inno Setup script
├── .github/workflows/           # CI + release pipelines
├── Cargo.toml
└── build.rs                     # Embeds the app icon into the exe
```

## Common Tasks

### Adding a Config Setting

1. Add the field to the relevant struct in `config.rs` and give it a default in that struct's `Default` impl. Every config struct is `#[serde(default)]`, so an older `config.toml` without the field still loads. No per-field serde attribute is needed.
2. For a fixed set of choices, use an enum with `#[serde(rename_all = "lowercase")]` and a `#[serde(other)]` fallback variant (it must be the last variant) instead of a `String`. See `Codec` / `EncoderMode`.
3. Add a round-trip test alongside the existing config tests.
4. Wire it into the UI (`src/ui/dashboard/`) if user-facing.

### Adding UI Text

All static UI strings live in `src/i18n.rs`'s `Strings` struct — add the field there (both the `EN` and `JA` instances; a missing field on either is a compile error) rather than hardcoding a string literal under `src/ui/dashboard/`.

### Debugging

```powershell
$env:RUST_LOG="debug"; cargo run
```

Uses `tracing`/`tracing-subscriber` for structured logging.

## CI/CD

The release pipeline (`.github/workflows/release.yml`) triggers on a `v*.*.*` tag push, and:
- Verifies the tag matches `Cargo.toml`'s `version` field (bump `Cargo.toml` and retag if these drift).
- Builds `cargo build --release --locked`.
- Packages a zip, generates `SHA256SUMS.txt`, and attests build provenance.
- Leaves it as a **draft** GitHub Release. The maintainer signs and publishes it (see Release Process below).

Make sure `cargo build --release`, `cargo clippy --all-targets`, and `cargo test` all pass before submitting a PR.

## Getting Help

- 📖 **Documentation**: [docs/USER_GUIDE.md](docs/USER_GUIDE.md) covers usage, features, and requirements.
- 🐛 **Issues**: browse existing issues or open a new one.

## Pull Request Guidelines

### Before Submitting

- [ ] `cargo build --release` succeeds
- [ ] `cargo test` passes
- [ ] `cargo clippy --all-targets` is clean
- [ ] Commit messages follow Conventional Commits
- [ ] `CHANGELOG.md` updated under `[Unreleased]` (if user-facing)
- [ ] PR description explains what and why

## Release Process

(For maintainers only)

1. Update `CHANGELOG.md` — move `[Unreleased]` entries under a new version heading.
2. Bump `Cargo.toml`'s `version` field to match.
3. Commit, push, then tag and push the tag:
   ```powershell
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```
4. CI builds, verifies the tag matches `Cargo.toml`, and leaves a draft release. See `.github/workflows/release.yml`.
5. Sign and publish it, in a terminal of your own (minisign asks for the release key's password), from the repo checkout the tag was pushed from:
   ```powershell
   pwsh -NoProfile -File $env:USERPROFILE\.minisign\Sign-Release.ps1 vX.Y.Z
   ```
   That's a copy of `.github/scripts/Sign-Release.ps1` kept next to your keys, outside the repo, because anyone able to push to GitHub could change the repo's copy, and it runs while you type the password. Install it once after reading it (`Copy-Item .github/scripts/Sign-Release.ps1 $env:USERPROFILE\.minisign\`). Re-install it, after reading the diff, when it tells you the repo's copy has changed.

   It:
   - checks that `keys/` at the tag are your public keys;
   - lists what changed since the last release you signed, flagging files that change the build, the release or the keys, and waits for you to type the tag;
   - checks the draft's files: checksums, and build provenance from that tag's commit on `main`;
   - signs `SHA256SUMS.txt`;
   - uploads `SHA256SUMS.txt.minisig`;
   - checks the draft holds exactly those four files, then publishes it.

   PolyRec's self-updater installs only releases signed with one of the keys in `keys/`: the release key, or the backup key kept offline should it be lost (`-KeyFile <backup key>`).

   Signing says you've looked at what's in the release, so read the list. Never take the password off the release key to save typing it.

## Code of Conduct

- Be respectful and inclusive.
- Provide constructive feedback.
- Focus on the code, not the person.

## License

By contributing, you agree that your contributions will be licensed under the Apache 2.0 License.

---

**Questions?** Open an issue.
