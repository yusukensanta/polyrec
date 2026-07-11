use crate::error::AppError;
use crate::session::clock::RecordingClock;
use crate::types::{AudioDevice, AudioSamples, TrackId};
use std::sync::Arc;
use tokio::sync::mpsc;
use windows::core::{implement, Interface};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eMultimedia, eRender, ActivateAudioInterfaceAsync,
    IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
    AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, AUDIOCLIENT_ACTIVATION_PARAMS,
    AUDIOCLIENT_ACTIVATION_PARAMS_0, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS, WAVEFORMATEX,
    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ, BLOB};
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0};
use windows::Win32::System::Variant::VT_BLOB;

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
            if i == 0 { prev } else { &input[(i - 1) * ch..i * ch] }
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
        self.prev_frame.copy_from_slice(&input[(in_frames - 1) * ch..in_frames * ch]);
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
            let name = get_device_friendly_name(&dev)
                .unwrap_or_else(|| "Default Output".to_string());
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
            let name = get_device_friendly_name(&dev)
                .unwrap_or_else(|| "Default Input".to_string());
            devices.push(AudioDevice {
                id,
                name,
                is_loopback: false,
            });
        }
    }
    Ok(devices)
}

/// Read PKEY_Device_FriendlyName from the device property store.
/// GUID: {A45C254E-DF1C-4EFD-8020-67D146A850E0}, pid = 14
fn get_device_friendly_name(
    device: &windows::Win32::Media::Audio::IMMDevice,
) -> Option<String> {
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
pub async fn run_audio_capture(
    device_id: String,
    track_id: TrackId,
    is_loopback: bool,
    clock: Arc<RecordingClock>,
    pause_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stop_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    tx: mpsc::Sender<AudioSamples>,
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

        run_capture_loop(audio_client, sample_rate, channels, bytes_per_frame, track_id, clock, pause_flag, stop_flag, tx).await
    }
}

/// Capture only the audio produced by `target_pid` (and, if `include_tree`, its child
/// processes) via the Windows 10 2004+ Process Loopback Capture API, instead of the
/// full desktop audio mix. Forwards PCM f32 samples to `tx`, same as `run_audio_capture`.
pub async fn run_process_loopback_capture(
    target_pid: u32,
    include_tree: bool,
    track_id: TrackId,
    clock: Arc<RecordingClock>,
    pause_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stop_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    tx: mpsc::Sender<AudioSamples>,
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
            .map_err(|e| AppError::Windows(format!("IAudioClient::Initialize (process loopback): {e}")))?;

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
) -> Result<(), AppError> { unsafe {
    let capture_client: IAudioCaptureClient = audio_client
        .GetService()
        .map_err(|e| AppError::Windows(format!("GetService IAudioCaptureClient: {e}")))?;

    audio_client
        .Start()
        .map_err(|e| AppError::Windows(format!("IAudioClient::Start: {e}")))?;

    let mut resampler = LinearResampler::new(sample_rate, TARGET_SAMPLE_RATE, TARGET_CHANNELS);
    let bytes_per_sample = bytes_per_frame / channels.max(1) as usize;

    loop {
        if stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }

        let mut data: *mut u8 = std::ptr::null_mut();
        let mut frames_available: u32 = 0;
        let mut flags: u32 = 0;

        match capture_client.GetBuffer(&mut data, &mut frames_available, &mut flags, None, None) {
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

                // Always release buffer first (above), then discard if paused.
                if pause_flag.load(std::sync::atomic::Ordering::Relaxed) {
                    continue;
                }

                let pts = clock.elapsed();
                let stereo = to_stereo(&samples, channels);
                let resampled = resampler.process(&stereo);
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
                // No data yet; yield to the async runtime
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            }
            Err(e) if e.code() == AUDCLNT_E_DEVICE_INVALIDATED => {
                // The endpoint was unplugged/disabled mid-capture -- retrying
                // GetBuffer on it forever just spins and spams the warning
                // below every 10ms, since the device is never coming back on
                // this handle. Log once at error level and stop the track
                // cleanly (other tracks/video are unaffected) instead.
                tracing::error!("audio device invalidated (unplugged/disabled), stopping this track: {e}");
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
}}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationCompletionHandler {
    tx: std::sync::Mutex<Option<std::sync::mpsc::Sender<windows::core::Result<windows::core::IUnknown>>>>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationCompletionHandler_Impl {
    fn ActivateCompleted(&self, activateoperation: windows::core::Ref<'_, IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        let result = (|| -> windows::core::Result<windows::core::IUnknown> {
            let op = activateoperation.as_ref().ok_or_else(|| {
                windows::core::Error::from(windows::Win32::Foundation::E_POINTER)
            })?;
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
) -> Result<IAudioClient, AppError> { unsafe {
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

    activated
        .cast::<IAudioClient>()
        .map_err(|e| AppError::Windows(format!("cast activated interface to IAudioClient: {e}")))
}}

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
                    pid, true, TrackId::new(0), clock, pause_flag, stop_flag, tx,
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
                    String::new(), TrackId::new(0), true, clock, pause_flag, stop_flag, tx,
                )
                .await;
            });
        });

        // Deliberately fire-and-forget: PlaySync() blocks *inside* the spawned
        // process until the sound finishes, but we want it playing concurrently
        // with our own capture loop below, not blocking this thread until done.
        #[allow(clippy::zombie_processes)]
        std::process::Command::new("powershell")
            .args(["-c", "(New-Object Media.SoundPlayer 'C:\\Windows\\Media\\Alarm01.wav').PlaySync()"])
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
        assert!(peak > 0.01, "peak sample magnitude {peak} looks like silence, not the played WAV");
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
