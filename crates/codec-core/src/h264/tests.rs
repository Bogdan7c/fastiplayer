use crate::{
    H264Profile, VideoCodec, VideoRequirementProbe, VideoRequirementRejection,
    probe_video_packet_keyframe, probe_video_packet_requirement_with_codec_private,
};

use super::{
    AVC_LENGTH_SIZE_MINUS_ONE_MASK, AVC_SPS_COUNT_MASK, H264ByteStreamError, H264NalLengthSize,
    H264Packetization, H264ParameterSetInjection, H264SpsError, h264_access_unit_to_annex_b,
    h264_access_unit_to_annex_b_into, h264_nal_units,
    h264_sps_metadata_from_avc_decoder_configuration_record,
    parse_avc_decoder_configuration_record, parse_h264_sps_metadata, probe_h264_packet_keyframe,
};

fn constrained_baseline_sps() -> Vec<u8> {
    build_sps(BuildSpsOptions {
        profile_idc: 66,
        constraint_flags: 0b1100_0000,
        level_idc: 31,
        chroma_format_idc: 1,
        bit_depth_minus8: 0,
        width: 1_280,
        height: 720,
    })
}

fn high_sps() -> Vec<u8> {
    build_sps(BuildSpsOptions {
        profile_idc: 100,
        constraint_flags: 0,
        level_idc: 40,
        chroma_format_idc: 1,
        bit_depth_minus8: 0,
        width: 1_920,
        height: 1_088,
    })
}

fn pps() -> Vec<u8> {
    vec![0x68, 0xce, 0x3c, 0x80]
}

fn idr_slice() -> Vec<u8> {
    vec![0x65, 0x88]
}

fn non_idr_slice() -> Vec<u8> {
    vec![0x41, 0x9a]
}

fn aud() -> Vec<u8> {
    vec![0x09, 0xf0]
}

fn avcc(
    nal_length_size: u8,
    sequence_parameter_set: &[u8],
    picture_parameter_set: &[u8],
) -> Vec<u8> {
    let mut record_bytes = vec![
        1,
        sequence_parameter_set[1],
        sequence_parameter_set[2],
        sequence_parameter_set[3],
        0b1111_1100 | (nal_length_size - 1),
        0b1110_0001,
    ];
    push_u16(&mut record_bytes, sequence_parameter_set.len());
    record_bytes.extend_from_slice(sequence_parameter_set);
    record_bytes.push(1);
    push_u16(&mut record_bytes, picture_parameter_set.len());
    record_bytes.extend_from_slice(picture_parameter_set);
    record_bytes
}

fn annex_b_access_unit(nal_units: &[Vec<u8>]) -> Vec<u8> {
    let mut access_unit = Vec::new();
    for nal_unit in nal_units {
        access_unit.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        access_unit.extend_from_slice(nal_unit);
    }
    access_unit
}

fn avcc_access_unit(nal_length_size: H264NalLengthSize, nal_units: &[Vec<u8>]) -> Vec<u8> {
    let mut access_unit = Vec::new();
    for nal_unit in nal_units {
        push_nal_length(&mut access_unit, nal_length_size, nal_unit.len());
        access_unit.extend_from_slice(nal_unit);
    }
    access_unit
}

#[test]
fn avcc_parser_accepts_valid_length_sizes_and_extracts_sps_pps() {
    let sequence_parameter_set = constrained_baseline_sps();
    let picture_parameter_set = pps();

    for (size_bytes, expected_length_size) in [
        (1, H264NalLengthSize::ONE),
        (2, H264NalLengthSize::TWO),
        (4, H264NalLengthSize::FOUR),
    ] {
        let record_bytes = avcc(size_bytes, &sequence_parameter_set, &picture_parameter_set);
        let record = parse_avc_decoder_configuration_record(&record_bytes)
            .expect("валидный avcC должен разбираться");

        assert_eq!(record.nal_length_size, expected_length_size);
        assert_eq!(
            record.sequence_parameter_sets(),
            std::slice::from_ref(&sequence_parameter_set)
        );
        assert_eq!(
            record.picture_parameter_sets(),
            std::slice::from_ref(&picture_parameter_set)
        );
    }
}

#[test]
fn avcc_parser_accepts_zeroed_reserved_bits_from_noncanonical_muxers() {
    let sequence_parameter_set = constrained_baseline_sps();
    let picture_parameter_set = pps();
    let mut record_bytes = avcc(4, &sequence_parameter_set, &picture_parameter_set);

    record_bytes[4] &= AVC_LENGTH_SIZE_MINUS_ONE_MASK;
    record_bytes[5] &= AVC_SPS_COUNT_MASK;
    let record = parse_avc_decoder_configuration_record(&record_bytes)
        .expect("avcC с zeroed reserved bits должен разбираться по значимым битам");

    assert_eq!(record.nal_length_size, H264NalLengthSize::FOUR);
    assert_eq!(
        record.sequence_parameter_sets(),
        std::slice::from_ref(&sequence_parameter_set)
    );
    assert_eq!(
        record.picture_parameter_sets(),
        std::slice::from_ref(&picture_parameter_set)
    );
}

