//! The bits of H.264 and HEVC bitstreams the fragmented MP4 writer needs:
//! splitting the encoders' Annex B output into NAL units, and the codec
//! configuration records (`avcC`, `hvcC`) MP4 keeps in its sample entry,
//! built from the parameter sets the encoder reports with its media type
//! (`MF_MT_MPEG_SEQUENCE_HEADER`).

/// The video codecs PolyRec records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    Hevc,
}

/// What a NAL unit is, as far as the writer cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NalKind {
    /// A video, sequence or picture parameter set.
    ParameterSet,
    /// An access unit delimiter: carries nothing MP4 needs.
    Delimiter,
    Other,
}

/// The NAL units of an Annex B byte stream, start codes removed. A NAL unit
/// never ends in a zero byte, so zeros before a start code are the start
/// code's own (the leading zero of a 4-byte one) or padding.
pub fn annex_b_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i..i + 3] == [0, 0, 1] {
            starts.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut units = Vec::new();
    for (n, &code) in starts.iter().enumerate() {
        let begin = code + 3;
        let mut end = starts.get(n + 1).copied().unwrap_or(data.len());
        while end > begin && data[end - 1] == 0 {
            end -= 1;
        }
        if end > begin {
            units.push(&data[begin..end]);
        }
    }
    units
}

pub fn nal_kind(codec: VideoCodec, unit: &[u8]) -> NalKind {
    let Some(&first) = unit.first() else {
        return NalKind::Other;
    };
    match codec {
        VideoCodec::H264 => match first & 0x1f {
            H264_SPS | H264_PPS => NalKind::ParameterSet,
            9 => NalKind::Delimiter,
            _ => NalKind::Other,
        },
        VideoCodec::Hevc => match hevc_type(first) {
            HEVC_VPS | HEVC_SPS | HEVC_PPS => NalKind::ParameterSet,
            35 => NalKind::Delimiter,
            _ => NalKind::Other,
        },
    }
}

const H264_SPS: u8 = 7;
const H264_PPS: u8 = 8;
const HEVC_VPS: u8 = 32;
const HEVC_SPS: u8 = 33;
const HEVC_PPS: u8 = 34;

fn hevc_type(first: u8) -> u8 {
    (first >> 1) & 0x3f
}

/// A sample's Annex B data as MP4 wants it: each NAL unit after its length
/// in 4 bytes, without delimiters or the parameter sets `config` already
/// holds.
pub fn to_length_prefixed(codec: VideoCodec, data: &[u8], config: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for unit in annex_b_units(data) {
        let known = match nal_kind(codec, unit) {
            NalKind::Delimiter => true,
            NalKind::ParameterSet => config.iter().any(|set| set == unit),
            NalKind::Other => false,
        };
        if !known {
            out.extend_from_slice(&(unit.len() as u32).to_be_bytes());
            out.extend_from_slice(unit);
        }
    }
    out
}

/// The codec configuration record (`avcC` or `hvcC` body) for the
/// parameter sets in `sequence_header` (Annex B, as the encoder reports
/// it), and those parameter sets.
pub fn configuration_record(
    codec: VideoCodec,
    sequence_header: &[u8],
) -> Result<(Vec<u8>, Vec<Vec<u8>>), String> {
    let sets: Vec<Vec<u8>> = annex_b_units(sequence_header)
        .into_iter()
        .filter(|unit| nal_kind(codec, unit) == NalKind::ParameterSet)
        .map(<[u8]>::to_vec)
        .collect();
    let record = match codec {
        VideoCodec::H264 => avcc(&sets)?,
        VideoCodec::Hevc => hvcc(&sets)?,
    };
    Ok((record, sets))
}

/// The sets of one NAL type, each after its 16-bit length.
fn length_prefixed_16(sets: &[&Vec<u8>]) -> Vec<u8> {
    sets.iter()
        .flat_map(|set| {
            (set.len() as u16)
                .to_be_bytes()
                .into_iter()
                .chain(set.iter().copied())
        })
        .collect()
}

