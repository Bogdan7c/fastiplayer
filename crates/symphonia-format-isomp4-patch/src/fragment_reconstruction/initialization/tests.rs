//! Focused hermetic tests initialization builder-а и production reader-а.

use std::cell::Cell;
use std::io::Cursor;
use std::num::NonZeroU32;

use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::well_known as audio_codec;
use symphonia_core::codecs::video::well_known as video_codec;
use symphonia_core::formats::{FormatOptions, FormatReader};
use symphonia_core::io::{MediaSourceStream, ReadBytes};

use super::build_fragmented_initialization_segment;
use super::error::{
    FragmentBoxType, FragmentCodecConfigurationIssue, FragmentCodecKind,
    FragmentInitializationError, FragmentInitializationField,
    FragmentInitializationLimitBuildError, FragmentInitializationLimitKind,
};
use super::model::{
    FragmentAacAudioSpecificConfig, FragmentAacChannelCount, FragmentAacLcConfiguration,
    FragmentAacSampleRate, FragmentH264Configuration, FragmentH264PictureParameterSet,
    FragmentH264SequenceParameterSet, FragmentInitializationCodec, FragmentInitializationLimits,
    FragmentInitializationRequest, FragmentTimescale, FragmentVideoDimensions, FragmentVideoHeight,
    FragmentVideoWidth,
};
use super::plan::checked_box_size;
use crate::{FragmentTrackId, IsoMp4Reader};

const CAPTURED_MANIFEST: &str = include_str!("../../../fixtures/smooth-piff/tears-of-steel.ismc");
const AUDIO_SPECIFIC_CONFIG_48K_STEREO: [u8; 2] = [0x11, 0x90];
const VIDEO_401K_SPS: [u8; 35] = [
    0x67, 0x42, 0xc0, 0x0d, 0xa6, 0x11, 0x0e, 0x3f, 0xe7, 0xc0, 0x44, 0x00, 0x00, 0x03, 0x00, 0x04,
    0x00, 0x00, 0x03, 0x00, 0xc3, 0x88, 0x80, 0x0c, 0x35, 0x00, 0x18, 0x6b, 0x82, 0xd0, 0x03, 0xe2,
    0x85, 0x42, 0x30,
];
const VIDEO_401K_PPS: [u8; 6] = [0x68, 0xc8, 0x42, 0x06, 0xcb, 0x20];
const VIDEO_1501K_SPS: [u8; 41] = [
    0x67, 0x64, 0x00, 0x28, 0xac, 0xc8, 0x70, 0x1a, 0x41, 0x7f, 0xeb, 0x01, 0x6a, 0x02, 0x02, 0x02,
    0x80, 0x00, 0x00, 0x03, 0x00, 0x80, 0x00, 0x00, 0x18, 0x70, 0x00, 0x00, 0x2d, 0xc6, 0x80, 0x00,
    0x44, 0xaa, 0x32, 0x59, 0xc0, 0x7c, 0x60, 0xc6, 0x78,
];
const VIDEO_1501K_PPS: [u8; 6] = [0x68, 0xe9, 0xb8, 0x29, 0x2c, 0x8b];

#[test]
fn captured_401k_h264_init_is_exact_and_readable_by_production_reader() {
    assert!(CAPTURED_MANIFEST.contains("Bitrate=\"401000\""));
    assert!(CAPTURED_MANIFEST.contains(
        "000000016742C00DA6110E3FE7C044000003000400000300C388800C3500186B82D003E2854230"
    ));

    let segment = build_video_segment(
        1,
        224,
        100,
        &VIDEO_401K_SPS,
        &VIDEO_401K_PPS,
        &standard_limits(),
        &never_cancel,
    )
    .expect("captured 401k initialization должен строиться");

    assert_exact_initialization_profile(segment.as_bytes(), 1, 658);
    assert_video_track(
        segment.into_bytes(),
        1,
        10_000_000,
        224,
        100,
        &VIDEO_401K_SPS,
        &VIDEO_401K_PPS,
    );
}

#[test]
fn captured_1501k_h264_init_is_exact_and_readable_by_production_reader() {
    assert!(CAPTURED_MANIFEST.contains("Bitrate=\"1501000\""));
    assert!(CAPTURED_MANIFEST.contains(
        "0000000167640028ACC8701A417FEB016A0202028000000300800000187000002DC6800044AA3259C07C60C6780"
    ));

    let segment = build_video_segment(
        3,
        1680,
        750,
        &VIDEO_1501K_SPS,
        &VIDEO_1501K_PPS,
        &standard_limits(),
        &never_cancel,
    )
    .expect("captured 1501k initialization должен строиться");

    assert_exact_initialization_profile(segment.as_bytes(), 3, 664);
    assert_video_track(
        segment.into_bytes(),
        3,
        10_000_000,
        1680,
        750,
        &VIDEO_1501K_SPS,
        &VIDEO_1501K_PPS,
    );
}

