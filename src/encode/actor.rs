use crate::capture::CaptureControl;
use crate::capture::audio::TrackCoverage;
use crate::capture::frame_rate::LiveFps;
use crate::disk_space::DiskSpaceGuard;
use crate::encode::{Container, EncoderParams, RecordingCommand, RecordingWriter, VideoSlot};
use crate::error::{AppError, ResultExt};
use crate::recording_naming::{RecordingNaming, reserve_finished_recording_path};
use crate::types::{AudioLevel, AudioSamples, TrackId, VideoFrame};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinHandle;

/// How often the recording loop re-checks free disk space -- see
/// `DiskSpaceGuard`'s doc comment for why this is on an interval, not every
/// frame.
const DISK_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// How long video writes may fail one after another, with none getting
/// through, before the recorder gives up: an encoder or GPU that has given
/// out (a driver reset, say) loses about this much video instead of the
/// rest of the recording, with "Recording" showing all along.
pub(crate) const GIVE_UP_AFTER: Duration = Duration::from_secs(2);

/// Why the recorder stopped itself before it was stopped, if it did --
/// shared with the dashboard, which says so once the recording is saved
/// (the file up to that point is finalized as usual).
#[derive(Clone, Default, Debug)]
pub struct StoppedItself {
    /// Free disk space dropped below `disk_space::MIN_FREE_BYTES`.
    disk_full: Arc<AtomicBool>,
    /// Writes kept failing (see `FailingWrites`): the last failure.
    gave_up: Arc<std::sync::Mutex<Option<String>>>,
}

impl StoppedItself {
    pub fn disk_full(&self) -> bool {
        self.disk_full.load(Ordering::Relaxed)
    }

    /// The flag `DiskSpaceGuard::should_stop` sets.
    pub(crate) fn disk_full_flag(&self) -> &AtomicBool {
        &self.disk_full
    }

    /// The last failed write's error, if writes kept failing until the
    /// recorder gave up.
    pub fn gave_up(&self) -> Option<String> {
        self.gave_up
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn give_up(&self, why: String) {
        *self
            .gave_up
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(why);
    }
}

/// Writes failing one after another.
#[derive(Default)]
struct FailingWrites {
    /// When the current run of failures began.
    since: Option<Instant>,
    /// The latest failure.
    last: Option<String>,
}

impl FailingWrites {
    /// Notes how one write of `what` went, at `now`. Whether it's time to
    /// give up: writes have failed for `GIVE_UP_AFTER` with none getting
    /// through. Logs the first failure of a run, not each one: every frame
    /// and audio packet fails once something has given out.
    fn note(&mut self, result: Result<(), AppError>, what: &str, now: Instant) -> bool {
        match result {
            Ok(()) => {
                if self.since.take().is_some() {
                    tracing::info!("writes are getting through again");
                }
                false
            }
            Err(e) => {
                let since = *self.since.get_or_insert_with(|| {
                    tracing::error!("write_{what} error: {e}");
                    now
                });
                self.last = Some(e.to_string());
                now.duration_since(since) >= GIVE_UP_AFTER
            }
        }
    }
}

/// How the writes are going, video and audio apart: either can give out
/// while the other carries on. A driver reset most likely takes only the
/// video encoder, while audio, with an encoder of its own, goes on being
/// written -- 100 packets a second a track.
#[derive(Default)]
pub(crate) struct Writes {
    video: FailingWrites,
    audio: FailingWrites,
}

impl Writes {
    /// Notes how a frame's write went, at `now`. Whether to give up: video
    /// writes have all failed for `GIVE_UP_AFTER`, whatever audio did
    /// meanwhile -- the recording is its video.
    pub(crate) fn note_video(&mut self, result: Result<(), AppError>, now: Instant) -> bool {
        self.video.note(result, "video", now)
    }

