//! Hermetic captures и test builders для media reconstruction.

use std::io::Cursor;
use std::num::NonZeroU32;

use symphonia_core::formats::{FormatOptions, FormatReader};
use symphonia_core::io::{MediaSourceStream, ReadBytes};
use symphonia_core::packet::Packet;

use super::super::super::initialization::{
    FragmentAacAudioSpecificConfig, FragmentAacChannelCount, FragmentAacLcConfiguration,
    FragmentAacSampleRate, FragmentH264Configuration, FragmentH264PictureParameterSet,
    FragmentH264SequenceParameterSet, FragmentInitializationCodec, FragmentInitializationLimits,
    FragmentInitializationRequest, FragmentTimescale, FragmentVideoDimensions, FragmentVideoHeight,
    FragmentVideoWidth, build_fragmented_initialization_segment,
};
use super::super::super::inspect::inspect_media_fragment;
use super::super::super::limits::FragmentInspectionLimits;
use super::super::super::model::{
    FragmentBaseDecodeTime, FragmentInspectionRequest, FragmentRapRequirement,
    FragmentSampleDefaults, FragmentTrackExpectation, FragmentTrackId, NormalizedFragmentPlan,
};
use super::super::{
    FragmentMediaKind, FragmentReconstructionRequest, FragmentTrackReconstructionIntent,
    FragmentWriteLimits, ReconstructedMediaSegment, reconstruct_media_fragment,
};
use crate::IsoMp4Reader;

pub(super) const VIDEO_LOW_FIRST: &[u8] =
    include_bytes!("../../../../fixtures/smooth-piff/video-401000-0.bin");
pub(super) const VIDEO_HIGH_FIRST: &[u8] =
    include_bytes!("../../../../fixtures/smooth-piff/video-1501000-0.bin");
pub(super) const VIDEO_HIGH_SECOND: &[u8] =
    include_bytes!("../../../../fixtures/smooth-piff/video-1501000-40000000.bin");
pub(super) const AUDIO_FIRST: &[u8] =
    include_bytes!("../../../../fixtures/smooth-piff/audio-64008-0.bin");
pub(super) const AUDIO_SECOND: &[u8] =
    include_bytes!("../../../../fixtures/smooth-piff/audio-64008-39680000.bin");

pub(super) const VIDEO_1501K_SPS: [u8; 41] = [
    0x67, 0x64, 0x00, 0x28, 0xac, 0xc8, 0x70, 0x1a, 0x41, 0x7f, 0xeb, 0x01, 0x6a, 0x02, 0x02, 0x02,
    0x80, 0x00, 0x00, 0x03, 0x00, 0x80, 0x00, 0x00, 0x18, 0x70, 0x00, 0x00, 0x2d, 0xc6, 0x80, 0x00,
    0x44, 0xaa, 0x32, 0x59, 0xc0, 0x7c, 0x60, 0xc6, 0x78,
];
pub(super) const VIDEO_1501K_PPS: [u8; 6] = [0x68, 0xe9, 0xb8, 0x29, 0x2c, 0x8b];
pub(super) const AUDIO_SPECIFIC_CONFIG_48K_STEREO: [u8; 2] = [0x11, 0x90];

/// Полные F1A budgets для маленького captured corpus.
pub(super) fn inspection_limits() -> FragmentInspectionLimits {
    FragmentInspectionLimits::builder()
        .max_input_bytes(256 * 1024)
        .max_box_count(64)
        .max_box_depth(4)
        .max_traf_count(1)
        .max_trun_count(8)
        .max_samples(512)
        .max_sample_table_bytes(64 * 1024)
        .max_box_payload_bytes(256 * 1024)
        .build()
        .expect("все inspection budgets заданы")
}

/// Output budget отдельно от input budgets.
pub(super) fn write_limits() -> FragmentWriteLimits {
    FragmentWriteLimits::try_new(512 * 1024).expect("ненулевой output budget")
}

/// Собирает public request для exact track 1.
pub(super) fn reconstruct(
    input: &[u8],
    base_decode_time: u64,
    media_kind: FragmentMediaKind,
) -> Result<ReconstructedMediaSegment, super::super::FragmentReconstructionError> {
    let inspection_limits = inspection_limits();
    reconstruct_with(
        input,
        base_decode_time,
        media_kind,
        &inspection_limits,
        write_limits(),
        &never_cancel,
    )
}

/// Собирает public request с injected budgets/cancellation.
pub(super) fn reconstruct_with<'policy>(
    input: &[u8],
    base_decode_time: u64,
    media_kind: FragmentMediaKind,
    inspection_limits: &'policy FragmentInspectionLimits,
    output_limits: FragmentWriteLimits,
    cancellation: &'policy dyn Fn() -> bool,
) -> Result<ReconstructedMediaSegment, super::super::FragmentReconstructionError> {
    let track = FragmentTrackReconstructionIntent::new(
        track_id(),
        FragmentBaseDecodeTime::new(base_decode_time),
        media_kind,
        FragmentSampleDefaults::absent(),
    );
    reconstruct_media_fragment(FragmentReconstructionRequest::new(
        input,
        track,
        inspection_limits,
        output_limits,
        cancellation,
    ))
}

