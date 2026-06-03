# Output Directory Picker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an editable text field + native folder picker button to the PolyRec center panel so users can change and persist the recording output directory.

**Architecture:** Add `output_dir_input: String` scratch buffer to `App`, rendered as an OUTPUT section in the center panel between STATUS and the REC button. Lost-focus on the text field and browse-dialog confirmation both immediately call `config.save()`.

**Tech Stack:** Rust, egui 0.29, eframe 0.29, rfd (native folder dialog)

---

## File Map

| File | Change |
|------|--------|
| `Cargo.toml` | Add `rfd` dependency |
| `src/ui/dashboard.rs` | Add field, init, OUTPUT section rendering |
| `src/config.rs` | Add one test for `output_dir` round-trip |

---

## Task 1: Add rfd dependency

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add rfd to dependencies**

In `Cargo.toml`, add after the `dirs` line:

```toml
rfd = "0.14"
```

> Note: if `0.14` resolves to an old version with different API, use the latest available from `cargo search rfd`. The API used is `rfd::FileDialog::new().set_directory(path).pick_folder()` which has been stable since 0.10.

- [ ] **Step 2: Verify it compiles**

```powershell
cd .worktrees/feat-output-dir-picker
cargo build 2>&1 | Select-String -Pattern "error" | head -20
```

Expected: no `error[E...]` lines. Warnings OK.

- [ ] **Step 3: Commit**

```powershell
git add Cargo.toml Cargo.lock
git commit -m "chore: add rfd dependency for native folder picker"
```

---

## Task 2: Add config round-trip test for output_dir

**Files:**
- Modify: `src/config.rs` (add one test)

- [ ] **Step 1: Write the failing test**

In `src/config.rs`, inside the `#[cfg(test)] mod tests` block, add after `save_and_load_roundtrip`:

```rust
#[test]
fn output_dir_survives_round_trip() {
    let dir = tempdir().unwrap();
    let expected = dir.path().join("recordings");
    let cfg = Config {
        output_dir: expected.clone(),
        ..Config::default()
    };
    let text = toml::to_string_pretty(&cfg).unwrap();
    let loaded: Config = toml::from_str(&text).unwrap();
    assert_eq!(loaded.output_dir, expected);
}
```

- [ ] **Step 2: Run it to verify it passes** (no new code needed — just a data test)

```powershell
cargo test output_dir_survives_round_trip -- --nocapture
```

Expected: `test config::tests::output_dir_survives_round_trip ... ok`

- [ ] **Step 3: Commit**

```powershell
git add src/config.rs
git commit -m "test: verify output_dir survives config round-trip"
```

---

## Task 3: Add output_dir_input field to App

**Files:**
- Modify: `src/ui/dashboard.rs`

- [ ] **Step 1: Add field to App struct**

In `src/ui/dashboard.rs`, add `output_dir_input` to the `App` struct after `last_output_path`:

```rust
pub struct App {
    config: Config,
    session: SessionManager,
    sources: Vec<CaptureSource>,
    selected_source: Option<usize>,
    audio_devices: Vec<AudioDevice>,
    selected_audio: Vec<bool>,
    overlay_enabled: bool,
    frame_count: Arc<AtomicU64>,
    recording_start: Option<Instant>,
    last_output_path: Option<PathBuf>,
    output_dir_input: String,          // ← add this
    show_export_dialog: bool,
    export_track_selection: Vec<bool>,
    hotkey_listener: HotkeyListener,
}
```

- [ ] **Step 2: Initialize it in App::new()**

In `App::new()`, extract the string before the `Self { ... }` block (must be before `config` is moved):

```rust
pub fn new(cc: &eframe::CreationContext<'_>, config: Config) -> Self {
    setup_theme(&cc.egui_ctx);
    let overlay_enabled = config.overlay.enabled;
    let output_dir_input = config.output_dir.to_string_lossy().into_owned(); // ← add this line
    let audio_devices = enumerate_audio_devices().unwrap_or_default();
    // ... rest unchanged ...
```

Then add `output_dir_input,` to the `Self { ... }` initializer:

```rust
    Self {
        config,
        session: SessionManager::new(),
        sources: enumerate_sources(),
        selected_source: None,
        audio_devices,
        selected_audio,
        overlay_enabled,
        frame_count: Arc::new(AtomicU64::new(0)),
        recording_start: None,
        last_output_path: None,
        output_dir_input,              // ← add this
        show_export_dialog: false,
        export_track_selection,
        hotkey_listener,
    }
```

- [ ] **Step 3: Verify it compiles**

```powershell
cargo build 2>&1 | Select-String "error" | head -20
```

Expected: no error lines.

- [ ] **Step 4: Commit**

```powershell
git add src/ui/dashboard.rs
git commit -m "feat: add output_dir_input scratch buffer to App"
```

---

## Task 4: Render OUTPUT section and wire interactions

**Files:**
- Modify: `src/ui/dashboard.rs`

- [ ] **Step 1: Add rfd import**

At the top of `src/ui/dashboard.rs`, add after the existing `use std::path::PathBuf;` line:

```rust
use std::path::PathBuf;
use rfd::FileDialog;
```

- [ ] **Step 2: Insert OUTPUT section in center panel**

In the `update` method, find the line:

```rust
            ui.add_space(16.0);

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
```

Replace it with:

```rust
            ui.add_space(16.0);
            section_header(ui, "OUTPUT");
            ui.horizontal(|ui| {
                let btn_width = 74.0;
                let tf_width = (ui.available_width() - btn_width - 8.0).max(60.0);
                let tf = ui.add_sized(
                    [tf_width, 22.0],
                    egui::TextEdit::singleline(&mut self.output_dir_input),
                );
                if tf.lost_focus() {
                    self.config.output_dir = PathBuf::from(&self.output_dir_input);
                    let _ = self.config.save();
                }
                if ui.button("Browse…").clicked() {
                    if let Some(path) = FileDialog::new()
                        .set_directory(&self.config.output_dir)
                        .pick_folder()
                    {
                        self.output_dir_input = path.to_string_lossy().into_owned();
                        self.config.output_dir = path;
                        let _ = self.config.save();
                    }
                }
            });

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
```

- [ ] **Step 3: Verify it compiles**

```powershell
cargo build 2>&1 | Select-String "error" | head -20
```

Expected: no error lines.

- [ ] **Step 4: Run all tests**

```powershell
cargo test 2>&1 | tail -20
```

Expected: all tests pass, `0 failed`.

- [ ] **Step 5: Commit**

```powershell
git add src/ui/dashboard.rs
git commit -m "feat: add OUTPUT section with text field and Browse button to center panel"
```

---

## Task 5: Smoke test

- [ ] **Step 1: Run the app**

```powershell
cargo run
```

Expected: app opens without panic.

- [ ] **Step 2: Verify OUTPUT section renders**

Confirm center panel shows "OUTPUT" header with a text field pre-filled with the default video directory path and a "Browse…" button.

- [ ] **Step 3: Test Browse button**

Click "Browse…" — OS folder picker opens. Select a directory. Confirm text field updates to the selected path.

- [ ] **Step 4: Test persistence**

After selecting a folder via Browse, close the app and re-run. Confirm the text field shows the previously selected path (loaded from `config.toml`).

- [ ] **Step 5: Test text edit**

Type a valid path into the text field, click elsewhere (lose focus). Close and re-run. Confirm the typed path persisted.

---

## Self-Review

**Spec coverage:**
- ✅ `rfd` dependency added
- ✅ `output_dir_input: String` field on App
- ✅ Initialized from `config.output_dir` in `App::new()`
- ✅ OUTPUT section in center panel between STATUS and REC button
- ✅ Text field + Browse button
- ✅ `lost_focus()` → `config.save()`
- ✅ Browse → `FileDialog::pick_folder()` → `config.save()`

**Placeholder scan:** None found.

**Type consistency:** `PathBuf::from(&self.output_dir_input)` used consistently in both update paths. `FileDialog` imported as `use rfd::FileDialog`.