/// ISO/IEC 14496-15 5.3.3.1 `AVCDecoderConfigurationRecord`.
fn avcc(sets: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let of_type = |t: u8| -> Vec<&Vec<u8>> { sets.iter().filter(|s| s[0] & 0x1f == t).collect() };
    let (sps, pps) = (of_type(H264_SPS), of_type(H264_PPS));
    let first = sps.first().filter(|s| s.len() >= 4).ok_or("no H.264 SPS")?;
    if pps.is_empty() {
        return Err("no H.264 PPS".into());
    }
    let profile = first[1];
    let mut record = vec![
        1,
        profile,
        first[2],
        first[3],
        0xfc | 3,
        0xe0 | sps.len() as u8,
    ];
    record.extend(length_prefixed_16(&sps));
    record.push(pps.len() as u8);
    record.extend(length_prefixed_16(&pps));
    // The High profiles carry their chroma format and bit depths too.
    if matches!(profile, 100 | 110 | 122 | 144) {
        let rbsp = unescape(&first[1..]);
        let mut bits = Bits::new(&rbsp);
        bits.skip(24)?; // profile, constraints, level
        bits.ue()?; // seq_parameter_set_id
        let chroma = bits.ue()?;
        if chroma == 3 {
            bits.skip(1)?; // separate_colour_plane_flag
        }
        let (luma_depth, chroma_depth) = (bits.ue()?, bits.ue()?);
        record.extend([
            0xfc | chroma as u8,
            0xf8 | luma_depth as u8,
            0xf8 | chroma_depth as u8,
            0,
        ]);
    }
    Ok(record)
}

/// ISO/IEC 14496-15 8.3.3.1 `HEVCDecoderConfigurationRecord`: profile,
/// tier, level and the rest read from the SPS.
fn hvcc(sets: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let of_type =
        |t: u8| -> Vec<&Vec<u8>> { sets.iter().filter(|s| hevc_type(s[0]) == t).collect() };
    let (vps, sps, pps) = (of_type(HEVC_VPS), of_type(HEVC_SPS), of_type(HEVC_PPS));
    if vps.is_empty() || pps.is_empty() {
        return Err("no HEVC VPS or PPS".into());
    }
    let first = sps.first().ok_or("no HEVC SPS")?;
    let rbsp = unescape(first.get(2..).ok_or("HEVC SPS too short")?);
    let ptl = rbsp.get(1..13).ok_or("HEVC SPS too short")?;
    let mut bits = Bits::new(&rbsp);
    bits.skip(4)?; // sps_video_parameter_set_id
    let max_sub_layers_minus1 = bits.bits(3)?;
    let nested = bits.bits(1)?;
    bits.skip(96)?; // general profile, tier and level
    let sub_layers = max_sub_layers_minus1 as usize;
    let mut present = Vec::with_capacity(sub_layers);
    for _ in 0..sub_layers {
        present.push((bits.bits(1)? == 1, bits.bits(1)? == 1));
    }
    if sub_layers > 0 {
        bits.skip(2 * (8 - sub_layers))?;
    }
    for (profile, level) in present {
        bits.skip(if profile { 88 } else { 0 } + if level { 8 } else { 0 })?;
    }
    bits.ue()?; // sps_seq_parameter_set_id
    let chroma = bits.ue()?;
    if chroma == 3 {
        bits.skip(1)?; // separate_colour_plane_flag
    }
    bits.ue()?; // pic_width_in_luma_samples
    bits.ue()?; // pic_height_in_luma_samples
    if bits.bits(1)? == 1 {
        for _ in 0..4 {
            bits.ue()?; // conformance window offsets
        }
    }
    let (luma_depth, chroma_depth) = (bits.ue()?, bits.ue()?);

    let mut record = vec![1];
    record.extend_from_slice(ptl);
    record.extend([
        0xf0,
        0x00, // min_spatial_segmentation_idc unknown
        0xfc, // parallelismType unknown
        0xfc | chroma as u8,
        0xf8 | luma_depth as u8,
        0xf8 | chroma_depth as u8,
        0x00,
        0x00, // avgFrameRate unknown
        ((max_sub_layers_minus1 as u8 + 1) << 3) | ((nested as u8) << 2) | 3,
        3,
    ]);
    for (nal_type, of) in [(HEVC_VPS, &vps), (HEVC_SPS, &sps), (HEVC_PPS, &pps)] {
        record.push(0x80 | nal_type); // array_completeness: all of them here
        record.extend((of.len() as u16).to_be_bytes());
        record.extend(length_prefixed_16(of));
    }
    Ok(record)
}

/// A NAL unit's payload without its emulation prevention bytes (the 0x03
/// after two zeros).
fn unescape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0;
    for &byte in data {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        out.push(byte);
    }
    out
}

