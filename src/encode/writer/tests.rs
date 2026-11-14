use super::*;

/// An encoder's B-frame count is read back as the `u32` it's given as;
/// a value of another type isn't taken for a count.
#[test]
fn a_u32_variant_reads_back() {
    assert_eq!(variant_as_u32(&variant_u32(2)), Some(2));
    assert_eq!(variant_as_u32(&variant_u32(0)), Some(0));
    assert_eq!(variant_as_u32(&VARIANT::default()), None);
}

/// The answers remembered for one frame size -- a size no other test
/// uses, as answers are kept for the whole test run.
fn answers_for(width: u32, height: u32) -> Vec<(u32, bool)> {
    encoder_answers()
        .iter()
        .filter(|((_, w, h, ..), _)| (*w, *h) == (width, height))
        .map(|(&(_, _, _, fps, ..), &accepted)| (fps, accepted))
        .collect()
}

/// A recording file that can't be created says nothing about which
/// frame rates the encoder takes. It used to be remembered as the
/// encoder turning every rate above 60 down, for the rest of the run --
/// a bad output folder, once, and 120 and 144 fps were gone until
/// PolyRec was restarted.
#[test]
fn a_file_that_cant_be_created_isnt_taken_for_the_encoder_turning_rates_down() {
    let dir = tempfile::tempdir().unwrap();
    let nowhere = dir.path().join("not there").join("recording.tmp.mp4");
    let params = EncoderParams {
        width: 96,
        height: 48,
        fps: 120,
        ..EncoderParams::tiny_test(Codec::H264, &[])
    };
    assert!(RecordingWriter::new_at_highest_fps(&nowhere, &params).is_err());
    assert_eq!(answers_for(96, 48), vec![]);
}

/// The encoder turning a rate down is still remembered, once a lower
/// rate shows the encoder itself works.
#[test]
fn a_rate_the_encoder_turns_down_is_remembered_once_a_lower_one_works() {
    let dir = tempfile::tempdir().unwrap();
    let params = EncoderParams {
        width: 80,
        height: 48,
        fps: 240,
        ..EncoderParams::tiny_test(Codec::H264, &[])
    };
    let (_writer, fps) =
        RecordingWriter::new_at_highest_fps(&dir.path().join("rec.mp4"), &params).unwrap();
    assert!(fps < 240, "the software encoder took 240 fps");
    let answers = answers_for(80, 48);
    assert!(answers.contains(&(240, false)), "{answers:?}");
    assert!(answers.contains(&(fps, true)), "{answers:?}");
}

#[test]
fn video_bitrate_scales_with_resolution_and_fps() {
    let p1080_60 = video_bitrate_bps(1920, 1080, 60);
    let p1440_60 = video_bitrate_bps(2560, 1440, 60);
    let p1080_30 = video_bitrate_bps(1920, 1080, 30);
    assert!(
        p1440_60 > p1080_60,
        "1440p should need a higher bitrate than 1080p at the same fps"
    );
    assert!(
        p1080_60 > p1080_30,
        "60fps should need a higher bitrate than 30fps at the same resolution"
    );
    // Sanity check against common streaming-quality guidance for 1080p60 (~8-12 Mbps).
    assert!(
        p1080_60 > 8_000_000 && p1080_60 < 16_000_000,
        "1080p60 bitrate {p1080_60} out of expected range"
    );
}

#[test]
fn writer_accepts_explicit_bitrate_and_h264_codec() {
    use crate::types::VideoFrame;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("h264_test.mp4");
    let writer = RecordingWriter::new(&output, &EncoderParams::tiny_test(Codec::H264, &[]))
        .expect("RecordingWriter::new with explicit bitrate failed");
    writer.begin_writing().expect("begin_writing failed");
    writer
        .write_video(VideoFrame {
            pts: Duration::ZERO,
            data: vec![0u8; 64 * 64 * 4].into(),
        })
        .expect("write_video failed");
    let path = writer.finalize().expect("finalize failed");
    assert!(path.metadata().unwrap().len() > 0, "output file is empty");
}

#[test]
fn frames_go_on_the_constant_rate_grid() {
    assert_frames_go_on_the_grid(&EncoderParams::tiny_test(Codec::H264, &[]));
}

