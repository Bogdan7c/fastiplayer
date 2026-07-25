//! Pure checked planning canonical `moof+mdat` layout-а.

use super::super::model::NormalizedFragmentPlan;
use super::error::{
    FragmentMediaBoxType, FragmentWriteArithmeticOperation, FragmentWriteCancellationPhase,
    FragmentWriteError,
};
use super::model::{FragmentMediaKind, FragmentWriteLimits};

/// Проверяем cancellation каждые 32 sample metadata entries.
pub(super) const WRITE_CANCELLATION_INTERVAL: usize = 32;

/// `mfhd` с full-box header и sequence number.
const MOVIE_FRAGMENT_HEADER_SIZE: u64 = 16;
/// `tfhd` с одним track ID и `default-base-is-moof`.
const TRACK_FRAGMENT_HEADER_SIZE: u64 = 16;
/// `tfdt` version 0.
const TRACK_FRAGMENT_DECODE_TIME_V0_SIZE: u64 = 16;
/// `tfdt` version 1.
const TRACK_FRAGMENT_DECODE_TIME_V1_SIZE: u64 = 20;
/// `trun` до per-sample table: header, full-box, count, data offset.
const TRACK_RUN_BASE_SIZE: u64 = 20;
/// Обычный 32-bit ISO box header.
pub(super) const BOX_HEADER_SIZE: u64 = 8;

/// Lossless encoding, общая для всех CTO одного canonical `trun`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CompositionOffsetEncoding {
    /// Version 0 хранит каждый non-negative offset как `u32`.
    UnsignedVersionZero,
    /// Version 1 хранит каждый offset как `i32`.
    SignedVersionOne,
}

/// Exact `tfdt` wire encoding выбранная по coded coverage start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DecodeTimeEncoding {
    /// Version 0 с `u32` decode time.
    VersionZero,
    /// Version 1 с `u64` decode time.
    VersionOne,
}

/// Полностью checked layout без media bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PlannedMediaFragment {
    movie_fragment_size: u32,
    track_fragment_size: u32,
    track_run_size: u32,
    media_data_size: u32,
    total_size: usize,
    media_payload_size: usize,
    sample_count: u32,
    data_offset: i32,
    decode_time_encoding: DecodeTimeEncoding,
    composition_offset_encoding: CompositionOffsetEncoding,
    write_sample_flags: bool,
}

impl PlannedMediaFragment {
    pub(super) const fn movie_fragment_size(self) -> u32 {
        self.movie_fragment_size
    }

    pub(super) const fn track_fragment_size(self) -> u32 {
        self.track_fragment_size
    }

    pub(super) const fn track_run_size(self) -> u32 {
        self.track_run_size
    }

    pub(super) const fn media_data_size(self) -> u32 {
        self.media_data_size
    }

    pub(super) const fn total_size(self) -> usize {
        self.total_size
    }

    pub(super) const fn media_payload_size(self) -> usize {
        self.media_payload_size
    }

    pub(super) const fn sample_count(self) -> u32 {
        self.sample_count
    }

    pub(super) const fn data_offset(self) -> i32 {
        self.data_offset
    }

    pub(super) const fn decode_time_encoding(self) -> DecodeTimeEncoding {
        self.decode_time_encoding
    }

    pub(super) const fn composition_offset_encoding(self) -> CompositionOffsetEncoding {
        self.composition_offset_encoding
    }

    pub(super) const fn write_sample_flags(self) -> bool {
        self.write_sample_flags
    }
}

