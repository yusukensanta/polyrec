//! What the installed video encoders take, asked once per setting and
//! remembered for the process.

use super::*;

/// What the encoders have said about a video setting, for the whole process:
/// the installed encoders don't change while it runs, and asking one means
/// creating it (about half a second for a hardware encoder). Keyed by
/// (is HEVC, width, height, fps, hardware allowed, frames as textures); the
/// bitrate doesn't decide whether a frame rate is accepted.
pub(super) type EncoderAnswerKey = (bool, u32, u32, u32, bool, bool);
pub(super) static ENCODER_ANSWERS: Mutex<BTreeMap<EncoderAnswerKey, bool>> =
    Mutex::new(BTreeMap::new());

pub(super) fn encoder_answer_key(params: &EncoderParams) -> EncoderAnswerKey {
    (
        params.codec == Codec::H265,
        params.width,
        params.height,
        params.fps,
        params.allow_hardware_encode,
        params.gpu.as_ref().is_some_and(|gpu| gpu.direct),
    )
}

pub(super) fn encoder_answers() -> std::sync::MutexGuard<'static, BTreeMap<EncoderAnswerKey, bool>>
{
    ENCODER_ANSWERS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Whether `RecordingWriter::new` would accept `params`' video settings.
/// Found by creating one for real (on a scratch file, deleted again): what an
/// encoder takes depends on the codec, the GPU vendor's encoder and H.264's
/// level limits (1440p tops out at 144 fps), and only it knows. Answers are
/// cached (see `ENCODER_ANSWERS`). Slow when uncached, so keep it off the UI
/// thread.
pub(crate) fn video_encoder_accepts(params: &EncoderParams) -> bool {
    static PROBE_ID: AtomicU32 = AtomicU32::new(0);

    let key = encoder_answer_key(params);
    if let Some(&accepted) = encoder_answers().get(&key) {
        return accepted;
    }
    let probe_path = std::env::temp_dir().join(format!(
        "polyrec-encoder-probe-{}-{}.mp4",
        std::process::id(),
        PROBE_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let video_only = EncoderParams {
        audio_tracks: Vec::new(),
        container: Container::Mp4,
        ..params.clone()
    };
    let answer = match RecordingWriter::try_new(&probe_path, &video_only) {
        Ok(_writer) => Some(true),
        Err(WriterError::Rejected(e)) => {
            tracing::info!(
                "encoder rejects {:?} {}x{} at {} fps: {e}",
                params.codec,
                params.width,
                params.height,
                params.fps
            );
            Some(false)
        }
        // Not an answer: asked again next time.
        Err(WriterError::Other(e)) => {
            tracing::warn!("couldn't ask the encoder about {} fps: {e}", params.fps);
            None
        }
    };
    let _ = std::fs::remove_file(&probe_path);
    if let Some(accepted) = answer {
        encoder_answers().insert(key, accepted);
    }
    answer.unwrap_or(false)
}