/// `frames_go_on_the_constant_rate_grid` through this PC's hardware
/// encoders. Those kept whatever times frames came with (60 fps
/// recordings came out at 59.94), where the software path already
/// resampled them onto a grid of its own.
#[test]
#[ignore]
fn frames_go_on_the_constant_rate_grid_through_the_hardware_encoder() {
    for codec in [Codec::H264, Codec::H265] {
        assert_frames_go_on_the_grid(&EncoderParams {
            width: 1920,
            height: 1080,
            bitrate_bps: video_bitrate_bps(1920, 1080, 30),
            allow_hardware_encode: true,
            ..EncoderParams::tiny_test(codec, &[])
        });
    }
}

/// Writes frames as Windows hands them out (at 30 fps, slots start every
/// 33.3 ms): the first one 40 ms into the recording, a dropped one after
/// 100 ms, a still picture sent again after 250 ms, and two nearest the
/// same slot -- and checks the file has a frame in every slot from the
/// start to the last frame's, at its exact time.
fn assert_frames_go_on_the_grid(params: &EncoderParams) {
    use crate::types::VideoFrame;
    use std::time::Duration;

    assert_eq!(params.fps, 30);
    let _mf = MfSession::start_mta().expect("MFStartup");
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("cfr.mp4");
    let writer = RecordingWriter::new(&output, params).expect("RecordingWriter::new");
    writer.begin_writing().expect("begin_writing");
    let frame_len = params.width as usize * params.height as usize * 4;
    for ms in [40, 70, 100, 170, 420, 430] {
        writer
            .write_video(VideoFrame {
                pts: Duration::from_millis(ms),
                data: vec![0u8; frame_len].into(),
            })
            .expect("write_video");
    }
    let samples = read_video_samples(&writer.finalize().expect("finalize"));

    let slot = 10_000_000.0 / 30.0;
    let times: Vec<i64> = samples.iter().map(|(timestamp, _)| *timestamp).collect();
    assert_eq!(samples.len(), 15, "{:?}: {times:?}", params.codec);
    for (n, (timestamp, sample)) in samples.iter().enumerate() {
        let expected = n as f64 * slot;
        assert!(
            (*timestamp as f64 - expected).abs() <= 1.0,
            "{:?} slot {n}: {times:?}",
            params.codec
        );
        let duration = unsafe { sample.GetSampleDuration() }.expect("duration");
        assert!(
            (duration as f64 - slot).abs() <= 1.0,
            "{:?} slot {n} lasts {duration}",
            params.codec
        );
    }
}

/// HEVC MFT availability varies by Windows version/install — this exercises the
/// real encoder (or its automatic H264 fallback) rather than asserting a specific
/// codec landed in the file, since either outcome is a pass for this codebase.
#[tokio::test]
#[ignore]
async fn writer_accepts_h265_codec_or_falls_back_cleanly() {
    use crate::types::VideoFrame;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("h265_test.mp4");
    let writer = RecordingWriter::new(&output, &EncoderParams::tiny_test(Codec::H265, &[]))
        .expect("RecordingWriter::new with h265 (or its fallback) failed");
    writer.begin_writing().expect("begin_writing failed");
    writer
        .write_video(VideoFrame {
            pts: Duration::ZERO,
            data: vec![0u8; 64 * 64 * 4].into(),
        })
        .expect("write_video failed");
    let path = writer.finalize().expect("finalize failed");
    assert!(path.metadata().unwrap().len() > 0, "output file is empty");
}

