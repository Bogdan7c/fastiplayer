use super::{
    H265ByteStreamError, H265NalLengthSize, H265PacketDecodeStartProbe, H265Packetization,
    H265ParameterSetInjection, H265RequirementError, H265SpsError, h265_access_unit_to_annex_b,
    h265_access_unit_to_annex_b_into,
    h265_decode_requirement_from_hevc_decoder_configuration_record,
    h265_decode_requirement_from_packet,
    h265_header_metadata_from_hevc_decoder_configuration_record, h265_nal_units,
    h265_sps_metadata_from_hevc_decoder_configuration_record, infer_h265_packetization,
    parse_h265_sps_metadata, parse_hevc_decoder_configuration_record,
    probe_h265_packet_decode_start,
};
use crate::{BitDepth, ChromaSubsampling, H265Profile, VideoFramePixelLayout, VideoProfile};

fn vps() -> Vec<u8> {
    nal_unit(32, &[0x01, 0x60])
}

fn pps() -> Vec<u8> {
    nal_unit(34, &[0xc0])
}

fn aud() -> Vec<u8> {
    nal_unit(35, &[0x50])
}

fn cra_slice() -> Vec<u8> {
    nal_unit(21, &[0x01])
}

fn trail_slice() -> Vec<u8> {
    nal_unit(1, &[0x01])
}

fn main_sps() -> Vec<u8> {
    build_sps(BuildSpsOptions {
        profile_idc: 1,
        chroma_format_idc: 1,
        bit_depth: 8,
        width: 1_920,
        height: 1_080,
    })
}

fn main10_sps() -> Vec<u8> {
    build_sps(BuildSpsOptions {
        profile_idc: 2,
        chroma_format_idc: 1,
        bit_depth: 10,
        width: 3_840,
        height: 2_160,
    })
}

fn nal_unit(nal_unit_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut nal_unit = vec![(nal_unit_type & 0x3f) << 1, 0x01];
    nal_unit.extend_from_slice(payload);
    nal_unit
}

fn annex_b_access_unit(nal_units: &[Vec<u8>]) -> Vec<u8> {
    let mut access_unit = Vec::new();
    for nal_unit in nal_units {
        access_unit.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        access_unit.extend_from_slice(nal_unit);
    }
    access_unit
}

fn hvcc_access_unit(nal_length_size: H265NalLengthSize, nal_units: &[Vec<u8>]) -> Vec<u8> {
    let mut access_unit = Vec::new();
    for nal_unit in nal_units {
        push_nal_length(&mut access_unit, nal_length_size, nal_unit.len());
        access_unit.extend_from_slice(nal_unit);
    }
    access_unit
}

fn hvcc(
    nal_length_size: u8,
    profile_idc: u8,
    chroma_format_idc: u8,
    bit_depth: u8,
    arrays: &[HvccArray],
) -> Vec<u8> {
    let mut record_bytes = vec![
        1,
        profile_idc & 0x1f,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        120,
        0xf0,
        0x00,
        0xfc,
        0xfc | (chroma_format_idc & 0x03),
        0xf8 | ((bit_depth - 8) & 0x07),
        0xf8 | ((bit_depth - 8) & 0x07),
        0,
        0,
        0b0000_1100 | ((nal_length_size - 1) & 0x03),
        arrays.len() as u8,
    ];

    set_profile_compatibility_flag(&mut record_bytes, profile_idc);
    for array in arrays {
        record_bytes.push(array.nal_unit_type & 0x3f);
        record_bytes.extend_from_slice(&(array.nal_units.len() as u16).to_be_bytes());
        for nal_unit in array.nal_units {
            record_bytes.extend_from_slice(&(nal_unit.len() as u16).to_be_bytes());
            record_bytes.extend_from_slice(nal_unit);
        }
    }

    record_bytes
}

fn set_profile_compatibility_flag(record_bytes: &mut [u8], profile_idc: u8) {
    let flag_index = usize::from(profile_idc);
    let byte_index = 2 + flag_index / 8;
    let bit_index = 7 - (flag_index % 8);
    record_bytes[byte_index] |= 1 << bit_index;
}

struct HvccArray<'a> {
    nal_unit_type: u8,
    nal_units: &'a [Vec<u8>],
}

