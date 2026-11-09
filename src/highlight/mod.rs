//! Continuous background "Highlight" buffer: while enabled, rotates short
//! segment files to disk on its own capture threads (kept entirely separate
//! from manual recording's), and lets the caller save the last N seconds on
//! demand via `encode::highlight_export::concat_and_trim`. This module owns
//! only the segment-rotation actor itself -- lifecycle (start/stop, mutual
//! exclusion with manual recording, foreground-window tracking) lives in
//! `session::mod` and the dashboard.

use crate::capture::frame_rate::{FrameGrid, LiveFps};
use crate::disk_space::DiskSpaceGuard;
use crate::encode::actor::{GIVE_UP_AFTER, StoppedItself, Writes};
use crate::encode::{EncoderParams, RecordingCommand, RecordingWriter};
use crate::error::AppError;
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// Segment length is an internal implementation detail, not user-facing --
/// exposing a second "segment length" number alongside buffer duration would
/// make the settings UI harder to explain for no real benefit. 10s gives at
/// least 3 segments of trim granularity even at the minimum 30s buffer.
pub const HIGHLIGHT_SEGMENT_SECONDS: u32 = 10;

const HIGHLIGHT_CHANNEL_CAPACITY: usize = 256;

/// Segment files are named `highlight_seg_<id>.mp4`; nothing else in the
/// buffer's folder is theirs to delete (see `clear_leftover_segments`).
const SEGMENT_PREFIX: &str = "highlight_seg_";
const SEGMENT_SUFFIX: &str = ".mp4";

/// One finalized segment file in the rotating buffer. `duration` is the
/// segment's own re-based PTS span (see `spawn_highlight_actor`), not a wall
/// clock measurement -- it's what `encode::highlight_export::concat_and_trim`
/// needs to know how much each segment actually contributes.
#[derive(Debug, Clone)]
pub struct SegmentInfo {
    pub path: PathBuf,
    /// Where the segment file's time 0 is on the buffering session's clock:
    /// the time its first sample was written at, which Media Foundation's
    /// MP4 sink puts at 0 (see `segment_base`) -- a frame's slot, or a sound
    /// packet's capture time. Where it goes in a save.
    pub start: Duration,
    pub duration: Duration,
}

/// How much of the buffer the actor keeps: the newest segments that hold
/// `keep` between them, and never more than `max_segments` of them.
#[derive(Debug, Clone, Copy)]
pub struct Retention {
    keep: Duration,
    max_segments: usize,
}

impl Retention {
    /// Keeps `buffer_seconds` of segments `segment_seconds` long. The count
    /// cap -- twice the segments the buffer takes, and one more -- only
    /// bounds the disk used should durations ever read short; it takes a
    /// save every few seconds to reach it.
    pub fn new(buffer_seconds: u32, segment_seconds: u32) -> Self {
        let per_buffer = buffer_seconds.max(1).div_ceil(segment_seconds.max(1)) as usize;
        Self {
            keep: Duration::from_secs(u64::from(buffer_seconds.max(1))),
            max_segments: 2 * per_buffer + 1,
        }
    }

    /// Takes the segments `segments` no longer needs off its front (the
    /// oldest), for the caller to delete: those the newer ones cover `keep`
    /// without. By what they hold, not how many there are -- a save
    /// finalizes the open segment early, and evicting by count dropped a
    /// full segment for that short one, so a 30 s buffer saved 20-30 s.
    fn evict(&self, segments: &mut VecDeque<SegmentInfo>) -> Vec<SegmentInfo> {
        let mut held: Duration = segments.iter().map(|s| s.duration).sum();
        let mut evicted = Vec::new();
        while let Some(oldest) = segments.front() {
            let without = held.saturating_sub(oldest.duration);
            if without < self.keep && segments.len() <= self.max_segments {
                break;
            }
            held = without;
            evicted.extend(segments.pop_front());
        }
        evicted
    }
}

/// How finalizing a segment went.
#[derive(Debug, PartialEq)]
enum Finished {
    /// The segment's file, ready to be read.
    Kept(PathBuf),
    /// Nothing was written to it, so there was nothing to finalize.
    Empty,
    /// It had samples and didn't finalize: the encoder or the disk has
    /// given out. The error.
    Lost(String),
}