/// The high fps presets (120/144/240) at 1440p through the real hardware
/// encoder. H.264 tops out at level 5.2, which caps 1440p at 144 fps, so
/// not every preset is accepted. This checks `video_encoder_accepts`
/// agrees with the writer on which are, and that a file at an accepted
/// rate really carries that frame rate and every frame written. Needs a
/// GPU encoder, so it's ignored by default.
#[tokio::test]
#[ignore]
async fn writer_encodes_high_fps_presets_at_1440p() {
    use crate::capture::frame_rate::FPS_PRESETS;
    use crate::types::VideoFrame;
    use std::time::Duration;

    const W: u32 = 2560;
    const H: u32 = 1440;
    let dir = tempfile::tempdir().unwrap();
    let _mf = crate::encode::mf::MfSession::start_mta().expect("MFStartup");
    for codec in [Codec::H264, Codec::H265] {
        for fps in FPS_PRESETS.into_iter().filter(|&fps| fps > 60) {
            let output = dir.path().join(format!("{codec:?}_{fps}.mp4"));
            let params = EncoderParams {
                width: W,
                height: H,
                fps,
                codec,
                bitrate_bps: video_bitrate_bps(W, H, fps),
                audio_tracks: vec![],
                allow_hardware_encode: true,
                gpu: None,
                container: Container::Mp4,
            };
            if !video_encoder_accepts(&params) {
                println!("{codec:?} {fps} fps: rejected by this PC's encoder");
                assert!(
                    RecordingWriter::new(&output, &params).is_err(),
                    "{codec:?} {fps} fps: the probe rejected what the writer accepts"
                );
                continue;
            }
            let writer = RecordingWriter::new(&output, &params)
                .unwrap_or_else(|e| panic!("{codec:?} {fps} fps: writer: {e}"));
            writer.begin_writing().expect("begin_writing failed");
            for i in 0..fps {
                // A moving bar, so the encoder can't collapse every frame
                // into a skip.
                let mut data = vec![0u8; (W * H * 4) as usize];
                // A 16 px (64 byte) bar sliding across the row.
                let x = (i * (W - 16) / fps) as usize * 4;
                for row in data.as_chunks_mut::<{ (W * 4) as usize }>().0 {
                    row[x..x + 64].fill(0xFF);
                }
                writer
                    .write_video(VideoFrame {
                        pts: Duration::from_secs(1) * i / fps,
                        data: data.into(),
                    })
                    .unwrap_or_else(|e| panic!("{codec:?} {fps} fps: frame {i}: {e}"));
            }
            let path = writer.finalize().expect("finalize failed");
            let (file_fps, samples) = read_video_rate_and_sample_count(&path);
            println!("{codec:?} {fps} fps: file says {file_fps} fps, {samples} samples");
            assert_eq!(file_fps, fps, "{codec:?}: frame rate in the file");
            assert_eq!(samples, fps, "{codec:?} {fps} fps: frames in the file");
        }
    }

    // What a recording at the highest preset ends up with: the highest
    // rate the encoder accepts, and a writer that works at it.
    for codec in [Codec::H264, Codec::H265] {
        let params = EncoderParams {
            width: W,
            height: H,
            fps: 240,
            codec,
            bitrate_bps: video_bitrate_bps(W, H, 240),
            audio_tracks: vec![],
            allow_hardware_encode: true,
            gpu: None,
            container: Container::Mp4,
        };
        let output = dir.path().join(format!("{codec:?}_stepped.mp4"));
        let (writer, fps) =
            RecordingWriter::new_at_highest_fps(&output, &params).expect("new_at_highest_fps");
        println!("{codec:?} asked for 240 fps, settled on {fps}");
        assert!(
            video_encoder_accepts(&EncoderParams {
                fps,
                ..params.clone()
            }),
            "{codec:?}: settled on a rate the probe rejects"
        );
        writer.begin_writing().expect("begin_writing failed");
        writer
            .write_video(VideoFrame {
                pts: Duration::ZERO,
                data: vec![0u8; (W * H * 4) as usize].into(),
            })
            .expect("write_video failed");
        let (file_fps, _) = read_video_rate_and_sample_count(&writer.finalize().unwrap());
        assert_eq!(file_fps, fps, "{codec:?}: stepped-down rate in the file");
    }
}

/// The video stream's `MF_MT_FRAME_RATE` (as whole fps) and its sample
/// count, read back through a source reader.
fn read_video_rate_and_sample_count(path: &std::path::Path) -> (u32, u32) {
    use windows::Win32::Media::MediaFoundation::MFCreateSourceReaderFromURL;
    let url = windows::core::HSTRING::from(path.to_str().unwrap());
    let reader = unsafe { MFCreateSourceReaderFromURL(&url, None) }.expect("source reader");
    let native = unsafe { reader.GetNativeMediaType(FIRST_VIDEO_STREAM, 0) }.expect("type");
    assert_eq!(
        unsafe { native.GetGUID(&MF_MT_MAJOR_TYPE) }.unwrap(),
        MFMediaType_Video
    );
    let packed = unsafe { native.GetUINT64(&MF_MT_FRAME_RATE) }.expect("frame rate");
    let fps = ((packed >> 32) as f64 / (packed & 0xFFFF_FFFF) as f64).round() as u32;
    (fps, read_video_samples(path).len() as u32)
}

const FIRST_VIDEO_STREAM: u32 =
    windows::Win32::Media::MediaFoundation::MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