/// Вызывает F1A inspector напрямую только внутри owner tests.
pub(super) fn inspect<'input>(
    input: &'input [u8],
    base_decode_time: u64,
    media_kind: FragmentMediaKind,
    limits: &FragmentInspectionLimits,
) -> NormalizedFragmentPlan<'input> {
    inspect_with_policy(input, base_decode_time, media_kind, limits, &never_cancel)
        .expect("test fragment должен пройти inspection")
}

/// Возвращает F1A result для negative tests.
pub(super) fn inspect_with_policy<'input>(
    input: &'input [u8],
    base_decode_time: u64,
    media_kind: FragmentMediaKind,
    limits: &FragmentInspectionLimits,
    cancellation: &dyn Fn() -> bool,
) -> Result<NormalizedFragmentPlan<'input>, super::super::super::error::FragmentInspectionError> {
    let rap_requirement = match media_kind {
        FragmentMediaKind::VideoWithRequiredProvenRandomAccess => {
            FragmentRapRequirement::RequireProvenVideoRandomAccess
        }
        FragmentMediaKind::AudioWithoutRandomAccessRequirement => {
            FragmentRapRequirement::NotRequiredForAudio
        }
    };
    let expectation = FragmentTrackExpectation::new(
        track_id(),
        FragmentBaseDecodeTime::new(base_decode_time),
        rap_requirement,
        FragmentSampleDefaults::absent(),
    );
    inspect_media_fragment(&FragmentInspectionRequest::new(
        input,
        expectation,
        limits,
        cancellation,
    ))
}

/// Создаёт accepted video init для captured 1501k representation.
pub(super) fn video_initialization() -> Vec<u8> {
    let dimensions = FragmentVideoDimensions::new(
        FragmentVideoWidth::try_new(1680).expect("width представим"),
        FragmentVideoHeight::try_new(750).expect("height представим"),
    );
    let configuration = FragmentH264Configuration::new(
        dimensions,
        FragmentH264SequenceParameterSet::try_new(&VIDEO_1501K_SPS).expect("captured SPS валиден"),
        FragmentH264PictureParameterSet::try_new(&VIDEO_1501K_PPS).expect("captured PPS валиден"),
    );
    build_fragmented_initialization_segment(FragmentInitializationRequest::new(
        track_id(),
        timescale(),
        FragmentInitializationCodec::H264Avc1(configuration),
        &initialization_limits(),
        &never_cancel,
    ))
    .expect("accepted video init")
    .into_bytes()
}

/// Создаёт accepted AAC-LC init.
pub(super) fn audio_initialization() -> Vec<u8> {
    let configuration = FragmentAacLcConfiguration::try_new(
        FragmentAacSampleRate::try_new(48_000).expect("48 kHz представимы"),
        FragmentAacChannelCount::try_new(2).expect("stereo представимо"),
        FragmentAacAudioSpecificConfig::try_new(&AUDIO_SPECIFIC_CONFIG_48K_STEREO)
            .expect("captured ASC валиден"),
    )
    .expect("typed AAC metadata совпадает с ASC");
    build_fragmented_initialization_segment(FragmentInitializationRequest::new(
        track_id(),
        timescale(),
        FragmentInitializationCodec::AacLowComplexity(configuration),
        &initialization_limits(),
        &never_cancel,
    ))
    .expect("accepted audio init")
    .into_bytes()
}

/// Открывает concatenated init+media production reader-ом.
pub(super) fn production_reader(bytes: Vec<u8>) -> IsoMp4Reader<'static> {
    let source = Cursor::new(bytes);
    let mut media_source_stream = MediaSourceStream::new(Box::new(source), Default::default());
    media_source_stream
        .read_quad_bytes()
        .expect("production probe читает первые четыре bytes");
    IsoMp4Reader::try_new(media_source_stream, FormatOptions::default())
        .expect("production IsoMp4Reader открывает canonical stream")
}

/// Читает все production packets до clean EOS.
pub(super) fn read_packets(reader: &mut IsoMp4Reader<'static>) -> Vec<Packet> {
    let mut packets = Vec::new();
    while let Some(packet) = reader.next_packet().expect("production packet read") {
        packets.push(packet);
    }
    packets
}

