//! Continuous background "Highlight" buffer: while enabled, rotates short
//! segment files to disk on its own capture threads (kept entirely separate
//! from manual recording's), and lets the caller save the last N seconds on
//! demand via `encode::highlight_export::concat_and_trim`. This module owns
//! only the segment-rotation actor itself -- lifecycle (start/stop, mutual
//! exclusion with manual recording, foreground-window tracking) lives in
//! `session::mod` and the dashboard.

use crate::encode::{RecordingCommand, RecordingWriter};
use crate::error::AppError;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// Segment length is an internal implementation detail, not user-facing --
/// exposing a second "segment length" number alongside buffer duration would
/// make the settings UI harder to explain for no real benefit. 10s gives at
/// least 3 segments of trim granularity even at the minimum 30s buffer.
pub const HIGHLIGHT_SEGMENT_SECONDS: u32 = 10;

/// How often the rotation loop re-checks free disk space -- same cadence and
/// reasoning as `encode::actor`'s `DISK_CHECK_INTERVAL`.
const DISK_CHECK_INTERVAL: Duration = Duration::from_secs(5);

const HIGHLIGHT_CHANNEL_CAPACITY: usize = 256;

/// One finalized segment file in the rotating buffer. `duration` is the
/// segment's own re-based PTS span (see `spawn_highlight_actor`), not a wall
/// clock measurement -- it's what `encode::highlight_export::concat_and_trim`
/// needs to know how much each segment actually contributes.
#[derive(Debug, Clone)]
pub struct SegmentInfo {
    pub path: PathBuf,
    pub duration: Duration,
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
/// Segments are written to `segment_dir`, kept to the newest `max_segments`
/// (older ones deleted as new ones finalize), and tracked in `segments` for
/// `encode::highlight_export::concat_and_trim` to read later. Ends (dropping
/// no in-progress segment silently -- the current one is always finalized
/// first) when `RecordingCommand::Stop` arrives or free disk space on
/// `segment_dir`'s volume drops below `disk_space::MIN_FREE_BYTES` (setting
/// `disk_full_flag` in that case).
#[allow(clippy::too_many_arguments)]
pub fn spawn_highlight_actor(
    segment_dir: PathBuf,
    width: u32,
    height: u32,
    fps: u32,
    codec: String,
    bitrate_bps: u32,
    audio_device_specs: Vec<(u32, u16)>,
    segment_seconds: u32,
    max_segments: usize,
    segments: Arc<Mutex<VecDeque<SegmentInfo>>>,
    disk_full_flag: Arc<AtomicBool>,
    allow_hardware_encode: bool,
) -> (
    mpsc::Sender<RecordingCommand>,
    mpsc::UnboundedSender<SaveNowRequest>,
    JoinHandle<Result<(), AppError>>,
) {
    let (tx, mut rx) = mpsc::channel::<RecordingCommand>(HIGHLIGHT_CHANNEL_CAPACITY);
    let (save_now_tx, mut save_now_rx) = mpsc::unbounded_channel::<SaveNowRequest>();

    let handle = tokio::task::spawn_blocking(move || {
        let segment_duration = Duration::from_secs(segment_seconds.max(1) as u64);
        let mut last_disk_check = Instant::now();

        loop {
            let seg_id = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let seg_path = segment_dir.join(format!("highlight_seg_{seg_id}.mp4"));

            let writer = RecordingWriter::new(
                &seg_path, width, height, fps, &codec, bitrate_bps, &audio_device_specs, allow_hardware_encode,
            )?;
            writer.begin_writing()?;

            let seg_start = Instant::now();
            // Capture threads stamp frame/sample PTS from the shared
            // RecordingClock spanning the whole buffering session, not reset
            // per segment (see session/mod.rs) -- re-base here instead of
            // touching the capture layer, so each segment's own file starts
            // at PTS ~0 like a normal recording does.
            let mut pts_base: Option<Duration> = None;
            let mut last_pts = Duration::ZERO;
            let mut stop_requested = false;
            let mut save_now_reply: Option<SaveNowRequest> = None;

            loop {
                match rx.blocking_recv() {
                    Some(RecordingCommand::WriteVideo(mut frame)) => {
                        let base = *pts_base.get_or_insert(frame.pts);
                        frame.pts = frame.pts.saturating_sub(base);
                        last_pts = last_pts.max(frame.pts);
                        if let Err(e) = writer.write_video(frame) {
                            tracing::error!("highlight write_video error: {e}");
                        }
                    }
                    Some(RecordingCommand::WriteAudio(mut samples)) => {
                        let base = *pts_base.get_or_insert(samples.pts);
                        samples.pts = samples.pts.saturating_sub(base);
                        last_pts = last_pts.max(samples.pts);
                        let track_idx = samples.track_id.0 as usize;
                        if let Err(e) = writer.write_audio(track_idx, samples) {
                            tracing::error!("highlight write_audio error: {e}");
                        }
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
                if last_disk_check.elapsed() >= DISK_CHECK_INTERVAL {
                    last_disk_check = Instant::now();
                    match crate::disk_space::free_bytes(&segment_dir) {
                        Ok(free) if free < crate::disk_space::MIN_FREE_BYTES => {
                            tracing::warn!(
                                "disk space low ({} MB free on {}) -- stopping highlight buffering",
                                free / (1024 * 1024),
                                segment_dir.display()
                            );
                            disk_full_flag.store(true, Ordering::Relaxed);
                            stop_requested = true;
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!("highlight disk space check failed, continuing: {e}"),
                    }
                    if stop_requested {
                        break;
                    }
                }
            }

            // A segment that never received a single sample (e.g. Stop arrives
            // right after a fresh rotation, or right after save_now finalized
            // the previous one) can't be finalized -- Media Foundation refuses
            // to finalize a sink with nothing written to it. That's expected
            // here, not a real failure: skip it instead of treating the whole
            // actor as errored.
            match writer.finalize() {
                Ok(finished_path) => {
                    let info = SegmentInfo { path: finished_path, duration: last_pts };
                    let mut guard = segments.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.push_back(info);
                    while guard.len() > max_segments {
                        if let Some(old) = guard.pop_front() {
                            let _ = std::fs::remove_file(&old.path);
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!("highlight segment had no samples, discarding: {e}");
                    let _ = std::fs::remove_file(&seg_path);
                }
            }
            if let Some(reply) = save_now_reply {
                let _ = reply.send(());
            }

            if stop_requested {
                break;
            }
        }
        Ok(())
    });

    (tx, save_now_tx, handle)
}

/// Deletes every segment file currently tracked and empties the deque --
/// used when highlight buffering is disabled or restarted for a different
/// foreground window (segments from a different app/resolution can't be
/// concatenated with new ones, see `encode::highlight_export::concat_and_trim`).
pub fn discard_segments(segments: &Mutex<VecDeque<SegmentInfo>>) {
    let mut guard = segments.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    for seg in guard.drain(..) {
        let _ = std::fs::remove_file(&seg.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::VideoFrame;

    fn tiny_video_frame(pts: Duration) -> VideoFrame {
        VideoFrame { pts, data: vec![0u8; 64 * 64 * 4] }
    }

    #[tokio::test]
    async fn rotation_deletes_old_segments_and_bounds_disk_usage() {
        let dir = tempfile::tempdir().unwrap();
        let segments = Arc::new(Mutex::new(VecDeque::new()));
        let disk_full_flag = Arc::new(AtomicBool::new(false));

        let (tx, _save_now_tx, handle) = spawn_highlight_actor(
            dir.path().to_path_buf(),
            64,
            64,
            30,
            "h264".to_string(),
            500_000,
            vec![],
            1, // 1-second segments, so a handful of frames rolls over multiple times
            3, // keep at most 3 segments
            Arc::clone(&segments),
            disk_full_flag,
            false,
        );

        // Send frames spaced out past several 1s segment boundaries.
        for i in 0..8u64 {
            tx.send(RecordingCommand::WriteVideo(tiny_video_frame(Duration::from_millis(i * 400))))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        tx.send(RecordingCommand::Stop).await.unwrap();
        handle.await.unwrap().expect("highlight actor returned an error");

        let guard = segments.lock().unwrap();
        assert!(guard.len() <= 3, "expected at most 3 retained segments, got {}", guard.len());
        assert!(!guard.is_empty(), "expected at least one finalized segment");
        for seg in guard.iter() {
            assert!(seg.path.exists(), "retained segment file should exist on disk");
        }
        // Count segment files actually left in the directory -- old ones must
        // be deleted, not just dropped from the in-memory deque.
        let files_on_disk = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(files_on_disk, guard.len(), "deleted segments must not survive on disk");
    }

    #[tokio::test]
    async fn discard_segments_removes_all_tracked_files() {
        let dir = tempfile::tempdir().unwrap();
        let segments = Arc::new(Mutex::new(VecDeque::new()));
        let disk_full_flag = Arc::new(AtomicBool::new(false));

        let (tx, _save_now_tx, handle) = spawn_highlight_actor(
            dir.path().to_path_buf(),
            64,
            64,
            30,
            "h264".to_string(),
            500_000,
            vec![],
            1,
            10,
            Arc::clone(&segments),
            disk_full_flag,
            false,
        );
        tx.send(RecordingCommand::WriteVideo(tiny_video_frame(Duration::ZERO))).await.unwrap();
        tx.send(RecordingCommand::Stop).await.unwrap();
        handle.await.unwrap().expect("highlight actor returned an error");

        assert!(!segments.lock().unwrap().is_empty(), "expected at least one segment before discarding");
        discard_segments(&segments);
        assert!(segments.lock().unwrap().is_empty());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "all segment files should be deleted");
    }

    #[tokio::test]
    async fn save_now_forces_early_finalize_without_stopping_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let segments = Arc::new(Mutex::new(VecDeque::new()));
        let disk_full_flag = Arc::new(AtomicBool::new(false));

        let (tx, save_now_tx, handle) = spawn_highlight_actor(
            dir.path().to_path_buf(),
            64,
            64,
            30,
            "h264".to_string(),
            500_000,
            vec![],
            60, // long segment -- save_now must finalize well before this elapses
            10,
            Arc::clone(&segments),
            disk_full_flag,
            false,
        );

        tx.send(RecordingCommand::WriteVideo(tiny_video_frame(Duration::ZERO))).await.unwrap();

        let (reply_tx, reply_rx) = oneshot::channel();
        save_now_tx.send(reply_tx).unwrap();
        tokio::time::timeout(Duration::from_secs(5), reply_rx)
            .await
            .expect("save_now reply timed out")
            .expect("save_now reply sender dropped");

        assert_eq!(segments.lock().unwrap().len(), 1, "expected the forced segment to be finalized");

        // Rotation must have already opened a fresh segment after the forced
        // save -- write to it before stopping so this second segment is a
        // real one to confirm rotation actually continued, not just that the
        // first segment closed correctly.
        tx.send(RecordingCommand::WriteVideo(tiny_video_frame(Duration::ZERO))).await.unwrap();
        tx.send(RecordingCommand::Stop).await.unwrap();
        handle.await.unwrap().expect("highlight actor returned an error");
        assert_eq!(segments.lock().unwrap().len(), 2, "rotation should continue after a forced save");
    }
}