/// The video stream's samples with their timestamps (in 100 ns units),
/// read back through a source reader.
fn read_video_samples(
    path: &std::path::Path,
) -> Vec<(i64, windows::Win32::Media::MediaFoundation::IMFSample)> {
    use windows::Win32::Media::MediaFoundation::{
        MF_SOURCE_READERF_ENDOFSTREAM, MFCreateSourceReaderFromURL,
    };
    let url = windows::core::HSTRING::from(path.to_str().unwrap());
    let reader = unsafe { MFCreateSourceReaderFromURL(&url, None) }.expect("source reader");
    let mut samples = Vec::new();
    loop {
        let mut flags = 0u32;
        let mut timestamp = 0i64;
        let mut sample = None;
        unsafe {
            reader.ReadSample(
                FIRST_VIDEO_STREAM,
                0,
                None,
                Some(&mut flags),
                Some(&mut timestamp),
                Some(&mut sample),
            )
        }
        .expect("ReadSample");
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            return samples;
        }
        if let Some(sample) = sample {
            samples.push((timestamp, sample));
        }
    }
}

/// The timestamps of the video stream's keyframes (samples flagged
/// `MFSampleExtension_CleanPoint`), read back through a source reader.
fn read_keyframe_times(path: &std::path::Path) -> Vec<std::time::Duration> {
    use windows::Win32::Media::MediaFoundation::MFSampleExtension_CleanPoint;
    read_video_samples(path)
        .into_iter()
        .filter(|(_, sample)| {
            unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0) == 1
        })
        .map(|(timestamp, _)| std::time::Duration::from_nanos(timestamp as u64 * 100))
        .collect()
}

type Rgb = (u8, u8, u8);

/// The size of the first video frame in `path` and its colour (R, G, B)
/// at a quarter and three quarters of the way down, decoded to RGB32
/// through a source reader (reading the rows the right way round
/// whichever way the frame is stored).
pub(crate) fn first_frame_colours(path: &std::path::Path) -> ((u32, u32), [Rgb; 2]) {
    use windows::Win32::Media::MediaFoundation::{
        MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
        MFCreateAttributes, MFCreateSourceReaderFromURL, MFVideoFormat_RGB32,
    };
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 1).unwrap();
        let attrs = attrs.unwrap();
        attrs
            .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
            .unwrap();
        let url = windows::core::HSTRING::from(path.to_str().unwrap());
        let reader = MFCreateSourceReaderFromURL(&url, &attrs).expect("source reader");
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let rgb = MFCreateMediaType().unwrap();
        rgb.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).unwrap();
        rgb.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).unwrap();
        reader
            .SetCurrentMediaType(stream, None, &rgb)
            .expect("decode to RGB32");
        let current = reader.GetCurrentMediaType(stream).unwrap();
        let size = current.GetUINT64(&MF_MT_FRAME_SIZE).unwrap();
        let (width, height) = ((size >> 32) as u32, size as u32);
        // Negative: stored bottom-up.
        let stride = current
            .GetUINT32(&MF_MT_DEFAULT_STRIDE)
            .map_or(width as i32 * 4, |s| s as i32);
        let mut flags = 0u32;
        let mut sample = None;
        reader
            .ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample))
            .expect("ReadSample");
        let buffer = sample
            .expect("a decoded frame")
            .ConvertToContiguousBuffer()
            .unwrap();
        let mut data = std::ptr::null_mut();
        buffer.Lock(&mut data, None, None).unwrap();
        let pixel = |y: u32| {
            let row = if stride < 0 { height - 1 - y } else { y };
            let p =
                data.add(row as usize * stride.unsigned_abs() as usize + (width / 2 * 4) as usize);
            (*p.add(2), *p.add(1), *p)
        };
        let colours = [pixel(height / 4), pixel(height * 3 / 4)];
        buffer.Unlock().unwrap();
        ((width, height), colours)
    }
}

