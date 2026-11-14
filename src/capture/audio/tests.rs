use super::*;
use std::time::Duration;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// The dashboard lists Windows' default output and input; a track
/// records the default it was picked as, by the same role, so the
/// device it names is the one it records.
#[test]
fn a_device_track_records_the_default_it_was_listed_as() {
    for (endpoint, _, is_loopback) in LISTED_DEFAULTS {
        assert_eq!(DefaultEndpoint::for_track(is_loopback), endpoint);
    }
    assert_ne!(LISTED_DEFAULTS[0].0, LISTED_DEFAULTS[1].0);
}

#[test]
fn a_track_follows_a_new_default_but_not_the_lack_of_one() {
    assert!(!default_moved("{headset}", Some("{headset}")));
    assert!(default_moved("{headset}", Some("{speakers}")));
    // Nothing to move to: the stream on hand either still works, or
    // goes away and the track looks for one (`reopen`).
    assert!(!default_moved("{headset}", None));
    assert!(!default_moved("{headset}", Some("")));
}

#[test]
fn a_check_comes_round_no_more_often_than_asked() {
    let t0 = std::time::Instant::now();
    let mut last = t0;
    assert!(!due(&mut last, t0 + ms(1999), ms(2000)));
    assert!(due(&mut last, t0 + ms(2000), ms(2000)));
    assert!(!due(&mut last, t0 + ms(3000), ms(2000)));
    assert!(due(&mut last, t0 + ms(4000), ms(2000)));
}

/// Windows takes a moment to move its default once the old device has
/// gone. A gap that short isn't worth a notice; a longer one is, once.
#[test]
fn only_an_outage_that_lasts_is_reported_and_only_once() {
    let t0 = std::time::Instant::now();
    let after = ms(2000);
    let mut outage = Outage::default();
    assert!(!outage.no_endpoint(t0, after));
    assert!(!outage.no_endpoint(t0 + ms(1500), after));
    assert!(!outage.ended());

    assert!(!outage.no_endpoint(t0 + ms(3000), after));
    assert!(outage.no_endpoint(t0 + ms(5000), after));
    assert!(!outage.no_endpoint(t0 + ms(6000), after));
    assert!(outage.ended());
    assert!(!outage.ended());
}

const QUICK: ReopenTiming = ReopenTiming {
    every: Duration::from_millis(20),
    report_after: Duration::from_millis(150),
};

/// Stands in for `DefaultEndpoint::open`: no endpoint `failures` times,
/// then one.
fn opener(failures: usize) -> impl FnMut() -> Result<&'static str, AppError> {
    let mut left = failures;
    move || {
        if left == 0 {
            Ok("stream")
        } else {
            left -= 1;
            Err(AppError::Windows(
                "GetDefaultAudioEndpoint: not found".into(),
            ))
        }
    }
}

#[tokio::test]
async fn a_default_that_is_there_is_opened_straight_away_without_a_notice() {
    let ctrl = CaptureControl::new();
    let (tx, mut rx) = mpsc::channel(64);
    let track = TrackId::new(0);
    let mut coverage = TrackCoverage::new(track, ctrl.clock.elapsed());
    let opened = reopen(opener(0), &QUICK, track, &ctrl, &tx, &mut coverage).await;
    assert_eq!(opened, Some("stream"));
    assert_eq!(ctrl.lost_sources(), crate::capture::LostSources::default());
    assert!(rx.try_recv().is_err());
}

/// The only mic unplugged, then plugged back in: meanwhile the track is
/// silence rather than nothing (which holds the whole recording up, see
/// `SilenceFill`), and the recording hears it was interrupted.
#[tokio::test]
async fn a_track_waits_for_a_device_in_silence_and_reports_the_gap() {
    let ctrl = CaptureControl::new();
    let (tx, mut rx) = mpsc::channel(64);
    let track = TrackId::new(1);
    let mut coverage = TrackCoverage::new(track, ctrl.clock.elapsed());
    let opened = reopen(opener(15), &QUICK, track, &ctrl, &tx, &mut coverage).await;
    assert_eq!(opened, Some("stream"));
    assert_eq!(
        ctrl.lost_sources(),
        crate::capture::LostSources {
            video: false,
            audio: vec![],
            audio_interrupted: vec![track],
        }
    );
    let mut silent = Duration::ZERO;
    while let Ok(samples) = rx.try_recv() {
        assert_eq!(samples.track_id, track);
        assert!(samples.samples.iter().all(|&x| x == 0.0));
        silent += frames_duration(samples.samples.len() / usize::from(TARGET_CHANNELS));
    }
    assert!(silent >= SILENCE_FILL_AFTER, "silence sent: {silent:?}");
}

