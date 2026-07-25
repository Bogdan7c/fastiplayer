//! Deterministic serialization заранее проверенного media layout-а.

use super::super::model::NormalizedFragmentPlan;
use super::error::{
    FragmentWriteArithmeticOperation, FragmentWriteCancellationPhase, FragmentWriteError,
};
use super::plan::{
    CompositionOffsetEncoding, DecodeTimeEncoding, PlannedMediaFragment,
    WRITE_CANCELLATION_INTERVAL, check_cancelled,
};

/// `tfhd.default-base-is-moof`.
const TFHD_DEFAULT_BASE_IS_MOOF: u32 = 0x02_0000;
/// `trun.data-offset-present`.
const TRUN_DATA_OFFSET_PRESENT: u32 = 0x0001;
/// `trun.sample-duration-present`.
const TRUN_SAMPLE_DURATION_PRESENT: u32 = 0x0100;
/// `trun.sample-size-present`.
const TRUN_SAMPLE_SIZE_PRESENT: u32 = 0x0200;
/// `trun.sample-flags-present`.
const TRUN_SAMPLE_FLAGS_PRESENT: u32 = 0x0400;
/// `trun.sample-composition-time-offsets-present`.
const TRUN_SAMPLE_COMPOSITION_OFFSET_PRESENT: u32 = 0x0800;

/// Пишет ровно `moof(mfhd,traf(tfhd,tfdt,trun))+mdat`.
pub(super) fn write_media_fragment(
    normalized: &NormalizedFragmentPlan<'_>,
    layout: PlannedMediaFragment,
    cancellation: &dyn Fn() -> bool,
) -> Result<Vec<u8>, FragmentWriteError> {
    let requested =
        u64::try_from(layout.total_size()).map_err(|_| FragmentWriteError::ArithmeticOverflow {
            operation: FragmentWriteArithmeticOperation::OutputSize,
        })?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(layout.total_size())
        .map_err(|_| FragmentWriteError::AllocationFailed { requested })?;
    write_movie_fragment(&mut bytes, normalized, layout, cancellation)?;
    check_cancelled(
        cancellation,
        FragmentWriteCancellationPhase::BeforeMediaPayload,
    )?;
    write_box_header(&mut bytes, layout.media_data_size(), *b"mdat");
    let media_payload_start = bytes.len();
    copy_media_payload(&mut bytes, normalized);
    let written_payload_size = bytes.len().checked_sub(media_payload_start).ok_or(
        FragmentWriteError::ArithmeticOverflow {
            operation: FragmentWriteArithmeticOperation::OutputSize,
        },
    )?;
    if bytes.len() != layout.total_size() || written_payload_size != layout.media_payload_size() {
        return Err(FragmentWriteError::ArithmeticOverflow {
            operation: FragmentWriteArithmeticOperation::OutputSize,
        });
    }
    check_cancelled(
        cancellation,
        FragmentWriteCancellationPhase::BeforePublication,
    )?;
    Ok(bytes)
}

/// Пишет canonical fragment metadata.
fn write_movie_fragment(
    bytes: &mut Vec<u8>,
    normalized: &NormalizedFragmentPlan<'_>,
    layout: PlannedMediaFragment,
    cancellation: &dyn Fn() -> bool,
) -> Result<(), FragmentWriteError> {
    write_box_header(bytes, layout.movie_fragment_size(), *b"moof");
    write_full_box_header(bytes, 16, *b"mfhd", 0, 0);
    write_u32(bytes, normalized.sequence_number());

    write_box_header(bytes, layout.track_fragment_size(), *b"traf");
    write_full_box_header(bytes, 16, *b"tfhd", 0, TFHD_DEFAULT_BASE_IS_MOOF);
    write_u32(bytes, normalized.track_id().get());

    let decode_time = normalized.coded_coverage().start();
    match layout.decode_time_encoding() {
        DecodeTimeEncoding::VersionZero => {
            write_full_box_header(bytes, 16, *b"tfdt", 0, 0);
            let decode_time =
                u32::try_from(decode_time).map_err(|_| FragmentWriteError::ArithmeticOverflow {
                    operation: FragmentWriteArithmeticOperation::DecodeTime,
                })?;
            write_u32(bytes, decode_time);
        }
        DecodeTimeEncoding::VersionOne => {
            write_full_box_header(bytes, 20, *b"tfdt", 1, 0);
            write_u64(bytes, decode_time);
        }
    }

    write_track_run(bytes, normalized, layout, cancellation)
}