/// Frames handed over as textures (`EncodeConfig::gpu_frames`) come out
/// of the encoder at the output size and in their own colours, the
/// right way up -- as frames handed over in memory do (checked side by
/// side, so a swapped red/blue or a flip in either shows). So do
/// textures the writer reads back into memory: for an encoder set up
/// without them (`GpuInput::direct` false), for one that turned them
/// down partway (see `RecordingWriter::write_video`), and for one that
/// turned them down from the start (see `RecordingWriter::start`). Top
/// half red, bottom half blue. Needs a GPU encoder, so it's ignored by
/// default.
#[test]
#[ignore]
fn texture_frames_encode_scaled_in_their_own_colours() {
    use crate::encode::gpu::{GpuFrames, GpuInput};
    use crate::types::{FrameData, VideoFrame};
    use std::time::Duration;

    const CAPTURE: (u32, u32) = (1280, 720);
    const OUTPUT: (u32, u32) = (640, 360);
    const FPS: u32 = 30;
    let red_over_blue = |(width, height): (u32, u32)| -> Vec<u8> {
        let row = width as usize * 4;
        let mut bgra = vec![0u8; row * height as usize];
        for (y, line) in bgra.chunks_exact_mut(row).enumerate() {
            let pixel = if y < height as usize / 2 {
                [0, 0, 255, 255]
            } else {
                [255, 0, 0, 255]
            };
            for px in line.as_chunks_mut::<4>().0 {
                *px = pixel;
            }
        }
        bgra
    };

    let _mf = crate::encode::mf::MfSession::start_mta().expect("MFStartup");
    let dir = tempfile::tempdir().unwrap();
    let gpu = GpuFrames::new().expect("GpuFrames");
    let texture = gpu
        .texture_from_bgra(CAPTURE.0, CAPTURE.1, &red_over_blue(CAPTURE))
        .expect("texture");

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Input {
        Memory,
        Texture,
        ReadBack,
        ReadBackAfterRefusal,
        ReadBackAfterRefusalAtStart,
    }
    let mut outputs = Vec::new();
    for input in [
        Input::Memory,
        Input::Texture,
        Input::ReadBack,
        Input::ReadBackAfterRefusal,
        Input::ReadBackAfterRefusalAtStart,
    ] {
        let output = dir.path().join(format!("{input:?}.mp4"));
        let params = EncoderParams {
            width: OUTPUT.0,
            height: OUTPUT.1,
            fps: FPS,
            codec: Codec::H264,
            bitrate_bps: video_bitrate_bps(OUTPUT.0, OUTPUT.1, FPS),
            audio_tracks: vec![],
            allow_hardware_encode: true,
            gpu: (input != Input::Memory).then(|| GpuInput {
                frames: gpu.clone(),
                size: CAPTURE,
                direct: input != Input::ReadBack,
            }),
            container: Container::Mp4,
        };
        let writer = if input == Input::ReadBackAfterRefusalAtStart {
            // An encoder that won't be set up for textures at all.
            let (writer, fps) = RecordingWriter::start_with(&params, |params| {
                if params.gpu.as_ref().is_some_and(|gpu| gpu.direct) {
                    return Err(AppError::Encode("no textures here".into()));
                }
                RecordingWriter::start(&output, params)
            })
            .expect("start");
            assert_eq!(fps, FPS);
            writer
        } else {
            let writer = RecordingWriter::new(&output, &params).expect("writer");
            writer.begin_writing().expect("begin_writing");
            writer
        };
        if input == Input::ReadBackAfterRefusal {
            // What `write_video` does once the encoder turns a texture
            // down.
            writer.takes_textures.set(false);
        }
        assert_eq!(writer.takes_textures(), input == Input::Texture);
        for i in 0..FPS {
            let data = if input == Input::Memory {
                red_over_blue(OUTPUT).into()
            } else {
                FrameData::Gpu(texture.clone())
            };
            writer
                .write_video(VideoFrame {
                    pts: Duration::from_secs(1) * i / FPS,
                    data,
                })
                .expect("write_video");
        }
        let path = writer.finalize().expect("finalize");
        let frames = read_keyframe_times(&path);
        let (size, colours) = first_frame_colours(&path);
        println!("{input:?}: {size:?}, top/bottom {colours:?}, keyframes {frames:?}");
        outputs.push((size, colours));
    }
    for (size, [top, bottom]) in outputs {
        assert_eq!(size, OUTPUT);
        let near = |(r, g, b): (u8, u8, u8), want: (u8, u8, u8)| {
            r.abs_diff(want.0) < 40 && g.abs_diff(want.1) < 40 && b.abs_diff(want.2) < 40
        };
        assert!(near(top, (255, 0, 0)), "top should be red: {top:?}");
        assert!(
            near(bottom, (0, 0, 255)),
            "bottom should be blue: {bottom:?}"
        );
    }
}