#[test]
fn h265_hvcc_parser_accepts_length_sizes_and_extracts_vps_sps_pps() {
    let video_parameter_set = vps();
    let sequence_parameter_set = main_sps();
    let picture_parameter_set = pps();

    for (size_bytes, expected_length_size) in [
        (1, H265NalLengthSize::ONE),
        (2, H265NalLengthSize::TWO),
        (4, H265NalLengthSize::FOUR),
    ] {
        let record_bytes = hvcc(
            size_bytes,
            1,
            1,
            8,
            &[
                HvccArray {
                    nal_unit_type: 32,
                    nal_units: std::slice::from_ref(&video_parameter_set),
                },
                HvccArray {
                    nal_unit_type: 33,
                    nal_units: std::slice::from_ref(&sequence_parameter_set),
                },
                HvccArray {
                    nal_unit_type: 34,
                    nal_units: std::slice::from_ref(&picture_parameter_set),
                },
            ],
        );
        let record = parse_hevc_decoder_configuration_record(&record_bytes)
            .expect("валидный hvcC должен разбираться");

        assert_eq!(record.nal_length_size, expected_length_size);
        assert_eq!(
            record.video_parameter_sets(),
            std::slice::from_ref(&video_parameter_set)
        );
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
fn h265_hvcc_parser_accepts_incomplete_records_when_header_is_safe() {
    let record_bytes = hvcc(4, 1, 1, 8, &[]);
    let record = parse_hevc_decoder_configuration_record(&record_bytes)
        .expect("hvcC без parameter sets всё ещё сообщает packetization/header metadata");
    let metadata = h265_header_metadata_from_hevc_decoder_configuration_record(&record)
        .expect("Main 8-bit 4:2:0 header должен быть production-safe");

    assert!(record.video_parameter_sets().is_empty());
    assert!(record.sequence_parameter_sets().is_empty());
    assert!(record.picture_parameter_sets().is_empty());
    assert_eq!(
        record.packetization(),
        H265Packetization::HvccLengthPrefixed {
            nal_length_size: H265NalLengthSize::FOUR,
        }
    );
    assert_eq!(metadata.profile, H265Profile::Main);
    assert_eq!(metadata.surface_format, VideoFramePixelLayout::Nv12);
}

#[test]
fn h265_hvcc_parser_rejects_bad_version_lengths_and_truncated_arrays() {
    let sequence_parameter_set = main_sps();
    let mut bad_version = hvcc(4, 1, 1, 8, &[]);
    bad_version[0] = 2;
    let unsupported_length_size = hvcc(3, 1, 1, 8, &[]);
    let mut truncated_array = hvcc(
        4,
        1,
        1,
        8,
        &[HvccArray {
            nal_unit_type: 33,
            nal_units: std::slice::from_ref(&sequence_parameter_set),
        }],
    );
    truncated_array.truncate(24);

    assert!(parse_hevc_decoder_configuration_record(&bad_version).is_err());
    assert!(parse_hevc_decoder_configuration_record(&unsupported_length_size).is_err());
    assert!(parse_hevc_decoder_configuration_record(&truncated_array).is_err());
}

#[test]
fn h265_hvcc_to_annex_b_conversion_handles_length_sizes_and_injection() {
    let video_parameter_set = vps();
    let sequence_parameter_set = main_sps();
    let picture_parameter_set = pps();

    for nal_length_size in [
        H265NalLengthSize::ONE,
        H265NalLengthSize::TWO,
        H265NalLengthSize::FOUR,
    ] {
        let access_unit = hvcc_access_unit(nal_length_size, &[cra_slice()]);
        let annex_b = h265_access_unit_to_annex_b(
            &access_unit,
            H265Packetization::HvccLengthPrefixed { nal_length_size },
            H265ParameterSetInjection::BeforeAccessUnit {
                video_parameter_sets: std::slice::from_ref(&video_parameter_set),
                sequence_parameter_sets: std::slice::from_ref(&sequence_parameter_set),
                picture_parameter_sets: std::slice::from_ref(&picture_parameter_set),
            },
        )
        .expect("length-prefixed HEVC AU должен конвертироваться");

        assert_eq!(
            annex_b,
            annex_b_access_unit(&[
                video_parameter_set.clone(),
                sequence_parameter_set.clone(),
                picture_parameter_set.clone(),
                cra_slice(),
            ])
        );
    }
}

#[test]
fn h265_annex_b_conversion_preserves_annex_b_input_and_scratch_contract() {
    let annex_b_input = {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0x00, 0x00, 0x01]);
        bytes.extend_from_slice(&aud());
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        bytes.extend_from_slice(&cra_slice());
        bytes
    };
    let mut output = Vec::with_capacity(128);
    let capacity_before = output.capacity();

    h265_access_unit_to_annex_b_into(
        &annex_b_input,
        H265Packetization::AnnexB,
        H265ParameterSetInjection::None,
        &mut output,
    )
    .expect("Annex B input должен оставаться Annex B");

    assert_eq!(output, annex_b_input);
    assert_eq!(output.capacity(), capacity_before);

    let malformed_packet = vec![0x00, 0x00, 0x00, 0x10, 0x2a, 0x01];
    let result = h265_access_unit_to_annex_b_into(
        &malformed_packet,
        H265Packetization::HvccLengthPrefixed {
            nal_length_size: H265NalLengthSize::FOUR,
        },
        H265ParameterSetInjection::None,
        &mut output,
    );

    assert!(matches!(
        result,
        Err(H265ByteStreamError::TruncatedHvccNalUnit { .. })
    ));
    assert!(output.is_empty());
    assert_eq!(output.capacity(), capacity_before);
}