/// Минимальный configurable fragment для writer edge cases.
pub(super) fn synthetic_fragment(base_decode_time: u64, runs: &[SyntheticRun]) -> Vec<u8> {
    let sample_count: usize = runs.iter().map(|run| run.offsets.len()).sum();
    let tfdt_size = if base_decode_time <= u64::from(u32::MAX) {
        16_u32
    } else {
        20_u32
    };
    let trun_sizes: Vec<u32> = runs
        .iter()
        .map(|run| {
            let fields = if run.include_flags { 16_u32 } else { 12_u32 };
            20_u32 + fields * u32::try_from(run.offsets.len()).expect("small test run")
        })
        .collect();
    let traf_size = 8_u32 + 16 + tfdt_size + trun_sizes.iter().sum::<u32>();
    let moof_size = 8_u32 + 16 + traf_size;
    let mut bytes = Vec::new();

    write_box_header(&mut bytes, moof_size, *b"moof");
    write_full_box_header(&mut bytes, 16, *b"mfhd", 0, 0);
    write_u32(&mut bytes, 7);
    write_box_header(&mut bytes, traf_size, *b"traf");
    write_full_box_header(&mut bytes, 16, *b"tfhd", 0, 0x02_0000);
    write_u32(&mut bytes, 1);
    write_full_box_header(
        &mut bytes,
        tfdt_size,
        *b"tfdt",
        u8::from(base_decode_time > u64::from(u32::MAX)),
        0,
    );
    if tfdt_size == 16 {
        write_u32(&mut bytes, base_decode_time as u32);
    } else {
        bytes.extend_from_slice(&base_decode_time.to_be_bytes());
    }

    let mut payload_offset = i32::try_from(moof_size + 8).expect("small test offset");
    for (run, run_size) in runs.iter().zip(trun_sizes) {
        let mut flags = 0x0001 | 0x0100 | 0x0200 | 0x0800;
        if run.include_flags {
            flags |= 0x0400;
        }
        write_full_box_header(&mut bytes, run_size, *b"trun", run.version, flags);
        write_u32(
            &mut bytes,
            u32::try_from(run.offsets.len()).expect("small test run"),
        );
        bytes.extend_from_slice(&payload_offset.to_be_bytes());
        for offset in &run.offsets {
            write_u32(&mut bytes, 10);
            write_u32(&mut bytes, 1);
            if run.include_flags {
                write_u32(&mut bytes, 0x0200_0000);
            }
            if run.version == 0 {
                write_u32(&mut bytes, *offset as u32);
            } else {
                bytes.extend_from_slice(&(*offset as i32).to_be_bytes());
            }
            payload_offset += 1;
        }
    }

    write_box_header(
        &mut bytes,
        8 + u32::try_from(sample_count).expect("small sample count"),
        *b"mdat",
    );
    bytes.extend((0..sample_count).map(|sample_index| sample_index as u8));
    bytes
}

/// Один source `trun`.
pub(super) struct SyntheticRun {
    pub(super) version: u8,
    pub(super) offsets: Vec<i64>,
    pub(super) include_flags: bool,
}

/// Вставляет box в `traf` и чинит parent sizes; range validation не нужен для early rejection.
pub(super) fn insert_traf_child(bytes: &mut Vec<u8>, child: &[u8]) {
    let moof_size = read_u32(bytes, 0);
    let traf_start = 24_usize;
    let traf_size = read_u32(bytes, traf_start);
    let insertion = traf_start + usize::try_from(traf_size).expect("test traf size");
    bytes.splice(insertion..insertion, child.iter().copied());
    write_u32_at(
        bytes,
        0,
        moof_size + u32::try_from(child.len()).expect("small child"),
    );
    write_u32_at(
        bytes,
        traf_start,
        traf_size + u32::try_from(child.len()).expect("small child"),
    );
}

/// Собирает обычный test box.
pub(super) fn atom(box_type: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_box_header(
        &mut bytes,
        8 + u32::try_from(payload.len()).expect("small test payload"),
        box_type,
    );
    bytes.extend_from_slice(payload);
    bytes
}

pub(super) fn never_cancel() -> bool {
    false
}

fn initialization_limits() -> FragmentInitializationLimits {
    FragmentInitializationLimits::builder()
        .maximum_output_bytes(4_096)
        .maximum_codec_configuration_bytes(1_024)
        .build()
        .expect("все init budgets заданы")
}

fn timescale() -> FragmentTimescale {
    FragmentTimescale::new(NonZeroU32::new(10_000_000).expect("ненулевой timescale"))
}

fn track_id() -> FragmentTrackId {
    FragmentTrackId::new(NonZeroU32::new(1).expect("ненулевой track ID"))
}

fn write_box_header(bytes: &mut Vec<u8>, size: u32, box_type: [u8; 4]) {
    write_u32(bytes, size);
    bytes.extend_from_slice(&box_type);
}

fn write_full_box_header(
    bytes: &mut Vec<u8>,
    size: u32,
    box_type: [u8; 4],
    version: u8,
    flags: u32,
) {
    write_box_header(bytes, size, box_type);
    bytes.push(version);
    bytes.extend_from_slice(&flags.to_be_bytes()[1..]);
}

fn write_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().expect("test u32"))
}

fn write_u32_at(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}
