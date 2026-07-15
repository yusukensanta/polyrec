use crate::error::AppError;
use crate::session::clock::RecordingClock;
use crate::types::{AppAudioSource, AudioDevice, AudioSamples, TrackId};
use std::sync::Arc;
use tokio::sync::mpsc;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    ActivateAudioInterfaceAsync, AudioSessionStateExpired, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, IAudioSessionControl2, IAudioSessionManager2,
    IMMDeviceEnumerator, MMDeviceEnumerator, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX, eCapture, eConsole, eMultimedia, eRender,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{
    BLOB, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    STGM_READ,
};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{Interface, implement};

/// Fixed target format all captured audio is normalized to before it reaches the
/// encoder. Devices report all kinds of native mix formats (e.g. 96kHz/8ch);
/// the AAC MFT doesn't accept most of those directly, so we always downmix/resample
/// to this before packaging `AudioSamples`.
pub const TARGET_SAMPLE_RATE: u32 = 48000;
pub const TARGET_CHANNELS: u16 = 2;

/// Convert interleaved `input` (native `channels` per frame) to interleaved stereo.
/// - 1 channel: duplicated to L=R (no information loss)
/// - 2 channels: passed through unchanged
/// - \>2 channels: take channels 0/1 (front-left/front-right) directly. Per the WAVEFORMATEXTENSIBLE channel-mask convention, front L/R are always present at indices 0/1 regardless of total channel count — averaging all N channels instead (an earlier version of this code did) dilutes the signal severely whenever surround/center/LFE channels are silent, which is the common case: most content is plain stereo upmixed into the device's wider mix format.
fn to_stereo(input: &[f32], channels: u16) -> Vec<f32> {
    match channels {
        0 => Vec::new(),
        1 => input.iter().flat_map(|&s| [s, s]).collect(),
        2 => input.to_vec(),
        n => {
            let n = n as usize;
            input
                .chunks_exact(n)
                .flat_map(|frame| [frame[0], frame[1]])
                .collect()
        }
    }
}

/// Scales `samples` in place by `gain` (a linear multiplier -- 1.0 = unchanged,
/// from `Config::audio_gain`'s 0.0-2.0 range), clamping the result to
/// [-1.0, 1.0]. Boosting above 1.0 is allowed (the common "my mic is too
/// quiet" case) but reduces headroom, so the clamp prevents that from
/// becoming hard digital clipping/distortion in the encoded track. A no-op
/// loop at gain == 1.0 rather than skipping it entirely -- values already
/// within range are unaffected by clamping, so there's no behavioral
/// difference worth a branch for it.
fn apply_gain(samples: &mut [f32], gain: f32) {
    for s in samples.iter_mut() {
        *s = (*s * gain).clamp(-1.0, 1.0);
    }
}

/// Stateful linear-interpolation resampler. Carries fractional position and the
/// last input frame across calls so buffer boundaries (WASAPI delivers ~10ms
/// chunks continuously) don't introduce discontinuities.
struct LinearResampler {
    channels: usize,
    ratio: f64,
    pos: f64,
    prev_frame: Vec<f32>,
}

impl LinearResampler {
    fn new(in_rate: u32, out_rate: u32, channels: u16) -> Self {
        Self {
            channels: channels as usize,
            ratio: in_rate as f64 / out_rate as f64,
            pos: 1.0,
            prev_frame: vec![0.0; channels as usize],
        }
    }

    /// `input` is interleaved f32 at `self.channels` channels/frame.
    /// Returns resampled interleaved f32, same channel count.
    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if input.is_empty() {
            return Vec::new();
        }
        let ch = self.channels;
        let in_frames = input.len() / ch;
        fn frame_at<'a>(i: usize, input: &'a [f32], prev: &'a [f32], ch: usize) -> &'a [f32] {
            if i == 0 {
                prev
            } else {
                &input[(i - 1) * ch..i * ch]
            }
        }

        let mut out = Vec::new();
        while (self.pos.floor() as usize) <= in_frames {
            let i0 = self.pos.floor() as usize;
            let i1 = i0 + 1;
            let frac = self.pos - i0 as f64;
            let f0 = frame_at(i0, input, &self.prev_frame, ch);
            let f1_owned;
            let f1: &[f32] = if i1 <= in_frames {
                frame_at(i1, input, &self.prev_frame, ch)
            } else {
                f1_owned = f0.to_vec();
                &f1_owned
            };
            for c in 0..ch {
                let sample = f0[c] as f64 * (1.0 - frac) + f1[c] as f64 * frac;
                out.push(sample as f32);
            }
            self.pos += self.ratio;
        }