/// A keyframe at least every `KEYFRAME_INTERVAL` of recording time,
/// through the real encoders (hardware and software, both codecs), with
/// a still screen's sparse frames (every 10th frame of 30 fps, where the
/// encoders on their own gave one keyframe in ten seconds) -- and no
/// extra keyframes at the full frame rate. Needs a GPU encoder, so it's
/// ignored by default.
///
/// The sparse frames are written at their real pace (10 s per case): a
/// hardware encoder works asynchronously, so a force request applies to
/// whichever frame it takes next, and with frames written as fast as
/// possible that's one queued well before the frame asked for.
#[tokio::test]
#[ignore]
async fn writer_puts_a_keyframe_at_least_every_two_seconds() {
    use crate::types::VideoFrame;
    use std::time::Duration;

    const W: u32 = 1280;
    const H: u32 = 720;
    const FPS: u32 = 30;
    const SECS: u32 = 10;
    let _mf = crate::encode::mf::MfSession::start_mta().expect("MFStartup");
    let dir = tempfile::tempdir().unwrap();
    for (codec, hardware, step) in [Codec::H264, Codec::H265]
        .into_iter()
        .flat_map(|c| [true, false].map(|hw| (c, hw)))
        .flat_map(|(c, hw)| [1u32, 10].map(|step| (c, hw, step)))
    {
        let label = format!("{codec:?} hw={hardware} every {step} frame(s)");
        let output = dir.path().join(format!("{codec:?}_{hardware}_{step}.mp4"));
        let params = EncoderParams {
            width: W,
            height: H,
            fps: FPS,
            codec,
            bitrate_bps: video_bitrate_bps(W, H, FPS),
            audio_tracks: vec![],
            allow_hardware_encode: hardware,
            gpu: None,
            container: Container::Mp4,
        };
        let writer = RecordingWriter::new(&output, &params).expect("writer");
        writer.begin_writing().expect("begin_writing");
        let spacing = Duration::from_secs(1) * step / FPS;
        for i in (0..FPS * SECS).step_by(step as usize) {
            if step > 1 && i > 0 {
                std::thread::sleep(spacing);
            }
            let mut data = vec![0u8; (W * H * 4) as usize];
            let x = (i % (W - 16)) as usize * 4;
            for row in data.as_chunks_mut::<{ (W * 4) as usize }>().0 {
                row[x..x + 64].fill(0xFF);
            }
            writer
                .write_video(VideoFrame {
                    pts: Duration::from_secs(1) * i / FPS,
                    data: data.into(),
                })
                .expect("write_video");
        }
        let keyframes = read_keyframe_times(&writer.finalize().expect("finalize"));
        let longest_gap = keyframes
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .chain(
                keyframes
                    .last()
                    .map(|&last| Duration::from_secs(SECS.into()) - last),
            )
            .max()
            .unwrap_or(Duration::from_secs(SECS.into()));
        println!("{label}: keyframes at {keyframes:?}, longest gap {longest_gap:?}");
        assert!(
            longest_gap <= KEYFRAME_INTERVAL,
            "{label}: {longest_gap:?} between keyframes"
        );
        if step == 1 {
            // The encoder's own one a second, none forced on top.
            assert!(keyframes.len() <= SECS as usize, "{label}: {keyframes:?}");
        }
    }
}

/// The frames (by pts, in seconds) a `KeyframeSchedule` of `gop` forces
/// to be keyframes, given frames at `pts_secs`.
fn forced_keyframes(gop: u32, pts_secs: impl IntoIterator<Item = f64>) -> Vec<f64> {
    forced_keyframes_with(Some(gop), pts_secs)
}

fn forced_keyframes_with(gop: Option<u32>, pts_secs: impl IntoIterator<Item = f64>) -> Vec<f64> {
    let mut schedule = KeyframeSchedule::new(gop);
    pts_secs
        .into_iter()
        .filter(|&s| schedule.force_for(std::time::Duration::from_secs_f64(s)))
        .collect()
}

#[test]
fn keyframe_schedule_never_forces_at_the_full_frame_rate() {
    let every_frame = (0..30 * 10).map(|i| f64::from(i) / 30.0);
    assert_eq!(forced_keyframes(30, every_frame), Vec::<f64>::new());
}

#[test]
fn keyframe_schedule_forces_one_every_interval_on_sparse_frames() {
    // A still screen: every 10th frame of 30 fps -- the encoder alone
    // would place one keyframe in ten seconds.
    let every_tenth = (0..30 * 10).step_by(10).map(|i| f64::from(i) / 30.0);
    assert_eq!(forced_keyframes(30, every_tenth), [2.0, 4.0, 6.0, 8.0]);
}

#[test]
fn keyframe_schedule_forces_one_every_interval_when_the_gop_is_unknown() {
    // The encoder turned the GOP size down, so its own keyframes could be
    // any distance apart: even at the full frame rate, one is forced
    // every interval.
    let every_frame = (0..30 * 10).map(|i| f64::from(i) / 30.0);
    assert_eq!(
        forced_keyframes_with(None, every_frame),
        [2.0, 4.0, 6.0, 8.0]
    );
}