#[tokio::test]
async fn stopping_while_there_is_no_device_leaves_the_track_lost() {
    let ctrl = CaptureControl::new();
    let (tx, _rx) = mpsc::channel(64);
    let track = TrackId::new(0);
    let mut coverage = TrackCoverage::new(track, ctrl.clock.elapsed());
    let stopper = ctrl.clone();
    tokio::spawn(async move {
        tokio::time::sleep(ms(300)).await;
        stopper.request_stop();
    });
    let opened = reopen(opener(usize::MAX), &QUICK, track, &ctrl, &tx, &mut coverage).await;
    assert_eq!(opened, None);
    assert_eq!(ctrl.lost_sources().audio, vec![track]);
}

/// Feeds `SilenceFill` an idle stretch, polled every 10 ms like the
/// capture loop does, and returns the fills it asked for.
fn idle_fills(fill: &mut SilenceFill, from: Duration, to: Duration) -> Vec<(Duration, usize)> {
    let mut out = Vec::new();
    let mut now = from;
    while now <= to {
        out.extend(fill.gap_to_fill(now));
        now += ms(10);
    }
    out
}

#[test]
fn silence_fill_covers_a_track_that_never_delivers_a_packet() {
    let mut fill = SilenceFill::new(Duration::ZERO);
    let fills = idle_fills(&mut fill, Duration::ZERO, ms(3000));
    // Contiguous: each fill starts exactly where the previous one ended.
    let mut cursor = Duration::ZERO;
    for &(pts, frames) in &fills {
        assert_eq!(pts, cursor);
        cursor += frames_duration(frames);
    }
    // ...and keeps the track within one fill threshold of the clock.
    assert!(
        ms(3000) - cursor <= SILENCE_FILL_AFTER,
        "covered {cursor:?}"
    );
}

#[test]
fn silence_fill_stays_out_of_the_way_while_packets_arrive() {
    let mut fill = SilenceFill::new(Duration::ZERO);
    // 10 ms packets, each received as it ends.
    for i in 1..=300u64 {
        fill.on_packet(ms(i * 10), ms(10));
        assert_eq!(fill.gap_to_fill(ms(i * 10 + 5)), None);
    }
}

#[test]
fn silence_fill_starts_where_the_last_packet_ended() {
    let mut fill = SilenceFill::new(Duration::ZERO);
    fill.on_packet(ms(990), ms(10)); // audio up to 1.0 s
    let fills = idle_fills(&mut fill, ms(1010), ms(1500));
    assert_eq!(fills.first().map(|&(pts, _)| pts), Some(ms(1000)));
}

#[test]
fn silence_fill_never_hands_out_more_than_a_second_at_once() {
    let mut fill = SilenceFill::new(Duration::ZERO);
    let (_, frames) = fill.gap_to_fill(ms(5000)).expect("a 5 s gap gets filled");
    assert!(frames_duration(frames) <= ms(1000));
}

/// Exercises the Process Loopback Capture API end to end (activation,
/// IAudioClient::Initialize, GetService/Start, buffer reads) against our own
/// process. Needs Windows 10 2004+ and an audio subsystem, so it's ignored by
/// default — run with `--ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn process_loopback_capture_produces_samples() {
    let (tx, mut rx) = mpsc::channel::<AudioSamples>(64);
    let ctrl = CaptureControl::new();
    let capture_ctrl = ctrl.clone();
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
            let _ = run_process_loopback_capture(pid, TrackId::new(0), capture_ctrl, tx, 1.0).await;
        });
    });

    let first = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for first AudioSamples packet")
        .expect("channel closed before any packet arrived");

    assert_eq!(first.sample_rate, TARGET_SAMPLE_RATE);
    assert_eq!(first.channels, TARGET_CHANNELS);
    assert!(!first.samples.is_empty(), "packet had no samples");

    // Aborting a blocking task doesn't stop it: the capture is asked to.
    ctrl.request_stop();
    drop(rx);
    tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("the capture didn't stop")
        .expect("capture thread");
}

