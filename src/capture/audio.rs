use crate::capture::CaptureControl;
use crate::error::{AppError, ResultExt};
use crate::types::{AppAudioSource, AudioDevice, AudioSamples, TrackId};
use tokio::sync::mpsc;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    ActivateAudioInterfaceAsync, AudioSessionStateExpired, EDataFlow, ERole,
    IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient,
    IAudioSessionControl2, IAudioSessionManager2, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX, eCapture, eConsole, eMultimedia, eRender,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{BLOB, CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{Interface, Owned, implement};

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
        // Up to the last frame, not past it: a sample between the last
        // frame and the next chunk's first waits for that chunk (and comes
        // out of it, at a position under 1) rather than being guessed.
        while self.pos <= in_frames as f64 {
            let i0 = self.pos.floor() as usize;
            let frac = self.pos - i0 as f64;
            let f0 = frame_at(i0, input, &self.prev_frame, ch);
            // Exactly on a frame (always, when the rates match), there's
            // nothing to interpolate towards -- and at the last frame,
            // nothing there yet.
            let f1 = if frac == 0.0 {
                f0
            } else {
                frame_at(i0 + 1, input, &self.prev_frame, ch)
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
    let enumerator = device_enumerator()?;
    let mut devices = Vec::new();
    for (endpoint, fallback_name, is_loopback) in LISTED_DEFAULTS {
        unsafe {
            let Ok(dev) = endpoint.device(&enumerator) else {
                continue;
            };
            let id = endpoint_id(&dev);
            let name = get_device_friendly_name(&dev).unwrap_or_else(|| fallback_name.to_string());
            devices.push(AudioDevice {
                id,
                name,
                is_loopback,
            });
        }
    }
    Ok(devices)
}

/// The endpoint enumerator every device lookup here starts from, on a
/// thread in COM (`com::join_mta`).
fn device_enumerator() -> Result<IMMDeviceEnumerator, AppError> {
    crate::com::join_mta();
    unsafe {
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .windows_ctx("CoCreateInstance MMDeviceEnumerator")
    }
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
///
/// Each session's pid is first canonicalized to the topmost ancestor still
/// running the same exe (see `canonical_root_pid`) before being added to
/// `process_ids`. This matters because `run_process_loopback_capture` always
/// captures with `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE` (its
/// whole process tree, not just that one pid) -- so a multi-process app
/// (Electron apps like Discord spawn several child processes under the same
/// exe: GPU, renderer, utility, audio service) that has more than one of
/// those hold its own session would otherwise get one capture per pid, each
/// already covering the other's audio via the tree -- producing duplicate,
/// fully redundant tracks. Canonicalizing first means only genuinely
/// independent process trees (unrelated top-level launches of the same exe)
/// end up as separate `process_ids` entries.
pub fn enumerate_app_audio_sessions() -> Result<Vec<AppAudioSource>, AppError> {
    let mut sources = Vec::new();
    let processes = snapshot_processes();
    let enumerator = device_enumerator()?;
    unsafe {
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .windows_ctx("GetDefaultAudioEndpoint")?;
        let session_manager: IAudioSessionManager2 = device
            .Activate(CLSCTX_ALL, None)
            .windows_ctx("Activate IAudioSessionManager2")?;
        let session_enum = session_manager
            .GetSessionEnumerator()
            .windows_ctx("GetSessionEnumerator")?;
        let count = session_enum.GetCount().windows_ctx("GetCount")?;

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
            let Some(exe_name) = exe_path.as_deref().and_then(crate::sources::exe_file_name) else {
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

            let canonical_pid = canonical_root_pid(pid, &exe_name, &processes);

            match sources
                .iter_mut()
                .find(|s: &&mut AppAudioSource| s.exe_name == exe_name)
            {
                Some(existing) => {
                    if !existing.process_ids.contains(&canonical_pid) {
                        existing.process_ids.push(canonical_pid);
                    }
                }
                None => {
                    let icon_rgba = exe_path
                        .as_deref()
                        .and_then(crate::sources::extract_exe_icon_rgba);
                    sources.push(AppAudioSource {
                        process_ids: vec![canonical_pid],
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

/// Every running process, by pid: its parent's pid and its exe file name
/// (e.g. "Discord.exe"), as one snapshot saw them.
type Processes = std::collections::HashMap<u32, (u32, String)>;

/// Snapshot of every running process (`Processes`), for `canonical_root_pid`
/// to walk. Built once per `enumerate_app_audio_sessions` call rather than
/// once per session found -- `CreateToolhelp32Snapshot` walks the whole
/// system process list regardless of how many sessions end up using it, so
/// one snapshot serves them all. It has each process's exe name too, so the
/// walk needn't open each ancestor to read its name, and the names are from
/// the same moment as the parents. Best-effort: an empty map on failure just means
/// `canonical_root_pid` can't walk upward at all, falling back to per-pid
/// behavior for every session, not a hard error.
fn snapshot_processes() -> Processes {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let mut map = std::collections::HashMap::new();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return map;
        };
        let snapshot = Owned::new(snapshot);
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(*snapshot, &mut entry).is_ok() {
            loop {
                let name = &entry.szExeFile;
                let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
                map.insert(
                    entry.th32ProcessID,
                    (
                        entry.th32ParentProcessID,
                        String::from_utf16_lossy(&name[..len]),
                    ),
                );
                if Process32NextW(*snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
    }
    map
}

/// Walks up the process tree from `pid` through `processes`, staying only
/// within processes that share `exe_name`, to find the topmost still-same-exe
/// ancestor -- see `enumerate_app_audio_sessions`'s doc comment for why this
/// is the right identity to capture on rather than the raw session pid.
/// Stops (returning `pid` itself, or whatever ancestor it reached) as soon as
/// the parent is missing from the snapshot, already exited, or a different
/// exe entirely -- that boundary is exactly where the "same app" tree ends
/// (e.g. Discord.exe's topmost process's own parent is explorer.exe, a
/// different exe, so the walk correctly stops there). Bounded to 64 hops as
/// a defensive guard against a pid/ppid cycle from a snapshot racing process
/// exit and id reuse -- not expected on a real process tree, but cheap to
/// guard against.
fn canonical_root_pid(pid: u32, exe_name: &str, processes: &Processes) -> u32 {
    let mut current = pid;
    for _ in 0..64 {
        let Some(&(parent, _)) = processes.get(&current) else {
            break;
        };
        if parent == 0 || parent == current {
            break;
        }
        let Some((_, parent_exe_name)) = processes.get(&parent) else {
            break;
        };
        if !parent_exe_name.eq_ignore_ascii_case(exe_name) {
            break;
        }
        current = parent;
    }
    current
}

/// Read PKEY_Device_FriendlyName from the device property store.
/// GUID: {A45C254E-DF1C-4EFD-8020-67D146A850E0}, pid = 14
fn get_device_friendly_name(device: &IMMDevice) -> Option<String> {
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

/// Records a device track -- Windows' default output as loopback (the
/// system mix), or its default input -- and forwards PCM f32 samples to
/// `tx`. It follows the default (see `DefaultEndpoint`): when Windows moves
/// it, or the device goes away, the track carries on from whatever the
/// default is then, as silence while there's none (`reopen`).
pub async fn run_audio_capture(
    track_id: TrackId,
    is_loopback: bool,
    ctrl: CaptureControl,
    tx: mpsc::Sender<AudioSamples>,
    gain: f32,
) -> Result<(), AppError> {
    let enumerator = device_enumerator()?;
    let endpoint = DefaultEndpoint::for_track(is_loopback);
    let open = || unsafe { endpoint.open(&enumerator) };
    let mut coverage = TrackCoverage::new(track_id, ctrl.clock.elapsed());
    // Failing here is the track failing to start, as it always was.
    let mut stream = open()?;
    loop {
        let options = TrackOptions {
            gain,
            watch_pid: None,
            fill_silence: is_loopback,
            follow: Some(FollowDefault::new(enumerator.clone(), endpoint, stream.id)),
        };
        let end = unsafe {
            run_capture_loop(
                stream.client,
                stream.format,
                track_id,
                &ctrl,
                &tx,
                options,
                &mut coverage,
            )
            .await?
        };
        match end {
            StreamEnd::Done => return Ok(()),
            StreamEnd::Moved => tracing::info!(
                "AudioCapture[{track_id:?}]: Windows' default device changed; recording from the new one"
            ),
            StreamEnd::Lost => {
                tracing::info!("AudioCapture[{track_id:?}]: opening Windows' default device again")
            }
        }
        match reopen(&open, &REOPEN, track_id, &ctrl, &tx, &mut coverage).await {
            Some(next) => stream = next,
            None => return Ok(()),
        }
    }
}

/// A COM-allocated (`CoTaskMemAlloc`) pointer, freed with `CoTaskMemFree`
/// on drop.
struct CoTaskMem<T>(*mut T);

impl<T> Drop for CoTaskMem<T> {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0 as *const _)) };
    }
}

impl CoTaskMem<u16> {
    /// The wide string it holds; empty if there's none, or it isn't UTF-16.
    fn text(&self) -> String {
        if self.0.is_null() {
            return String::new();
        }
        unsafe { windows::core::PWSTR(self.0).to_string() }.unwrap_or_default()
    }
}

/// Capture only the audio produced by `target_pid` and its child processes via the
/// Windows 10 2004+ Process Loopback Capture API, instead of the full desktop audio
/// mix. Forwards PCM f32 samples to `tx`, same as `run_audio_capture`.
pub async fn run_process_loopback_capture(
    target_pid: u32,
    track_id: TrackId,
    ctrl: CaptureControl,
    tx: mpsc::Sender<AudioSamples>,
    gain: f32,
) -> Result<(), AppError> {
    crate::com::join_mta();
    unsafe {
        let audio_client = activate_process_loopback_audio_client(target_pid)?;

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
                WASAPI_BUFFER_HNS,
                0i64,
                &wave_format,
                None,
            )
            .windows_ctx("IAudioClient::Initialize (process loopback)")?;

        let mut coverage = TrackCoverage::new(track_id, ctrl.clock.elapsed());
        run_capture_loop(
            audio_client,
            StreamFormat {
                sample_rate: TARGET_SAMPLE_RATE,
                channels: TARGET_CHANNELS,
                bytes_per_frame: block_align as usize,
            },
            track_id,
            &ctrl,
            &tx,
            TrackOptions {
                gain,
                watch_pid: Some(target_pid),
                fill_silence: true,
                follow: None,
            },
            &mut coverage,
        )
        .await
        // However it ended, the track has: its pump goes on as silence.
        .map(|_| ())
    }
}

/// Best-effort: `OpenProcess` succeeding is enough to call `pid` alive,
/// doesn't need real query rights. A pid getting reused by an unrelated
/// process right after the original exits is the standard, accepted caveat
/// of any pid-based liveness check -- rare enough in the ~1s polling window
/// this is checked on not to matter here.
fn process_is_alive(pid: u32) -> bool {
    unsafe {
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .map(|handle| Owned::new(handle))
            .is_ok()
    }
}

/// The negotiated stream format `run_capture_loop` unpacks each WASAPI
/// buffer with, before downmixing/resampling to the fixed encoder target.
#[derive(Clone, Copy)]
struct StreamFormat {
    sample_rate: u32,
    channels: u16,
    bytes_per_frame: usize,
}

/// Per-track settings for `run_capture_loop`.
struct TrackOptions {
    gain: f32,
    /// Stop the track once this process exits (process loopback).
    watch_pid: Option<u32>,
    /// Fill stretches without packets with silence -- see `SilenceFill`.
    fill_silence: bool,
    /// End the stream once Windows' default moves elsewhere (device tracks).
    follow: Option<FollowDefault>,
}

/// Why `run_capture_loop` returned.
enum StreamEnd {
    /// Stop, the recording no longer taking samples, or the watched process
    /// exiting.
    Done,
    /// The endpoint went away (unplugged, disabled), or `GetBuffer` kept
    /// failing on it.
    Lost,
    /// Windows' default moved to another endpoint (see `FollowDefault`).
    Moved,
}

/// How often the capture loop looks for a packet while there's none, and
/// `reopen` for silence to send -- about as often as WASAPI hands them out.
const IDLE_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// How long a loopback track may go without packets before it gets silence.
/// WASAPI hands out a packet about every 10 ms while audio flows, so this is
/// well clear of normal packet spacing.
const SILENCE_FILL_AFTER: std::time::Duration = std::time::Duration::from_millis(100);
/// The most silence handed out at once, so a long gap is filled in pieces.
const SILENCE_FILL_MAX: std::time::Duration = std::time::Duration::from_secs(1);

/// Duration of `frames` at `TARGET_SAMPLE_RATE`.
fn frames_duration(frames: usize) -> std::time::Duration {
    std::time::Duration::from_nanos(frames as u64 * 1_000_000_000 / u64::from(TARGET_SAMPLE_RATE))
}

/// Keeps a loopback track's audio continuous. Loopback capture (desktop or
/// per-app) delivers no packets at all while nothing is playing; that's how
/// WASAPI works. A track that stops getting samples stalls the whole
/// recording: the MP4 sink writer can't interleave the other streams past
/// it, holds their data in memory, and its throttling then slows every
/// `WriteSample` to about a second. So the recording falls further behind,
/// and stopping takes minutes. This tracks how far the track's audio covers
/// the recording clock and hands out silence for the gaps.
struct SilenceFill {
    /// Recording time the track's samples so far reach up to.
    covered_until: std::time::Duration,
}

impl SilenceFill {
    fn new(start: std::time::Duration) -> Self {
        Self {
            covered_until: start,
        }
    }

    /// A real packet stamped `pts`, lasting `duration`, was sent.
    fn on_packet(&mut self, pts: std::time::Duration, duration: std::time::Duration) {
        self.covered_until = self.covered_until.max(pts + duration);
    }

    /// Silence to send at `now`, if the track has gone quiet: its pts and
    /// length in frames. It starts exactly where the track's audio ends, so
    /// the track stays contiguous.
    fn gap_to_fill(&mut self, now: std::time::Duration) -> Option<(std::time::Duration, usize)> {
        let gap = now.checked_sub(self.covered_until)?;
        if gap < SILENCE_FILL_AFTER {
            return None;
        }
        let gap = gap.min(SILENCE_FILL_MAX);
        let frames = (gap.as_nanos() * u128::from(TARGET_SAMPLE_RATE) / 1_000_000_000) as usize;
        let pts = self.covered_until;
        self.covered_until += frames_duration(frames);
        Some((pts, frames))
    }
}

/// How far an audio track's samples reach, and the silence that fills its
/// gaps -- `SilenceFill` for one track's `AudioSamples`. Used by the capture
/// loop for a quiet loopback track, and by the track's pump once its capture
/// has ended while the recording goes on (see
/// `encode::actor::spawn_audio_pump`).
pub(crate) struct TrackCoverage {
    track_id: TrackId,
    silence: SilenceFill,
}

impl TrackCoverage {
    /// A track whose audio starts at recording time `start`.
    pub(crate) fn new(track_id: TrackId, start: std::time::Duration) -> Self {
        Self {
            track_id,
            silence: SilenceFill::new(start),
        }
    }

    /// `samples` were sent for the track.
    pub(crate) fn on_samples(&mut self, samples: &AudioSamples) {
        self.silence.on_packet(samples.pts, samples.duration());
    }

    /// Silence for the track's gap up to recording time `now`, once there's
    /// enough of one to fill -- see `SilenceFill::gap_to_fill`.
    pub(crate) fn silence_due(&mut self, now: std::time::Duration) -> Option<AudioSamples> {
        let (pts, frames) = self.silence.gap_to_fill(now)?;
        Some(AudioSamples {
            track_id: self.track_id,
            pts,
            samples: vec![0.0; frames * usize::from(TARGET_CHANNELS)],
            sample_rate: TARGET_SAMPLE_RATE,
            channels: TARGET_CHANNELS,
        })
    }
}

/// Windows' default endpoint a device track records: the default output (as
/// loopback) or the default input. They're the two devices the dashboard
/// lists (`enumerate_audio_devices`), and a recording follows them: plug a
/// headset in mid-recording and the track moves to it (`run_audio_capture`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DefaultEndpoint {
    flow: EDataFlow,
    role: ERole,
}

impl DefaultEndpoint {
    const OUTPUT: Self = Self {
        flow: eRender,
        role: eMultimedia,
    };
    const INPUT: Self = Self {
        flow: eCapture,
        role: eConsole,
    };

    fn for_track(is_loopback: bool) -> Self {
        if is_loopback {
            Self::OUTPUT
        } else {
            Self::INPUT
        }
    }

    /// The endpoint Windows has as this default now.
    unsafe fn device(self, enumerator: &IMMDeviceEnumerator) -> Result<IMMDevice, AppError> {
        unsafe {
            enumerator
                .GetDefaultAudioEndpoint(self.flow, self.role)
                .windows_ctx("GetDefaultAudioEndpoint")
        }
    }

    /// Opens a stream on the endpoint Windows has as this default now: as
    /// loopback for the output.
    unsafe fn open(self, enumerator: &IMMDeviceEnumerator) -> Result<EndpointStream, AppError> {
        unsafe {
            let device = self.device(enumerator)?;
            let id = endpoint_id(&device);
            let audio_client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .windows_ctx("Activate IAudioClient")?;

            // GetMixFormat hands back a CoTaskMemAlloc'd WAVEFORMATEX the
            // caller owns. Initialize copies what it needs, so it's freed as
            // soon as this returns, on every path.
            let mix_format = CoTaskMem(audio_client.GetMixFormat().windows_ctx("GetMixFormat")?);

            let stream_flags: u32 = if self == Self::OUTPUT {
                AUDCLNT_STREAMFLAGS_LOOPBACK
            } else {
                0u32
            };

            // hnsperiodicity = 0 for shared mode
            audio_client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    stream_flags,
                    WASAPI_BUFFER_HNS,
                    0i64,
                    mix_format.0,
                    None,
                )
                .windows_ctx("IAudioClient::Initialize")?;

            let channels = (*mix_format.0).nChannels;
            if channels == 0 {
                return Err(AppError::Windows("WASAPI mix format has 0 channels".into()));
            }
            Ok(EndpointStream {
                client: audio_client,
                format: StreamFormat {
                    sample_rate: (*mix_format.0).nSamplesPerSec,
                    channels,
                    bytes_per_frame: (*mix_format.0).nBlockAlign as usize,
                },
                id,
            })
        }
    }
}

/// A device track's stream, ready to `run_capture_loop`, and the endpoint
/// it's on.
struct EndpointStream {
    client: IAudioClient,
    format: StreamFormat,
    id: String,
}

/// `device`'s endpoint id, or "" if it can't say.
unsafe fn endpoint_id(device: &IMMDevice) -> String {
    // Ours to free: the dashboard asks every ~2 s, and so does each device
    // track (`FollowDefault`).
    unsafe {
        device
            .GetId()
            .map(|p| CoTaskMem(p.0).text())
            .unwrap_or_default()
    }
}

/// How often a device track checks where Windows' default is.
const DEFAULT_CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// Notices Windows' default moving off the endpoint a device track records
/// -- a headset plugged in, or another default picked in Sound settings.
/// The old endpoint stays open and valid, it just gets no audio any more: a
/// loopback track used to record silence from there to the end, with
/// nothing to say so.
struct FollowDefault {
    enumerator: IMMDeviceEnumerator,
    endpoint: DefaultEndpoint,
    bound_id: String,
    checked_at: std::time::Instant,
}

impl FollowDefault {
    fn new(enumerator: IMMDeviceEnumerator, endpoint: DefaultEndpoint, bound_id: String) -> Self {
        Self {
            enumerator,
            endpoint,
            bound_id,
            checked_at: std::time::Instant::now(),
        }
    }

    /// Whether the default has moved; it's looked up every
    /// `DEFAULT_CHECK_EVERY`.
    fn moved(&mut self) -> bool {
        if !due(
            &mut self.checked_at,
            std::time::Instant::now(),
            DEFAULT_CHECK_EVERY,
        ) {
            return false;
        }
        let current = unsafe { self.endpoint.device(&self.enumerator) }
            .map(|device| unsafe { endpoint_id(&device) });
        default_moved(&self.bound_id, current.ok().as_deref())
    }
}

/// The devices the dashboard lists, in order: the default each one is, the
/// name to show if it has none, and whether it records as loopback.
const LISTED_DEFAULTS: [(DefaultEndpoint, &str, bool); 2] = [
    (DefaultEndpoint::OUTPUT, "Default Output", true),
    (DefaultEndpoint::INPUT, "Default Input", false),
];

/// Whether Windows' default, now `current` (`None`: there's none at all), is
/// another endpoint than the `bound` one a track records.
fn default_moved(bound: &str, current: Option<&str>) -> bool {
    current.is_some_and(|id| !id.is_empty() && id != bound)
}

/// Whether `every` has passed since `last` at `now`; if so, `last` becomes
/// `now`.
fn due(last: &mut std::time::Instant, now: std::time::Instant, every: std::time::Duration) -> bool {
    if now.saturating_duration_since(*last) < every {
        return false;
    }
    *last = now;
    true
}

/// A device track's time without an endpoint to record from (see `reopen`).
#[derive(Default)]
struct Outage {
    since: Option<std::time::Instant>,
    reported: bool,
}

impl Outage {
    /// There's no endpoint at `now`. True once, when that has lasted
    /// `report_after`.
    fn no_endpoint(&mut self, now: std::time::Instant, report_after: std::time::Duration) -> bool {
        let since = *self.since.get_or_insert(now);
        if self.reported || now.saturating_duration_since(since) < report_after {
            return false;
        }
        self.reported = true;
        true
    }

    /// There's an endpoint again. True if the outage had been reported.
    fn ended(&mut self) -> bool {
        std::mem::take(self).reported
    }
}

/// How `reopen` paces itself.
struct ReopenTiming {
    /// How often it looks for an endpoint.
    every: std::time::Duration,
    /// How long it goes without one before telling the recording.
    report_after: std::time::Duration,
}

const REOPEN: ReopenTiming = ReopenTiming {
    every: std::time::Duration::from_millis(500),
    report_after: std::time::Duration::from_secs(2),
};

/// Opens a device track's stream again (`open`) after its endpoint went
/// away or Windows' default moved. While there's no endpoint at all -- the
/// only mic unplugged, say -- it keeps looking, sending silence meanwhile
/// so the recording isn't held up (see `SilenceFill`). Once that has lasted
/// (`Outage`), the recording hears the track was lost
/// (`CaptureControl::audio_source_lost`), and if a device comes back, that
/// it was only interrupted (`audio_source_back`). `None`: stopped, or the
/// recording no longer takes samples, first.
async fn reopen<S>(
    mut open: impl FnMut() -> Result<S, AppError>,
    timing: &ReopenTiming,
    track_id: TrackId,
    ctrl: &CaptureControl,
    tx: &mpsc::Sender<AudioSamples>,
    coverage: &mut TrackCoverage,
) -> Option<S> {
    let mut outage = Outage::default();
    loop {
        match open() {
            Ok(stream) => {
                if outage.ended() {
                    tracing::info!("AudioCapture[{track_id:?}]: a device is back; recording again");
                    ctrl.audio_source_back(track_id);
                }
                return Some(stream);
            }
            Err(e) => {
                if outage.no_endpoint(std::time::Instant::now(), timing.report_after) {
                    tracing::warn!(
                        "AudioCapture[{track_id:?}]: no device to record from ({e}); silent until one is back"
                    );
                    ctrl.audio_source_lost(track_id);
                }
            }
        }
        let retry_at = std::time::Instant::now() + timing.every;
        while std::time::Instant::now() < retry_at {
            if ctrl.is_stopped() {
                return None;
            }
            if !ctrl.is_paused()
                && let Some(silence) = coverage.silence_due(ctrl.clock.elapsed())
                && tx.send(silence).await.is_err()
            {
                return None;
            }
            tokio::time::sleep(IDLE_POLL).await;
        }
    }
}

/// How long `GetBuffer` may keep failing (with anything but
/// `AUDCLNT_E_DEVICE_INVALIDATED`, which ends the track at once) before the
/// track is given up on -- the audio service restarting, say. Its pump then
/// keeps it going as silence.
const GET_BUFFER_FAILING_FOR: std::time::Duration = std::time::Duration::from_secs(1);

/// How much sound Windows keeps for a capture stream to read, as
/// `IAudioClient::Initialize` takes it (100 ns units): half a second. It is
/// room, not delay -- packets are read as they come either way -- and what a
/// capture thread held up for longer than this loses (see `LostSound`). It
/// was 100 ms, less than the stalls a busy game's machine shows.
const WASAPI_BUFFER_HNS: i64 = 5_000_000;

/// `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY` -- WASAPI's "sound was lost
/// before this packet" flag.
const AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY: u32 = 1;

/// Counts the times Windows says a stream lost sound: its buffer ran full
/// because the capture thread didn't read it in time, and what didn't fit is
/// gone -- the track runs short by that much from there on. Nothing showed
/// it before; a long recording had a track lose a quarter of a second in
/// steps this way.
#[derive(Debug, Default, PartialEq)]
struct LostSound {
    packets: u64,
    times: u64,
}

impl LostSound {
    /// Notes a packet's `flags`: whether this is the first time the stream
    /// lost sound, for the log to say so once as it happens. A stream's
    /// first packet carries the flag as a matter of course, with nothing
    /// before it to lose.
    fn note(&mut self, flags: u32) -> bool {
        self.packets += 1;
        if self.packets == 1 || flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY == 0 {
            return false;
        }
        self.times += 1;
        self.times == 1
    }
}

/// `AUDCLNT_BUFFERFLAGS_SILENT` -- WASAPI's "this packet is silence" flag.
const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 2;

/// One WASAPI packet as f32 samples: `sample_count` interleaved samples,
/// each `bytes_per_sample` wide, read from `packet`. f32 (4 bytes/sample) and
/// i16 (2 bytes/sample) mix formats are the only ones understood; `None` for
/// anything else (e.g. 24-bit, real on some pro/USB audio interfaces), which
/// the caller records as silence rather than garbage.
fn decode_packet(packet: &[u8], sample_count: usize, bytes_per_sample: usize) -> Option<Vec<f32>> {
    let samples = match bytes_per_sample {
        4 => packet.as_chunks::<4>().0[..sample_count.min(packet.len() / 4)]
            .iter()
            .map(|&b| f32::from_ne_bytes(b))
            .collect(),
        2 => packet.as_chunks::<2>().0[..sample_count.min(packet.len() / 2)]
            .iter()
            .map(|&b| i16::from_ne_bytes(b) as f32 / 32768.0)
            .collect(),
        _ => return None,
    };
    Some(samples)
}

/// WASAPI's contract for `AUDCLNT_BUFFERFLAGS_SILENT`: the packet is
/// silence, whatever its data bytes happen to contain -- so it's recorded
/// as zeros.
fn apply_silent_flag(samples: &mut [f32], flags: u32) {
    if flags & AUDCLNT_BUFFERFLAGS_SILENT != 0 {
        samples.fill(0.0);
    }
}

/// One-shot diagnostic for a report of app-audio tracks recording as
/// silence despite the source app audibly producing sound (mic/device
/// tracks unaffected) -- distinguishes "WASAPI itself reports this
/// buffer as silent" (AUDCLNT_BUFFERFLAGS_SILENT, meaning the process-
/// loopback session genuinely isn't seeing the target's real audio
/// graph -- a Windows/targeting issue, not something fixable here)
/// from "WASAPI handed us a real, non-silent buffer but every sample
/// we read out is still zero" (a parsing bug in this loop). Logged
/// once per track after ~1s of buffers so it doesn't spam.
struct PacketDiagnostics {
    buffers_seen: u32,
    any_silent_flag: bool,
    max_abs_sample: f32,
    logged: bool,
    start: std::time::Instant,
    zero_buffer_warned: bool,
}

impl PacketDiagnostics {
    fn new() -> Self {
        Self {
            buffers_seen: 0,
            any_silent_flag: false,
            max_abs_sample: 0.0,
            logged: false,
            start: std::time::Instant::now(),
            zero_buffer_warned: false,
        }
    }

    /// Called for every non-empty packet, with its WASAPI flags and decoded
    /// samples.
    fn record_packet(&mut self, track_id: TrackId, flags: u32, samples: &[f32]) {
        if self.logged {
            return;
        }
        if flags & AUDCLNT_BUFFERFLAGS_SILENT != 0 {
            self.any_silent_flag = true;
        }
        for &s in samples {
            self.max_abs_sample = self.max_abs_sample.max(s.abs());
        }
        self.buffers_seen += 1;
        if self.buffers_seen >= 100 {
            self.logged = true;
            tracing::info!(
                "AudioCapture[{track_id:?}] diagnostic (first ~{} buffers): any_silent_flag={} max_abs_sample={:.6}",
                self.buffers_seen,
                self.any_silent_flag,
                self.max_abs_sample
            );
        }
    }

    /// Companion to `record_packet`, called on every empty poll: if this
    /// track has gone 5+ seconds without GetBuffer ever reporting
    /// frames_available > 0 even once, WriteSample is never called for it at
    /// all -- which (per the export-dialog report this is chasing) means the
    /// track can end up entirely absent from the finished file, not just
    /// silent. One-shot so it doesn't spam a track that's just between
    /// sounds.
    fn check_no_packets_yet(&mut self, track_id: TrackId) {
        if !self.zero_buffer_warned
            && self.buffers_seen == 0
            && self.start.elapsed() >= std::time::Duration::from_secs(5)
        {
            self.zero_buffer_warned = true;
            tracing::warn!(
                "AudioCapture[{track_id:?}] diagnostic: no buffer with frames_available > 0 in {:.1}s -- this track may end up with zero samples written",
                self.start.elapsed().as_secs_f32()
            );
        }
    }
}

/// Shared buffer-read loop: starts the (already-initialized) client, unpacks each
/// buffer at its native `sample_rate`/`channels`, downmixes + resamples to the fixed
/// encoder target, and forwards `AudioSamples` until the receiver drops, the
/// stop signal is set, or the device/target process goes away or Windows'
/// default moves (`StreamEnd` says which). `coverage` is the track's, kept
/// across the streams a device track goes through.
///
/// `watch_pid` is `Some` only for process-loopback capture -- a target
/// process simply exiting mid-recording isn't a WASAPI device error (unlike
/// `AUDCLNT_E_DEVICE_INVALIDATED`, handled below), so `GetBuffer` just
/// returns zero frames forever; without watching for the process's own exit
/// separately, the capture thread spins for the rest of the recording
/// producing a track of pure silence instead of ending cleanly.
async unsafe fn run_capture_loop(
    audio_client: IAudioClient,
    StreamFormat {
        sample_rate,
        channels,
        bytes_per_frame,
    }: StreamFormat,
    track_id: TrackId,
    ctrl: &CaptureControl,
    tx: &mpsc::Sender<AudioSamples>,
    mut options: TrackOptions,
    coverage: &mut TrackCoverage,
) -> Result<StreamEnd, AppError> {
    unsafe {
        let capture_client: IAudioCaptureClient = audio_client
            .GetService()
            .windows_ctx("GetService IAudioCaptureClient")?;

        audio_client.Start().windows_ctx("IAudioClient::Start")?;

        let mut resampler = LinearResampler::new(sample_rate, TARGET_SAMPLE_RATE, TARGET_CHANNELS);
        let bytes_per_sample = bytes_per_frame / channels.max(1) as usize;

        let mut diagnostics = PacketDiagnostics::new();
        let mut lost_sound = LostSound::default();
        // Since when `GetBuffer` has been failing, if it is -- see
        // `GET_BUFFER_FAILING_FOR`.
        let mut failing_since: Option<std::time::Instant> = None;
        // Rate-limited like the disk-space guard elsewhere -- OpenProcess is
        // a real syscall, not worth doing on every 10ms idle poll.
        let mut last_pid_check = std::time::Instant::now();
        // A device that negotiates a mix format `decode_packet` doesn't
        // understand would otherwise silently write zeros for every sample:
        // a genuinely connected, genuinely producing device that ends up
        // indistinguishable from "just quiet" in the recording, with nothing
        // in the existing "missing audio track" warning able to catch it
        // (samples ARE written, just wrong). One-shot so it doesn't spam
        // every packet.
        let mut warned_unrecognized_bit_depth = false;

        let end = loop {
            if ctrl.is_stopped() {
                break StreamEnd::Done;
            }
            if let Some(follow) = &mut options.follow
                && follow.moved()
            {
                break StreamEnd::Moved;
            }

            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames_available: u32 = 0;
            let mut flags: u32 = 0;

            let got =
                capture_client.GetBuffer(&mut data, &mut frames_available, &mut flags, None, None);
            if got.is_ok() {
                failing_since = None;
            }
            match got {
                Ok(()) if frames_available > 0 => {
                    let sample_count = frames_available as usize * channels as usize;
                    // SAFETY: GetBuffer just handed us `frames_available`
                    // frames of `bytes_per_frame` bytes each at `data`,
                    // valid until the ReleaseBuffer below.
                    let packet = std::slice::from_raw_parts(
                        data,
                        frames_available as usize * bytes_per_frame,
                    );
                    let mut samples = decode_packet(packet, sample_count, bytes_per_sample)
                        .unwrap_or_else(|| {
                            if !warned_unrecognized_bit_depth {
                                warned_unrecognized_bit_depth = true;
                                tracing::warn!(
                                    "AudioCapture[{track_id:?}]: unrecognized mix format ({} bytes/sample) -- writing silence for this track instead of garbage/OOB reads",
                                    bytes_per_sample
                                );
                            }
                            vec![0.0f32; sample_count]
                        });

                    capture_client
                        .ReleaseBuffer(frames_available)
                        .windows_ctx("ReleaseBuffer")?;

                    // Diagnostics see the data as delivered (their whole point
                    // is telling "flagged silent" apart from "zeros"); the
                    // recording gets what the flag says it is.
                    diagnostics.record_packet(track_id, flags, &samples);
                    if lost_sound.note(flags) {
                        tracing::warn!(
                            "AudioCapture[{track_id:?}]: Windows says sound was lost before this packet (the capture fell behind); the track runs short by it from here"
                        );
                    }
                    apply_silent_flag(&mut samples, flags);

                    // Always release buffer first (above), then discard if paused.
                    if ctrl.is_paused() {
                        continue;
                    }

                    let pts = ctrl.clock.elapsed();
                    let stereo = to_stereo(&samples, channels);
                    let mut resampled = resampler.process(&stereo);
                    apply_gain(&mut resampled, options.gain);
                    let audio = AudioSamples {
                        track_id,
                        pts,
                        samples: resampled,
                        sample_rate: TARGET_SAMPLE_RATE,
                        channels: TARGET_CHANNELS,
                    };
                    coverage.on_samples(&audio);

                    if tx.send(audio).await.is_err() {
                        // Receiver dropped — stop gracefully
                        break StreamEnd::Done;
                    }
                }
                Ok(()) => {
                    diagnostics.check_no_packets_yet(track_id);
                    if options.fill_silence
                        && !ctrl.is_paused()
                        && let Some(audio) = coverage.silence_due(ctrl.clock.elapsed())
                        && tx.send(audio).await.is_err()
                    {
                        break StreamEnd::Done;
                    }
                    if let Some(pid) = options.watch_pid
                        && due(
                            &mut last_pid_check,
                            std::time::Instant::now(),
                            std::time::Duration::from_secs(1),
                        )
                        && !process_is_alive(pid)
                    {
                        tracing::info!(
                            "AudioCapture[{track_id:?}]: target process {pid} exited, stopping this track"
                        );
                        break StreamEnd::Done;
                    }

                    // No data yet; yield to the async runtime
                    tokio::time::sleep(IDLE_POLL).await;
                }
                Err(e) if e.code() == AUDCLNT_E_DEVICE_INVALIDATED => {
                    // The endpoint was unplugged/disabled mid-capture -- retrying
                    // GetBuffer on it forever just spins and spams the warning
                    // below every 10ms, since the device is never coming back on
                    // this handle. A device track goes on from Windows' default
                    // (`run_audio_capture`); otherwise the track's pump keeps it
                    // going as silence.
                    tracing::warn!(
                        "AudioCapture[{track_id:?}]: its device went away (unplugged or disabled): {e}"
                    );
                    break StreamEnd::Lost;
                }
                Err(e) => {
                    let since = *failing_since.get_or_insert_with(|| {
                        tracing::warn!(
                            "AudioCapture[{track_id:?}]: GetBuffer failed ({e}); trying again for up to {GET_BUFFER_FAILING_FOR:?}"
                        );
                        std::time::Instant::now()
                    });
                    if since.elapsed() >= GET_BUFFER_FAILING_FOR {
                        tracing::error!(
                            "AudioCapture[{track_id:?}]: GetBuffer kept failing ({e}), giving up on this stream"
                        );
                        break StreamEnd::Lost;
                    }
                    tokio::time::sleep(IDLE_POLL).await;
                }
            }
        };

        if lost_sound.times > 0 {
            tracing::warn!(
                "AudioCapture[{track_id:?}]: lost sound {} times in {} packets",
                lost_sound.times,
                lost_sound.packets
            );
        }
        let _ = audio_client.Stop();
        Ok(end)
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
/// Retries on a timed-out activation instead of giving up after one attempt
/// -- under exactly the CPU/GPU contention this app is built to handle
/// gracefully (a demanding game + hardware encode + capture all competing),
/// a single slow activation previously dropped that track for the entire
/// recording with no second chance.
unsafe fn activate_process_loopback_audio_client(
    target_pid: u32,
) -> Result<IAudioClient, AppError> {
    const ACTIVATION_ATTEMPTS: u32 = 3;
    const ACTIVATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    for attempt in 1..=ACTIVATION_ATTEMPTS {
        let result =
            unsafe { try_activate_process_loopback_audio_client(target_pid, ACTIVATION_TIMEOUT) }?;
        if let Some(client) = result {
            return Ok(client);
        }
        if attempt < ACTIVATION_ATTEMPTS {
            tracing::warn!(
                "process loopback activation timed out for pid={target_pid} (attempt {attempt}/{ACTIVATION_ATTEMPTS}), retrying"
            );
        }
    }
    Err(AppError::Windows(format!(
        "ActivateAudioInterfaceAsync timed out after {ACTIVATION_ATTEMPTS} attempts for pid={target_pid}"
    )))
}

/// One activation attempt -- `Ok(None)` means it timed out (the caller may
/// retry), `Ok(Some(client))` is success, `Err` is a real failure not worth
/// retrying (activation API call itself failing, the OS reporting activation
/// failed, or the returned interface not actually being an `IAudioClient`).
unsafe fn try_activate_process_loopback_audio_client(
    target_pid: u32,
    timeout: std::time::Duration,
) -> Result<Option<IAudioClient>, AppError> {
    unsafe {
        let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: target_pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
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
        .windows_ctx("ActivateAudioInterfaceAsync");

        // `variant.blob.pBlobData` points at our stack `params`, not CoTaskMem-allocated
        // memory — dropping `variant` normally would run PropVariantClear, which for
        // VT_BLOB calls CoTaskMemFree on that pointer. Forget it instead; there's nothing
        // heap-owned in it for us to leak.
        core::mem::forget(variant);
        activate_result?;

        let activated = match result_rx.recv_timeout(timeout) {
            Ok(r) => r.windows_ctx("process loopback activation failed")?,
            Err(_) => return Ok(None),
        };

        activated
            .cast::<IAudioClient>()
            .map(Some)
            .windows_ctx("cast activated interface to IAudioClient")
    }
}

#[cfg(test)]
mod tests;
