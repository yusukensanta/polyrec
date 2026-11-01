//! Warnings that keep a recording from going to waste: how long the
//! recording drive's free space lasts at the current quality, and a
//! microphone that has given no sound at all.

use super::App;
use super::actions::{TrackKind, TrackLabel};
use super::toast::Toast;
use crate::config::{BitrateMode, ResolutionMode};
use crate::disk_space::MIN_FREE_BYTES;
use crate::i18n::Strings;
use crate::session::{EncodeSettings, estimated_bytes_per_sec};
use crate::types::{AudioLevel, CaptureKind, CaptureSource};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// Less recording time than this left on the drive reads as a warning, and
/// once while recording, says so on screen.
const SHORT_TIME_LEFT: Duration = Duration::from_secs(10 * 60);

/// How long a recording runs before its microphones are checked for sound.
const MIC_CHECK_AFTER: Duration = Duration::from_secs(10);

/// A peak below this is no sound at all (-80 dBFS): a muted or dead
/// microphone gives digital silence, while a working one's own noise sits
/// well above it.
const SILENT_PEAK: f32 = 1e-4;

/// How long a recording runs before its file's growth is trusted as its rate
/// (the start includes the file's header and the encoder settling in), and
/// how often it's measured after that.
const MEASURE_AFTER: Duration = Duration::from_secs(15);
const MEASURE_EVERY: Duration = Duration::from_secs(5);

/// How often the estimate for the next recording is worked out again while
/// nothing changed: the selected window may have been resized.
const ESTIMATE_EVERY: Duration = Duration::from_secs(10);

/// How long recording can go on with `free` bytes left on the drive, writing
/// `bytes_per_sec`: up to where PolyRec stops a recording
/// (`MIN_FREE_BYTES`), not to empty. `None` without a rate.
fn recording_time_left(free: u64, bytes_per_sec: u64) -> Option<Duration> {
    (bytes_per_sec > 0)
        .then(|| Duration::from_secs(free.saturating_sub(MIN_FREE_BYTES) / bytes_per_sec))
}

/// `left` as the free-space line says it: whole hours from 10 h up, hours
/// and minutes (to 5) below that, then minutes.
fn format_time_left(left: Duration, s: &Strings) -> String {
    let minutes = left.as_secs() / 60;
    let (hours, minutes) = (minutes / 60, minutes % 60);
    let minutes_to_5 = minutes / 5 * 5;
    match (hours, minutes) {
        (0, 0) => s.time_left_under_a_minute.to_owned(),
        (0, m) => s.time_left_minutes_template.replace("{m}", &m.to_string()),
        (h, _) if h >= 10 || minutes_to_5 == 0 => {
            s.time_left_hours_template.replace("{h}", &h.to_string())
        }
        (h, _) => s
            .time_left_hours_minutes_template
            .replace("{h}", &h.to_string())
            .replace("{m}", &minutes_to_5.to_string()),
    }
}

fn is_short(left: Duration) -> bool {
    left < SHORT_TIME_LEFT
}

/// The microphones, out of a recording's tracks (`labels`, with each track's
/// `loudest` level so far), that have given no sound at all. Speakers and
/// apps are often silent for a while, so only microphones are checked.
fn silent_microphones<'a>(labels: &'a [TrackLabel], loudest: &[f32]) -> Vec<&'a str> {
    labels
        .iter()
        .zip(loudest)
        .filter(|&(label, &peak)| label.kind == TrackKind::Microphone && peak < SILENT_PEAK)
        .map(|(label, _)| label.name.as_str())
        .collect()
}

fn silent_microphones_message(s: &Strings, names: &[&str]) -> String {
    s.notice_silent_microphones_template
        .replace("{mics}", &names.join(", "))
}

/// Everything the estimate depends on.
type EstimateKey = (usize, CaptureKind, u32, ResolutionMode, BitrateMode, usize);