#[test]
fn avcc_parser_rejects_malformed_lengths_and_empty_parameter_sets() {
    let sequence_parameter_set = constrained_baseline_sps();
    let picture_parameter_set = pps();
    let unsupported_length_size_record = avcc(3, &sequence_parameter_set, &picture_parameter_set);
    let mut empty_sps_record = avcc(4, &sequence_parameter_set, &picture_parameter_set);
    empty_sps_record[6] = 0;
    empty_sps_record[7] = 0;
    let mut truncated_sps_record = avcc(4, &sequence_parameter_set, &picture_parameter_set);
    truncated_sps_record.truncate(8);

    assert!(parse_avc_decoder_configuration_record(&unsupported_length_size_record).is_err());
    assert!(parse_avc_decoder_configuration_record(&empty_sps_record).is_err());
    assert!(parse_avc_decoder_configuration_record(&truncated_sps_record).is_err());
}

#[test]
fn annex_b_parser_accepts_three_and_four_byte_start_codes() {
    let sequence_parameter_set = constrained_baseline_sps();
    let picture_parameter_set = pps();
    let mut access_unit = Vec::new();
    access_unit.extend_from_slice(&[0x00, 0x00, 0x01]);
    access_unit.extend_from_slice(&aud());
    access_unit.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
    access_unit.extend_from_slice(&sequence_parameter_set);
    access_unit.extend_from_slice(&[0x00, 0x00, 0x01]);
    access_unit.extend_from_slice(&picture_parameter_set);
    access_unit.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
    access_unit.extend_from_slice(&idr_slice());

    let nal_units = h264_nal_units(&access_unit, H264Packetization::AnnexB)
        .expect("Annex B packet должен разбираться по start codes");
    let nal_types = nal_units
        .iter()
        .map(|nal_unit| nal_unit.nal_unit_type())
        .collect::<Vec<_>>();

    assert_eq!(nal_types, vec![9, 7, 8, 5]);
}

#[test]
fn avcc_to_annex_b_conversion_uses_explicit_parameter_set_injection() {
    let sequence_parameter_set = constrained_baseline_sps();
    let picture_parameter_set = pps();
    let access_unit = avcc_access_unit(H264NalLengthSize::FOUR, &[idr_slice()]);
    let annex_b = h264_access_unit_to_annex_b(
        &access_unit,
        H264Packetization::AvccLengthPrefixed {
            nal_length_size: H264NalLengthSize::FOUR,
        },
        H264ParameterSetInjection::BeforeAccessUnit {
            sequence_parameter_sets: std::slice::from_ref(&sequence_parameter_set),
            picture_parameter_sets: std::slice::from_ref(&picture_parameter_set),
        },
    )
    .expect("AVCC access unit должен конвертироваться в Annex B");

    let expected =
        annex_b_access_unit(&[sequence_parameter_set, picture_parameter_set, idr_slice()]);
    assert_eq!(annex_b, expected);
}

#[test]
fn avcc_to_annex_b_into_matches_legacy_wrapper() {
    let sequence_parameter_set = constrained_baseline_sps();
    let picture_parameter_set = pps();
    let access_unit = avcc_access_unit(
        H264NalLengthSize::FOUR,
        &[aud(), idr_slice(), non_idr_slice()],
    );
    let packetization = H264Packetization::AvccLengthPrefixed {
        nal_length_size: H264NalLengthSize::FOUR,
    };
    let injection = H264ParameterSetInjection::BeforeAccessUnit {
        sequence_parameter_sets: std::slice::from_ref(&sequence_parameter_set),
        picture_parameter_sets: std::slice::from_ref(&picture_parameter_set),
    };
    let legacy_annex_b = h264_access_unit_to_annex_b(&access_unit, packetization, injection)
        .expect("legacy wrapper должен сохранить прежнюю конвертацию");
    let mut caller_owned_annex_b = Vec::with_capacity(legacy_annex_b.len() + 64);

    h264_access_unit_to_annex_b_into(
        &access_unit,
        packetization,
        H264ParameterSetInjection::BeforeAccessUnit {
            sequence_parameter_sets: std::slice::from_ref(&sequence_parameter_set),
            picture_parameter_sets: std::slice::from_ref(&picture_parameter_set),
        },
        &mut caller_owned_annex_b,
    )
    .expect("_into API должен писать те же Annex B bytes");

    assert_eq!(caller_owned_annex_b, legacy_annex_b);
}