/// A default-output track on the real audio stack, past its first check
/// of where the default is (`FollowDefault`): it keeps the track covered
/// -- packets, or silence while nothing plays -- notes nothing lost, and
/// ends soon after Stop. Plays nothing. Needs an audio device, so it's
/// ignored by default -- run with
/// `--ignored a_default_output_track_records_until_stopped`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn a_default_output_track_records_until_stopped() {
    let (tx, mut rx) = mpsc::channel::<AudioSamples>(256);
    let ctrl = CaptureControl::new();
    let track_ctrl = ctrl.clone();
    let handle = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("capture runtime");
        let local = tokio::task::LocalSet::new();
        local.block_on(
            &rt,
            run_audio_capture(TrackId::new(0), true, track_ctrl, tx, 1.0),
        )
    });

    let deadline = tokio::time::Instant::now() + DEFAULT_CHECK_EVERY + ms(1000);
    let mut covered_until = Duration::ZERO;
    while tokio::time::Instant::now() < deadline {
        if let Ok(Some(samples)) = tokio::time::timeout(ms(200), rx.recv()).await {
            let frames = samples.samples.len() / usize::from(samples.channels);
            covered_until = covered_until.max(samples.pts + frames_duration(frames));
        }
    }
    let elapsed = ctrl.clock.elapsed();
    assert!(
        elapsed.saturating_sub(covered_until) < ms(500),
        "track covers {covered_until:?} of {elapsed:?}"
    );

    ctrl.request_stop();
    let ended = tokio::time::timeout(ms(1000), handle)
        .await
        .expect("the track didn't end within 1 s of Stop");
    ended.expect("capture thread").expect("capture result");
    assert_eq!(ctrl.lost_sources(), crate::capture::LostSources::default());
}

/// Windows' default moving, as the capture loop sees it: a stream on an
/// endpoint that isn't the default any more ends as `Moved` at its next
/// check, for `run_audio_capture` to go on from the new one. (The test
/// can't move the user's default; it binds the check to an id that
/// isn't it.) Plays nothing; run with
/// `--ignored a_stream_off_the_default_endpoint_ends_as_moved`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn a_stream_off_the_default_endpoint_ends_as_moved() {
    let handle = tokio::task::spawn_blocking(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("capture runtime");
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            let enumerator = device_enumerator().expect("enumerator");
            let stream = unsafe { DefaultEndpoint::OUTPUT.open(&enumerator) }.expect("open");
            let ctrl = CaptureControl::new();
            let (tx, _rx) = mpsc::channel(256);
            let track = TrackId::new(0);
            let mut coverage = TrackCoverage::new(track, ctrl.clock.elapsed());
            let options = TrackOptions {
                gain: 1.0,
                watch_pid: None,
                fill_silence: true,
                follow: Some(FollowDefault::new(
                    enumerator,
                    DefaultEndpoint::OUTPUT,
                    "{the endpoint that was the default}".into(),
                )),
            };
            unsafe {
                run_capture_loop(
                    stream.client,
                    stream.format,
                    track,
                    &ctrl,
                    &tx,
                    options,
                    &mut coverage,
                )
                .await
            }
        })
    });
    let end = tokio::time::timeout(DEFAULT_CHECK_EVERY + ms(1000), handle)
        .await
        .expect("the stream didn't end at its check")
        .expect("capture thread")
        .expect("capture result");
    assert!(matches!(end, StreamEnd::Moved));
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

/// WASAPI hands audio over 10 ms at a time. At a rate that doesn't
/// divide evenly into 48 kHz (44.1 kHz), the last output sample of every
/// chunk fell between its last input frame and the next chunk's first,
/// not yet seen -- and was output as the last frame, held, instead of
/// waiting for it: a wrong sample every 10 ms, a faint 100 Hz buzz.
/// Chunked or not, the output must be the same.
#[test]
fn resampling_in_10ms_chunks_matches_resampling_all_at_once() {
    let channels = 2;
    // A ramp, so a held sample differs from an interpolated one.
    let input: Vec<f32> = (0..4410 * channels)
        .map(|i| (i / channels) as f32 / 4410.0)
        .collect();
    let whole = LinearResampler::new(44_100, 48_000, 2).process(&input);
    let mut chunked = LinearResampler::new(44_100, 48_000, 2);
    let pieces: Vec<f32> = input
        .chunks(441 * channels)
        .flat_map(|chunk| chunked.process(chunk))
        .collect();
    assert_eq!(pieces.len(), whole.len());
    for (i, (a, b)) in pieces.iter().zip(&whole).enumerate() {
        assert!((a - b).abs() < 1e-6, "sample {i}: {a} chunked, {b} whole");
    }
}