/// About how fast the next recording will fill the drive, for the selected
/// source and settings (`estimated_bytes_per_sec`), worked out on a thread
/// of its own whenever they change, and every `ESTIMATE_EVERY`.
#[derive(Default)]
pub(super) struct RateEstimate {
    key: Option<EstimateKey>,
    at: Option<Instant>,
    rx: Option<Receiver<u64>>,
    bytes_per_sec: Option<u64>,
}

impl RateEstimate {
    fn update(&mut self, source: Option<&CaptureSource>, encode: &EncodeSettings, tracks: usize) {
        let Some(source) = source else {
            *self = Self::default();
            return;
        };
        if let Some(rx) = &self.rx {
            match rx.try_recv() {
                Ok(bytes_per_sec) => {
                    self.bytes_per_sec = Some(bytes_per_sec);
                    self.rx = None;
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => self.rx = None,
            }
        }
        let key = (
            source.hwnd,
            source.kind,
            encode.fps,
            encode.resolution_mode,
            encode.bitrate_mode,
            tracks,
        );
        let changed = self.key != Some(key);
        if !changed && self.at.is_some_and(|at| at.elapsed() < ESTIMATE_EVERY) {
            return;
        }
        if changed {
            // The old answer was for something else.
            self.bytes_per_sec = None;
        }
        let (tx, rx) = mpsc::channel();
        let (source, encode) = (source.clone(), encode.clone());
        let spawned = std::thread::Builder::new()
            .name("RecordingRateEstimate".into())
            .spawn(move || {
                let _ = tx.send(estimated_bytes_per_sec(&source, &encode, tracks));
            });
        if spawned.is_ok() {
            self.rx = Some(rx);
        }
        self.key = Some(key);
        self.at = Some(Instant::now());
    }
}

/// What the checks have found and said during the running recording.
#[derive(Default)]
pub(super) struct RecordingChecks {
    /// Bytes a second the recording's file has been growing by, and the
    /// recorded time it was measured at.
    measured: Option<(u64, Duration)>,
    mics_checked: bool,
    low_space_said: bool,
}

impl App {
    /// Runs every frame: keeps the estimate for the next recording up to
    /// date, or checks the running one, and says on screen -- where a game
    /// may have it -- what needs seeing to.
    pub(super) fn run_recording_checks(&mut self, s: &'static Strings) {
        let Some(active) = self.session.active() else {
            let tracks = self.selected_audio.iter().filter(|&&on| on).count()
                + self.selected_app_audio.iter().filter(|&&on| on).count();
            let source = self.selected_source.and_then(|i| self.sources.get(i));
            let encode = self.encode_settings();
            self.rate_estimate.update(source, &encode, tracks);
            return;
        };
        let elapsed = active.elapsed();
        let checks = &mut self.recording_checks;

        let measure_due = elapsed >= MEASURE_AFTER
            && checks
                .measured
                .is_none_or(|(_, at)| elapsed >= at + MEASURE_EVERY);
        if measure_due && let Ok(meta) = std::fs::metadata(&active.output_path) {
            let rate = meta.len() / elapsed.as_secs().max(1);
            checks.measured = (rate > 0).then_some((rate, elapsed));
        }

        let mut say = Vec::new();
        if !checks.mics_checked && elapsed >= MIC_CHECK_AFTER {
            checks.mics_checked = true;
            let loudest: Vec<f32> = active
                .audio_levels()
                .iter()
                .map(AudioLevel::loudest)
                .collect();
            let silent = silent_microphones(&self.last_recording_audio_labels, &loudest);
            if !silent.is_empty() {
                let message = silent_microphones_message(s, &silent);
                self.notice = Some(message.clone());
                say.push(message);
            }
        }
        if !self.recording_checks.low_space_said
            && let Some(left) = self.recording_time_left()
            && is_short(left)
        {
            self.recording_checks.low_space_said = true;
            say.push(
                s.toast_low_space_template
                    .replace("{left}", &format_time_left(left, s)),
            );
        }
        if !say.is_empty() {
            self.toast = Some(Toast::new(say.join("\n")));
        }
    }