        self.pos -= in_frames as f64;
        self.prev_frame
            .copy_from_slice(&input[(in_frames - 1) * ch..in_frames * ch]);
        out
    }
}

/// Enumerate default render (loopback) and default capture (microphone) endpoints.
pub fn enumerate_audio_devices() -> Result<Vec<AudioDevice>, AppError> {
    let mut devices = Vec::new();
    unsafe {
        // COINIT_MULTITHREADED is fine — S_FALSE (already init'd) is also OK
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| {
                AppError::Windows(format!("CoCreateInstance MMDeviceEnumerator: {e}"))
            })?;

        // Default render endpoint — used for loopback capture
        if let Ok(dev) = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia) {
            let id = dev
                .GetId()
                .map(|p| p.to_string().unwrap_or_default())
                .unwrap_or_default();
            let name =
                get_device_friendly_name(&dev).unwrap_or_else(|| "Default Output".to_string());
            devices.push(AudioDevice {
                id,
                name,
                is_loopback: true,
            });
        }

        // Default capture endpoint — microphone
        if let Ok(dev) = enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) {
            let id = dev
                .GetId()
                .map(|p| p.to_string().unwrap_or_default())
                .unwrap_or_default();
            let name =
                get_device_friendly_name(&dev).unwrap_or_else(|| "Default Input".to_string());
            devices.push(AudioDevice {
                id,
                name,
                is_loopback: false,
            });
        }
    }
    Ok(devices)
}

/// Enumerate running applications that currently have a WASAPI audio session
/// on the default render (Speakers) endpoint -- lets a specific app (Discord,
/// Spotify, a game) be picked as its own recording track via
/// `run_process_loopback_capture`, independent of whichever window is
/// selected as the video capture source (see `AppAudioSource`'s doc comment).
///
/// Includes both `Active` and `Inactive` sessions (an app that's currently
/// silent -- paused music, nobody talking -- still keeps its session and
/// should still be pickable), excludes `Expired` ones (the app's audio
/// client was torn down, effectively closed) and the OS's own system-sounds
/// session (not a real app). Grouped by exe name, not process id: one app
/// can hold several concurrent sessions on the same pid (e.g. multiple
/// simultaneous sounds), and two independent top-level processes of the
/// same exe (not parent/child -- e.g. two separate game/app windows) are
/// still the same app from a recording-selection standpoint, so both land
/// in one `AppAudioSource::process_ids` rather than two separate entries.
pub fn enumerate_app_audio_sessions() -> Result<Vec<AppAudioSource>, AppError> {
    let mut sources = Vec::new();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| {
                AppError::Windows(format!("CoCreateInstance MMDeviceEnumerator: {e}"))
            })?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .map_err(|e| AppError::Windows(format!("GetDefaultAudioEndpoint: {e}")))?;
        let session_manager: IAudioSessionManager2 = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| AppError::Windows(format!("Activate IAudioSessionManager2: {e}")))?;
        let session_enum = session_manager
            .GetSessionEnumerator()
            .map_err(|e| AppError::Windows(format!("GetSessionEnumerator: {e}")))?;
        let count = session_enum
            .GetCount()
            .map_err(|e| AppError::Windows(format!("GetCount: {e}")))?;

        let mut seen_pids = std::collections::HashSet::new();
        for i in 0..count {
            let Ok(control) = session_enum.GetSession(i) else {
                continue;
            };
            let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
                continue;
            };
            // HRESULT is a "boolean success code" here (S_OK = is the system
            // sounds session, S_FALSE = isn't) -- NOT a Result to treat with
            // is_ok(), which would be true for both (S_FALSE is still
            // SUCCEEDED). Compare the raw code directly.
            if control2.IsSystemSoundsSession().0 == 0 {
                continue;
            }
            if control.GetState().unwrap_or(AudioSessionStateExpired) == AudioSessionStateExpired {
                continue;
            }
            let Ok(pid) = control2.GetProcessId() else {
                continue;
            };
            if pid == 0 || !seen_pids.insert(pid) {
                continue;
            }

            let exe_path = crate::sources::get_exe_path(pid);
            let Some(exe_name) = exe_path
                .as_deref()
                .and_then(|p| std::path::Path::new(p).file_name())
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
            else {
                // Can't identify the process (already exited, or access
                // denied) -- not worth listing an audio source with no name.
                continue;
            };

            // WASAPI's own session display name, if the app bothered to set
            // one -- most don't, so this is usually empty and we fall back
            // to the exe name, same convention CaptureSource uses.
            let wasapi_name = control.GetDisplayName().ok().and_then(|pwsz| {
                let s = pwsz.to_string().ok().filter(|s| !s.is_empty());
                CoTaskMemFree(Some(pwsz.0 as *const _));
                s
            });
            let display_name = wasapi_name
                .unwrap_or_else(|| crate::sources::display_name_from_exe_name(&exe_name));

            match sources
                .iter_mut()
                .find(|s: &&mut AppAudioSource| s.exe_name == exe_name)
            {
                Some(existing) => existing.process_ids.push(pid),
                None => {
                    let icon_rgba = exe_path
                        .as_deref()
                        .and_then(crate::sources::extract_exe_icon_rgba);
                    sources.push(AppAudioSource {
                        process_ids: vec![pid],
                        exe_name,
                        display_name,
                        icon_rgba,
                    });
                }
            }
        }
    }
    Ok(sources)
}