/// Пишет один explicit sample table.
fn write_track_run(
    bytes: &mut Vec<u8>,
    normalized: &NormalizedFragmentPlan<'_>,
    layout: PlannedMediaFragment,
    cancellation: &dyn Fn() -> bool,
) -> Result<(), FragmentWriteError> {
    let mut flags = TRUN_DATA_OFFSET_PRESENT
        | TRUN_SAMPLE_DURATION_PRESENT
        | TRUN_SAMPLE_SIZE_PRESENT
        | TRUN_SAMPLE_COMPOSITION_OFFSET_PRESENT;
    if layout.write_sample_flags() {
        flags |= TRUN_SAMPLE_FLAGS_PRESENT;
    }
    let version = match layout.composition_offset_encoding() {
        CompositionOffsetEncoding::UnsignedVersionZero => 0,
        CompositionOffsetEncoding::SignedVersionOne => 1,
    };
    write_full_box_header(bytes, layout.track_run_size(), *b"trun", version, flags);
    write_u32(bytes, layout.sample_count());
    write_i32(bytes, layout.data_offset());

    for (sample_index, sample) in normalized.samples().iter().enumerate() {
        if sample_index % WRITE_CANCELLATION_INTERVAL == 0 {
            check_cancelled(cancellation, FragmentWriteCancellationPhase::SampleTable)?;
        }
        let typed_index =
            u32::try_from(sample_index).map_err(|_| FragmentWriteError::ArithmeticOverflow {
                operation: FragmentWriteArithmeticOperation::SampleCount,
            })?;
        write_u32(bytes, sample.duration());
        let sample_size = normalized
            .sample_payload(sample_index)
            .map_or(0, <[u8]>::len);
        let sample_size =
            u32::try_from(sample_size).map_err(|_| FragmentWriteError::ArithmeticOverflow {
                operation: FragmentWriteArithmeticOperation::MediaPayloadSize,
            })?;
        write_u32(bytes, sample_size);
        if layout.write_sample_flags() {
            let sample_flags =
                sample
                    .flags()
                    .ok_or(FragmentWriteError::MissingVideoSampleFlags {
                        sample_index: typed_index,
                    })?;
            write_u32(bytes, sample_flags);
        }
        match layout.composition_offset_encoding() {
            CompositionOffsetEncoding::UnsignedVersionZero => {
                let offset = u32::try_from(sample.composition_offset()).map_err(|_| {
                    FragmentWriteError::CompositionOffsetUnrepresentable {
                        sample_index: typed_index,
                        offset: sample.composition_offset(),
                    }
                })?;
                write_u32(bytes, offset);
            }
            CompositionOffsetEncoding::SignedVersionOne => {
                let offset = i32::try_from(sample.composition_offset()).map_err(|_| {
                    FragmentWriteError::CompositionOffsetUnrepresentable {
                        sample_index: typed_index,
                        offset: sample.composition_offset(),
                    }
                })?;
                write_i32(bytes, offset);
            }
        }
    }
    Ok(())
}

/// Копирует каждый normalized sample payload ровно один раз и в доказанном порядке.
fn copy_media_payload(bytes: &mut Vec<u8>, normalized: &NormalizedFragmentPlan<'_>) {
    for sample_index in 0..normalized.samples().len() {
        if let Some(payload) = normalized.sample_payload(sample_index) {
            bytes.extend_from_slice(payload);
        }
    }
}

/// Пишет обычный box header.
fn write_box_header(bytes: &mut Vec<u8>, size: u32, box_type: [u8; 4]) {
    write_u32(bytes, size);
    bytes.extend_from_slice(&box_type);
}

/// Пишет ISO full-box header с 24-bit flags.
fn write_full_box_header(
    bytes: &mut Vec<u8>,
    size: u32,
    box_type: [u8; 4],
    version: u8,
    flags: u32,
) {
    write_box_header(bytes, size, box_type);
    bytes.push(version);
    let flag_bytes = flags.to_be_bytes();
    bytes.extend_from_slice(&flag_bytes[1..]);
}

/// Пишет big-endian `u32`.
fn write_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

/// Пишет big-endian `i32`.
fn write_i32(bytes: &mut Vec<u8>, value: i32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

/// Пишет big-endian `u64`.
fn write_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_be_bytes());
}
