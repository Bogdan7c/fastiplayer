//! Узкий bounded parser разрешённого Smooth/PIFF fragment layout.

use std::ops::Range;

use crate::atoms::{AtomType, MfhdAtom, TfdtAtom, TfhdAtom, TrunAtom};

use super::atom::{
    atom_iterator, box_end, classify_or_layout, is_fourcc, is_ignorable_piff_absolute_time,
    next_header, payload_range, validate_segment_type,
};
use super::budget::InspectionBudget;
use super::error::{
    FragmentArithmeticOperation, FragmentBoxKind, FragmentInspectionError,
    FragmentInspectionLimitKind, FragmentStructureContext, FragmentUnsupportedLayout,
};
use super::model::FragmentInspectionRequest;
use super::support::{check_cancelled, checked_add, checked_multiply, enforce_limit};

/// Промежуточный результат разбора `traf`.
pub(super) struct ParsedTrackFragment {
    /// Проверенный track fragment header.
    pub(super) tfhd: TfhdAtom,
    /// Optional exact decode-time evidence.
    pub(super) tfdt: Option<TfdtAtom>,
    /// Один или несколько bounded runs.
    pub(super) truns: Vec<TrunAtom>,
}

/// Полный parsed fragment до timing/range normalization.
pub(super) struct ParsedMediaFragment {
    /// Абсолютная позиция `moof`.
    pub(super) moof_position: u64,
    /// Sequence из `mfhd`.
    pub(super) sequence_number: u32,
    /// Единственный track fragment.
    pub(super) track_fragment: ParsedTrackFragment,
    /// Payload range единственного `mdat`.
    pub(super) mdat_payload_range: Range<usize>,
}

/// Разбирает опциональный `styp`, один `moof` и один `mdat`.
pub(super) fn parse_top_level(
    request: &FragmentInspectionRequest<'_, '_>,
    budget: &mut InspectionBudget<'_>,
) -> Result<ParsedMediaFragment, FragmentInspectionError> {
    let input = request.input();
    let mut iterator = atom_iterator(input);
    let first = next_header(
        &mut iterator,
        budget,
        request,
        1,
        FragmentStructureContext::TopLevel,
    )?
    .ok_or(FragmentInspectionError::MissingBox {
        kind: FragmentBoxKind::MovieFragment,
    })?;

    let moof_header = if is_fourcc(first.atom_type(), *b"styp") {
        validate_segment_type(input, &first)?;
        next_header(
            &mut iterator,
            budget,
            request,
            1,
            FragmentStructureContext::TopLevel,
        )?
        .ok_or(FragmentInspectionError::MissingBox {
            kind: FragmentBoxKind::MovieFragment,
        })?
    } else {
        first
    };
    if moof_header.atom_type() != AtomType::MovieFragment {
        return classify_or_layout(input, &moof_header);
    }
    let (sequence_number, track_fragment) =
        parse_movie_fragment(input, &moof_header, budget, request)?;

    let mdat_header = next_header(
        &mut iterator,
        budget,
        request,
        1,
        FragmentStructureContext::TopLevel,
    )?
    .ok_or(FragmentInspectionError::MissingBox {
        kind: FragmentBoxKind::MediaData,
    })?;
    if mdat_header.atom_type() == AtomType::MovieFragment {
        return Err(FragmentInspectionError::DuplicateBox {
            kind: FragmentBoxKind::MovieFragment,
        });
    }
    if mdat_header.atom_type() != AtomType::MediaData {
        return classify_or_layout(input, &mdat_header);
    }

    let mdat_payload_range = payload_range(input, &mdat_header)?;
    let mdat_end = box_end(input, &mdat_header)?;
    if input[mdat_end..].iter().any(|byte| *byte != 0) {
        return Err(FragmentInspectionError::UnsupportedLayout {
            reason: FragmentUnsupportedLayout::TrailingNonPadding,
        });
    }

    Ok(ParsedMediaFragment {
        moof_position: moof_header.pos(),
        sequence_number,
        track_fragment,
        mdat_payload_range,
    })
}

/// Разбирает `mfhd` и единственный `traf`, не применяя S28A-only обязательный `tfdt`.
fn parse_movie_fragment(
    input: &[u8],
    header: &crate::atoms::AtomHeader,
    budget: &mut InspectionBudget<'_>,
    request: &FragmentInspectionRequest<'_, '_>,
) -> Result<(u32, ParsedTrackFragment), FragmentInspectionError> {
    let payload = &input[payload_range(input, header)?];
    let mut iterator = atom_iterator(payload);
    let mut sequence_number = None;
    let mut track_fragment = None;

    while let Some(child) = next_header(
        &mut iterator,
        budget,
        request,
        2,
        FragmentStructureContext::MovieFragment,
    )? {
        match child.atom_type() {
            AtomType::MovieFragmentHeader => {
                if sequence_number.is_some() {
                    return Err(FragmentInspectionError::DuplicateBox {
                        kind: FragmentBoxKind::MovieFragmentHeader,
                    });
                }
                let mfhd = iterator.read_atom::<MfhdAtom>().map_err(|_| {
                    FragmentInspectionError::StructuralTruncation {
                        context: FragmentStructureContext::MovieFragmentHeader,
                    }
                })?;
                sequence_number = Some(mfhd.sequence_number);
            }
            AtomType::TrackFragment => {
                budget.accept_traf()?;
                if track_fragment.is_some() {
                    return Err(FragmentInspectionError::DuplicateBox {
                        kind: FragmentBoxKind::TrackFragment,
                    });
                }
                check_cancelled(request)?;
                let child_payload = payload_range(payload, &child)?;
                track_fragment = Some(parse_track_fragment(
                    &payload[child_payload],
                    budget,
                    request,
                )?);
            }
            _ => return classify_or_layout(payload, &child),
        }
    }

    Ok((
        sequence_number.ok_or(FragmentInspectionError::MissingBox {
            kind: FragmentBoxKind::MovieFragmentHeader,
        })?,
        track_fragment.ok_or(FragmentInspectionError::MissingBox {
            kind: FragmentBoxKind::TrackFragment,
        })?,
    ))
}