/// `writer.finalize()`'s `result` for a segment that had something written
/// to it, or not (`wrote_any`). Media Foundation won't finish a sink with
/// nothing written to it -- Stop right after a fresh rotation, or right
/// after save_now finalized the previous segment -- which loses nothing.
/// Every failed finalize used to be taken for that; one with samples in it
/// is the encoder or the disk giving out.
fn finished(result: Result<PathBuf, AppError>, wrote_any: bool) -> Finished {
    match result {
        Ok(path) => Finished::Kept(path),
        Err(_) if !wrote_any => Finished::Empty,
        Err(e) => Finished::Lost(e.to_string()),
    }
}

/// Sent on `spawn_highlight_actor`'s returned sender to force the
/// currently-open segment to finalize immediately instead of waiting out the
/// rest of its `segment_seconds` -- used when saving a highlight, so the save
/// always includes everything captured up to the moment it was requested,
/// rather than missing up to one full segment's worth of the most recent
/// activity. The reply fires once the forced segment has been pushed onto
/// the shared `segments` deque and is safe to read.
pub type SaveNowRequest = oneshot::Sender<()>;

/// Spawns the highlight rotation actor on a dedicated blocking OS thread.
/// Segments are written to `segment_dir`, kept as `retention` says (older
/// ones deleted as new ones finalize), and tracked in `segments` for
/// `encode::highlight_export::concat_and_trim` to read later. Ends (dropping
/// no in-progress segment silently -- the current one is always finalized
/// first) when `RecordingCommand::Stop` arrives or free disk space on
/// `segment_dir`'s volume drops below `disk_space::MIN_FREE_BYTES` (noted in
/// `stopped`).
pub fn spawn_highlight_actor(
    segment_dir: PathBuf,
    params: EncoderParams,
    segment_seconds: u32,
    retention: Retention,
    segments: Arc<Mutex<VecDeque<SegmentInfo>>>,
    stopped: StoppedItself,
    fps: LiveFps,
) -> (
    mpsc::Sender<RecordingCommand>,
    mpsc::UnboundedSender<SaveNowRequest>,
    JoinHandle<Result<(), AppError>>,
) {
    let (tx, mut rx) = mpsc::channel::<RecordingCommand>(HIGHLIGHT_CHANNEL_CAPACITY);
    let (save_now_tx, mut save_now_rx) = mpsc::unbounded_channel::<SaveNowRequest>();

    let handle = tokio::task::spawn_blocking(move || {
        let mut params = params;
        let segment_duration = Duration::from_secs(segment_seconds.max(1) as u64);
        let mut disk_guard = DiskSpaceGuard::new(segment_dir.clone());
        let mut writes = Writes::default();
        // The slot after the last segment's last frame, on the frame grid
        // (by its fps) of the buffering session's clock.
        let mut video_end: Option<(u32, u64)> = None;

        loop {
            let seg_id = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let seg_path = segment_dir.join(format!("{SEGMENT_PREFIX}{seg_id}{SEGMENT_SUFFIX}"));

            // Later segments start straight at the rate the first one settled
            // on. Creating the writer creates its file, so one that didn't
            // start leaves an empty file behind otherwise.
            let (writer, settled_fps) =
                RecordingWriter::start(&seg_path, &params).inspect_err(|e| {
                    let _ = std::fs::remove_file(&seg_path);
                    stopped.give_up(e.to_string());
                })?;
            params.fps = settled_fps;
            fps.set(settled_fps);
            let frame_time = Duration::from_secs(1) / settled_fps.max(1);
            let grid = FrameGrid::new(settled_fps);
            let video_end_here = video_end
                .filter(|&(fps, _)| fps == settled_fps)
                .map(|(_, slot)| slot);

            let seg_start = Instant::now();
            // Capture threads stamp frame/sample PTS from the shared
            // RecordingClock spanning the whole buffering session, not reset
            // per segment (see session/mod.rs) -- re-base here instead of
            // touching the capture layer, so each segment's own file starts
            // at PTS ~0 like a normal recording does.
            let mut pts_base: Option<Duration> = None;
            let mut base_for = |pts: Duration| {
                *pts_base.get_or_insert_with(|| segment_base(grid, pts, video_end_here, &writer))
            };
            // Where whatever was written last ends, re-based: the segment's
            // duration. Up to the last sample's *start*, the next segment
            // began on the time of this one's last frame.
            let mut end = Duration::ZERO;
            let mut wrote_any = false;
            let mut stop_requested = false;
            let mut save_now_reply: Option<SaveNowRequest> = None;

            loop {
                match rx.blocking_recv() {
                    Some(RecordingCommand::WriteVideo(mut frame, _slot)) => {
                        let base = base_for(frame.pts);
                        frame.pts = frame.pts.saturating_sub(base);
                        end = end.max(frame.pts + frame_time);
                        let written = writer.write_video(frame);
                        wrote_any |= written.is_ok();
                        if writes.note_video(written, Instant::now()) {
                            let why = writes.video_failure();
                            tracing::error!(
                                "Highlight video writes have failed for {GIVE_UP_AFTER:?}; stopping Highlight buffering: {why}"
                            );
                            stopped.give_up(why);
                            stop_requested = true;
                            break;
                        }
                    }
                    Some(RecordingCommand::WriteAudio(mut samples)) => {
                        let base = base_for(samples.pts);
                        samples.pts = samples.pts.saturating_sub(base);
                        end = end.max(samples.pts + samples.duration());
                        let track_idx = samples.track_id.0 as usize;
                        let written = writer.write_audio(track_idx, samples);
                        wrote_any |= written.is_ok();
                        writes.note_audio(written, Instant::now());
                    }
                    Some(RecordingCommand::Stop) | None => {
                        stop_requested = true;
                        break;
                    }
                }

                if let Ok(reply) = save_now_rx.try_recv() {
                    save_now_reply = Some(reply);
                    break;
                }
                if seg_start.elapsed() >= segment_duration {
                    break;
                }
                if disk_guard.should_stop(stopped.disk_full_flag(), "highlight buffering") {
                    stop_requested = true;
                    break;
                }
            }

            if let (Some(base), Some(slot)) = (pts_base, writer.video_end()) {
                video_end = Some((settled_fps, grid.slot_at_or_before(base) + slot));
            }
            // The file's 0, and how long it runs from there.
            let first_written = writer.first_written().unwrap_or_default();
            let start = pts_base.map(|base| base + first_written);
            let end = end.saturating_sub(first_written);
            // Later segments read texture frames back from the start if this
            // one had to (see `RecordingWriter::takes_textures`).
            if let Some(gpu) = &mut params.gpu {
                gpu.direct = writer.takes_textures();
            }
            match finished(writer.finalize(), wrote_any) {
                Finished::Kept(finished_path) => {
                    let info = SegmentInfo {
                        path: finished_path,
                        // Something was written: there's a first sample.
                        start: start.unwrap_or_default(),
                        duration: end,
                    };
                    let mut guard = segments
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.push_back(info);
                    for old in retention.evict(&mut guard) {
                        let _ = std::fs::remove_file(&old.path);
                    }
                }
                Finished::Empty => {
                    tracing::debug!("highlight segment had no samples, discarding");
                    let _ = std::fs::remove_file(&seg_path);
                }
                Finished::Lost(why) => {
                    tracing::error!("a Highlight segment didn't finalize; stopping: {why}");
                    let _ = std::fs::remove_file(&seg_path);
                    // Writes that gave out say more than the finalize after.
                    if stopped.gave_up().is_none() {
                        stopped.give_up(why);
                    }
                    stop_requested = true;
                }
            }
            if let Some(reply) = save_now_reply {
                let _ = reply.send(());
            }

            if stop_requested {
                break;
            }
        }
        match stopped.gave_up() {
            Some(why) => Err(AppError::Encode(why)),
            None => Ok(()),
        }
    });

    (tx, save_now_tx, handle)
}

