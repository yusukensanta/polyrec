use crate::error::AppError;
use crate::session::clock::RecordingClock;
use crate::types::{AudioDevice, AudioSamples, TrackId};
use std::sync::Arc;
use tokio::sync::mpsc;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eMultimedia, eRender, IAudioCaptureClient, IAudioClient,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_LOOPBACK,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};
use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;

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

        // PROPVARIANT for VT_LPWSTR: the wide-string pointer lives at .Anonymous.Anonymous.Anonymous.pwszVal
        let raw = prop.as_raw();
        let pwsz_raw = raw.Anonymous.Anonymous.Anonymous.pwszVal;
        if pwsz_raw.is_null() {
            return None;
        }
        // Wrap the raw *mut u16 pointer in PWSTR so we can call its safe to_string helper
        let pwsz = windows::core::PWSTR(pwsz_raw);
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

        let capture_client: IAudioCaptureClient = audio_client
            .GetService()
            .map_err(|e| AppError::Windows(format!("GetService IAudioCaptureClient: {e}")))?;

        audio_client
            .Start()
            .map_err(|e| AppError::Windows(format!("IAudioClient::Start: {e}")))?;

        let sample_rate = (*mix_format).nSamplesPerSec;
        let channels = (*mix_format).nChannels;
        if channels == 0 {
            return Err(AppError::Windows("WASAPI mix format has 0 channels".into()));
        }

        loop {
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames_available: u32 = 0;
            let mut flags: u32 = 0;

            match capture_client.GetBuffer(&mut data, &mut frames_available, &mut flags, None, None) {
                Ok(()) if frames_available > 0 => {
                    let bytes_per_frame = (*mix_format).nBlockAlign as usize;
                    let bytes_per_sample = bytes_per_frame / channels as usize;
                    let sample_count = frames_available as usize * channels as usize;
                    let mut samples = vec![0.0f32; sample_count];

                    for i in 0..sample_count {
                        let byte_offset = i * bytes_per_sample;
                        let sample_ptr = data.add(byte_offset);
                        samples[i] = if bytes_per_sample == 4 {
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
                    let audio = AudioSamples {
                        track_id,
                        pts,
                        samples,
                        sample_rate,
                        channels,
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
                Err(e) => {
                    tracing::warn!("GetBuffer error: {e}");
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                }
            }
        }

        let _ = audio_client.Stop();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerate_audio_devices_finds_at_least_one() {
        let devices = enumerate_audio_devices().expect("enumerate_audio_devices failed");
        assert!(
            !devices.is_empty(),
            "expected at least one audio device on this machine"
        );
    }

    #[test]
    fn default_output_is_loopback() {
        let devices = enumerate_audio_devices().expect("enumerate_audio_devices failed");
        let loopback = devices.iter().find(|d| d.is_loopback);
        assert!(
            loopback.is_some(),
            "expected a loopback (render) device in the enumeration"
        );
    }
}