#[test]
fn resampler_constant_signal_stays_constant() {
    let mut r = LinearResampler::new(96000, 48000, 2);
    let input = vec![0.5f32; 200 * 2];
    let out = r.process(&input);
    assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-5));
}

/// A process snapshot of `(pid, parent pid, exe name)` entries.
fn tree(entries: &[(u32, u32, &str)]) -> Processes {
    entries
        .iter()
        .map(|&(pid, parent, exe)| (pid, (parent, exe.to_string())))
        .collect()
}

/// Discord.exe-shaped tree: main process 100 spawned renderer/GPU helper
/// 200, which in turn spawned a utility process 300 -- all still
/// Discord.exe. A session found on the deepest child (300) should
/// canonicalize all the way up to the topmost same-exe ancestor (100),
/// regardless of walk depth.
#[test]
fn canonical_root_pid_walks_up_through_multiple_same_exe_ancestors() {
    let processes = tree(&[
        (300, 200, "Discord.exe"),
        (200, 100, "Discord.exe"),
        (100, 1, "Discord.exe"),
    ]);
    assert_eq!(canonical_root_pid(300, "Discord.exe", &processes), 100);
}

/// The walk stops at the boundary where the parent is a different exe
/// (e.g. the topmost Discord.exe process was launched by explorer.exe) --
/// it must not keep climbing into an unrelated process tree.
#[test]
fn canonical_root_pid_stops_at_a_different_parent_exe() {
    let processes = tree(&[(100, 1, "Discord.exe"), (1, 0, "explorer.exe")]);
    assert_eq!(canonical_root_pid(100, "Discord.exe", &processes), 100);
}

/// Two genuinely independent top-level launches of the same exe (neither
/// is an ancestor of the other) must resolve to two different canonical
/// pids -- this is the "two separate game/app windows" case that should
/// still get two separate tracks.
#[test]
fn canonical_root_pid_leaves_unrelated_instances_distinct() {
    let processes = tree(&[
        (100, 1, "Game.exe"),
        (500, 1, "Game.exe"),
        (1, 0, "explorer.exe"),
    ]);
    let a = canonical_root_pid(100, "Game.exe", &processes);
    let b = canonical_root_pid(500, "Game.exe", &processes);
    assert_eq!(a, 100);
    assert_eq!(b, 500);
    assert_ne!(a, b);
}

/// A pid missing from the snapshot (e.g. the snapshot failed, or this
/// is the true root of the system process tree) must resolve to itself
/// rather than panicking or looping.
#[test]
fn canonical_root_pid_with_an_empty_snapshot_returns_pid_itself() {
    assert_eq!(
        canonical_root_pid(42, "Anything.exe", &Processes::new()),
        42
    );
}

/// A pid/ppid cycle (shouldn't happen on a real process tree, but a
/// snapshot racing pid reuse could in principle produce one) must not
/// hang -- the 64-hop bound should kick in and return some pid rather
/// than looping forever.
#[test]
fn canonical_root_pid_bounded_against_a_parent_cycle() {
    let processes = tree(&[(1, 2, "Loop.exe"), (2, 1, "Loop.exe")]);
    // Must return promptly (no infinite loop) -- which pid it lands on
    // isn't load-bearing for a case that shouldn't occur in practice.
    let _ = canonical_root_pid(1, "Loop.exe", &processes);
}

/// The per-sample pointer reads `run_capture_loop` used before
/// `decode_packet` existed -- kept here as the reference it must match.
fn decode_packet_reference(
    packet: &[u8],
    sample_count: usize,
    bytes_per_sample: usize,
) -> Vec<f32> {
    let mut samples = vec![0.0f32; sample_count];
    for (i, sample) in samples.iter_mut().enumerate() {
        let p = unsafe { packet.as_ptr().add(i * bytes_per_sample) };
        *sample = if bytes_per_sample == 4 {
            unsafe { (p as *const f32).read_unaligned() }
        } else if bytes_per_sample == 2 {
            (unsafe { (p as *const i16).read_unaligned() }) as f32 / 32768.0
        } else {
            0.0
        };
    }
    samples
}

fn pseudo_random_bytes(len: usize) -> Vec<u8> {
    let mut x: u32 = 0x1234_5678;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 24) as u8
        })
        .collect()
}

