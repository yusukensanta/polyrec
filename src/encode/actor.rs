use crate::encode::{RecordingCommand, RecordingWriter};
use crate::error::AppError;
use crate::types::{AudioSamples, VideoFrame};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// How often the recording loop re-checks free disk space. Checking every
/// frame would mean a syscall 30-60+ times a second for no benefit -- a few
/// seconds of lag between "disk went low" and "recording stops" is fine given
/// the whole point is stopping before Media Foundation fails mid-write, not
/// stopping at the exact byte it would have failed.
const DISK_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// Spawns the RecordingActor on a dedicated blocking OS thread.
///
/// Recording is written to `temp_path` throughout (Media Foundation's sink writer needs
/// a fixed destination from the moment it opens the file, before the finish time is
/// known). Once `finalize()` closes the file, it's renamed to `<app_name>_<finish
/// timestamp>.mp4` inside `output_dir` — the filename reflects when the recording
/// actually finished, not when it started.
///
/// Returns (command_sender, handle_resolving_to_the_final_renamed_path).
// Each param is an independently-resolved piece the caller already has on hand
// (paths, encoder settings, audio specs) -- a param struct would just move this
// same list into a type definition without changing what start_capture passes.
#[allow(clippy::too_many_arguments)]
pub fn spawn_recording_actor(
    temp_path: PathBuf,
    output_dir: PathBuf,
    app_name: String,
    width: u32,
    height: u32,
    fps: u32,
    codec: String,
    bitrate_bps: u32,
    audio_device_specs: Vec<(u32, u16)>,
    disk_full_flag: Arc<AtomicBool>,
    allow_hardware_encode: bool,
) -> (
    mpsc::Sender<RecordingCommand>,
    JoinHandle<Result<PathBuf, AppError>>,
) {
    let (tx, mut rx) = mpsc::channel::<RecordingCommand>(256);

    let handle = tokio::task::spawn_blocking(move || {
        let writer = RecordingWriter::new(&temp_path, width, height, fps, &codec, bitrate_bps, &audio_device_specs, allow_hardware_encode)?;
        writer.begin_writing()?;

        let mut last_disk_check = Instant::now();

        while let Some(cmd) = rx.blocking_recv() {
            match cmd {
                RecordingCommand::WriteVideo(frame) => {
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

            if last_disk_check.elapsed() >= DISK_CHECK_INTERVAL {
                last_disk_check = Instant::now();
                match crate::disk_space::free_bytes(&output_dir) {
                    Ok(free) if free < crate::disk_space::MIN_FREE_BYTES => {
                        tracing::warn!(
                            "disk space low ({} MB free on {}) -- stopping recording early",
                            free / (1024 * 1024),
                            output_dir.display()
                        );
                        disk_full_flag.store(true, Ordering::Relaxed);
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("disk space check failed, continuing recording: {e}"),
                }
            }
        }

        let finished_temp_path = writer.finalize()?;
        let finish_stamp = chrono::Local::now().format("%Y-%m-%d-%H-%M-%S");
        let final_path = output_dir.join(format!("{app_name}_{finish_stamp}.mp4"));
        std::fs::rename(&finished_temp_path, &final_path).map_err(|e| {
            AppError::Encode(format!(
                "failed to rename {} to {}: {e}",
                finished_temp_path.display(),
                final_path.display()
            ))
        })?;
        Ok(final_path)
    });

    (tx, handle)
}

/// Spawns a pump task forwarding video frames to the recording actor.
/// Increments `frame_count` for each frame forwarded.
pub fn spawn_video_pump(
    mut video_rx: mpsc::Receiver<VideoFrame>,
    recording_tx: mpsc::Sender<RecordingCommand>,
    frame_count: Arc<AtomicU64>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(frame) = video_rx.recv().await {
            frame_count.fetch_add(1, Ordering::Relaxed);
            if recording_tx
                .send(RecordingCommand::WriteVideo(frame))
                .await
                .is_err()
            {
                break;
            }
        }
    })
}

/// Spawns a pump task forwarding audio samples to the recording actor.
pub fn spawn_audio_pump(
    mut audio_rx: mpsc::Receiver<AudioSamples>,
    recording_tx: mpsc::Sender<RecordingCommand>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(samples) = audio_rx.recv().await {
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

        let pump = spawn_video_pump(cap_rx, rec_tx, Arc::clone(&frame_count));

        cap_tx
            .send(VideoFrame {
                pts: Duration::ZERO,
                data: vec![0u8; 64],
            })
            .await
            .unwrap();
        drop(cap_tx); // close channel so pump exits

        pump.await.unwrap();
        assert_eq!(frame_count.load(Ordering::Relaxed), 1);
        assert!(matches!(
            rec_rx.recv().await,
            Some(RecordingCommand::WriteVideo(_))
        ));
    }

    #[tokio::test]
    async fn pump_forwards_audio() {
        let (cap_tx, cap_rx) = mpsc::channel::<AudioSamples>(4);
        let (rec_tx, mut rec_rx) = mpsc::channel::<RecordingCommand>(8);

        let pump = spawn_audio_pump(cap_rx, rec_tx);

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
}