#[test]
fn captured_64008_aac_init_is_exact_and_readable_by_production_reader() {
    assert!(CAPTURED_MANIFEST.contains("Bitrate=\"64008\""));
    assert!(CAPTURED_MANIFEST.contains("CodecPrivateData=\"1190\""));

    let segment = build_audio_segment(2, &standard_limits(), &never_cancel)
        .expect("captured AAC initialization должен строиться");

    assert_exact_initialization_profile(segment.as_bytes(), 2, 583);
    let reader = parse_with_production_reader(segment.into_bytes());
    let track = reader.tracks().first().expect("ровно один audio track");
    assert_eq!(reader.tracks().len(), 1);
    assert_eq!(track.id, 2);
    assert_time_base(track.time_base, 10_000_000);
    let Some(CodecParameters::Audio(parameters)) = track.codec_params.as_ref() else {
        panic!("production reader должен вернуть audio codec parameters");
    };
    assert_eq!(parameters.codec, audio_codec::CODEC_ID_AAC);
    assert_eq!(parameters.sample_rate, Some(48_000));
    assert_eq!(
        parameters
            .channels
            .as_ref()
            .map(|channels| channels.count()),
        Some(2)
    );
    assert_eq!(
        parameters.extra_data.as_deref(),
        Some(AUDIO_SPECIFIC_CONFIG_48K_STEREO.as_slice())
    );
}

#[test]
fn identical_request_produces_deterministic_bytes() {
    let limits = standard_limits();
    let first = build_video_segment(
        1,
        224,
        100,
        &VIDEO_401K_SPS,
        &VIDEO_401K_PPS,
        &limits,
        &never_cancel,
    )
    .expect("первый init");
    let second = build_video_segment(
        1,
        224,
        100,
        &VIDEO_401K_SPS,
        &VIDEO_401K_PPS,
        &limits,
        &never_cancel,
    )
    .expect("второй init");

    assert_eq!(first, second);
}

#[test]
fn malformed_h264_parameter_sets_fail_closed() {
    assert_eq!(
        FragmentH264SequenceParameterSet::try_new(&[]).expect_err("empty SPS"),
        FragmentInitializationError::InvalidCodecConfiguration {
            codec: FragmentCodecKind::H264Avc1,
            issue: FragmentCodecConfigurationIssue::Empty,
        }
    );
    assert_eq!(
        FragmentH264SequenceParameterSet::try_new(&[0, 0, 0, 1, 0x67]).expect_err("Annex-B SPS"),
        FragmentInitializationError::InvalidCodecConfiguration {
            codec: FragmentCodecKind::H264Avc1,
            issue: FragmentCodecConfigurationIssue::AnnexBStartCode,
        }
    );
    assert_eq!(
        FragmentH264SequenceParameterSet::try_new(&[0x67, 0x42, 0xc0]).expect_err("truncated SPS"),
        FragmentInitializationError::InvalidCodecConfiguration {
            codec: FragmentCodecKind::H264Avc1,
            issue: FragmentCodecConfigurationIssue::TruncatedSequenceParameterSet,
        }
    );
    assert_eq!(
        FragmentH264PictureParameterSet::try_new(&[0x67]).expect_err("SPS вместо PPS"),
        FragmentInitializationError::InvalidCodecConfiguration {
            codec: FragmentCodecKind::H264Avc1,
            issue: FragmentCodecConfigurationIssue::UnexpectedNalUnitType {
                expected: 8,
                actual: 7,
            },
        }
    );
    assert_eq!(
        FragmentH264PictureParameterSet::try_new(&[0x68]).expect_err("truncated PPS"),
        FragmentInitializationError::InvalidCodecConfiguration {
            codec: FragmentCodecKind::H264Avc1,
            issue: FragmentCodecConfigurationIssue::TruncatedPictureParameterSet,
        }
    );
    let mut concatenated_parameter_sets = VIDEO_401K_SPS.to_vec();
    concatenated_parameter_sets.extend_from_slice(&[0, 0, 1]);
    concatenated_parameter_sets.extend_from_slice(&VIDEO_401K_PPS);
    assert_eq!(
        FragmentH264SequenceParameterSet::try_new(&concatenated_parameter_sets)
            .expect_err("SPS slice с embedded PPS"),
        FragmentInitializationError::InvalidCodecConfiguration {
            codec: FragmentCodecKind::H264Avc1,
            issue: FragmentCodecConfigurationIssue::AnnexBStartCode,
        }
    );
}