/// Read PKEY_Device_FriendlyName from the device property store.
/// GUID: {A45C254E-DF1C-4EFD-8020-67D146A850E0}, pid = 14
fn get_device_friendly_name(device: &windows::Win32::Media::Audio::IMMDevice) -> Option<String> {
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ).ok()?;

        let key = PROPERTYKEY {
            fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
            pid: 14,
        };

        let prop = store.GetValue(&key).ok()?;

        // PROPVARIANT for VT_LPWSTR: the wide-string pointer lives at .Anonymous.Anonymous.Anonymous.pwszVal.
        // windows-rs 0.62 generates PROPVARIANT as a plain public struct directly (no
        // separate "safe wrapper" needing an as_raw() conversion the way 0.58 did), and
        // pwszVal is now already a PWSTR (was a raw *mut u16 in 0.58, needing an extra
        // manual wrap to get PWSTR's safe to_string() helper).
        let pwsz = prop.Anonymous.Anonymous.Anonymous.pwszVal;
        if pwsz.is_null() {
            return None;
        }
        pwsz.to_string().ok()
    }
}

/// Capture audio from a WASAPI endpoint and forward PCM f32 samples to `tx`.
///
/// `device_id` — empty string selects the system default endpoint.
/// `is_loopback` — when true the stream uses `AUDCLNT_STREAMFLAGS_LOOPBACK`
///                 (system audio mix) on the render endpoint.
#[allow(clippy::too_many_arguments)]
pub async fn run_audio_capture(
    device_id: String,
    track_id: TrackId,
    is_loopback: bool,
    clock: Arc<RecordingClock>,
    pause_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stop_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    tx: mpsc::Sender<AudioSamples>,
    gain: f32,
) -> Result<(), AppError> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|e| AppError::Windows(format!("CoCreateInstance: {e}")))?;

        let device = if device_id.is_empty() {
            let flow = if is_loopback { eRender } else { eCapture };
            enumerator
                .GetDefaultAudioEndpoint(flow, eMultimedia)
                .map_err(|e| AppError::Windows(format!("GetDefaultAudioEndpoint: {e}")))?
        } else {
            let id: windows::core::HSTRING = device_id.as_str().into();
            enumerator
                .GetDevice(&id)
                .map_err(|e| AppError::Windows(format!("GetDevice: {e}")))?
        };

        let audio_client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| AppError::Windows(format!("Activate IAudioClient: {e}")))?;

        let mix_format = audio_client
            .GetMixFormat()
            .map_err(|e| AppError::Windows(format!("GetMixFormat: {e}")))?;

        let stream_flags: u32 = if is_loopback {
            AUDCLNT_STREAMFLAGS_LOOPBACK
        } else {
            0u32
        };

        // 100 ms buffer; hnsperiodicity = 0 for shared mode
        audio_client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                stream_flags,
                1_000_000i64,
                0i64,
                mix_format,
                None,
            )
            .map_err(|e| AppError::Windows(format!("IAudioClient::Initialize: {e}")))?;

        let sample_rate = (*mix_format).nSamplesPerSec;
        let channels = (*mix_format).nChannels;
        if channels == 0 {
            return Err(AppError::Windows("WASAPI mix format has 0 channels".into()));
        }
        let bytes_per_frame = (*mix_format).nBlockAlign as usize;

        run_capture_loop(
            audio_client,
            sample_rate,
            channels,
            bytes_per_frame,
            track_id,
            clock,
            pause_flag,
            stop_flag,
            tx,
            gain,
        )
        .await
    }
}