#[test]
fn decode_packet_matches_the_old_per_sample_reads_for_f32_and_i16() {
    for bytes_per_sample in [4usize, 2] {
        let sample_count = 480 * 2;
        // Finite f32 bit patterns only, so NaN != NaN can't confuse the
        // comparison: build f32 packets from real values.
        let packet: Vec<u8> = if bytes_per_sample == 4 {
            (0..sample_count)
                .flat_map(|i| ((i as f32 * 0.37).sin()).to_ne_bytes())
                .collect()
        } else {
            pseudo_random_bytes(sample_count * 2)
        };
        let decoded = decode_packet(&packet, sample_count, bytes_per_sample).unwrap();
        assert_eq!(
            decoded,
            decode_packet_reference(&packet, sample_count, bytes_per_sample),
            "{bytes_per_sample} bytes/sample"
        );
    }
}

#[test]
fn decode_packet_converts_i16_to_unit_range() {
    let packet: Vec<u8> = [16384i16, -32768, 0]
        .iter()
        .flat_map(|s| s.to_ne_bytes())
        .collect();
    assert_eq!(decode_packet(&packet, 3, 2), Some(vec![0.5, -1.0, 0.0]));
}

#[test]
fn decode_packet_reads_only_sample_count_samples() {
    let packet: Vec<u8> = [0.5f32, 0.25, 0.125]
        .iter()
        .flat_map(|s| s.to_ne_bytes())
        .collect();
    assert_eq!(decode_packet(&packet, 2, 4), Some(vec![0.5, 0.25]));
}

#[test]
fn decode_packet_rejects_unrecognized_sample_widths_without_panicking() {
    let packet = vec![1u8; 12];
    assert_eq!(decode_packet(&packet, 4, 3), None); // 24-bit
    assert_eq!(decode_packet(&packet, 4, 0), None); // degenerate format
}

#[test]
fn lost_sound_is_counted_and_said_once_but_not_for_a_streams_first_packet() {
    let mut lost = LostSound::default();
    // The first packet carries the flag with nothing lost.
    assert!(!lost.note(AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY));
    assert!(!lost.note(0));
    assert!(!lost.note(AUDCLNT_BUFFERFLAGS_SILENT));
    assert!(lost.note(AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY));
    assert!(!lost.note(AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY | AUDCLNT_BUFFERFLAGS_SILENT));
    assert_eq!(
        lost,
        LostSound {
            packets: 5,
            times: 2
        }
    );
}

#[test]
fn packet_diagnostics_log_once_after_100_packets_and_track_the_silent_flag() {
    let mut diag = PacketDiagnostics::new();
    let track = TrackId::new(0);
    diag.record_packet(track, AUDCLNT_BUFFERFLAGS_SILENT, &[0.0; 4]);
    diag.record_packet(track, 0, &[0.25, -0.75]);
    assert!(diag.any_silent_flag);
    assert_eq!(diag.max_abs_sample, 0.75);
    for _ in 2..100 {
        diag.record_packet(track, 0, &[0.1]);
    }
    assert!(diag.logged);
    assert_eq!(diag.buffers_seen, 100);
    // Frozen once logged.
    diag.record_packet(track, 0, &[0.9]);
    assert_eq!(diag.buffers_seen, 100);
    assert_eq!(diag.max_abs_sample, 0.75);
}

#[test]
fn packet_diagnostics_warn_about_no_packets_only_after_5s_and_only_once() {
    let track = TrackId::new(0);
    let mut diag = PacketDiagnostics::new();
    diag.check_no_packets_yet(track);
    assert!(!diag.zero_buffer_warned, "too early");

    diag.start = std::time::Instant::now() - std::time::Duration::from_secs(6);
    diag.check_no_packets_yet(track);
    assert!(diag.zero_buffer_warned);

    let mut had_packets = PacketDiagnostics::new();
    had_packets.start = std::time::Instant::now() - std::time::Duration::from_secs(6);
    had_packets.record_packet(track, 0, &[0.1]);
    had_packets.check_no_packets_yet(track);
    assert!(
        !had_packets.zero_buffer_warned,
        "a track that produced packets"
    );
}

#[test]
fn a_silent_flagged_packet_is_recorded_as_zeros() {
    let mut samples = vec![0.5f32, -0.25, 0.125];
    apply_silent_flag(&mut samples, AUDCLNT_BUFFERFLAGS_SILENT);
    assert_eq!(samples, vec![0.0; 3]);
}

#[test]
fn an_unflagged_packet_is_left_alone() {
    let mut samples = vec![0.5f32, -0.25];
    // Some other flag bit (DATA_DISCONTINUITY = 1) doesn't silence it.
    apply_silent_flag(&mut samples, 1);
    assert_eq!(samples, vec![0.5, -0.25]);
}