/// What a segment whose first sample was captured at `first` times its
/// samples from: the start of that sample's frame slot, so every segment's
/// frames sit on the one frame grid of the buffering session's clock and a
/// save keeps the beat across its joins. (Media Foundation's MP4 sink then
/// moves everything in the file back by the time the first sample was
/// written at, to put it at 0: `SegmentInfo::start` says where that is.)
/// Its video goes on from the slot after the segment before's last frame
/// (`video_end`): the slots before that are that segment's, and a frame of
/// this one's there would come between two of it.
fn segment_base(
    grid: FrameGrid,
    first: Duration,
    video_end: Option<u64>,
    writer: &RecordingWriter,
) -> Duration {
    let slot = grid.slot_at_or_before(first);
    if let Some(end) = video_end.filter(|&end| end > slot) {
        writer.start_video_at(end - slot);
    }
    grid.time(slot)
}

/// Deletes every segment file currently tracked and empties the deque --
/// used when highlight buffering is disabled or restarted for a different
/// foreground window (segments from a different app/resolution can't be
/// concatenated with new ones, see `encode::highlight_export::concat_and_trim`).
pub fn discard_segments(segments: &Mutex<VecDeque<SegmentInfo>>) {
    let mut guard = segments
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for seg in guard.drain(..) {
        let _ = std::fs::remove_file(&seg.path);
    }
}