/// Capture only the audio produced by `target_pid` (and, if `include_tree`, its child
/// processes) via the Windows 10 2004+ Process Loopback Capture API, instead of the
/// full desktop audio mix. Forwards PCM f32 samples to `tx`, same as `run_audio_capture`.
#[allow(clippy::too_many_arguments)]
pub async fn run_process_loopback_capture(
    target_pid: u32,
    include_tree: bool,
    track_id: TrackId,
    clock: Arc<RecordingClock>,
    pause_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stop_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    tx: mpsc::Sender<AudioSamples>,
    gain: f32,
) -> Result<(), AppError> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let audio_client = activate_process_loopback_audio_client(target_pid, include_tree)?;

        // Process loopback is a synthesized stream with no natural device mix format,
        // so we pick the format ourselves — matches our fixed encoder target directly,
        // meaning no resampling is actually needed for this path (still routed through
        // the same conversion code for a single, uniform capture loop).
        let bits_per_sample = 32u16;
        let block_align = TARGET_CHANNELS * (bits_per_sample / 8);
        let wave_format = WAVEFORMATEX {
            wFormatTag: 3, // WAVE_FORMAT_IEEE_FLOAT
            nChannels: TARGET_CHANNELS,
            nSamplesPerSec: TARGET_SAMPLE_RATE,
            nAvgBytesPerSec: TARGET_SAMPLE_RATE * block_align as u32,
            nBlockAlign: block_align,
            wBitsPerSample: bits_per_sample,
            cbSize: 0,
        };

        audio_client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                1_000_000i64,
                0i64,
                &wave_format,
                None,
            )
            .map_err(|e| {
                AppError::Windows(format!("IAudioClient::Initialize (process loopback): {e}"))
            })?;

        run_capture_loop(
            audio_client,
            TARGET_SAMPLE_RATE,
            TARGET_CHANNELS,
            block_align as usize,
            track_id,
            clock,
            pause_flag,
            stop_flag,
            tx,
            gain,
        )
        .await
    }
}