/// Доказывает все размеры и представимость до allocation/write.
pub(super) fn plan_media_fragment(
    normalized: &NormalizedFragmentPlan<'_>,
    media_kind: FragmentMediaKind,
    limits: FragmentWriteLimits,
    cancellation: &dyn Fn() -> bool,
) -> Result<PlannedMediaFragment, FragmentWriteError> {
    check_cancelled(cancellation, FragmentWriteCancellationPhase::Planning)?;

    let sample_count = u32::try_from(normalized.samples().len()).map_err(|_| {
        FragmentWriteError::ArithmeticOverflow {
            operation: FragmentWriteArithmeticOperation::SampleCount,
        }
    })?;
    let (media_payload_size, composition_offset_encoding, write_sample_flags) =
        inspect_samples(normalized, media_kind, cancellation)?;
    let sample_entry_size = if write_sample_flags { 16_u64 } else { 12_u64 };
    let sample_table_size = sample_entry_size
        .checked_mul(u64::from(sample_count))
        .ok_or(FragmentWriteError::ArithmeticOverflow {
            operation: FragmentWriteArithmeticOperation::SampleTableSize,
        })?;
    let track_run_size = checked_box_size(
        FragmentMediaBoxType::TrackFragmentRun,
        TRACK_RUN_BASE_SIZE.checked_add(sample_table_size).ok_or(
            FragmentWriteError::ArithmeticOverflow {
                operation: FragmentWriteArithmeticOperation::BoxSize,
            },
        )?,
    )?;
    let decode_time_encoding = if normalized.coded_coverage().start() <= u64::from(u32::MAX) {
        DecodeTimeEncoding::VersionZero
    } else {
        DecodeTimeEncoding::VersionOne
    };
    let decode_time_size = match decode_time_encoding {
        DecodeTimeEncoding::VersionZero => TRACK_FRAGMENT_DECODE_TIME_V0_SIZE,
        DecodeTimeEncoding::VersionOne => TRACK_FRAGMENT_DECODE_TIME_V1_SIZE,
    };
    let track_fragment_size = checked_box_size(
        FragmentMediaBoxType::TrackFragment,
        checked_sum(&[
            BOX_HEADER_SIZE,
            TRACK_FRAGMENT_HEADER_SIZE,
            decode_time_size,
            u64::from(track_run_size),
        ])?,
    )?;
    let movie_fragment_size = checked_box_size(
        FragmentMediaBoxType::MovieFragment,
        checked_sum(&[
            BOX_HEADER_SIZE,
            MOVIE_FRAGMENT_HEADER_SIZE,
            u64::from(track_fragment_size),
        ])?,
    )?;
    let data_offset = checked_data_offset(
        u64::from(movie_fragment_size)
            .checked_add(BOX_HEADER_SIZE)
            .ok_or(FragmentWriteError::ArithmeticOverflow {
                operation: FragmentWriteArithmeticOperation::DataOffset,
            })?,
    )?;
    let media_data_size = checked_box_size(
        FragmentMediaBoxType::MediaData,
        BOX_HEADER_SIZE
            .checked_add(u64_from_usize(media_payload_size)?)
            .ok_or(FragmentWriteError::ArithmeticOverflow {
                operation: FragmentWriteArithmeticOperation::BoxSize,
            })?,
    )?;
    let total_u64 = u64::from(movie_fragment_size)
        .checked_add(u64::from(media_data_size))
        .ok_or(FragmentWriteError::ArithmeticOverflow {
            operation: FragmentWriteArithmeticOperation::OutputSize,
        })?;
    let total_size =
        usize::try_from(total_u64).map_err(|_| FragmentWriteError::ArithmeticOverflow {
            operation: FragmentWriteArithmeticOperation::OutputSize,
        })?;
    if total_size > limits.maximum_output_bytes() {
        return Err(FragmentWriteError::OutputLimitExceeded {
            limit: u64_from_usize(limits.maximum_output_bytes())?,
            required: total_u64,
        });
    }

    Ok(PlannedMediaFragment {
        movie_fragment_size,
        track_fragment_size,
        track_run_size,
        media_data_size,
        total_size,
        media_payload_size,
        sample_count,
        data_offset,
        decode_time_encoding,
        composition_offset_encoding,
        write_sample_flags,
    })
}

