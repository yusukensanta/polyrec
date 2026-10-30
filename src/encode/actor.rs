use crate::capture::frame_rate::LiveFps;
use crate::disk_space::DiskSpaceGuard;
use crate::encode::{EncoderParams, RecordingCommand, RecordingWriter, VideoSlot};
use crate::error::{AppError, ResultExt};
use crate::recording_naming::{RecordingNaming, resolve_finished_recording_stem};
use crate::types::{AudioLevel, AudioSamples, VideoFrame};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinHandle;

/// How often the recording loop re-checks free disk space -- see
/// `DiskSpaceGuard`'s doc comment for why this is on an interval, not every
/// frame.
const DISK_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// Spawns the RecordingActor on a dedicated blocking OS thread.
///
/// Recording is written to `temp_path` throughout (Media Foundation's sink writer needs
/// a fixed destination from the moment it opens the file, before the finish time is
/// known). Once `finalize()` closes the file, it's renamed to whatever `naming`
/// resolves to (see `resolve_finished_recording_stem`) inside `output_dir` --
/// resolved at finish time, not start time, since the default naming mode's
/// timestamp should reflect when the recording actually finished.
///
/// Returns (command_sender, handle_resolving_to_the_final_renamed_path).
pub fn spawn_recording_actor(
    temp_path: PathBuf,
    output_dir: PathBuf,
    app_name: String,
    naming: RecordingNaming,
    params: EncoderParams,
    disk_full_flag: Arc<AtomicBool>,
    fps: LiveFps,
) -> (
    mpsc::Sender<RecordingCommand>,
    JoinHandle<Result<PathBuf, AppError>>,
) {
    let (tx, mut rx) = mpsc::channel::<RecordingCommand>(256);

    let handle = tokio::task::spawn_blocking(move || {
        let (writer, settled_fps) = RecordingWriter::new_at_highest_fps(&temp_path, &params)?;
        fps.set(settled_fps);
        writer.begin_writing()?;

        let mut disk_guard = DiskSpaceGuard::new(output_dir.clone(), DISK_CHECK_INTERVAL);

        while let Some(cmd) = rx.blocking_recv() {
            match cmd {
                RecordingCommand::WriteVideo(frame, _slot) => {
                    if let Err(e) = writer.write_video(frame) {
                        tracing::error!("write_video error: {e}");
                    }
                }
                RecordingCommand::WriteAudio(samples) => {
                    let track_idx = samples.track_id.0 as usize;
                    if let Err(e) = writer.write_audio(track_idx, samples) {
                        tracing::error!("write_audio error: {e}");
                    }
                }
                RecordingCommand::Stop => break,
            }

            if disk_guard.should_stop(&disk_full_flag, "recording") {
                break;
            }
        }

        let finished_temp_path = writer.finalize()?;
        let stem = resolve_finished_recording_stem(&naming, &app_name, &output_dir);
        let final_path = output_dir.join(format!("{stem}.mp4"));
        std::fs::rename(&finished_temp_path, &final_path).encode_ctx(format_args!(
            "failed to rename {} to {}",
            finished_temp_path.display(),
            final_path.display()
        ))?;
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

/// Spawns a pump task forwarding audio samples to the recording actor,
/// noting each chunk's peak in `level` for the dashboard's meters.
pub fn spawn_audio_pump(
    mut audio_rx: mpsc::Receiver<AudioSamples>,
    recording_tx: mpsc::Sender<RecordingCommand>,
    level: AudioLevel,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(samples) = audio_rx.recv().await {
            level.record(&samples.samples);
            if recording_tx
                .send(RecordingCommand::WriteAudio(samples))
                .await
                .is_err()
            {
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TrackId;
    use std::time::Duration;

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

        let pump = spawn_audio_pump(cap_rx, rec_tx, AudioLevel::default());

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

    #[tokio::test]
    async fn audio_keeps_flowing_while_video_slots_are_exhausted() {
        let (video_tx, video_rx) = mpsc::channel::<VideoFrame>(16);
        let (audio_tx, audio_rx) = mpsc::channel::<AudioSamples>(16);
        let (rec_tx, mut rec_rx) = mpsc::channel::<RecordingCommand>(64);
        let _video = spawn_video_pump(video_rx, rec_tx.clone(), Arc::new(AtomicU64::new(0)), 1);
        let _audio = spawn_audio_pump(audio_rx, rec_tx, AudioLevel::default());

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