#[test]
fn avcc_to_annex_b_none_injection_does_not_add_parameter_sets() {
    let access_unit = avcc_access_unit(H264NalLengthSize::FOUR, &[idr_slice()]);
    let mut annex_b = annex_b_access_unit(&[aud()]);

    h264_access_unit_to_annex_b_into(
        &access_unit,
        H264Packetization::AvccLengthPrefixed {
            nal_length_size: H264NalLengthSize::FOUR,
        },
        H264ParameterSetInjection::None,
        &mut annex_b,
    )
    .expect("None injection должен только перепаковать NAL units");

    assert_eq!(annex_b, annex_b_access_unit(&[idr_slice()]));
}

#[test]
fn h264_keyframe_probe_distinguishes_idr_non_idr_and_sps_only_packets() {
    let sequence_parameter_set = constrained_baseline_sps();
    let picture_parameter_set = pps();
    let idr_access_unit = annex_b_access_unit(&[
        sequence_parameter_set.clone(),
        picture_parameter_set.clone(),
        idr_slice(),
    ]);
    let non_idr_access_unit = annex_b_access_unit(&[non_idr_slice()]);
    let parameter_sets_only = annex_b_access_unit(&[sequence_parameter_set, picture_parameter_set]);

    assert!(
        probe_h264_packet_keyframe(&idr_access_unit, H264Packetization::AnnexB)
            .expect("IDR access unit должен разбираться")
    );
    assert!(
        !probe_h264_packet_keyframe(&non_idr_access_unit, H264Packetization::AnnexB)
            .expect("non-IDR access unit должен разбираться")
    );
    assert!(
        !probe_h264_packet_keyframe(&parameter_sets_only, H264Packetization::AnnexB)
            .expect("SPS/PPS-only packet должен быть валидным, но не presentable keyframe")
    );
}

#[test]
fn generic_h264_keyframe_probe_keeps_unknown_recoverable_without_codec_private() {
    let avcc_packet = avcc_access_unit(H264NalLengthSize::FOUR, &[idr_slice()]);
    let probe = probe_video_packet_keyframe(VideoCodec::H264, &avcc_packet);

    assert!(matches!(
        probe,
        crate::VideoPacketKeyframeProbe::Uncertain(_)
    ));
}

#[test]
fn h264_requirement_probe_uses_avcc_sps_and_rejects_unsupported_variants() {
    let sequence_parameter_set = high_sps();
    let picture_parameter_set = pps();
    let record_bytes = avcc(4, &sequence_parameter_set, &picture_parameter_set);
    let avcc_packet = avcc_access_unit(H264NalLengthSize::FOUR, &[idr_slice()]);
    let probe = probe_video_packet_requirement_with_codec_private(
        VideoCodec::H264,
        &avcc_packet,
        Some(&record_bytes),
    );

    let VideoRequirementProbe::Candidate(candidate) = probe else {
        panic!("High 8-bit 4:2:0 SPS должен стать candidate");
    };
    assert_eq!(
        candidate.requirement.profile,
        Some(crate::VideoProfile::H264(H264Profile::High))
    );
    assert_eq!(candidate.requirement.width, Some(1_920));
    assert_eq!(candidate.requirement.height, Some(1_088));
    assert_eq!(
        crate::video_frame_pixel_layout_from_decode_requirement(&candidate.requirement),
        Some(crate::VideoFramePixelLayout::Nv12)
    );

    let high_10_sps = build_sps(BuildSpsOptions {
        profile_idc: 110,
        constraint_flags: 0,
        level_idc: 40,
        chroma_format_idc: 1,
        bit_depth_minus8: 2,
        width: 1_280,
        height: 720,
    });
    let high_10_record = avcc(4, &high_10_sps, &picture_parameter_set);
    let rejected_probe = probe_video_packet_requirement_with_codec_private(
        VideoCodec::H264,
        &avcc_packet,
        Some(&high_10_record),
    );

    assert!(matches!(
        rejected_probe,
        VideoRequirementProbe::Rejected(VideoRequirementRejection::UnsupportedProfile { .. })
    ));
}

#[test]
fn sps_parser_rejects_yuv422_and_yuv444_as_typed_unsupported_chroma() {
    for chroma_format_idc in [2, 3] {
        let sequence_parameter_set = build_sps(BuildSpsOptions {
            profile_idc: 100,
            constraint_flags: 0,
            level_idc: 40,
            chroma_format_idc,
            bit_depth_minus8: 0,
            width: 1_280,
            height: 720,
        });

        assert!(matches!(
            parse_h264_sps_metadata(&sequence_parameter_set),
            Err(H264SpsError::UnsupportedChroma { .. })
        ));
    }
}