#[test]
fn malformed_and_incompatible_aac_configs_fail_closed() {
    assert_eq!(
        FragmentAacAudioSpecificConfig::try_new(&[0x2b, 0x90]).expect_err("HE-AAC object type"),
        FragmentInitializationError::InvalidCodecConfiguration {
            codec: FragmentCodecKind::AacLowComplexity,
            issue: FragmentCodecConfigurationIssue::UnsupportedAacObjectType { actual: 5 },
        }
    );
    assert_eq!(
        FragmentAacAudioSpecificConfig::try_new(&[0x17, 0x90]).expect_err("escape sample rate"),
        FragmentInitializationError::InvalidCodecConfiguration {
            codec: FragmentCodecKind::AacLowComplexity,
            issue: FragmentCodecConfigurationIssue::UnsupportedAacSamplingFrequency,
        }
    );
    let asc = FragmentAacAudioSpecificConfig::try_new(&AUDIO_SPECIFIC_CONFIG_48K_STEREO)
        .expect("валидный ASC");
    let wrong_rate = FragmentAacSampleRate::try_new(44_100).expect("валидное typed field");
    let stereo = FragmentAacChannelCount::try_new(2).expect("валидное typed field");
    assert_eq!(
        FragmentAacLcConfiguration::try_new(wrong_rate, stereo, asc)
            .expect_err("metadata mismatch"),
        FragmentInitializationError::IncompatibleCodecConfiguration {
            codec: FragmentCodecKind::AacLowComplexity,
            issue: FragmentCodecConfigurationIssue::AacSampleRateMismatch,
        }
    );
}

#[test]
fn mandatory_limits_reject_missing_zero_codec_and_output_budgets() {
    assert_eq!(
        FragmentInitializationLimits::builder()
            .maximum_codec_configuration_bytes(128)
            .build(),
        Err(FragmentInitializationLimitBuildError::Missing {
            kind: FragmentInitializationLimitKind::OutputBytes,
        })
    );
    assert_eq!(
        FragmentInitializationLimits::builder()
            .maximum_output_bytes(0)
            .maximum_codec_configuration_bytes(128)
            .build(),
        Err(FragmentInitializationLimitBuildError::Zero {
            kind: FragmentInitializationLimitKind::OutputBytes,
        })
    );

    let codec_limited = FragmentInitializationLimits::builder()
        .maximum_output_bytes(4_096)
        .maximum_codec_configuration_bytes(1)
        .build()
        .expect("полные limits");
    assert_eq!(
        build_video_segment(
            1,
            224,
            100,
            &VIDEO_401K_SPS,
            &VIDEO_401K_PPS,
            &codec_limited,
            &never_cancel,
        )
        .expect_err("codec limit"),
        FragmentInitializationError::LimitExceeded {
            kind: FragmentInitializationLimitKind::CodecConfigurationBytes,
            limit: 1,
            observed: 41,
        }
    );

    let output_limited = FragmentInitializationLimits::builder()
        .maximum_output_bytes(64)
        .maximum_codec_configuration_bytes(128)
        .build()
        .expect("полные limits");
    let error = build_audio_segment(2, &output_limited, &never_cancel).expect_err("output limit");
    assert!(matches!(
        error,
        FragmentInitializationError::LimitExceeded {
            kind: FragmentInitializationLimitKind::OutputBytes,
            limit: 64,
            observed: _
        }
    ));
}

#[test]
fn cancellation_is_checked_during_planning_and_before_publish() {
    assert_eq!(
        build_audio_segment(2, &standard_limits(), &always_cancel)
            .expect_err("initial cancellation"),
        FragmentInitializationError::Cancelled
    );

    let call_count = Cell::new(0_u32);
    let cancel_before_publish = || {
        let next = call_count.get() + 1;
        call_count.set(next);
        next >= 3
    };
    assert_eq!(
        build_audio_segment(2, &standard_limits(), &cancel_before_publish)
            .expect_err("publish fence cancellation"),
        FragmentInitializationError::Cancelled
    );
    assert_eq!(call_count.get(), 3);
}