/// Разбирает один `tfhd`, zero/one `tfdt` и one-or-more `trun`.
fn parse_track_fragment(
    payload: &[u8],
    budget: &mut InspectionBudget<'_>,
    request: &FragmentInspectionRequest<'_, '_>,
) -> Result<ParsedTrackFragment, FragmentInspectionError> {
    let mut iterator = atom_iterator(payload);
    let mut tfhd = None;
    let mut tfdt = None;
    let mut truns = Vec::new();

    while let Some(child) = next_header(
        &mut iterator,
        budget,
        request,
        3,
        FragmentStructureContext::TrackFragment,
    )? {
        check_cancelled(request)?;
        if is_ignorable_piff_absolute_time(payload, &child)? {
            // ManifestWindow остаётся авторитетным: F1A не применяет PIFF absolute-time values.
            continue;
        }
        match child.atom_type() {
            AtomType::TrackFragmentHeader => {
                if tfhd.is_some() {
                    return Err(FragmentInspectionError::DuplicateBox {
                        kind: FragmentBoxKind::TrackFragmentHeader,
                    });
                }
                tfhd = Some(iterator.read_atom::<TfhdAtom>().map_err(|_| {
                    FragmentInspectionError::StructuralTruncation {
                        context: FragmentStructureContext::TrackFragmentHeader,
                    }
                })?);
            }
            AtomType::TrackFragmentDecodeTime => {
                if tfdt.is_some() {
                    return Err(FragmentInspectionError::DuplicateBox {
                        kind: FragmentBoxKind::TrackFragmentDecodeTime,
                    });
                }
                tfdt = Some(iterator.read_atom::<TfdtAtom>().map_err(|_| {
                    FragmentInspectionError::StructuralTruncation {
                        context: FragmentStructureContext::TrackFragmentDecodeTime,
                    }
                })?);
            }
            AtomType::TrackFragmentRun => {
                let child_payload = payload_range(payload, &child)?;
                let (sample_count, table_bytes) = preflight_trun(&payload[child_payload], budget)?;
                budget.accept_trun(sample_count, table_bytes)?;
                truns.push(iterator.read_atom::<TrunAtom>().map_err(|_| {
                    FragmentInspectionError::StructuralTruncation {
                        context: FragmentStructureContext::TrackFragmentRun,
                    }
                })?);
            }
            _ => return classify_or_layout(payload, &child),
        }
    }

    let tfhd = tfhd.ok_or(FragmentInspectionError::MissingBox {
        kind: FragmentBoxKind::TrackFragmentHeader,
    })?;
    if tfhd.duration_is_empty {
        return Err(FragmentInspectionError::UnsupportedLayout {
            reason: FragmentUnsupportedLayout::EmptyTrackFragment,
        });
    }
    if truns.is_empty() {
        return Err(FragmentInspectionError::MissingBox {
            kind: FragmentBoxKind::TrackFragmentRun,
        });
    }
    Ok(ParsedTrackFragment { tfhd, tfdt, truns })
}

/// Валидирует `trun` layout и budgets до allocations/loop existing parser-а.
fn preflight_trun(
    payload: &[u8],
    budget: &InspectionBudget<'_>,
) -> Result<(usize, usize), FragmentInspectionError> {
    if payload.len() < 8 {
        return Err(FragmentInspectionError::StructuralTruncation {
            context: FragmentStructureContext::TrackFragmentRun,
        });
    }
    let flags = u32::from_be_bytes([0, payload[1], payload[2], payload[3]]);
    let sample_count =
        u32::from_be_bytes([payload[4], payload[5], payload[6], payload[7]]) as usize;
    if sample_count == 0 {
        return Err(FragmentInspectionError::UnsupportedLayout {
            reason: FragmentUnsupportedLayout::EmptyTrackRun,
        });
    }
    let fixed_field_count = usize::from(flags & 0x000001 != 0) + usize::from(flags & 0x000004 != 0);
    let sample_field_count = usize::from(flags & 0x000100 != 0)
        + usize::from(flags & 0x000200 != 0)
        + usize::from(flags & 0x000400 != 0)
        + usize::from(flags & 0x000800 != 0);
    let fixed_bytes = checked_add(
        8,
        checked_multiply(
            fixed_field_count,
            4,
            FragmentArithmeticOperation::SampleMetadataBytes,
        )?,
        FragmentArithmeticOperation::SampleMetadataBytes,
    )?;
    let table_bytes = checked_multiply(
        checked_multiply(
            sample_count,
            sample_field_count,
            FragmentArithmeticOperation::SampleMetadataBytes,
        )?,
        4,
        FragmentArithmeticOperation::SampleMetadataBytes,
    )?;
    if checked_add(
        fixed_bytes,
        table_bytes,
        FragmentArithmeticOperation::SampleMetadataBytes,
    )? != payload.len()
    {
        return Err(FragmentInspectionError::StructuralTruncation {
            context: FragmentStructureContext::TrackFragmentRun,
        });
    }
    enforce_limit(
        FragmentInspectionLimitKind::Samples,
        budget.limits().max_samples(),
        checked_add(
            budget.sample_count(),
            sample_count,
            FragmentArithmeticOperation::SampleCount,
        )?,
    )?;
    Ok((sample_count, table_bytes))
}
