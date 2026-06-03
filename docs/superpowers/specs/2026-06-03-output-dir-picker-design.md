# Output Directory Picker — Design Spec

**Date:** 2026-06-03  
**Branch:** feat/output-dir-picker  
**Status:** Approved

## Summary

Add a selectable output directory control to the PolyRec dashboard. The user can type a path directly or click "Browse…" to open a native folder picker. Changes persist immediately to `config.toml`.

## Scope

- Two files changed: `Cargo.toml`, `src/ui/dashboard.rs`
- No new modules; no structural refactor

## Dependencies

Add to `Cargo.toml`:
```toml
rfd = "0.15"
```
`rfd` provides a synchronous native folder picker (`FileDialog::pick_folder()`). Synchronous is acceptable here — OS folder dialogs open instantly on Windows.

## Data Model

New field on `App`:
```rust
output_dir_input: String,
```
Scratch buffer initialized in `App::new()` from `config.output_dir.to_string_lossy().into_owned()`. Stays in sync with `config.output_dir` after every accepted edit.

## UI Layout

Center panel, inserted between the STATUS section and the existing `bottom_up` REC button block:

```
STATUS section          (unchanged)
  recording state / idle message

── OUTPUT ──────────────────────────
[/path/to/output/dir          ] [Browse…]

                     (spacer)

── bottom_up ────────────────────────
  [⏺ REC]
  State: ...
```

## Interaction

### Text field
- `ui.text_edit_singleline(&mut self.output_dir_input)` stretched to fill available width minus Browse button
- On `response.lost_focus()`: run **update path steps**

### Browse button
- `rfd::FileDialog::new().set_directory(&self.config.output_dir).pick_folder()`
- If `Some(path)` returned: run **update path steps**

### Update path steps (shared)
1. `self.output_dir_input = path.to_string_lossy().into_owned()`
2. `self.config.output_dir = PathBuf::from(&self.output_dir_input)`
3. `self.config.save()` — error silently dropped (consistent with existing codebase pattern; no error UI exists yet)

## Non-Goals

- Input validation / error display (path-not-found, not-a-directory) — future work
- Settings panel / tabbed config UI — out of scope
- Async picker — unnecessary; native dialogs open instantly