/// Deletes the segment files an earlier buffering session left in
/// `segment_dir` (see `SessionManager::start_highlight_buffering`) -- only
/// those: anything else put there stays. And all of it stays when the
/// folder is a link (a junction or symlink) to somewhere else, which this
/// used to empty, whatever was in it; that's an error.
pub fn clear_leftover_segments(segment_dir: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(segment_dir)?.is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{} links to another folder; Highlight won't buffer there",
                segment_dir.display()
            ),
        ));
    }
    for entry in std::fs::read_dir(segment_dir)?.flatten() {
        let is_file = entry.file_type().is_ok_and(|t| t.is_file());
        if is_file && is_segment_file_name(&entry.file_name()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

fn is_segment_file_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.starts_with(SEGMENT_PREFIX) && name.ends_with(SEGMENT_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Codec;
    use crate::types::{AudioSamples, TrackId, VideoFrame};

    fn tiny_video_frame(pts: Duration) -> VideoFrame {
        VideoFrame {
            pts,
            data: vec![0u8; 64 * 64 * 4].into(),
        }
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn segment(duration: Duration) -> SegmentInfo {
        SegmentInfo {
            path: PathBuf::from("highlight_seg_0.mp4"),
            start: Duration::ZERO,
            duration,
        }
    }

    fn durations<'a>(segments: impl IntoIterator<Item = &'a SegmentInfo>) -> Vec<Duration> {
        segments.into_iter().map(|s| s.duration).collect()
    }

    fn spawn(
        dir: &Path,
        params: EncoderParams,
        segment_seconds: u32,
        retention: Retention,
        segments: &Arc<Mutex<VecDeque<SegmentInfo>>>,
        stopped: &StoppedItself,
    ) -> (
        mpsc::Sender<RecordingCommand>,
        mpsc::UnboundedSender<SaveNowRequest>,
        JoinHandle<Result<(), AppError>>,
    ) {
        spawn_highlight_actor(
            dir.to_path_buf(),
            params,
            segment_seconds,
            retention,
            Arc::clone(segments),
            stopped.clone(),
            LiveFps::new(30),
        )
    }

    /// A save finalizes the open segment early, so the buffer holds a short
    /// one. Evicting by count (3 for a 30 s buffer) dropped a full 10 s
    /// segment for it, and the save after that got 23 s of a 30 s buffer.
    #[test]
    fn a_short_segment_doesnt_push_out_a_full_one() {
        let retention = Retention::new(30, 10);
        let mut kept = VecDeque::from([segment(secs(10)), segment(secs(10)), segment(secs(10))]);
        kept.push_back(segment(secs(3)));
        assert!(retention.evict(&mut kept).is_empty());
        assert_eq!(durations(&kept), [secs(10), secs(10), secs(10), secs(3)]);
    }

    #[test]
    fn segments_the_rest_cover_the_buffer_without_are_evicted_oldest_first() {
        let retention = Retention::new(30, 10);
        let mut kept: VecDeque<_> = [3, 10, 10, 10, 10]
            .map(|s| segment(secs(s)))
            .into_iter()
            .collect();
        let evicted = retention.evict(&mut kept);
        assert_eq!(durations(&evicted), [secs(3), secs(10)]);
        assert_eq!(durations(&kept), [secs(10), secs(10), secs(10)]);
    }

    /// Whatever the durations say, the buffer never grows past twice its
    /// segments and one more: 7 for 30 s of 10 s segments.
    #[test]
    fn the_segment_count_stays_capped_whatever_the_durations_say() {
        let retention = Retention::new(30, 10);
        let mut kept: VecDeque<_> = (0..10).map(|_| segment(Duration::ZERO)).collect();
        assert_eq!(retention.evict(&mut kept).len(), 3);
        assert_eq!(kept.len(), 7);
    }

    /// A segment lasts until whatever was written last ends, not until it
    /// starts: counting to the start, the next segment began on the time of
    /// this one's last frame, at every seam.
    #[tokio::test]
    async fn a_segment_lasts_until_whatever_was_written_last_ends() {
        let dir = tempfile::tempdir().unwrap();
        let segments = Arc::new(Mutex::new(VecDeque::new()));
        let stopped = StoppedItself::default();
        let (tx, _save_now_tx, handle) = spawn(
            dir.path(),
            EncoderParams::tiny_test(Codec::H264, &[(48_000, 2)]),
            60,
            Retention::new(60, 60),
            &segments,
            &stopped,
        );

        // Capture's clock runs from the session's start; the segment starts
        // at its first sample.
        let base = secs(1);
        for at in [0, 100] {
            tx.send(RecordingCommand::video(tiny_video_frame(
                base + Duration::from_millis(at),
            )))
            .await
            .unwrap();
        }
        tx.send(RecordingCommand::WriteAudio(AudioSamples {
            track_id: TrackId::new(0),
            pts: base + Duration::from_millis(150),
            samples: vec![0.0; 480 * 2],
            sample_rate: 48_000,
            channels: 2,
        }))
        .await
        .unwrap();
        tx.send(RecordingCommand::Stop).await.unwrap();
        handle.await.unwrap().unwrap();

        let ends_with_the_audio = Duration::from_millis(160);
        assert_eq!(
            durations(segments.lock().unwrap().iter()),
            [ends_with_the_audio]
        );
    }

    /// Where the file's 0 is on the buffering session's clock, which a save
    /// places it at (see `encode::highlight_export`): for a segment that
    /// begins with a frame, the start of that frame's slot, where it's
    /// written. (One that begins with sound is in
    /// `a_save_keeps_one_frame_rate_across_its_joins`.)
    #[tokio::test]
    async fn a_segment_starts_where_its_first_sample_was_written() {
        let dir = tempfile::tempdir().unwrap();
        let segments = Arc::new(Mutex::new(VecDeque::new()));
        let stopped = StoppedItself::default();
        let (tx, _save_now_tx, handle) = spawn(
            dir.path(),
            EncoderParams::tiny_test(Codec::H264, &[]),
            60,
            Retention::new(60, 60),
            &segments,
            &stopped,
        );
        // 10 ms into the 30 fps slot that starts at 1.5 s.
        for at in [1510, 1610] {
            tx.send(RecordingCommand::video(tiny_video_frame(
                Duration::from_millis(at),
            )))
            .await
            .unwrap();
        }
        tx.send(RecordingCommand::Stop).await.unwrap();
        handle.await.unwrap().unwrap();

        let starts: Vec<_> = segments.lock().unwrap().iter().map(|s| s.start).collect();
        assert_eq!(starts, [Duration::from_millis(1500)]);
    }

    /// The video's last frame, a frame's time long at the segment's rate.
    #[tokio::test]
    async fn a_segment_of_video_lasts_until_its_last_frame_ends() {
        let dir = tempfile::tempdir().unwrap();
        let segments = Arc::new(Mutex::new(VecDeque::new()));
        let stopped = StoppedItself::default();
        let (tx, _save_now_tx, handle) = spawn(
            dir.path(),
            EncoderParams::tiny_test(Codec::H264, &[]),
            60,
            Retention::new(60, 60),
            &segments,
            &stopped,
        );
        for at in [0, 100] {
            tx.send(RecordingCommand::video(tiny_video_frame(
                Duration::from_millis(at),
            )))
            .await
            .unwrap();
        }
        tx.send(RecordingCommand::Stop).await.unwrap();
        handle.await.unwrap().unwrap();

        assert_eq!(
            durations(segments.lock().unwrap().iter()),
            [Duration::from_millis(100) + secs(1) / 30]
        );
    }

    /// 10 ms of audio captured at `pts`, as WASAPI hands it over.
    fn audio_packet(pts: Duration) -> RecordingCommand {
        RecordingCommand::WriteAudio(AudioSamples {
            track_id: TrackId::new(0),
            pts,
            samples: vec![0.0; 480 * 2],
            sample_rate: 48_000,
            channels: 2,
        })
    }

    /// Ends the open segment with `last`: once the actor has taken
    /// everything sent before, asks it to save now -- which it does after
    /// the next sample -- and sends `last`.
    async fn end_segment_with(
        tx: &mpsc::Sender<RecordingCommand>,
        save_now_tx: &mpsc::UnboundedSender<SaveNowRequest>,
        last: RecordingCommand,
    ) {
        while tx.capacity() < tx.max_capacity() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Done writing what it took last, and waiting for more.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let (reply_tx, reply_rx) = oneshot::channel();
        save_now_tx.send(reply_tx).unwrap();
        tx.send(last).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), reply_rx)
            .await
            .expect("save_now reply timed out")
            .expect("save_now reply sender dropped");
    }

    /// A save is one video at the buffer's frame rate, joins and all: every
    /// frame lasts as long as the next, as in a recording. Each segment
    /// used to keep its own frame timing, from its first sample -- often an
    /// audio packet a few milliseconds after the segment before's last
    /// frame -- so at each join the frames came off the beat, with an extra
    /// one in between, and editors took the save for a variable-frame-rate
    /// video.
    #[tokio::test]
    async fn a_save_keeps_one_frame_rate_across_its_joins() {
        let dir = tempfile::tempdir().unwrap();
        let segments = Arc::new(Mutex::new(VecDeque::new()));
        let stopped = StoppedItself::default();
        let (tx, save_now_tx, handle) = spawn(
            dir.path(),
            EncoderParams::tiny_test(Codec::H264, &[(48_000, 2)]),
            60,
            Retention::new(60, 60),
            &segments,
            &stopped,
        );
        let ms = Duration::from_millis;
        let frame = |n: u64| RecordingCommand::video(tiny_video_frame(secs(n) / 30));
        // Sends frames `frames` at 30 fps and audio every 10 ms from
        // `from_ms` up to `to_ms`, in the order they were captured.
        let send = |frames: std::ops::Range<u64>, from_ms: u64, to_ms: u64| {
            let tx = tx.clone();
            async move {
                let mut events: Vec<(Duration, RecordingCommand)> = frames
                    .map(|n| (secs(n) / 30, frame(n)))
                    .chain(
                        (from_ms..to_ms)
                            .step_by(10)
                            .map(|t| (ms(t), audio_packet(ms(t)))),
                    )
                    .collect();
                events.sort_by_key(|(at, _)| *at);
                for (_, event) in events {
                    tx.send(event).await.unwrap();
                }
            }
        };

        // The first segment: frames 0-9, to the audio at 300 ms.
        send(0..10, 0, 300).await;
        end_segment_with(&tx, &save_now_tx, audio_packet(ms(300))).await;
        // The second begins with the audio at 310 ms, between frames 9
        // (300 ms) and 10 (333 ms): frames 10-19, to the audio at 660 ms.
        send(0..0, 310, 320).await;
        send(10..20, 320, 660).await;
        end_segment_with(&tx, &save_now_tx, audio_packet(ms(660))).await;
        // The third begins with frame 20, captured 6 ms after its slot's
        // start (667 ms), the audio at 670 ms after it.
        tx.send(RecordingCommand::video(tiny_video_frame(
            secs(20) / 30 + ms(6),
        )))
        .await
        .unwrap();
        send(21..30, 670, 1000).await;
        tx.send(RecordingCommand::Stop).await.unwrap();
        handle.await.unwrap().unwrap();

        let saved: Vec<SegmentInfo> = segments.lock().unwrap().iter().cloned().collect();
        assert_eq!(saved.len(), 3, "three segments");
        // The second file runs from its first sound, at 310 ms, to the end
        // of its last, at 670 ms.
        assert_eq!((saved[1].start, saved[1].duration), (ms(310), ms(360)));
        let output = dir.path().join("saved.mp4");
        crate::encode::highlight_export::concat_and_trim(&saved, 3600, &output)
            .expect("concat_and_trim");
        let times: Vec<i64> = crate::encode::highlight_export::video_sample_times(&output)
            .into_iter()
            .map(|(time, _)| time)
            .collect();
        assert_eq!(times.len(), 30, "a frame a slot, none extra: {times:?}");
        // 1/30 s in 100 ns units, to the unit reading the file rounds to.
        let frame_hns = 10_000_000 / 30;
        assert!(
            times
                .windows(2)
                .all(|pair| (pair[1] - pair[0] - frame_hns).abs() <= 1),
            "frames 1/30 s apart: {times:?}"
        );
    }

    /// A writer that can't start (an encoder that won't open, a folder
    /// that's gone) ended the actor with its error dropped; the dashboard
    /// started it again on its next pass, and again, with nothing shown.
    #[tokio::test]
    async fn a_writer_that_cant_start_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let segments = Arc::new(Mutex::new(VecDeque::new()));
        let stopped = StoppedItself::default();
        let (_tx, _save_now_tx, handle) = spawn(
            &dir.path().join("gone"),
            EncoderParams::tiny_test(Codec::H264, &[]),
            60,
            Retention::new(60, 60),
            &segments,
            &stopped,
        );
        assert!(handle.await.unwrap().is_err());
        assert!(stopped.gave_up().is_some());
    }

    /// Finalizing failed for a segment with samples in it: that's the
    /// segment lost -- the encoder or disk has given out -- not one that was
    /// empty, as every failed finalize used to be taken for.
    #[test]
    fn a_segment_with_samples_that_wont_finalize_is_lost_not_empty() {
        let failed = || Err(AppError::Encode("Finalize: 0xC00D4A44".into()));
        assert_eq!(finished(failed(), false), Finished::Empty);
        assert!(
            matches!(finished(failed(), true), Finished::Lost(why) if why.contains("Finalize"))
        );
        assert_eq!(
            finished(Ok(PathBuf::from("a.mp4")), true),
            Finished::Kept(PathBuf::from("a.mp4"))
        );
    }

    /// A save right after a rotation can end a segment holding a few audio
    /// packets and no frame yet: it finalizes, and buffering goes on.
    #[tokio::test]
    async fn a_segment_of_audio_alone_doesnt_stop_buffering() {
        let dir = tempfile::tempdir().unwrap();
        let segments = Arc::new(Mutex::new(VecDeque::new()));
        let stopped = StoppedItself::default();
        let (tx, _save_now_tx, handle) = spawn(
            dir.path(),
            EncoderParams::tiny_test(Codec::H264, &[(48_000, 2)]),
            60,
            Retention::new(60, 60),
            &segments,
            &stopped,
        );
        tx.send(RecordingCommand::WriteAudio(AudioSamples {
            track_id: TrackId::new(0),
            pts: Duration::ZERO,
            samples: vec![0.0; 480 * 2],
            sample_rate: 48_000,
            channels: 2,
        }))
        .await
        .unwrap();
        tx.send(RecordingCommand::Stop).await.unwrap();
        assert!(handle.await.unwrap().is_ok());
        assert_eq!(stopped.gave_up(), None);
        assert_eq!(segments.lock().unwrap().len(), 1, "kept, audio and all");
    }

    /// Starting cleared every file in the buffer's folder, whatever it was.
    #[test]
    fn clearing_leftovers_deletes_only_segment_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str| {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"x").unwrap();
            path
        };
        let leftover = file("highlight_seg_1.mp4");
        let others = [
            file("notes.txt"),
            file("highlight_seg_2.mp4.txt"),
            file("inner/highlight_seg_3.mp4"),
        ];
        clear_leftover_segments(dir.path()).unwrap();
        assert!(!leftover.exists());
        for path in others {
            assert!(path.exists(), "{} was deleted", path.display());
        }
    }

    /// A buffer folder that's a junction to somewhere else: starting
    /// emptied wherever it pointed.
    #[test]
    fn a_buffer_folder_that_links_elsewhere_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let files = ["highlight_seg_1.mp4", "precious.txt"].map(|name| {
            let path = elsewhere.join(name);
            std::fs::write(&path, b"x").unwrap();
            path
        });
        let link = dir.path().join("_highlight_buffer");
        let made = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&link)
            .arg(&elsewhere)
            .output()
            .unwrap();
        assert!(made.status.success(), "mklink /J: {made:?}");

        assert!(clear_leftover_segments(&link).is_err());
        for path in files {
            assert!(path.exists(), "{} was deleted", path.display());
        }
    }

    #[tokio::test]
    async fn rotation_deletes_old_segments_and_bounds_disk_usage() {
        let dir = tempfile::tempdir().unwrap();
        let segments = Arc::new(Mutex::new(VecDeque::new()));
        let stopped = StoppedItself::default();

        let (tx, _save_now_tx, handle) = spawn(
            dir.path(),
            EncoderParams::tiny_test(Codec::H264, &[]),
            1, // 1-second segments, so a handful of frames rolls over multiple times
            Retention::new(1, 1),
            &segments,
            &stopped,
        );

        // Send frames spaced out past several 1s segment boundaries.
        for i in 0..8u64 {
            tx.send(RecordingCommand::video(tiny_video_frame(
                Duration::from_millis(i * 400),
            )))
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        tx.send(RecordingCommand::Stop).await.unwrap();
        handle
            .await
            .unwrap()
            .expect("highlight actor returned an error");

        let guard = segments.lock().unwrap();
        assert!(
            guard.len() <= 3,
            "expected at most 3 retained segments, got {}",
            guard.len()
        );
        assert!(!guard.is_empty(), "expected at least one finalized segment");
        for seg in guard.iter() {
            assert!(
                seg.path.exists(),
                "retained segment file should exist on disk"
            );
        }
        // Count segment files actually left in the directory -- old ones must
        // be deleted, not just dropped from the in-memory deque.
        let files_on_disk = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(
            files_on_disk,
            guard.len(),
            "deleted segments must not survive on disk"
        );
    }

    #[tokio::test]
    async fn discard_segments_removes_all_tracked_files() {
        let dir = tempfile::tempdir().unwrap();
        let segments = Arc::new(Mutex::new(VecDeque::new()));
        let stopped = StoppedItself::default();

        let (tx, _save_now_tx, handle) = spawn(
            dir.path(),
            EncoderParams::tiny_test(Codec::H264, &[]),
            1,
            Retention::new(10, 1),
            &segments,
            &stopped,
        );
        tx.send(RecordingCommand::video(tiny_video_frame(Duration::ZERO)))
            .await
            .unwrap();
        tx.send(RecordingCommand::Stop).await.unwrap();
        handle
            .await
            .unwrap()
            .expect("highlight actor returned an error");

        assert!(
            !segments.lock().unwrap().is_empty(),
            "expected at least one segment before discarding"
        );
        discard_segments(&segments);
        assert!(segments.lock().unwrap().is_empty());
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "all segment files should be deleted"
        );
    }

    #[tokio::test]
    async fn save_now_forces_early_finalize_without_stopping_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let segments = Arc::new(Mutex::new(VecDeque::new()));
        let stopped = StoppedItself::default();

        let (tx, save_now_tx, handle) = spawn(
            dir.path(),
            EncoderParams::tiny_test(Codec::H264, &[]),
            60, // long segment -- save_now must finalize well before this elapses
            Retention::new(600, 60),
            &segments,
            &stopped,
        );

        tx.send(RecordingCommand::video(tiny_video_frame(Duration::ZERO)))
            .await
            .unwrap();

        let (reply_tx, reply_rx) = oneshot::channel();
        save_now_tx.send(reply_tx).unwrap();
        tokio::time::timeout(Duration::from_secs(5), reply_rx)
            .await
            .expect("save_now reply timed out")
            .expect("save_now reply sender dropped");

        assert_eq!(
            segments.lock().unwrap().len(),
            1,
            "expected the forced segment to be finalized"
        );

        // Rotation must have already opened a fresh segment after the forced
        // save -- write to it before stopping so this second segment is a
        // real one to confirm rotation actually continued, not just that the
        // first segment closed correctly.
        tx.send(RecordingCommand::video(tiny_video_frame(Duration::ZERO)))
            .await
            .unwrap();
        tx.send(RecordingCommand::Stop).await.unwrap();
        handle
            .await
            .unwrap()
            .expect("highlight actor returned an error");
        assert_eq!(
            segments.lock().unwrap().len(),
            2,
            "rotation should continue after a forced save"
        );
    }
}