/// Shared buffer-read loop: starts the (already-initialized) client, unpacks each
/// buffer at its native `sample_rate`/`channels`, downmixes + resamples to the fixed
/// encoder target, and forwards `AudioSamples` until the receiver drops or the client
/// is stopped from outside (abort).
// Grouping these into a struct wouldn't add clarity here -- each param is an
// independent piece the WASAPI setup already resolved (format fields, sync
// primitives, output channel); a param struct would just add indirection at
// this thread-spawn boundary without changing what the caller has to pass.
#[allow(clippy::too_many_arguments)]
async unsafe fn run_capture_loop(
    audio_client: IAudioClient,
    sample_rate: u32,
    channels: u16,
    bytes_per_frame: usize,
    track_id: TrackId,
    clock: Arc<RecordingClock>,
    pause_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stop_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    tx: mpsc::Sender<AudioSamples>,
    gain: f32,
) -> Result<(), AppError> {
    unsafe {
        let capture_client: IAudioCaptureClient = audio_client
            .GetService()
            .map_err(|e| AppError::Windows(format!("GetService IAudioCaptureClient: {e}")))?;

        audio_client
            .Start()
            .map_err(|e| AppError::Windows(format!("IAudioClient::Start: {e}")))?;

        let mut resampler = LinearResampler::new(sample_rate, TARGET_SAMPLE_RATE, TARGET_CHANNELS);
        let bytes_per_sample = bytes_per_frame / channels.max(1) as usize;

        // One-shot diagnostic for a report of app-audio tracks recording as
        // silence despite the source app audibly producing sound (mic/device
        // tracks unaffected) -- distinguishes "WASAPI itself reports this
        // buffer as silent" (AUDCLNT_BUFFERFLAGS_SILENT, meaning the process-
        // loopback session genuinely isn't seeing the target's real audio
        // graph -- a Windows/targeting issue, not something fixable here)
        // from "WASAPI handed us a real, non-silent buffer but every sample
        // we read out is still zero" (a parsing bug in this loop). Logged
        // once per track after ~1s of buffers so it doesn't spam.
        const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 2;
        let mut diag_buffers_seen: u32 = 0;
        let mut diag_any_silent_flag = false;
        let mut diag_max_abs_sample: f32 = 0.0;
        let mut diag_logged = false;
        let diag_start = std::time::Instant::now();
        let mut diag_zero_buffer_warned = false;

        loop {
            if stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }

            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames_available: u32 = 0;
            let mut flags: u32 = 0;

            match capture_client.GetBuffer(&mut data, &mut frames_available, &mut flags, None, None)
            {
                Ok(()) if frames_available > 0 => {
                    let sample_count = frames_available as usize * channels as usize;
                    let mut samples = vec![0.0f32; sample_count];

                    for (i, sample) in samples.iter_mut().enumerate() {
                        let byte_offset = i * bytes_per_sample;
                        let sample_ptr = data.add(byte_offset);
                        *sample = if bytes_per_sample == 4 {
                            *(sample_ptr as *const f32)
                        } else if bytes_per_sample == 2 {
                            *(sample_ptr as *const i16) as f32 / 32768.0
                        } else {
                            0.0
                        };
                    }

                    capture_client
                        .ReleaseBuffer(frames_available)
                        .map_err(|e| AppError::Windows(format!("ReleaseBuffer: {e}")))?;

                    if !diag_logged {
                        if flags & AUDCLNT_BUFFERFLAGS_SILENT != 0 {
                            diag_any_silent_flag = true;
                        }
                        for &s in &samples {
                            diag_max_abs_sample = diag_max_abs_sample.max(s.abs());
                        }
                        diag_buffers_seen += 1;
                        if diag_buffers_seen >= 100 {
                            diag_logged = true;
                            tracing::info!(
                                "AudioCapture[{track_id:?}] diagnostic (first ~{diag_buffers_seen} buffers): any_silent_flag={diag_any_silent_flag} max_abs_sample={diag_max_abs_sample:.6}"
                            );
                        }
                    }

                    // Always release buffer first (above), then discard if paused.
                    if pause_flag.load(std::sync::atomic::Ordering::Relaxed) {
                        continue;
                    }

                    let pts = clock.elapsed();
                    let stereo = to_stereo(&samples, channels);
                    let mut resampled = resampler.process(&stereo);
                    apply_gain(&mut resampled, gain);
                    let audio = AudioSamples {
                        track_id,
                        pts,
                        samples: resampled,
                        sample_rate: TARGET_SAMPLE_RATE,
                        channels: TARGET_CHANNELS,
                    };

                    if tx.send(audio).await.is_err() {
                        // Receiver dropped — stop gracefully
                        break;
                    }
                }
                Ok(()) => {
                    // Companion to the diagnostic above: if this track has
                    // gone 5+ seconds without GetBuffer ever reporting
                    // frames_available > 0 even once, WriteSample is never
                    // called for it at all -- which (per the export-dialog
                    // report this is chasing) means the track can end up
                    // entirely absent from the finished file, not just
                    // silent. One-shot so it doesn't spam a track that's
                    // just between sounds.
                    if !diag_zero_buffer_warned
                        && diag_buffers_seen == 0
                        && diag_start.elapsed() >= std::time::Duration::from_secs(5)
                    {
                        diag_zero_buffer_warned = true;
                        tracing::warn!(
                            "AudioCapture[{track_id:?}] diagnostic: no buffer with frames_available > 0 in {:.1}s -- this track may end up with zero samples written",
                            diag_start.elapsed().as_secs_f32()
                        );
                    }
                    // No data yet; yield to the async runtime
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                }
                Err(e) if e.code() == AUDCLNT_E_DEVICE_INVALIDATED => {
                    // The endpoint was unplugged/disabled mid-capture -- retrying
                    // GetBuffer on it forever just spins and spams the warning
                    // below every 10ms, since the device is never coming back on
                    // this handle. Log once at error level and stop the track
                    // cleanly (other tracks/video are unaffected) instead.
                    tracing::error!(
                        "audio device invalidated (unplugged/disabled), stopping this track: {e}"
                    );
                    break;
                }
                Err(e) => {
                    tracing::warn!("GetBuffer error: {e}");
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                }
            }
        }

        let _ = audio_client.Stop();
        Ok(())
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationCompletionHandler {
    tx: std::sync::Mutex<
        Option<std::sync::mpsc::Sender<windows::core::Result<windows::core::IUnknown>>>,
    >,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationCompletionHandler_Impl {
    fn ActivateCompleted(
        &self,
        activateoperation: windows::core::Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let result = (|| -> windows::core::Result<windows::core::IUnknown> {
            let op = activateoperation
                .as_ref()
                .ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_POINTER))?;
            let mut hr = windows::core::HRESULT(0);
            let mut activated: Option<windows::core::IUnknown> = None;
            unsafe { op.GetActivateResult(&mut hr, &mut activated)? };
            hr.ok()?;
            activated.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))
        })();

        if let Ok(mut guard) = self.tx.lock()
            && let Some(tx) = guard.take()
        {
            let _ = tx.send(result);
        }
        Ok(())
    }
}