#[test]
fn field_and_box_size_overflow_are_typed() {
    assert_eq!(
        FragmentVideoWidth::try_new(u32::from(u16::MAX) + 1).expect_err("width overflow"),
        FragmentInitializationError::FieldOverflow {
            field: FragmentInitializationField::VideoWidth,
            value: u64::from(u16::MAX) + 1,
        }
    );

    let track_id = FragmentTrackId::new(NonZeroU32::new(u32::MAX).expect("non-zero"));
    let codec = audio_codec_configuration();
    let limits = standard_limits();
    let error = build_fragmented_initialization_segment(FragmentInitializationRequest::new(
        track_id,
        timescale(),
        FragmentInitializationCodec::AacLowComplexity(codec),
        &limits,
        &never_cancel,
    ))
    .expect_err("next track id overflow");
    assert_eq!(
        error,
        FragmentInitializationError::FieldOverflow {
            field: FragmentInitializationField::NextTrackId,
            value: u64::from(u32::MAX),
        }
    );

    assert_eq!(
        checked_box_size(FragmentBoxType::Movie, u64::from(u32::MAX))
            .expect_err("box size overflow"),
        FragmentInitializationError::BoxSizeOverflow {
            box_type: FragmentBoxType::Movie,
            size: u64::from(u32::MAX) + 8,
        }
    );
}

fn build_video_segment<'policy>(
    track_id: u32,
    width: u32,
    height: u32,
    sequence_parameter_set: &[u8],
    picture_parameter_set: &[u8],
    limits: &'policy FragmentInitializationLimits,
    cancellation: &'policy dyn Fn() -> bool,
) -> Result<super::FragmentInitializationSegment, FragmentInitializationError> {
    let dimensions = FragmentVideoDimensions::new(
        FragmentVideoWidth::try_new(width)?,
        FragmentVideoHeight::try_new(height)?,
    );
    let configuration = FragmentH264Configuration::new(
        dimensions,
        FragmentH264SequenceParameterSet::try_new(sequence_parameter_set)?,
        FragmentH264PictureParameterSet::try_new(picture_parameter_set)?,
    );
    build_fragmented_initialization_segment(FragmentInitializationRequest::new(
        FragmentTrackId::new(NonZeroU32::new(track_id).expect("test track id ненулевой")),
        timescale(),
        FragmentInitializationCodec::H264Avc1(configuration),
        limits,
        cancellation,
    ))
}

fn build_audio_segment<'policy>(
    track_id: u32,
    limits: &'policy FragmentInitializationLimits,
    cancellation: &'policy dyn Fn() -> bool,
) -> Result<super::FragmentInitializationSegment, FragmentInitializationError> {
    build_fragmented_initialization_segment(FragmentInitializationRequest::new(
        FragmentTrackId::new(NonZeroU32::new(track_id).expect("test track id ненулевой")),
        timescale(),
        FragmentInitializationCodec::AacLowComplexity(audio_codec_configuration()),
        limits,
        cancellation,
    ))
}

fn audio_codec_configuration() -> FragmentAacLcConfiguration<'static> {
    FragmentAacLcConfiguration::try_new(
        FragmentAacSampleRate::try_new(48_000).expect("48 kHz представимы"),
        FragmentAacChannelCount::try_new(2).expect("stereo представимо"),
        FragmentAacAudioSpecificConfig::try_new(&AUDIO_SPECIFIC_CONFIG_48K_STEREO)
            .expect("captured ASC валиден"),
    )
    .expect("typed AAC metadata совпадает с captured ASC")
}

fn standard_limits() -> FragmentInitializationLimits {
    FragmentInitializationLimits::builder()
        .maximum_output_bytes(4_096)
        .maximum_codec_configuration_bytes(1_024)
        .build()
        .expect("все mandatory budgets заданы")
}

fn timescale() -> FragmentTimescale {
    FragmentTimescale::new(NonZeroU32::new(10_000_000).expect("ненулевой timescale"))
}

fn parse_with_production_reader(bytes: Vec<u8>) -> IsoMp4Reader<'static> {
    let source = Cursor::new(bytes);
    let mut media_source_stream = MediaSourceStream::new(Box::new(source), Default::default());
    media_source_stream
        .read_quad_bytes()
        .expect("production probe должен прочитать первые четыре bytes");
    IsoMp4Reader::try_new(media_source_stream, FormatOptions::default())
        .expect("production IsoMp4Reader должен открыть generated init")
}

fn assert_video_track(
    bytes: Vec<u8>,
    expected_track_id: u32,
    expected_timescale: u32,
    expected_width: u16,
    expected_height: u16,
    sequence_parameter_set: &[u8],
    picture_parameter_set: &[u8],
) {
    let reader = parse_with_production_reader(bytes);
    assert_eq!(reader.tracks().len(), 1);
    let track = reader.tracks().first().expect("ровно один video track");
    assert_eq!(track.id, expected_track_id);
    assert_time_base(track.time_base, expected_timescale);
    let Some(CodecParameters::Video(parameters)) = track.codec_params.as_ref() else {
        panic!("production reader должен вернуть video codec parameters");
    };
    assert_eq!(parameters.codec, video_codec::CODEC_ID_H264);
    assert_eq!(parameters.width, Some(expected_width));
    assert_eq!(parameters.height, Some(expected_height));
    let codec_private = parameters
        .extra_data
        .first()
        .expect("avcC codec private должен присутствовать");
    assert_eq!(
        codec_private.data.as_ref(),
        expected_avcc(sequence_parameter_set, picture_parameter_set)
    );
}