/// Reads a bitstream most significant bit first.
struct Bits<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    fn bits(&mut self, n: u32) -> Result<u32, String> {
        let mut value = 0;
        for _ in 0..n {
            let byte = self
                .data
                .get(self.at / 8)
                .ok_or("parameter set ends early")?;
            value = (value << 1) | u32::from((byte >> (7 - self.at % 8)) & 1);
            self.at += 1;
        }
        Ok(value)
    }

    fn skip(&mut self, n: usize) -> Result<(), String> {
        self.at += n;
        if self.at > self.data.len() * 8 {
            return Err("parameter set ends early".into());
        }
        Ok(())
    }

    /// An unsigned Exp-Golomb value.
    fn ue(&mut self) -> Result<u32, String> {
        let mut zeros = 0;
        while self.bits(1)? == 0 {
            zeros += 1;
            if zeros > 31 {
                return Err("bad Exp-Golomb value".into());
            }
        }
        Ok((1 << zeros) - 1 + self.bits(zeros)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a hardware HEVC encoder reported for 1920x1080 Main
    /// (`MF_MT_MPEG_SEQUENCE_HEADER`, captured 2026-11-05): VPS, SPS, PPS.
    const HEVC_HEADER: [u8; 86] = [
        0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x21, 0x60, 0x00, 0x00, 0x03,
        0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x78, 0xac, 0x09, 0x00, 0x00, 0x00,
        0x01, 0x42, 0x01, 0x01, 0x21, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
        0x00, 0x03, 0x00, 0x78, 0xa0, 0x03, 0xc0, 0x80, 0x11, 0x07, 0xcb, 0x96, 0xb4, 0xa4, 0x21,
        0x19, 0x2c, 0x20, 0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00, 0x00, 0x03, 0x01, 0xe0, 0x80,
        0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xc0, 0xf7, 0xc0, 0xcc, 0x90,
    ];

    /// The same for H.264 Constrained Baseline: SPS, PPS.
    const H264_HEADER: [u8; 37] = [
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xc0, 0x28, 0x95, 0xa0, 0x1e, 0x00, 0x89, 0xf9, 0x70,
        0x11, 0x00, 0x00, 0x03, 0x03, 0xe8, 0x00, 0x00, 0xea, 0x60, 0x0f, 0x1c, 0x2a, 0xa0, 0x00,
        0x00, 0x00, 0x01, 0x68, 0xce, 0x3c, 0x80,
    ];

    #[test]
    fn annex_b_splits_on_three_and_four_byte_start_codes() {
        let data = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5,
        ];
        let units = annex_b_units(&data);
        assert_eq!(units, [&[0x67, 1, 2][..], &[0x68, 3], &[0x65, 4, 5]]);
    }

    #[test]
    fn the_encoders_headers_hold_their_parameter_sets() {
        let kinds = |codec, header: &[u8]| -> Vec<NalKind> {
            annex_b_units(header)
                .into_iter()
                .map(|u| nal_kind(codec, u))
                .collect()
        };
        assert_eq!(
            kinds(VideoCodec::Hevc, &HEVC_HEADER),
            [NalKind::ParameterSet; 3]
        );
        assert_eq!(
            kinds(VideoCodec::H264, &H264_HEADER),
            [NalKind::ParameterSet; 2]
        );
        assert_eq!(
            nal_kind(VideoCodec::Hevc, &[0x46, 0x01, 0x10]),
            NalKind::Delimiter
        );
        assert_eq!(
            nal_kind(VideoCodec::H264, &[0x09, 0x10]),
            NalKind::Delimiter
        );
        assert_eq!(
            nal_kind(VideoCodec::Hevc, &[0x26, 0x01, 0xaf]),
            NalKind::Other
        );
        assert_eq!(nal_kind(VideoCodec::H264, &[0x65, 0x88]), NalKind::Other);
    }

    /// A keyframe as the encoders write it: delimiter, the parameter sets
    /// again, the slice. MP4 wants the slice alone, after its length.
    #[test]
    fn samples_lose_delimiters_and_known_parameter_sets() {
        let (_, config) = configuration_record(VideoCodec::Hevc, &HEVC_HEADER).unwrap();
        let mut sample = vec![0, 0, 0, 1, 0x46, 0x01, 0x10];
        sample.extend_from_slice(&HEVC_HEADER);
        sample.extend_from_slice(&[0, 0, 0, 1, 0x26, 0x01, 0xaf, 0x00, 0x00, 0x03, 0x01]);
        assert_eq!(
            to_length_prefixed(VideoCodec::Hevc, &sample, &config),
            [0, 0, 0, 7, 0x26, 0x01, 0xaf, 0x00, 0x00, 0x03, 0x01]
        );
    }

    /// One the configuration doesn't hold stays in the sample.
    #[test]
    fn a_new_parameter_set_stays_in_the_sample() {
        let (_, config) = configuration_record(VideoCodec::H264, &H264_HEADER).unwrap();
        let sample = [0, 0, 0, 1, 0x68, 0xaa, 0, 0, 0, 1, 0x65, 0x88];
        assert_eq!(
            to_length_prefixed(VideoCodec::H264, &sample, &config),
            [0, 0, 0, 2, 0x68, 0xaa, 0, 0, 0, 2, 0x65, 0x88]
        );
    }

    /// ISO/IEC 14496-15 5.3.3.1, from the captured SPS and PPS.
    #[test]
    fn avcc_from_the_encoders_header() {
        let (avcc, sets) = configuration_record(VideoCodec::H264, &H264_HEADER).unwrap();
        let sps = &H264_HEADER[4..29];
        let pps = &H264_HEADER[33..];
        assert_eq!(sets, [sps.to_vec(), pps.to_vec()]);
        let mut expected = vec![1, 0x42, 0xc0, 0x28, 0xff, 0xe1, 0, 25];
        expected.extend_from_slice(sps);
        expected.extend_from_slice(&[1, 0, 4]);
        expected.extend_from_slice(pps);
        assert_eq!(avcc, expected);
    }

    /// ISO/IEC 14496-15 8.3.3.1, from the captured VPS, SPS and PPS:
    /// profile, tier and level as the SPS has them (Main, high tier, 4.0),
    /// 4:2:0, 8 bits, one temporal layer, nested.
    #[test]
    fn hvcc_from_the_encoders_header() {
        let (hvcc, sets) = configuration_record(VideoCodec::Hevc, &HEVC_HEADER).unwrap();
        assert_eq!(sets.len(), 3);
        let fixed = [
            1,    // configurationVersion
            0x21, // profile_space 0, tier 1, profile_idc 1 (Main)
            0x60, 0x00, 0x00, 0x00, // compatibility flags
            0x90, 0x00, 0x00, 0x00, 0x00, 0x00, // constraint flags
            0x78, // level 4.0
            0xf0, 0x00, // min_spatial_segmentation_idc 0
            0xfc, // parallelismType 0
            0xfd, // chromaFormat 1 (4:2:0)
            0xf8, // bitDepthLumaMinus8 0
            0xf8, // bitDepthChromaMinus8 0
            0x00, 0x00, // avgFrameRate unknown
            0x0f, // constantFrameRate 0, 1 temporal layer, nested, 4-byte lengths
            3,    // arrays
        ];
        assert_eq!(&hvcc[..fixed.len()], fixed);
        let mut rest = &hvcc[fixed.len()..];
        for (nal_type, set) in [32u8, 33, 34].into_iter().zip(&sets) {
            assert_eq!(rest[0], 0x80 | nal_type, "array of type {nal_type}");
            assert_eq!(&rest[1..3], [0, 1]);
            let len = usize::from(u16::from_be_bytes([rest[3], rest[4]]));
            assert_eq!(&rest[5..5 + len], set.as_slice());
            rest = &rest[5 + len..];
        }
        assert!(rest.is_empty());
    }

    /// High profile (100) carries chroma format and bit depths after the
    /// sets: this SPS says 4:2:0, 8 bits (ue 0, ue 1, ue 0, ue 0).
    #[test]
    fn avcc_for_high_profile_says_its_chroma_and_depths() {
        let header = [
            0, 0, 0, 1, 0x67, 100, 0x00, 0x28, 0xae, 0, 0, 0, 1, 0x68, 0xce,
        ];
        let (avcc, _) = configuration_record(VideoCodec::H264, &header).unwrap();
        assert_eq!(&avcc[avcc.len() - 4..], [0xfd, 0xf8, 0xf8, 0]);
    }

    #[test]
    fn a_header_without_its_parameter_sets_is_refused() {
        assert!(configuration_record(VideoCodec::Hevc, &H264_HEADER).is_err());
        assert!(configuration_record(VideoCodec::H264, &[]).is_err());
    }
}
