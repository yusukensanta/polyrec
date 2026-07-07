use crate::encode::{RecordingCommand, RecordingWriter};
use crate::error::AppError;
use crate::types::{AudioSamples, VideoFrame};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Spawns the RecordingActor on a dedicated blocking OS thread.
/// Returns (command_sender, handle_resolving_to_output_path).
pub fn spawn_recording_actor(
    output_path: PathBuf,
    width: u32,
    height: u32,
    fps: u32,
    audio_device_specs: Vec<(u32, u16)>,
) -> (
    mpsc::Sender<RecordingCommand>,
    JoinHandle<Result<PathBuf, AppError>>,
) {
    let (tx, mut rx) = mpsc::channel::<RecordingCommand>(256);

    let handle = tokio::task::spawn_blocking(move || {
        let writer = RecordingWriter::new(&output_path, width, height, fps, &audio_device_specs)?;
        writer.begin_writing()?;

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
        }

        writer.finalize()
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