    /// Notes how an audio packet's write went, at `now`. Audio failing on
    /// its own, with the video still written, doesn't end the recording (it
    /// never did); it's logged, once a run.
    pub(crate) fn note_audio(&mut self, result: Result<(), AppError>, now: Instant) {
        self.audio.note(result, "audio", now);
    }

    /// The last failed video write's error, once `note_video` has said to
    /// give up.
    pub(crate) fn video_failure(&mut self) -> String {
        self.video.last.take().unwrap_or_default()
    }
}

/// Spawns the RecordingActor on a dedicated blocking OS thread.
///
/// Recording is written to `temp_path` throughout (Media Foundation's sink writer needs
/// a fixed destination from the moment it opens the file, before the finish time is
/// known), as a fragmented MP4 (`Container::FragmentedMp4`): one cut off -- PolyRec
/// killed, a crash, a power cut -- reads back up to its last fragment, and is
/// recovered at the next start (see `session::recovery`). Once `finalize()` closes the
/// file, it's remuxed into a plain MP4 under whatever `naming` resolves to (see
/// `reserve_finished_recording_path`) inside `output_dir` -- resolved at finish time,
/// not start time, since the default naming mode's timestamp should reflect when the
/// recording actually finished.
///
/// Returns (command_sender, handle_resolving_to_the_final_renamed_path).
pub fn spawn_recording_actor(
    temp_path: PathBuf,
    output_dir: PathBuf,
    app_name: String,
    naming: RecordingNaming,
    params: EncoderParams,
    stopped_itself: StoppedItself,
    fps: LiveFps,
) -> (
    mpsc::Sender<RecordingCommand>,
    JoinHandle<Result<PathBuf, AppError>>,
) {
    let (tx, mut rx) = mpsc::channel::<RecordingCommand>(256);

    let audio_tracks = params.audio_tracks.len();
    let params = EncoderParams {
        container: Container::FragmentedMp4,
        ..params
    };
    let handle = tokio::task::spawn_blocking(move || {
        // Creating the writer creates its file, so one that didn't start
        // leaves an empty file behind otherwise.
        let (writer, settled_fps) =
            RecordingWriter::start(&temp_path, &params).inspect_err(|_| {
                let _ = std::fs::remove_file(&temp_path);
            })?;
        fps.set(settled_fps);

        let mut disk_guard = DiskSpaceGuard::new(output_dir.clone(), DISK_CHECK_INTERVAL);
        let mut wrote_any = false;
        let mut writes = Writes::default();

        while let Some(cmd) = rx.blocking_recv() {
            let gave_out = match cmd {
                RecordingCommand::WriteVideo(frame, _slot) => {
                    let written = writer.write_video(frame);
                    wrote_any |= written.is_ok();
                    writes.note_video(written, Instant::now())
                }
                RecordingCommand::WriteAudio(samples) => {
                    let track_idx = samples.track_id.0 as usize;
                    let written = writer.write_audio(track_idx, samples);
                    wrote_any |= written.is_ok();
                    writes.note_audio(written, Instant::now());
                    false
                }
                RecordingCommand::Stop => break,
            };
            if gave_out {
                let why = writes.video_failure();
                tracing::error!(
                    "video writes have failed for {GIVE_UP_AFTER:?}; stopping the recording: {why}"
                );
                stopped_itself.give_up(why);
                break;
            }

            if disk_guard.should_stop(&stopped_itself.disk_full, "recording") {
                break;
            }
        }

        tracing::info!("recorded video: {}", writer.video_stats());
        // Media Foundation won't finish a file with nothing written to it (a
        // recording stopped as soon as it started); nothing is lost with it.
        // One that holds something stays, under its temporary name.
        let finished_temp_path = writer.finalize().inspect_err(|_| {
            if !wrote_any {
                let _ = std::fs::remove_file(&temp_path);
            }
        })?;
        let final_path =
            reserve_finished_recording_path(&naming, &app_name, &output_dir).encode_ctx(
                format_args!("no name to save {} as", finished_temp_path.display()),
            )?;
        // Replaces the empty file that reserved the name. On failure the
        // recording stays where it is, under its temporary name.
        crate::encode::fmp4::finish(&finished_temp_path, &final_path, audio_tracks).inspect_err(
            |_| {
                let _ = std::fs::remove_file(&final_path);
            },
        )?;
        Ok(final_path)
    });

    (tx, handle)
}

/// Spawns a pump task forwarding video frames to the recording actor.
/// Increments `frame_count` for each frame forwarded.
///
/// At most `max_in_flight` of its frames are queued for (or being written
/// by) the actor at once: each forwarded frame holds a slot until the actor
/// has written it. The actor's command channel is shared with every audio
/// track and is deep enough for audio bursts (256 commands) -- without this
/// cap, a stalled encoder or disk could fill all of it with full frames
/// (8.3 MB each at 1080p, 33 MB at 4K) before capture felt any backpressure.
/// With it, a stall makes this pump wait for a slot, its 4-frame input
/// channel fills, and the capture loop blocks on its send -- the same
/// eventual backpressure as before, reached after far less memory. Audio
/// commands never wait on video slots.
pub fn spawn_video_pump(
    mut video_rx: mpsc::Receiver<VideoFrame>,
    recording_tx: mpsc::Sender<RecordingCommand>,
    frame_count: Arc<AtomicU64>,
    max_in_flight: usize,
) -> JoinHandle<()> {
    let slots = Arc::new(Semaphore::new(max_in_flight));
    tokio::spawn(async move {
        while let Some(frame) = video_rx.recv().await {
            frame_count.fetch_add(1, Ordering::Relaxed);
            let Ok(slot) = Arc::clone(&slots).acquire_owned().await else {
                break;
            };
            if recording_tx
                .send(RecordingCommand::WriteVideo(frame, VideoSlot::held(slot)))
                .await
                .is_err()
            {
                break;
            }
        }
    })
}

/// How often a pump whose capture has ended checks for silence to send --
/// about as often as WASAPI hands out packets.
const LOST_TRACK_POLL: Duration = Duration::from_millis(10);

/// Spawns a pump task forwarding audio samples for `track` to the recording
/// actor, noting each chunk's peak in `level` for the dashboard's meters.
///
/// If the track's capture ends while the recording goes on -- its device was
/// unplugged, the app it captured closed, or capturing it failed -- the pump
/// notes the loss (`CaptureControl::audio_source_lost`) and keeps the track
/// going as silence, from where its audio stopped, until the recording
/// stops. A track that stops getting samples stalls the whole file: Stop
/// could then never finish saving it.
pub fn spawn_audio_pump(
    mut audio_rx: mpsc::Receiver<AudioSamples>,
    recording_tx: mpsc::Sender<RecordingCommand>,
    level: AudioLevel,
    track: TrackId,
    ctrl: CaptureControl,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut coverage = TrackCoverage::new(track, ctrl.clock.elapsed());
        while let Some(samples) = audio_rx.recv().await {
            coverage.on_samples(&samples);
            level.record(&samples.samples);
            if recording_tx
                .send(RecordingCommand::WriteAudio(samples))
                .await
                .is_err()
            {
                return;
            }
        }
        if ctrl.is_stopped() {
            return;
        }
        tracing::warn!("audio track {track:?} lost its source; the rest of it is silence");
        ctrl.audio_source_lost(track);
        while !ctrl.is_stopped() {
            if !ctrl.is_paused()
                && let Some(silence) = coverage.silence_due(ctrl.clock.elapsed())
                && recording_tx
                    .send(RecordingCommand::WriteAudio(silence))
                    .await
                    .is_err()
            {
                return;
            }
            tokio::time::sleep(LOST_TRACK_POLL).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Codec;
    use crate::types::TrackId;
    use std::time::Duration;

    fn failed() -> Result<(), AppError> {
        Err(AppError::Encode("ProcessSample: device removed".into()))
    }

    /// An encoder or GPU that gives out fails every write from then on. The
    /// recorder used to log each one (60+ a second) and carry on, the
    /// dashboard showing "Recording" until Stop, and the video lost.
    #[test]
    fn two_seconds_of_nothing_but_failed_video_writes_is_giving_up() {
        let t0 = Instant::now();
        let mut writes = Writes::default();
        assert!(!writes.note_video(failed(), t0));
        assert!(!writes.note_video(failed(), t0 + Duration::from_millis(1500)));
        assert!(writes.note_video(failed(), t0 + GIVE_UP_AFTER));
        assert_eq!(
            writes.video.last.as_deref(),
            Some("Encode error: ProcessSample: device removed")
        );
    }

    /// A driver reset most likely takes only the video encoder: audio, with
    /// an encoder of its own, goes on being written -- 100 packets a second
    /// a track -- which mustn't keep a recording with no video going.
    #[test]
    fn video_failing_gives_up_even_while_audio_gets_through() {
        let t0 = Instant::now();
        let mut writes = Writes::default();
        let mut gave_up_at = None;
        for ms in (0..=2500).step_by(10) {
            let at = t0 + Duration::from_millis(ms);
            writes.note_audio(Ok(()), at);
            if writes.note_video(failed(), at) {
                gave_up_at = Some(ms);
                break;
            }
        }
        assert_eq!(gave_up_at, Some(2000));
    }

    /// Audio failing on its own, video still written, doesn't end the
    /// recording -- it never did.
    #[test]
    fn audio_failing_on_its_own_doesnt_end_the_recording() {
        let t0 = Instant::now();
        let mut writes = Writes::default();
        for ms in (0..=10_000).step_by(10) {
            let at = t0 + Duration::from_millis(ms);
            writes.note_audio(failed(), at);
            assert!(!writes.note_video(Ok(()), at), "{ms} ms");
        }
    }

    /// A write failing now and then, with others getting through, isn't
    /// giving up.
    #[test]
    fn a_failed_write_now_and_then_isnt_giving_up() {
        let t0 = Instant::now();
        let mut writes = Writes::default();
        for second in 0..10 {
            let at = t0 + Duration::from_secs(second);
            assert!(!writes.note_video(failed(), at));
            assert!(!writes.note_video(Ok(()), at + Duration::from_millis(500)));
        }
    }

    /// A recording stopped before anything reached it can't be finalized
    /// (Media Foundation won't finish a file with nothing in it), and that
    /// used to leave its `.tmp.mp4` behind for good: hidden from Recent,
    /// never cleaned up.
    #[tokio::test]
    async fn a_recording_stopped_before_anything_was_written_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, handle) = spawn_recording_actor(
            dir.path().join("game_recording_1.tmp.mp4"),
            dir.path().to_path_buf(),
            "game".into(),
            RecordingNaming::ProcessName,
            EncoderParams::tiny_test(Codec::H264, &[]),
            StoppedItself::default(),
            LiveFps::new(30),
        );
        tx.send(RecordingCommand::Stop).await.unwrap();
        assert!(handle.await.unwrap().is_err());
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[tokio::test]
    async fn pump_forwards_video_and_increments_counter() {
        let (cap_tx, cap_rx) = mpsc::channel::<VideoFrame>(4);
        let (rec_tx, mut rec_rx) = mpsc::channel::<RecordingCommand>(8);
        let frame_count = Arc::new(AtomicU64::new(0));

        let pump = spawn_video_pump(cap_rx, rec_tx, Arc::clone(&frame_count), 8);

        cap_tx
            .send(VideoFrame {
                pts: Duration::ZERO,
                data: vec![0u8; 64].into(),
            })
            .await
            .unwrap();
        drop(cap_tx); // close channel so pump exits

        pump.await.unwrap();
        assert_eq!(frame_count.load(Ordering::Relaxed), 1);
        assert!(matches!(
            rec_rx.recv().await,
            Some(RecordingCommand::WriteVideo(..))
        ));
    }

    #[tokio::test]
    async fn pump_forwards_audio() {
        let (cap_tx, cap_rx) = mpsc::channel::<AudioSamples>(4);
        let (rec_tx, mut rec_rx) = mpsc::channel::<RecordingCommand>(8);

        // Stopped: the capture's end is the recording's, not a lost source.
        let ctrl = CaptureControl::new();
        ctrl.request_stop();
        let pump = spawn_audio_pump(cap_rx, rec_tx, AudioLevel::default(), TrackId::new(0), ctrl);

        cap_tx
            .send(AudioSamples {
                track_id: TrackId::new(0),
                pts: Duration::ZERO,
                samples: vec![0.0f32; 10],
                sample_rate: 48000,
                channels: 2,
            })
            .await
            .unwrap();
        drop(cap_tx);

        pump.await.unwrap();
        assert!(matches!(
            rec_rx.recv().await,
            Some(RecordingCommand::WriteAudio(_))
        ));
    }

    fn tiny_frame() -> VideoFrame {
        VideoFrame {
            pts: Duration::ZERO,
            data: vec![0u8; 16].into(),
        }
    }

    /// Lets spawned pump tasks run until they block.
    async fn settle() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    fn drain_video(rx: &mut mpsc::Receiver<RecordingCommand>) -> Vec<RecordingCommand> {
        let mut out = Vec::new();
        while let Ok(cmd) = rx.try_recv() {
            assert!(matches!(cmd, RecordingCommand::WriteVideo(..)));
            out.push(cmd);
        }
        out
    }

    #[tokio::test]
    async fn video_pump_holds_at_most_max_in_flight_frames_for_the_actor() {
        let (cap_tx, cap_rx) = mpsc::channel::<VideoFrame>(16);
        // Far deeper than the cap, so only the cap can be what stops it.
        let (rec_tx, mut rec_rx) = mpsc::channel::<RecordingCommand>(64);
        let frame_count = Arc::new(AtomicU64::new(0));
        let _pump = spawn_video_pump(cap_rx, rec_tx, Arc::clone(&frame_count), 3);
        for _ in 0..10 {
            cap_tx.send(tiny_frame()).await.unwrap();
        }
        settle().await;

        // Held (not yet "written"): their slots stay taken.
        let mut queued = drain_video(&mut rec_rx);
        assert_eq!(queued.len(), 3);

        // The actor finishing one frame frees exactly one slot.
        drop(queued.pop());
        settle().await;
        queued.extend(drain_video(&mut rec_rx));
        assert_eq!(queued.len(), 3);

        // Finishing all of them lets the rest through.
        drop(queued);
        settle().await;
        let rest = drain_video(&mut rec_rx);
        assert_eq!(rest.len(), 3, "next batch, still capped");
        drop(rest);
        settle().await;
        assert_eq!(drain_video(&mut rec_rx).len(), 3);
    }

    fn audio_from(rec_rx: &mut mpsc::Receiver<RecordingCommand>) -> Vec<AudioSamples> {
        let mut audio = Vec::new();
        while let Ok(cmd) = rec_rx.try_recv() {
            if let RecordingCommand::WriteAudio(samples) = cmd {
                audio.push(samples);
            }
        }
        audio
    }

    /// A track whose capture ends while the recording goes on -- its device
    /// unplugged, say -- is noted as lost and goes on as silence, from where
    /// its audio stopped, until the recording stops: a stream that stops
    /// getting samples stalls the whole file.
    #[tokio::test]
    async fn a_track_whose_capture_ends_goes_on_as_silence_until_stopped() {
        let (cap_tx, cap_rx) = mpsc::channel::<AudioSamples>(4);
        let (rec_tx, mut rec_rx) = mpsc::channel::<RecordingCommand>(64);
        let ctrl = CaptureControl::new();
        let track = TrackId::new(1);
        let pump = spawn_audio_pump(cap_rx, rec_tx, AudioLevel::default(), track, ctrl.clone());

        // 10 ms of real audio, then the device is gone.
        let start = ctrl.clock.elapsed();
        cap_tx
            .send(AudioSamples {
                track_id: track,
                pts: start,
                samples: vec![0.5; 480 * 2],
                sample_rate: 48000,
                channels: 2,
            })
            .await
            .unwrap();
        drop(cap_tx);
        tokio::time::sleep(Duration::from_millis(500)).await;
        ctrl.request_stop();
        pump.await.unwrap();

        let audio = audio_from(&mut rec_rx);
        let (real, silence) = audio.split_first().unwrap();
        assert!(real.samples.iter().all(|&s| s == 0.5));
        assert!(!silence.is_empty(), "no silence after the capture ended");
        let mut covered_until = start + Duration::from_millis(10);
        for chunk in silence {
            assert_eq!(chunk.track_id, track);
            assert_eq!(chunk.pts, covered_until, "silence isn't contiguous");
            assert!(chunk.samples.iter().all(|&s| s == 0.0));
            covered_until +=
                Duration::from_nanos((chunk.samples.len() / 2) as u64 * 1_000_000_000 / 48000);
        }
        assert!(covered_until >= start + Duration::from_millis(300));
        assert_eq!(ctrl.lost_sources().audio, vec![track]);
    }

    /// A capture that ends because the recording stopped isn't a lost
    /// source, and adds no silence.
    #[tokio::test]
    async fn a_capture_ending_with_the_recording_adds_nothing() {
        let (cap_tx, cap_rx) = mpsc::channel::<AudioSamples>(4);
        let (rec_tx, mut rec_rx) = mpsc::channel::<RecordingCommand>(64);
        let ctrl = CaptureControl::new();
        let pump = spawn_audio_pump(
            cap_rx,
            rec_tx,
            AudioLevel::default(),
            TrackId::new(0),
            ctrl.clone(),
        );
        ctrl.request_stop();
        drop(cap_tx);
        pump.await.unwrap();
        assert!(audio_from(&mut rec_rx).is_empty());
        assert_eq!(ctrl.lost_sources(), crate::capture::LostSources::default());
    }

    #[tokio::test]
    async fn audio_keeps_flowing_while_video_slots_are_exhausted() {
        let (video_tx, video_rx) = mpsc::channel::<VideoFrame>(16);
        let (audio_tx, audio_rx) = mpsc::channel::<AudioSamples>(16);
        let (rec_tx, mut rec_rx) = mpsc::channel::<RecordingCommand>(64);
        let _video = spawn_video_pump(video_rx, rec_tx.clone(), Arc::new(AtomicU64::new(0)), 1);
        let _audio = spawn_audio_pump(
            audio_rx,
            rec_tx,
            AudioLevel::default(),
            TrackId::new(0),
            CaptureControl::new(),
        );

        video_tx.send(tiny_frame()).await.unwrap();
        video_tx.send(tiny_frame()).await.unwrap(); // blocked: no slot
        for _ in 0..5 {
            audio_tx
                .send(AudioSamples {
                    track_id: TrackId::new(0),
                    pts: Duration::ZERO,
                    samples: vec![0.0f32; 4],
                    sample_rate: 48000,
                    channels: 2,
                })
                .await
                .unwrap();
        }
        settle().await;

        let mut video = Vec::new();
        let mut audio = 0;
        while let Ok(cmd) = rec_rx.try_recv() {
            match cmd {
                RecordingCommand::WriteVideo(..) => video.push(cmd),
                RecordingCommand::WriteAudio(_) => audio += 1,
                RecordingCommand::Stop => unreachable!(),
            }
        }
        assert_eq!(video.len(), 1);
        assert_eq!(audio, 5);
    }
}