#[test]
fn keyframe_schedule_forces_the_first_frame_after_a_long_gap() {
    assert_eq!(forced_keyframes(30, [0.0, 0.5, 9.0, 9.5]), [9.0]);
}

#[test]
fn keyframe_schedule_leaves_a_due_keyframe_to_the_encoder() {
    // One frame a second with a GOP of 2: the encoder's own keyframe
    // lands on the 2 s frame, exactly when the interval runs out.
    assert_eq!(
        forced_keyframes(2, (0..10).map(f64::from)),
        Vec::<f64>::new()
    );
}

#[test]
fn keyframe_schedule_counts_the_gop_from_a_forced_keyframe() {
    // Forced at 2.0; the encoder restarts its count there, so its own
    // next keyframe is 5 frames later (3.5, not 2.9), which leaves 4.95
    // within the interval; 5.6 is owed by time.
    let frames = [0.0, 0.1, 2.0, 2.3, 2.6, 2.9, 3.2, 3.5, 4.95, 5.6];
    assert_eq!(forced_keyframes(5, frames), [2.0, 5.6]);
}

#[test]
fn pack_u64_encodes_correctly() {
    assert_eq!(pack_u64(1920, 1080), 0x0000_0780_0000_0438);
    assert_eq!(pack_u64(60, 1), 0x0000_003C_0000_0001);
}

#[test]
fn f32_to_pcm16_conversion_clamps() {
    let samples = [0.0f32, 1.0, -1.0, 2.0, -2.0];
    let pcm: Vec<i16> = samples
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
        .collect();
    assert_eq!(pcm[0], 0);
    assert_eq!(pcm[1], 32767);
    assert_eq!(pcm[2], -32767);
    assert_eq!(pcm[3], 32767);
    assert_eq!(pcm[4], -32767);
}

#[test]
fn writer_creates_mp4_for_tiny_resolution() {
    use crate::types::{AudioSamples, TrackId, VideoFrame};
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("test.mp4");

    // 64×64 at 30fps, one stereo 48kHz audio track
    let audio_tracks = vec![(48000u32, 2u16)];
    let writer = RecordingWriter::new(
        &output,
        &EncoderParams::tiny_test(Codec::H264, &audio_tracks),
    );
    assert!(
        writer.is_ok(),
        "RecordingWriter::new failed: {:?}",
        writer.err()
    );
    let writer = writer.unwrap();

    writer.begin_writing().expect("begin_writing failed");

    // One blank BGRA frame (64×64×4 bytes)
    let frame = VideoFrame {
        pts: Duration::ZERO,
        data: vec![0u8; 64 * 64 * 4].into(),
    };
    writer.write_video(frame).expect("write_video failed");

    // One 10ms audio chunk (480 samples × 2 channels at 48kHz)
    let samples = AudioSamples {
        track_id: TrackId::new(0),
        pts: Duration::ZERO,
        samples: vec![0.0f32; 480 * 2],
        sample_rate: 48000,
        channels: 2,
    };
    writer.write_audio(0, samples).expect("write_audio failed");

    let path = writer.finalize().expect("finalize failed");
    assert!(path.exists(), "output file not created");
    assert!(path.metadata().unwrap().len() > 0, "output file is empty");
}

