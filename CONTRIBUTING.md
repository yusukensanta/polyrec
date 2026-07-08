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
- Rust (stable toolchain) via [rustup](https://rustup.rs/).

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
   git checkout -b feat/my-feature
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
```

## Project Structure

```
polyrec/
├── src/
│   ├── main.rs              # Entry point, MF startup/shutdown, module declarations
│   ├── config.rs            # Config struct, TOML load/save, resolved encode settings
│   ├── i18n.rs               # Display-language string tables (EN/JA)
│   ├── disk_space.rs         # Free-disk-space check (GetDiskFreeSpaceExW)
│   ├── error.rs              # AppError enum (thiserror)
│   ├── types.rs              # Shared types: CaptureSource, SessionState, VideoFrame, ...
│   ├── sources.rs             # Window/process enumeration, exe icon extraction
│   ├── hotkeys.rs             # Global hotkey listener (RegisterHotKey)
│   ├── update_check.rs        # GitHub Releases version check
│   ├── capture/
│   │   ├── video.rs           # Windows.Graphics.Capture pipeline
│   │   ├── audio.rs            # WASAPI loopback/mic capture
│   │   └── device.rs           # D3D11 device creation
│   ├── encode/
│   │   ├── writer.rs           # IMFSinkWriter setup, media type construction
│   │   ├── actor.rs             # Recording actor task, capture/pump wiring
│   │   └── remux.rs             # Post-record audio-track-selective remux
│   ├── session/
│   │   ├── mod.rs               # SessionManager: owns capture/encode lifecycle
│   │   ├── state.rs              # Session state machine (Idle/Recording/Paused)
│   │   └── clock.rs               # Pause-aware recording clock (QPC-based)
│   └── ui/
│       └── dashboard.rs           # egui dashboard: panels, popups, theme
├── assets/                # App icon
├── .github/workflows/     # CI/CD pipelines
├── Cargo.toml
└── build.rs               # Embeds the app icon into the exe
```

## Common Tasks

### Adding a Config Setting

1. Add the field to the relevant struct in `config.rs` (with a default in `Config::default()`).
2. Add a round-trip test alongside the existing config tests.
3. Wire it into the UI (`src/ui/dashboard.rs`) if user-facing.

### Adding UI Text

All static UI strings live in `src/i18n.rs`'s `Strings` struct — add the field there (both the `EN` and `JA` instances; a missing field on either is a compile error) rather than hardcoding a string literal in `dashboard.rs`.

### Debugging

```powershell
$env:RUST_LOG="debug"; cargo run
```

Uses `tracing`/`tracing-subscriber` for structured logging.

## CI/CD

The release pipeline (`.github/workflows/release.yml`) triggers on a `v*.*.*` tag push or a published GitHub Release, and:
- Verifies the tag matches `Cargo.toml`'s `version` field (bump `Cargo.toml` and retag if these drift).
- Builds `cargo build --release --locked`.
- Packages a zip, generates `SHA256SUMS.txt`, and attests build provenance.
- Publishes to GitHub Releases.

Make sure `cargo build --release`, `cargo clippy --all-targets`, and `cargo test` all pass before submitting a PR.

## Getting Help

- 📖 **Documentation**: [README.md](README.md) covers usage, features, and requirements.
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
4. CI builds, verifies the tag matches `Cargo.toml`, and publishes the release automatically. See `.github/workflows/release.yml`.

## Code of Conduct

- Be respectful and inclusive.
- Provide constructive feedback.
- Focus on the code, not the person.

## License

By contributing, you agree that your contributions will be licensed under the Apache 2.0 License.

---

**Questions?** Open an issue.