#[test]
fn h265_injection_adds_only_available_parameter_sets() {
    let video_parameter_set = vps();
    let access_unit = hvcc_access_unit(H265NalLengthSize::FOUR, &[cra_slice()]);
    let mut output = Vec::new();

    h265_access_unit_to_annex_b_into(
        &access_unit,
        H265Packetization::HvccLengthPrefixed {
            nal_length_size: H265NalLengthSize::FOUR,
        },
        H265ParameterSetInjection::BeforeAccessUnit {
            video_parameter_sets: std::slice::from_ref(&video_parameter_set),
            sequence_parameter_sets: &[],
            picture_parameter_sets: &[],
        },
        &mut output,
    )
    .expect("неполный injection не должен падать из-за отсутствующих SPS/PPS");

    assert_eq!(
        output,
        annex_b_access_unit(&[video_parameter_set, cra_slice()])
    );
}

#[test]
fn h265_decode_start_probe_treats_all_irap_types_as_start() {
    for nal_unit_type in 16..=23 {
        let access_unit = annex_b_access_unit(&[nal_unit(nal_unit_type, &[0x01])]);
        let probe = probe_h265_packet_decode_start(&access_unit, H265Packetization::AnnexB);

        assert_eq!(probe, H265PacketDecodeStartProbe::DecodeStart);
    }
}

#[test]
fn h265_decode_start_probe_distinguishes_parameter_sets_and_uncertainty() {
    let parameter_sets_only = annex_b_access_unit(&[vps(), main_sps(), pps()]);
    let non_irap = annex_b_access_unit(&[trail_slice()]);
    let malformed_packet = vec![0x00, 0x00, 0x00, 0x10, 0x2a, 0x01];

    assert_eq!(
        probe_h265_packet_decode_start(&parameter_sets_only, H265Packetization::AnnexB),
        H265PacketDecodeStartProbe::NotDecodeStart
    );
    assert_eq!(
        probe_h265_packet_decode_start(&non_irap, H265Packetization::AnnexB),
        H265PacketDecodeStartProbe::NotDecodeStart
    );
    assert!(matches!(
        probe_h265_packet_decode_start(
            &malformed_packet,
            H265Packetization::HvccLengthPrefixed {
                nal_length_size: H265NalLengthSize::FOUR,
            },
        ),
        H265PacketDecodeStartProbe::Uncertain(H265ByteStreamError::TruncatedHvccNalUnit { .. })
    ));
}

#[test]
fn h265_hev1_style_in_band_parameter_sets_refine_requirement() {
    let record_bytes = hvcc(4, 2, 1, 10, &[]);
    let access_unit = hvcc_access_unit(
        H265NalLengthSize::FOUR,
        &[vps(), main10_sps(), pps(), cra_slice()],
    );

    let packetization = infer_h265_packetization(Some(&record_bytes), &access_unit)
        .expect("hvcC должен доказать length-prefixed packetization");
    let requirement = h265_decode_requirement_from_packet(&access_unit, packetization)
        .expect("in-band SPS должен уточнить Main10 requirement");
    let nal_types = h265_nal_units(&access_unit, packetization)
        .expect("hev1-style AU должен разбираться")
        .iter()
        .map(|nal_unit| nal_unit.nal_unit_type())
        .collect::<Vec<_>>();

    assert_eq!(
        requirement.profile,
        Some(VideoProfile::H265(H265Profile::Main10))
    );
    assert_eq!(requirement.bit_depth, Some(BitDepth::Ten));
    assert_eq!(requirement.chroma, Some(ChromaSubsampling::Yuv420));
    assert_eq!(requirement.width, Some(3_840));
    assert_eq!(requirement.height, Some(2_160));
    assert_eq!(
        crate::video_frame_pixel_layout_from_decode_requirement(&requirement),
        Some(VideoFramePixelLayout::P010)
    );
    assert_eq!(nal_types, vec![32, 33, 34, 21]);
}

