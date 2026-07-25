//! Checked timing, flags и byte-range normalization.

use crate::atoms::{TfhdAtom, TrunAtom};

use super::error::{
    FragmentArithmeticOperation, FragmentBoxKind, FragmentInspectionError, FragmentTimingEvidence,
};
use super::model::{
    FragmentCodedCoverage, FragmentInspectionRequest, FragmentRapRequirement,
    FragmentSampleDefaults, NormalizedFragmentPlan, NormalizedFragmentSample, verified_sample,
};
use super::parse::ParsedMediaFragment;
use super::support::check_cancelled;

/// Периодичность cancellation poll внутри sample loop.
const SAMPLE_CANCELLATION_INTERVAL: usize = 32;

/// Уже разрешённые optional defaults.
#[derive(Clone, Copy)]
struct ResolvedSampleDefaults {
    duration: Option<u32>,
    size: Option<u32>,
    flags: Option<u32>,
}

/// Нормализует timing, flags и byte ranges с checked arithmetic.
pub(super) fn normalize_samples<'input>(
    request: &FragmentInspectionRequest<'input, '_>,
    parsed: ParsedMediaFragment,
) -> Result<NormalizedFragmentPlan<'input>, FragmentInspectionError> {
    let expectation = request.expectation();
    let track_fragment = parsed.track_fragment;
    let actual_track_id = track_fragment.tfhd.track_id;
    if actual_track_id != expectation.track_id().get() {
        return Err(FragmentInspectionError::MultiTrack {
            expected_track_id: expectation.track_id().get(),
            actual_track_id,
        });
    }
    if let Some(tfdt) = &track_fragment.tfdt
        && tfdt.base_media_decode_time != expectation.base_decode_time().get()
    {
        return Err(FragmentInspectionError::TimingConflict {
            expected_base_decode_time: expectation.base_decode_time().get(),
            actual_base_decode_time: tfdt.base_media_decode_time,
        });
    }

    let defaults = resolved_defaults(&track_fragment.tfhd, expectation.sample_defaults());
    let mut samples = Vec::new();
    let mut next_dts = expectation.base_decode_time().get();
    let mut next_payload_offset = parsed.mdat_payload_range.start as u64;
    let mdat_end = parsed.mdat_payload_range.end as u64;
    let traf_base = track_fragment
        .tfhd
        .base_data_offset
        .unwrap_or(parsed.moof_position);

    for trun in &track_fragment.truns {
        check_cancelled(request)?;
        let run_start = match trun.data_offset {
            Some(offset) => checked_signed_offset(traf_base, offset)?,
            None if samples.is_empty() => traf_base,
            None => next_payload_offset,
        };
        validate_run_start(
            run_start,
            next_payload_offset,
            parsed.mdat_payload_range.start as u64,
            mdat_end,
            !samples.is_empty(),
        )?;
        next_payload_offset = run_start;

        for sample_index_in_run in 0..trun.sample_count {
            let global_sample_index = u32::try_from(samples.len()).map_err(|_| {
                FragmentInspectionError::ArithmeticOverflow {
                    operation: FragmentArithmeticOperation::SampleCount,
                }
            })?;
            if samples.len() % SAMPLE_CANCELLATION_INTERVAL == 0 {
                check_cancelled(request)?;
            }
            let duration = resolved_duration(trun, sample_index_in_run, defaults.duration).ok_or(
                FragmentInspectionError::TimingEvidenceMissing {
                    evidence: FragmentTimingEvidence::Duration,
                    sample_index: global_sample_index,
                },
            )?;
            let size = resolved_size(trun, sample_index_in_run, defaults.size).ok_or(
                FragmentInspectionError::TimingEvidenceMissing {
                    evidence: FragmentTimingEvidence::Size,
                    sample_index: global_sample_index,
                },
            )?;
            let composition_offset = trun.sample_composition_offset(sample_index_in_run);
            let pts = checked_presentation_time(next_dts, composition_offset)?;
            let flags = resolved_flags(trun, sample_index_in_run, defaults.flags);
            let sample_end = next_payload_offset.checked_add(u64::from(size)).ok_or(
                FragmentInspectionError::ArithmeticOverflow {
                    operation: FragmentArithmeticOperation::SampleRange,
                },
            )?;
            if next_payload_offset < parsed.mdat_payload_range.start as u64 || sample_end > mdat_end
            {
                return Err(FragmentInspectionError::SampleRangeOutsideMdat {
                    sample_start: next_payload_offset,
                    sample_end,
                    mdat_start: parsed.mdat_payload_range.start as u64,
                    mdat_end,
                });
            }
            let payload_range = usize::try_from(next_payload_offset)
                .map_err(|_| FragmentInspectionError::OffsetOverflow)?
                ..usize::try_from(sample_end)
                    .map_err(|_| FragmentInspectionError::OffsetOverflow)?;
            samples.push(verified_sample(
                next_dts,
                pts,
                duration,
                composition_offset,
                flags,
                payload_range,
            ));
            next_dts = next_dts.checked_add(u64::from(duration)).ok_or(
                FragmentInspectionError::ArithmeticOverflow {
                    operation: FragmentArithmeticOperation::DecodeTime,
                },
            )?;
            next_payload_offset = sample_end;
        }
    }

    validate_random_access(
        expectation.rap_requirement(),
        &track_fragment.truns,
        defaults.flags,
        &samples,
    )?;
    if next_payload_offset != mdat_end {
        return Err(FragmentInspectionError::PayloadMismatch {
            expected: mdat_end,
            actual: next_payload_offset,
        });
    }
    let coded_coverage =
        FragmentCodedCoverage::checked(expectation.base_decode_time().get(), next_dts)?;

    Ok(NormalizedFragmentPlan::verified(
        request.input(),
        parsed.sequence_number,
        expectation.track_id(),
        coded_coverage,
        parsed.mdat_payload_range,
        samples,
    ))
}