/// Собирает sample-dependent evidence без allocation.
fn inspect_samples(
    normalized: &NormalizedFragmentPlan<'_>,
    media_kind: FragmentMediaKind,
    cancellation: &dyn Fn() -> bool,
) -> Result<(usize, CompositionOffsetEncoding, bool), FragmentWriteError> {
    let mut payload_size = 0_usize;
    let mut first_negative = None;
    let mut largest_positive = None;
    let mut audio_flags_presence = None;

    for (sample_index, sample) in normalized.samples().iter().enumerate() {
        if sample_index % WRITE_CANCELLATION_INTERVAL == 0 {
            check_cancelled(cancellation, FragmentWriteCancellationPhase::SampleTable)?;
        }
        let typed_index =
            u32::try_from(sample_index).map_err(|_| FragmentWriteError::ArithmeticOverflow {
                operation: FragmentWriteArithmeticOperation::SampleCount,
            })?;
        match media_kind {
            FragmentMediaKind::VideoWithRequiredProvenRandomAccess if sample.flags().is_none() => {
                return Err(FragmentWriteError::MissingVideoSampleFlags {
                    sample_index: typed_index,
                });
            }
            FragmentMediaKind::AudioWithoutRandomAccessRequirement => {
                let flags_present = sample.flags().is_some();
                match audio_flags_presence {
                    Some(expected) if expected != flags_present => {
                        return Err(FragmentWriteError::AudioSampleFlagsNotUniform {
                            sample_index: typed_index,
                        });
                    }
                    None => audio_flags_presence = Some(flags_present),
                    Some(_) => {}
                }
            }
            FragmentMediaKind::VideoWithRequiredProvenRandomAccess => {}
        }
        let payload_length = normalized
            .sample_payload(sample_index)
            .map_or(0, <[u8]>::len);
        u32::try_from(payload_length).map_err(|_| FragmentWriteError::ArithmeticOverflow {
            operation: FragmentWriteArithmeticOperation::MediaPayloadSize,
        })?;
        payload_size = payload_size.checked_add(payload_length).ok_or(
            FragmentWriteError::ArithmeticOverflow {
                operation: FragmentWriteArithmeticOperation::MediaPayloadSize,
            },
        )?;
        let offset = sample.composition_offset();
        if offset < 0 {
            first_negative.get_or_insert((typed_index, offset));
        } else if largest_positive.is_none_or(|(_, value)| offset > value) {
            largest_positive = Some((typed_index, offset));
        }
    }

    let expected_payload = normalized.mdat_payload_range().len();
    if payload_size != expected_payload {
        return Err(FragmentWriteError::MediaPayloadLengthMismatch {
            expected: u64_from_usize(expected_payload)?,
            actual: u64_from_usize(payload_size)?,
        });
    }

    match first_negative {
        Some((sample_index, offset)) if offset < i64::from(i32::MIN) => {
            Err(FragmentWriteError::CompositionOffsetUnrepresentable {
                sample_index,
                offset,
            })
        }
        Some(_) => {
            if let Some((sample_index, offset)) =
                largest_positive.filter(|(_, offset)| *offset > i64::from(i32::MAX))
            {
                return Err(FragmentWriteError::CompositionOffsetUnrepresentable {
                    sample_index,
                    offset,
                });
            }
            Ok((
                payload_size,
                CompositionOffsetEncoding::SignedVersionOne,
                should_write_flags(media_kind, audio_flags_presence),
            ))
        }
        None => {
            if let Some((sample_index, offset)) =
                largest_positive.filter(|(_, offset)| *offset > i64::from(u32::MAX))
            {
                return Err(FragmentWriteError::CompositionOffsetUnrepresentable {
                    sample_index,
                    offset,
                });
            }
            Ok((
                payload_size,
                CompositionOffsetEncoding::UnsignedVersionZero,
                should_write_flags(media_kind, audio_flags_presence),
            ))
        }
    }
}

/// Video всегда пишет proven flags; audio сохраняет их только когда они есть у всех samples.
const fn should_write_flags(
    media_kind: FragmentMediaKind,
    audio_flags_presence: Option<bool>,
) -> bool {
    match media_kind {
        FragmentMediaKind::VideoWithRequiredProvenRandomAccess => true,
        FragmentMediaKind::AudioWithoutRandomAccessRequirement => {
            matches!(audio_flags_presence, Some(true))
        }
    }
}

/// Checked сумма небольшого фиксированного списка box sizes.
fn checked_sum(values: &[u64]) -> Result<u64, FragmentWriteError> {
    values.iter().try_fold(0_u64, |sum, value| {
        sum.checked_add(*value)
            .ok_or(FragmentWriteError::ArithmeticOverflow {
                operation: FragmentWriteArithmeticOperation::BoxSize,
            })
    })
}

/// Ограничивает canonical profile обычными 32-bit box headers.
pub(super) fn checked_box_size(
    box_type: FragmentMediaBoxType,
    size: u64,
) -> Result<u32, FragmentWriteError> {
    u32::try_from(size).map_err(|_| FragmentWriteError::BoxSizeUnrepresentable { box_type, size })
}

/// Ограничивает `trun.data_offset` его signed wire type.
pub(super) fn checked_data_offset(offset: u64) -> Result<i32, FragmentWriteError> {
    i32::try_from(offset).map_err(|_| FragmentWriteError::DataOffsetUnrepresentable { offset })
}

/// Конвертирует platform size без truncation.
fn u64_from_usize(value: usize) -> Result<u64, FragmentWriteError> {
    u64::try_from(value).map_err(|_| FragmentWriteError::ArithmeticOverflow {
        operation: FragmentWriteArithmeticOperation::OutputSize,
    })
}

/// Возвращает typed cancellation для конкретной write phase.
pub(super) fn check_cancelled(
    cancellation: &dyn Fn() -> bool,
    phase: FragmentWriteCancellationPhase,
) -> Result<(), FragmentWriteError> {
    if cancellation() {
        Err(FragmentWriteError::Cancelled { phase })
    } else {
        Ok(())
    }
}