#[test]
fn malformed_avcc_packet_is_recoverable_uncertain_keyframe_result() {
    let malformed_packet = vec![0x00, 0x00, 0x00, 0x10, 0x65];
    let result = probe_h264_packet_keyframe(
        &malformed_packet,
        H264Packetization::AvccLengthPrefixed {
            nal_length_size: H264NalLengthSize::FOUR,
        },
    );

    assert!(matches!(
        result,
        Err(H264ByteStreamError::TruncatedAvccNalUnit { .. })
    ));
}

#[test]
fn avcc_requirement_parser_extracts_supported_sps_metadata() {
    let sequence_parameter_set = constrained_baseline_sps();
    let picture_parameter_set = pps();
    let record_bytes = avcc(4, &sequence_parameter_set, &picture_parameter_set);
    let metadata = h264_sps_metadata_from_avc_decoder_configuration_record(&record_bytes)
        .expect("Constrained Baseline avcC должен дать SPS metadata");

    assert_eq!(metadata.profile, H264Profile::ConstrainedBaseline);
    assert_eq!(metadata.width, 1_280);
    assert_eq!(metadata.height, 720);
}

struct BuildSpsOptions {
    profile_idc: u8,
    constraint_flags: u8,
    level_idc: u8,
    chroma_format_idc: u32,
    bit_depth_minus8: u32,
    width: u32,
    height: u32,
}

fn build_sps(options: BuildSpsOptions) -> Vec<u8> {
    let mut bit_writer = BitWriter::new();
    bit_writer.u(8, u32::from(options.profile_idc));
    bit_writer.u(8, u32::from(options.constraint_flags));
    bit_writer.u(8, u32::from(options.level_idc));
    bit_writer.ue(0);

    if matches!(
        options.profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        bit_writer.ue(options.chroma_format_idc);
        if options.chroma_format_idc == 3 {
            bit_writer.u(1, 0);
        }
        bit_writer.ue(options.bit_depth_minus8);
        bit_writer.ue(options.bit_depth_minus8);
        bit_writer.u(1, 0);
        bit_writer.u(1, 0);
    }

    bit_writer.ue(0);
    bit_writer.ue(0);
    bit_writer.ue(0);
    bit_writer.ue(1);
    bit_writer.u(1, 0);
    bit_writer.ue(options.width / 16 - 1);
    bit_writer.ue(options.height / 16 - 1);
    bit_writer.u(1, 1);
    bit_writer.u(1, 1);
    bit_writer.u(1, 0);
    bit_writer.u(1, 0);
    bit_writer.rbsp_trailing_bits();

    let mut sps = vec![0x67];
    sps.extend(bit_writer.into_bytes());
    sps
}

fn push_u16(output_bytes: &mut Vec<u8>, value: usize) {
    output_bytes.extend_from_slice(&(value as u16).to_be_bytes());
}

fn push_nal_length(
    output_bytes: &mut Vec<u8>,
    nal_length_size: H264NalLengthSize,
    nal_size: usize,
) {
    match nal_length_size.get() {
        1 => output_bytes.push(nal_size as u8),
        2 => output_bytes.extend_from_slice(&(nal_size as u16).to_be_bytes()),
        4 => output_bytes.extend_from_slice(&(nal_size as u32).to_be_bytes()),
        _ => unreachable!("test uses validated H264NalLengthSize"),
    }
}

struct BitWriter {
    output_bytes: Vec<u8>,
    current_byte: u8,
    pending_bits: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            output_bytes: Vec::new(),
            current_byte: 0,
            pending_bits: 0,
        }
    }

    fn u(&mut self, bit_count: u8, value: u32) {
        for bit_index in (0..bit_count).rev() {
            self.push_bit(((value >> bit_index) & 1) != 0);
        }
    }

    fn ue(&mut self, value: u32) {
        let code_num = value + 1;
        let bit_count = 32 - code_num.leading_zeros();
        for _ in 0..bit_count - 1 {
            self.push_bit(false);
        }
        self.u(bit_count as u8, code_num);
    }

    fn rbsp_trailing_bits(&mut self) {
        self.push_bit(true);
        while self.pending_bits != 0 {
            self.push_bit(false);
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.output_bytes
    }

    fn push_bit(&mut self, bit: bool) {
        self.current_byte <<= 1;
        self.current_byte |= u8::from(bit);
        self.pending_bits += 1;

        if self.pending_bits == 8 {
            self.output_bytes.push(self.current_byte);
            self.current_byte = 0;
            self.pending_bits = 0;
        }
    }
}