/// Resolved defaults сохраняют precedence `tfhd` перед caller-provided `trex`.
fn resolved_defaults(tfhd: &TfhdAtom, fallback: FragmentSampleDefaults) -> ResolvedSampleDefaults {
    ResolvedSampleDefaults {
        duration: match tfhd.default_sample_duration {
            Some(0) => None,
            Some(value) => Some(value),
            None => fallback.sample_duration(),
        },
        size: match tfhd.default_sample_size {
            Some(0) => None,
            Some(value) => Some(value),
            None => fallback.sample_size(),
        },
        flags: tfhd.default_sample_flags.or(fallback.sample_flags()),
    }
}

/// Разрешает duration с `trun` precedence.
fn resolved_duration(trun: &TrunAtom, sample_index: u32, default: Option<u32>) -> Option<u32> {
    if trun.is_sample_duration_present() {
        nonzero(trun.sample_timing(sample_index, 0).1)
    } else {
        default
    }
}

/// Разрешает size с `trun` precedence.
fn resolved_size(trun: &TrunAtom, sample_index: u32, default: Option<u32>) -> Option<u32> {
    if trun.is_sample_size_present() {
        nonzero(trun.sample_size(sample_index, 0))
    } else {
        default
    }
}

/// Не превращает ISO zero в доказанное значение.
const fn nonzero(value: u32) -> Option<u32> {
    if value == 0 { None } else { Some(value) }
}

/// Разрешает effective flags, сохраняя отсутствие evidence.
fn resolved_flags(trun: &TrunAtom, sample_index: u32, default: Option<u32>) -> Option<u32> {
    if trun.are_sample_flags_present() {
        Some(trun.effective_sample_flags(sample_index, default.unwrap_or(0)))
    } else if sample_index == 0 {
        trun.first_sample_flags.or(default)
    } else {
        default
    }
}

/// Проверяет RAP только для typed video policy.
fn validate_random_access(
    requirement: FragmentRapRequirement,
    truns: &[TrunAtom],
    default_flags: Option<u32>,
    samples: &[NormalizedFragmentSample],
) -> Result<(), FragmentInspectionError> {
    if requirement == FragmentRapRequirement::NotRequiredForAudio {
        return Ok(());
    }
    let first_sample = samples.first().ok_or(FragmentInspectionError::MissingBox {
        kind: FragmentBoxKind::TrackFragmentRun,
    })?;
    if first_sample.flags().is_none() {
        return Err(FragmentInspectionError::TimingEvidenceMissing {
            evidence: FragmentTimingEvidence::Flags,
            sample_index: 0,
        });
    }
    let first_run = truns.first().ok_or(FragmentInspectionError::MissingBox {
        kind: FragmentBoxKind::TrackFragmentRun,
    })?;
    if !first_run.is_proven_sync_sample(0, default_flags.unwrap_or(0)) {
        return Err(FragmentInspectionError::RapFailure);
    }
    Ok(())
}

/// Проверяет непрерывность run ranges.
fn validate_run_start(
    run_start: u64,
    expected_start: u64,
    mdat_start: u64,
    mdat_end: u64,
    has_previous_run: bool,
) -> Result<(), FragmentInspectionError> {
    if run_start < expected_start {
        if !has_previous_run {
            return Err(FragmentInspectionError::SampleRangeOutsideMdat {
                sample_start: run_start,
                sample_end: run_start,
                mdat_start,
                mdat_end,
            });
        }
        return Err(FragmentInspectionError::SampleRangeOverlap {
            previous_end: expected_start,
            next_start: run_start,
        });
    }
    if run_start > expected_start {
        return Err(FragmentInspectionError::PayloadMismatch {
            expected: expected_start,
            actual: run_start,
        });
    }
    if run_start < mdat_start || run_start > mdat_end {
        return Err(FragmentInspectionError::SampleRangeOutsideMdat {
            sample_start: run_start,
            sample_end: run_start,
            mdat_start,
            mdat_end,
        });
    }
    Ok(())
}

/// Применяет signed `trun.data_offset` без wrapping arithmetic.
fn checked_signed_offset(base: u64, offset: i32) -> Result<u64, FragmentInspectionError> {
    if offset.is_negative() {
        base.checked_sub(u64::from(offset.unsigned_abs()))
            .ok_or(FragmentInspectionError::OffsetOverflow)
    } else {
        base.checked_add(offset as u64)
            .ok_or(FragmentInspectionError::OffsetOverflow)
    }
}

/// Вычисляет PTS, отклоняя отрицательное либо переполненное значение.
fn checked_presentation_time(
    dts: u64,
    composition_offset: i64,
) -> Result<u64, FragmentInspectionError> {
    let pts = i128::from(dts) + i128::from(composition_offset);
    u64::try_from(pts).map_err(|_| FragmentInspectionError::ArithmeticOverflow {
        operation: FragmentArithmeticOperation::PresentationTime,
    })
}