fn expected_avcc(sequence_parameter_set: &[u8], picture_parameter_set: &[u8]) -> Vec<u8> {
    let mut bytes = vec![
        1,
        sequence_parameter_set[1],
        sequence_parameter_set[2],
        sequence_parameter_set[3],
        0xff,
        0xe1,
    ];
    bytes.extend_from_slice(
        &u16::try_from(sequence_parameter_set.len())
            .expect("test SPS length")
            .to_be_bytes(),
    );
    bytes.extend_from_slice(sequence_parameter_set);
    bytes.push(1);
    bytes.extend_from_slice(
        &u16::try_from(picture_parameter_set.len())
            .expect("test PPS length")
            .to_be_bytes(),
    );
    bytes.extend_from_slice(picture_parameter_set);
    bytes
}

fn assert_time_base(time_base: Option<symphonia_core::units::TimeBase>, denominator: u32) {
    let time_base = time_base.expect("track time base должен присутствовать");
    assert_eq!(time_base.numer.get(), 1);
    assert_eq!(time_base.denom.get(), denominator);
}

fn assert_exact_initialization_profile(bytes: &[u8], track_id: u32, exact_size: usize) {
    assert_eq!(bytes.len(), exact_size);
    assert_eq!(&bytes[4..8], b"ftyp");
    let file_type_size =
        u32::from_be_bytes(bytes[0..4].try_into().expect("ftyp size bytes")) as usize;
    assert_eq!(&bytes[file_type_size + 4..file_type_size + 8], b"moov");
    let movie_size = u32::from_be_bytes(
        bytes[file_type_size..file_type_size + 4]
            .try_into()
            .expect("moov size bytes"),
    ) as usize;
    assert_eq!(file_type_size + movie_size, bytes.len());
    assert!(!bytes.windows(4).any(|window| window == b"moof"));
    assert!(!bytes.windows(4).any(|window| window == b"mdat"));
    assert!(!bytes.windows(4).any(|window| window == b"sidx"));
    assert!(!bytes.windows(4).any(|window| window == b"mehd"));
    assert!(!bytes.windows(4).any(|window| window == b"edts"));
    assert!(bytes.windows(4).any(|window| window == b"dinf"));
    assert!(bytes.windows(4).any(|window| window == b"dref"));
    assert!(bytes.windows(4).any(|window| window == b"url "));

    for empty_table in [b"stts", b"stsc", b"stco"] {
        let box_type_offset = find_box_type(bytes, empty_table);
        assert_eq!(
            &bytes[box_type_offset - 4..box_type_offset],
            &16_u32.to_be_bytes()
        );
        assert_eq!(&bytes[box_type_offset + 4..box_type_offset + 12], &[0; 8]);
    }
    let sample_size_offset = find_box_type(bytes, b"stsz");
    assert_eq!(
        &bytes[sample_size_offset - 4..sample_size_offset],
        &20_u32.to_be_bytes()
    );
    assert_eq!(
        &bytes[sample_size_offset + 4..sample_size_offset + 16],
        &[0; 12]
    );

    let track_extends_offset = find_box_type(bytes, b"trex");
    assert_eq!(
        &bytes[track_extends_offset - 4..track_extends_offset],
        &32_u32.to_be_bytes()
    );
    assert_eq!(
        &bytes[track_extends_offset + 4..track_extends_offset + 8],
        &[0; 4]
    );
    assert_eq!(
        &bytes[track_extends_offset + 8..track_extends_offset + 12],
        &track_id.to_be_bytes()
    );
    assert_eq!(
        &bytes[track_extends_offset + 12..track_extends_offset + 16],
        &1_u32.to_be_bytes()
    );
    assert_eq!(
        &bytes[track_extends_offset + 16..track_extends_offset + 28],
        &[0; 12]
    );
}

fn find_box_type(bytes: &[u8], box_type: &[u8; 4]) -> usize {
    bytes
        .windows(4)
        .position(|window| window == box_type)
        .unwrap_or_else(|| panic!("box {box_type:?} должен присутствовать"))
}

fn never_cancel() -> bool {
    false
}

fn always_cancel() -> bool {
    true
}