/// Activate an `IAudioClient` scoped to `target_pid`'s audio only (Windows 10 2004+
/// Process Loopback Capture API), instead of the whole default render device.
unsafe fn activate_process_loopback_audio_client(
    target_pid: u32,
    include_tree: bool,
) -> Result<IAudioClient, AppError> {
    unsafe {
        let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: target_pid,
                    ProcessLoopbackMode: if include_tree {
                        PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
                    } else {
                        PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
                    },
                },
            },
        };

        // Manually build the PROPVARIANT (VT_BLOB) the activation params must be passed
        // as. `pBlobData` points at our own stack-local `params` — the OS reads it
        // synchronously during the ActivateAudioInterfaceAsync call below, so it only needs
        // to outlive that call, not the whole async operation.
        //
        // windows-rs 0.62 makes PROPVARIANT a plain public struct (this module path)
        // instead of the private windows::core::imp:: raw type that needed a
        // from_raw() conversion in 0.58 -- constructed directly here instead.
        let variant = PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: core::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_BLOB,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: PROPVARIANT_0_0_0 {
                        blob: BLOB {
                            cbSize: core::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                            pBlobData: &mut params as *mut _ as *mut u8,
                        },
                    },
                }),
            },
        };

        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let handler_obj = ActivationCompletionHandler {
            tx: std::sync::Mutex::new(Some(result_tx)),
        };
        let handler: IActivateAudioInterfaceCompletionHandler = handler_obj.into();

        let activate_result = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &<IAudioClient as windows::core::Interface>::IID,
            Some(&variant as *const _),
            &handler,
        )
        .map_err(|e| AppError::Windows(format!("ActivateAudioInterfaceAsync: {e}")));

        // `variant.blob.pBlobData` points at our stack `params`, not CoTaskMem-allocated
        // memory — dropping `variant` normally would run PropVariantClear, which for
        // VT_BLOB calls CoTaskMemFree on that pointer. Forget it instead; there's nothing
        // heap-owned in it for us to leak.
        core::mem::forget(variant);
        activate_result?;

        let activated = result_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| AppError::Windows("ActivateAudioInterfaceAsync timed out".into()))?
            .map_err(|e| AppError::Windows(format!("process loopback activation failed: {e}")))?;

        activated.cast::<IAudioClient>().map_err(|e| {
            AppError::Windows(format!("cast activated interface to IAudioClient: {e}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercises the Process Loopback Capture API end to end (activation,
    /// IAudioClient::Initialize, GetService/Start, buffer reads) against our own
    /// process. Needs Windows 10 2004+ and an audio subsystem, so it's ignored by
    /// default — run with `--ignored --nocapture`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn process_loopback_capture_produces_samples() {
        let (tx, mut rx) = mpsc::channel::<AudioSamples>(64);
        let clock = RecordingClock::new();
        let pause_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pid = std::process::id();

        // COM objects (IAudioClient etc.) aren't Send, so this must run on its own
        // thread via a LocalSet — same pattern session::start_capture uses in production.
        let handle = tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("capture runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let _ = run_process_loopback_capture(
                    pid,
                    true,
                    TrackId::new(0),
                    clock,
                    pause_flag,
                    stop_flag,
                    tx,
                    1.0,
                )
                .await;
            });
        });

        let first = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for first AudioSamples packet")
            .expect("channel closed before any packet arrived");

        assert_eq!(first.sample_rate, TARGET_SAMPLE_RATE);
        assert_eq!(first.channels, TARGET_CHANNELS);
        assert!(!first.samples.is_empty(), "packet had no samples");

        handle.abort();
    }

    /// DIAGNOSTIC (temporary): captures the default loopback device for ~3s while a
    /// system WAV plays, and reports the peak sample magnitude actually received.
    /// Run: cargo test --lib diag_default_loopback -- --ignored --nocapture
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn diag_default_loopback_captures_real_signal() {
        let (tx, mut rx) = mpsc::channel::<AudioSamples>(256);
        let clock = RecordingClock::new();
        let pause_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let handle = tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("capture runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let _ = run_audio_capture(
                    String::new(),
                    TrackId::new(0),
                    true,
                    clock,
                    pause_flag,
                    stop_flag,
                    tx,
                    1.0,
                )
                .await;
            });
        });

        // Deliberately fire-and-forget: PlaySync() blocks *inside* the spawned
        // process until the sound finishes, but we want it playing concurrently
        // with our own capture loop below, not blocking this thread until done.
        #[allow(clippy::zombie_processes)]
        std::process::Command::new("powershell")
            .args([
                "-c",
                "(New-Object Media.SoundPlayer 'C:\\Windows\\Media\\Alarm01.wav').PlaySync()",
            ])
            .spawn()
            .expect("failed to spawn powershell sound player");

        let mut peak = 0.0f32;
        let mut packet_count = 0usize;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(4);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Some(samples)) => {
                    packet_count += 1;
                    for &s in &samples.samples {
                        if s.abs() > peak {
                            peak = s.abs();
                        }
                    }
                }
                _ => break,
            }
        }

        handle.abort();
        println!("DIAG: packets={packet_count} peak_abs_sample={peak}");
        assert!(packet_count > 0, "no audio packets received at all");
        assert!(
            peak > 0.01,
            "peak sample magnitude {peak} looks like silence, not the played WAV"
        );
    }

    // Ignored: assumes a real audio subsystem with at least one device, which
    // GitHub's hosted windows-latest CI runner doesn't have (headless, no
    // sound card) -- confirmed by ci.yml's first real run. Real desktop-only,
    // same reasoning as the other #[ignore]d hardware tests in this codebase.
    #[test]
    #[ignore]
    fn enumerate_audio_devices_finds_at_least_one() {
        let devices = enumerate_audio_devices().expect("enumerate_audio_devices failed");
        assert!(
            !devices.is_empty(),
            "expected at least one audio device on this machine"
        );
    }

    #[test]
    #[ignore]
    fn default_output_is_loopback() {
        let devices = enumerate_audio_devices().expect("enumerate_audio_devices failed");
        let loopback = devices.iter().find(|d| d.is_loopback);
        assert!(
            loopback.is_some(),
            "expected a loopback (render) device in the enumeration"
        );
    }

    // Ignored for the same reason as enumerate_audio_devices_finds_at_least_one
    // -- needs a real audio subsystem, which the hosted CI runner doesn't have.
    #[test]
    #[ignore]
    fn enumerate_app_audio_sessions_succeeds_and_has_no_duplicate_pids_across_groups() {
        // Not asserting non-empty -- unlike audio *devices* (always at least
        // a default endpoint), a real desktop can legitimately have zero
        // apps with an active/inactive audio session at the moment this
        // runs. Just confirms the WASAPI session-manager path itself works
        // end to end and that grouping doesn't lose or duplicate a pid
        // across groups (each pid appears in exactly one exe's
        // `process_ids`, even though a single exe can legitimately hold
        // several).
        let sources = enumerate_app_audio_sessions().expect("enumerate_app_audio_sessions failed");
        let mut exe_names: Vec<&str> = sources.iter().map(|s| s.exe_name.as_str()).collect();
        exe_names.sort_unstable();
        exe_names.dedup();
        assert_eq!(
            exe_names.len(),
            sources.len(),
            "expected no duplicate exe_name groups"
        );
        let mut pids: Vec<u32> = sources
            .iter()
            .flat_map(|s| s.process_ids.iter().copied())
            .collect();
        let mut deduped_pids = pids.clone();
        deduped_pids.sort_unstable();
        deduped_pids.dedup();
        pids.sort_unstable();
        assert_eq!(
            pids, deduped_pids,
            "expected no pid to appear in more than one exe's process_ids"
        );
        for s in &sources {
            assert!(
                !s.process_ids.is_empty(),
                "a live-enumerated source should always have at least one pid"
            );
            assert!(
                !s.exe_name.is_empty(),
                "every listed source should have a resolved exe name"
            );
            assert!(
                !s.display_name.is_empty(),
                "every listed source should have a non-empty display name"
            );
        }
    }

    #[test]
    fn to_stereo_mono_duplicates() {
        let out = to_stereo(&[1.0, 2.0, 3.0], 1);
        assert_eq!(out, vec![1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
    }

    #[test]
    fn to_stereo_stereo_passthrough() {
        let out = to_stereo(&[1.0, 2.0, 3.0, 4.0], 2);
        assert_eq!(out, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn to_stereo_multichannel_takes_front_lr() {
        // 4 channels, one frame: [FL, FR, C, LFE] -> keep FL/FR, drop the rest
        let out = to_stereo(&[1.0, 2.0, 3.0, 4.0], 4);
        assert_eq!(out, vec![1.0, 2.0]);
    }

    #[test]
    fn apply_gain_at_1_0_is_a_no_op_for_in_range_samples() {
        let mut samples = vec![0.1, -0.2, 0.5, -0.9];
        apply_gain(&mut samples, 1.0);
        assert_eq!(samples, vec![0.1, -0.2, 0.5, -0.9]);
    }

    #[test]
    fn apply_gain_below_1_0_attenuates() {
        let mut samples = vec![0.4, -0.4];
        apply_gain(&mut samples, 0.5);
        assert_eq!(samples, vec![0.2, -0.2]);
    }

    #[test]
    fn apply_gain_above_1_0_boosts_and_clamps_to_prevent_clipping() {
        let mut samples = vec![0.3, -0.3, 0.05];
        apply_gain(&mut samples, 2.0);
        // 0.3*2=0.6 and 0.05*2=0.1 stay in range; -0.3*2=-0.6 likewise --
        // none of these clip, so this proves boosting itself works.
        assert_eq!(samples, vec![0.6, -0.6, 0.1]);

        let mut loud = vec![0.9, -0.9];
        apply_gain(&mut loud, 2.0);
        // 0.9*2=1.8 and -0.9*2=-1.8 would clip without the clamp.
        assert_eq!(loud, vec![1.0, -1.0]);
    }

    #[test]
    fn resampler_identity_when_rates_match() {
        let mut r = LinearResampler::new(48000, 48000, 2);
        let input = vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
        let out = r.process(&input);
        assert_eq!(out.len(), input.len());
        for (a, b) in out.iter().zip(input.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn resampler_downsamples_to_expected_length() {
        // 96000 -> 48000 is a 2:1 ratio; across many frames, output should
        // land near half the input frame count (allow +/-2 for fractional carry).
        let mut r = LinearResampler::new(96000, 48000, 2);
        let in_frames = 4800usize;
        let input: Vec<f32> = (0..in_frames * 2).map(|i| (i % 7) as f32 * 0.1).collect();
        let out = r.process(&input);
        let out_frames = out.len() / 2;
        let expected = in_frames / 2;
        assert!(
            (out_frames as i64 - expected as i64).abs() <= 2,
            "expected ~{expected} frames, got {out_frames}"
        );
    }

    #[test]
    fn resampler_constant_signal_stays_constant() {
        let mut r = LinearResampler::new(96000, 48000, 2);
        let input = vec![0.5f32; 200 * 2];
        let out = r.process(&input);
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-5));
    }
}