#[test]
fn h265_requirement_from_hvcc_uses_sps_when_present_and_header_when_incomplete() {
    let sequence_parameter_set = main_sps();
    let record_with_sps = hvcc(
        4,
        1,
        1,
        8,
        &[HvccArray {
            nal_unit_type: 33,
            nal_units: std::slice::from_ref(&sequence_parameter_set),
        }],
    );
    let record_without_sps = hvcc(4, 2, 1, 10, &[]);

    let sps_metadata = h265_sps_metadata_from_hevc_decoder_configuration_record(&record_with_sps)
        .expect("hvcC SPS должен давать metadata");
    let requirement_with_dimensions =
        h265_decode_requirement_from_hevc_decoder_configuration_record(&record_with_sps)
            .expect("hvcC с SPS должен дать requirement с dimensions");
    let requirement_without_dimensions =
        h265_decode_requirement_from_hevc_decoder_configuration_record(&record_without_sps)
            .expect("неполный hvcC должен дать header-level requirement");

    assert_eq!(sps_metadata.profile, H265Profile::Main);
    assert_eq!(requirement_with_dimensions.width, Some(1_920));
    assert_eq!(requirement_with_dimensions.height, Some(1_080));
    assert_eq!(
        requirement_without_dimensions.profile,
        Some(VideoProfile::H265(H265Profile::Main10))
    );
    assert_eq!(requirement_without_dimensions.width, None);
    assert_eq!(
        crate::video_frame_pixel_layout_from_decode_requirement(&requirement_without_dimensions),
        Some(VideoFramePixelLayout::P010)
    );
}

#[test]
fn h265_requirement_rejects_unsupported_chroma_bit_depth_and_scc() {
    let twelve_bit_sps = build_sps(BuildSpsOptions {
        profile_idc: 2,
        chroma_format_idc: 1,
        bit_depth: 12,
        width: 1_280,
        height: 720,
    });
    let scc_record = hvcc(4, 9, 1, 8, &[]);

    for chroma_format_idc in [2, 3] {
        let unsupported_chroma_sps = build_sps(BuildSpsOptions {
            profile_idc: 1,
            chroma_format_idc,
            bit_depth: 8,
            width: 1_280,
            height: 720,
        });

        assert!(matches!(
            parse_h265_sps_metadata(&unsupported_chroma_sps),
            Err(H265SpsError::UnsupportedChroma { .. })
        ));
    }
    assert!(matches!(
        parse_h265_sps_metadata(&twelve_bit_sps),
        Err(H265SpsError::UnsupportedBitDepth { .. })
    ));
    assert!(matches!(
        h265_decode_requirement_from_hevc_decoder_configuration_record(&scc_record),
        Err(H265RequirementError::UnsupportedProfile { .. })
    ));
}

struct BuildSpsOptions {
    profile_idc: u8,
    chroma_format_idc: u32,
    bit_depth: u8,
    width: u32,
    height: u32,
}

fn build_sps(options: BuildSpsOptions) -> Vec<u8> {
    let mut bit_writer = BitWriter::new();
    bit_writer.u(4, 0);
    bit_writer.u(3, 0);
    bit_writer.u(1, 1);
    bit_writer.u(2, 0);
    bit_writer.u(1, 0);
    bit_writer.u(5, u64::from(options.profile_idc));
    for profile_index in 0..32 {
        bit_writer.u(
            1,
            u64::from(profile_index == u32::from(options.profile_idc)),
        );
    }
    bit_writer.u(48, 0);
    bit_writer.u(8, 120);
    bit_writer.ue(0);
    bit_writer.ue(options.chroma_format_idc);
    if options.chroma_format_idc == 3 {
        bit_writer.u(1, 0);
    }
    bit_writer.ue(options.width);
    bit_writer.ue(options.height);
    bit_writer.u(1, 0);
    bit_writer.ue(u32::from(options.bit_depth - 8));
    bit_writer.ue(u32::from(options.bit_depth - 8));
    bit_writer.rbsp_trailing_bits();

    let mut sps = nal_unit(33, &[]);
    sps.extend(bit_writer.into_bytes());
    sps
}

fn push_nal_length(
    output_bytes: &mut Vec<u8>,
    nal_length_size: H265NalLengthSize,
    nal_size: usize,
) {
    match nal_length_size.get() {
        1 => output_bytes.push(nal_size as u8),
        2 => output_bytes.extend_from_slice(&(nal_size as u16).to_be_bytes()),
        4 => output_bytes.extend_from_slice(&(nal_size as u32).to_be_bytes()),
        _ => unreachable!("test uses validated H265NalLengthSize"),
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

    fn u(&mut self, bit_count: u8, value: u64) {
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
        self.u(bit_count as u8, u64::from(code_num));
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
