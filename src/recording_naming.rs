//! How a finished manual recording or Highlight save gets its filename --
//! either the recorded process's name (historical default) or a
//! user-specified prefix with an auto-incrementing sequence number.

use std::path::Path;

/// Resolved once (from `Config`, by the caller) when a recording/highlight-
/// save *starts*, then used at *finish* time to build the actual filename --
/// mirroring how the finish-timestamp itself was already only known then,
/// not when recording started (see `session::prepare_recording_paths`'s doc
/// comment), so this doesn't move that decision point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingNaming {
    ProcessName,
    CustomPrefix(String),
}

/// Replaces anything that isn't ASCII alphanumeric/`-`/`_` with `_`, falling
/// back to `"recording"` if that leaves nothing -- the same rule
/// `session::app_name_from_exe` already applies to process names, reused
/// here for user-typed custom prefixes (which come from a free-text field,
/// not a trusted exe name).
pub fn sanitize_filename_component(s: &str) -> String {
    let sanitized: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "recording".to_string()
    } else {
        sanitized
    }
}

/// One past the highest `{prefix}_NNN.mp4` already in `dir` -- self-
/// correcting (always collision-free regardless of deleted files or app
/// restarts, no counter to persist) rather than a value stored in config
/// that could drift from what's actually on disk. A missing/unreadable
/// `dir` is treated the same as "no existing files" (returns 1), not an
/// error -- the caller creates `dir` before writing into it regardless.
fn next_sequence_number(dir: &Path, prefix: &str) -> u32 {
    let pattern_prefix = format!("{prefix}_");
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| {
            let stem = name.strip_suffix(".mp4")?;
            let num_str = stem.strip_prefix(&pattern_prefix)?;
            num_str.parse::<u32>().ok()
        })
        .max()
        .map_or(1, |max| max + 1)
}

/// Filename stem (no extension) for a just-finished recording/highlight-
/// save. `process_app_name` is only used for `RecordingNaming::ProcessName`
/// (already sanitized by `app_name_from_exe`); `dir` is the folder the file
/// is about to be written into -- manual recordings and Highlight saves
/// land in different folders (`polyrec/` vs `polyrec/highlights/`), so each
/// gets its own independent sequence.
pub fn resolve_finished_recording_stem(
    naming: &RecordingNaming,
    process_app_name: &str,
    dir: &Path,
) -> String {
    match naming {
        RecordingNaming::ProcessName => {
            let finish_stamp = chrono::Local::now().format("%Y-%m-%d-%H-%M-%S");
            format!("{process_app_name}_{finish_stamp}")
        }
        RecordingNaming::CustomPrefix(prefix) => {
            let prefix = sanitize_filename_component(prefix.trim());
            let next = next_sequence_number(dir, &prefix);
            format!("{prefix}_{next:03}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_filename_component_replaces_invalid_chars() {
        assert_eq!(sanitize_filename_component("My Stream!"), "My_Stream_");
    }

    #[test]
    fn sanitize_filename_component_empty_falls_back_to_recording() {
        assert_eq!(sanitize_filename_component(""), "recording");
        assert_eq!(sanitize_filename_component("!!!"), "___");
    }

    #[test]
    fn sanitize_filename_component_keeps_alphanumeric_dash_underscore() {
        assert_eq!(sanitize_filename_component("My-Stream_01"), "My-Stream_01");
    }

    #[test]
    fn next_sequence_number_starts_at_one_for_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(next_sequence_number(dir.path(), "MyStream"), 1);
    }

    #[test]
    fn next_sequence_number_returns_one_for_missing_dir() {
        let missing = std::path::Path::new("Z:\\this\\does\\not\\exist\\at\\all");
        assert_eq!(next_sequence_number(missing, "MyStream"), 1);
    }

    #[test]
    fn next_sequence_number_finds_max_plus_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("MyStream_001.mp4"), b"").unwrap();
        std::fs::write(dir.path().join("MyStream_007.mp4"), b"").unwrap();
        std::fs::write(dir.path().join("MyStream_003.mp4"), b"").unwrap();
        assert_eq!(next_sequence_number(dir.path(), "MyStream"), 8);
    }

    #[test]
    fn next_sequence_number_ignores_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("MyStream_001.mp4"), b"").unwrap();
        std::fs::write(dir.path().join("OtherApp_099.mp4"), b"").unwrap();
        std::fs::write(dir.path().join("MyStream_notanumber.mp4"), b"").unwrap();
        std::fs::write(dir.path().join("MyStream_002.txt"), b"").unwrap();
        assert_eq!(next_sequence_number(dir.path(), "MyStream"), 2);
    }

    #[test]
    fn next_sequence_number_does_not_confuse_prefix_substrings() {
        // "MyStreamExtra_005" must not be mistaken for a "MyStream" entry --
        // strip_prefix("MyStream_") on "MyStreamExtra_005" fails to match
        // (the literal underscore right after "MyStream" isn't there), so
        // this is really just documenting the expected non-match.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("MyStreamExtra_005.mp4"), b"").unwrap();
        assert_eq!(next_sequence_number(dir.path(), "MyStream"), 1);
    }

    #[test]
    fn resolve_finished_recording_stem_process_name_uses_app_name_and_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let stem = resolve_finished_recording_stem(&RecordingNaming::ProcessName, "vivaldi", dir.path());
        assert!(stem.starts_with("vivaldi_"));
    }

    #[test]
    fn resolve_finished_recording_stem_custom_prefix_uses_sequence_number() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("MyStream_001.mp4"), b"").unwrap();
        let stem = resolve_finished_recording_stem(
            &RecordingNaming::CustomPrefix("MyStream".into()),
            "vivaldi",
            dir.path(),
        );
        assert_eq!(stem, "MyStream_002");
    }

    #[test]
    fn resolve_finished_recording_stem_sanitizes_custom_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let stem = resolve_finished_recording_stem(
            &RecordingNaming::CustomPrefix("My Stream!".into()),
            "vivaldi",
            dir.path(),
        );
        assert_eq!(stem, "My_Stream__001");
    }

    #[test]
    fn resolve_finished_recording_stem_trims_custom_prefix_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let stem = resolve_finished_recording_stem(
            &RecordingNaming::CustomPrefix("  MyStream  ".into()),
            "vivaldi",
            dir.path(),
        );
        assert_eq!(stem, "MyStream_001");
    }
}