/// Reproduces a real-world "Saving recording..." hang: 3 selected audio
/// sources (e.g. 2 devices + 1 app-audio process) but the user stops
/// almost immediately, before the app-audio track's async
/// process-loopback activation has delivered its first buffer -- so
/// that track's stream never receives a single `WriteSample` call.
/// Bounded by a timeout thread rather than calling `finalize()` inline,
/// so if the hang is still present this fails with a clear panic after
/// 15s instead of hanging the test run indefinitely.
#[test]
fn finalize_does_not_hang_when_one_of_several_audio_tracks_receives_no_samples() {
    use crate::types::{AudioSamples, TrackId, VideoFrame};
    use std::sync::mpsc;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("multi_track_test.mp4");

    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let audio_tracks = vec![(48000u32, 2u16), (48000u32, 2u16), (48000u32, 2u16)];
        let writer = RecordingWriter::new(
            &output,
            &EncoderParams::tiny_test(Codec::H264, &audio_tracks),
        )
        .expect("RecordingWriter::new failed");
        writer.begin_writing().expect("begin_writing failed");

        writer
            .write_video(VideoFrame {
                pts: Duration::ZERO,
                data: vec![0u8; 64 * 64 * 4].into(),
            })
            .expect("write_video failed");

        let sample_for = |track_idx: usize| AudioSamples {
            track_id: TrackId::new(track_idx as u32),
            pts: Duration::ZERO,
            samples: vec![0.0f32; 480 * 2],
            sample_rate: 48000,
            channels: 2,
        };
        // Tracks 0 and 1 (the two "devices") get a sample. Track 2 (the
        // "app-audio" slot) never does.
        writer
            .write_audio(0, sample_for(0))
            .expect("write_audio 0 failed");
        writer
            .write_audio(1, sample_for(1))
            .expect("write_audio 1 failed");

        let _ = done_tx.send(writer.finalize());
    });

    match done_rx.recv_timeout(Duration::from_secs(15)) {
        Ok(result) => {
            let path = result.expect("finalize failed");
            assert!(path.exists(), "output file not created");
        }
        Err(_) => panic!(
            "finalize() did not return within 15s -- an audio track with zero \
                 samples appears to hang IMFSinkWriter::Finalize"
        ),
    }
}

/// The sample bytes Media Foundation reads, via the generic
/// ConvertToContiguousBuffer + Lock path every consumer uses.
unsafe fn sample_bytes(sample: &windows::Win32::Media::MediaFoundation::IMFSample) -> Vec<u8> {
    unsafe {
        let buffer = sample.ConvertToContiguousBuffer().unwrap();
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut len: u32 = 0;
        buffer.Lock(&mut ptr, None, Some(&mut len)).unwrap();
        let bytes = std::slice::from_raw_parts(ptr, len as usize).to_vec();
        buffer.Unlock().unwrap();
        bytes
    }
}

#[test]
fn frame_sample_matches_the_copying_sample_byte_for_byte() {
    let _mf = MfSession::start_mta().unwrap();
    let pixels: Vec<u8> = (0..64 * 64 * 4).map(|i| (i % 251) as u8).collect();
    unsafe {
        let copied = make_sample(&pixels, 1234, 166_666).unwrap();
        let buffer = frame_buffer(pixels.clone().into()).unwrap();
        let zero_copy = make_buffer_sample(&buffer, 1234, 166_666).unwrap();
        assert_eq!(sample_bytes(&zero_copy), sample_bytes(&copied));
        assert_eq!(sample_bytes(&zero_copy), pixels);
        assert_eq!(
            zero_copy.GetSampleTime().unwrap(),
            copied.GetSampleTime().unwrap()
        );
        assert_eq!(
            zero_copy.GetSampleDuration().unwrap(),
            copied.GetSampleDuration().unwrap()
        );
        assert_eq!(
            zero_copy.GetTotalLength().unwrap(),
            copied.GetTotalLength().unwrap()
        );
    }
}

#[test]
fn frame_media_buffer_reports_lengths_like_a_memory_buffer() {
    let buffer: IMFMediaBuffer = FrameMediaBuffer::new(vec![5u8; 16].into()).unwrap().into();
    unsafe {
        assert_eq!(buffer.GetMaxLength().unwrap(), 16);
        assert_eq!(buffer.GetCurrentLength().unwrap(), 16);
        buffer.SetCurrentLength(8).unwrap();
        assert_eq!(buffer.GetCurrentLength().unwrap(), 8);
        assert!(buffer.SetCurrentLength(17).is_err());

        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut max: u32 = 0;
        let mut cur: u32 = 0;
        buffer
            .Lock(&mut ptr, Some(&mut max), Some(&mut cur))
            .unwrap();
        assert_eq!((max, cur), (16, 8));
        assert_eq!(std::slice::from_raw_parts(ptr, 16), &[5u8; 16]);
        buffer.Unlock().unwrap();
    }
}

#[test]
fn a_released_frame_sample_returns_its_buffer_to_the_pool() {
    let _mf = MfSession::start_mta().unwrap();
    let pool = crate::capture::frame_pool::FramePool::new();
    let frame = pool.take(64);
    let frame_ptr = frame.as_ptr();
    let sample = unsafe { make_buffer_sample(&frame_buffer(frame).unwrap(), 0, 1).unwrap() };
    drop(sample);
    assert_eq!(
        pool.take(64).as_ptr(),
        frame_ptr,
        "the sample's buffer should be back in the pool once released"
    );
}