    /// How long recording can go on before the drive fills: at the running
    /// recording's measured rate, or the next one's estimated rate.
    fn recording_time_left(&self) -> Option<Duration> {
        let rate = if self.session.active().is_some() {
            self.recording_checks.measured.map(|(rate, _)| rate)
        } else {
            self.rate_estimate.bytes_per_sec
        };
        recording_time_left(self.free_space.bytes?, rate?)
    }

    /// The free-space line's "about 3 h of recording", and whether that's
    /// little enough to be a warning.
    pub(super) fn time_left_text(&self, s: &Strings) -> Option<(String, bool)> {
        let left = self.recording_time_left()?;
        Some((format_time_left(left, s), is_short(left)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk_space::MIN_FREE_BYTES;
    use crate::i18n::Lang;
    use std::time::Duration;

    const MB: u64 = 1024 * 1024;

    #[test]
    fn time_left_counts_down_to_where_recording_stops_not_to_empty() {
        // 1 MB a second with 600 MB more than the 500 MB reserve: 600 s.
        assert_eq!(
            recording_time_left(MIN_FREE_BYTES + 600 * MB, MB),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            recording_time_left(MIN_FREE_BYTES / 2, MB),
            Some(Duration::ZERO)
        );
        assert_eq!(recording_time_left(100 * MB, 0), None, "rate unknown");
    }

    #[test]
    fn time_left_reads_in_hours_then_minutes() {
        let s = Lang::En.strings();
        let read = |secs| format_time_left(Duration::from_secs(secs), s);
        assert_eq!(read(14 * 3600 + 1800), "about 14 h of recording");
        assert_eq!(read(3 * 3600 + 25 * 60), "about 3 h 25 min of recording");
        assert_eq!(read(3600 + 7 * 60 + 30), "about 1 h 5 min of recording");
        assert_eq!(read(45 * 60 + 59), "about 45 min of recording");
        assert_eq!(read(59), "under a minute of recording");
        for lang in [Lang::En, Lang::Ja] {
            let text = format_time_left(Duration::from_secs(2 * 3600), lang.strings());
            assert!(
                text.contains('2') && !text.contains('{'),
                "{lang:?}: {text}"
            );
        }
    }

    #[test]
    fn only_little_time_left_is_a_warning() {
        assert!(is_short(Duration::from_secs(9 * 60)));
        assert!(!is_short(Duration::from_secs(10 * 60)));
    }

    #[test]
    fn a_microphone_that_has_had_no_sound_at_all_is_flagged() {
        let mic = |name: &str| TrackLabel {
            kind: TrackKind::Microphone,
            name: name.into(),
        };
        let labels = [
            TrackLabel {
                kind: TrackKind::Speakers,
                name: "Speakers".into(),
            },
            mic("Yeti"),
            mic("Headset"),
            TrackLabel {
                kind: TrackKind::App,
                name: "Discord".into(),
            },
        ];
        // Speakers and apps are often silent; a quiet but working mic still
        // has its noise floor; only digital silence is flagged.
        let loudest = [0.0, 0.0, 0.002, 0.0];
        assert_eq!(silent_microphones(&labels, &loudest), vec!["Yeti"]);
        assert!(silent_microphones(&labels, &[0.5, 0.01, 0.01, 0.0]).is_empty());
    }

    #[test]
    fn the_silent_mic_warning_names_the_mics_in_every_language() {
        for lang in [Lang::En, Lang::Ja] {
            let text = silent_microphones_message(lang.strings(), &["Yeti", "Headset"]);
            assert!(text.contains("Yeti, Headset"), "{lang:?}: {text}");
            assert!(!text.contains('{'), "{lang:?}: {text}");
        }
    }
}
