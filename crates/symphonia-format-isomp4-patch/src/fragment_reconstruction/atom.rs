//! Borrowed AtomIterator helpers и fail-closed box classification.

use std::io::Cursor;
use std::ops::Range;

use symphonia_core::io::MediaSourceStream;

use crate::atoms::{AtomError, AtomHeader, AtomIterator, AtomType};

use super::budget::InspectionBudget;
use super::error::{
    FragmentDrmEvidence, FragmentInspectionError, FragmentInspectionLimitKind,
    FragmentPrivateExtension, FragmentStructureContext, FragmentUnsupportedLayout,
};
use super::model::FragmentInspectionRequest;
use super::support::check_cancelled;

/// UUID PIFF `tfrf`, который несёт live future-fragment metadata.
const PIFF_TFRF_UUID: [u8; 16] = [
    0xd4, 0x80, 0x7e, 0xf2, 0xca, 0x39, 0x46, 0x95, 0x8e, 0x54, 0x26, 0xcb, 0x9e, 0x46, 0xa7, 0x9f,
];
/// UUID PIFF `tfxd`; F1A не угадывает его время без existing parser ownership.
const PIFF_TFXD_UUID: [u8; 16] = [
    0x6d, 0x1d, 0x9b, 0x05, 0x42, 0xd5, 0x44, 0xe6, 0x80, 0xe2, 0x14, 0x1d, 0xaf, 0xf7, 0x57, 0xb2,
];
/// UUID PIFF Sample Encryption.
const PIFF_SAMPLE_ENCRYPTION_UUID: [u8; 16] = [
    0xa2, 0x39, 0x4f, 0x52, 0x5a, 0x9b, 0x4f, 0x14, 0xa2, 0x44, 0x6c, 0x42, 0x7c, 0x64, 0x8d, 0xf4,
];

/// Borrowed atom iterator не копирует media payload.
pub(super) type FragmentAtomIterator<'input> = AtomIterator<MediaSourceStream<'input>>;

/// Создаёт AtomIterator над borrowed slice.
pub(super) fn atom_iterator(input: &[u8]) -> FragmentAtomIterator<'_> {
    let source = MediaSourceStream::new(Box::new(Cursor::new(input)), Default::default());
    AtomIterator::new(source, Some(input.len() as u64))
}

/// Читает следующий header с cancellation и accounting.
pub(super) fn next_header(
    iterator: &mut FragmentAtomIterator<'_>,
    budget: &mut InspectionBudget<'_>,
    request: &FragmentInspectionRequest<'_, '_>,
    depth: usize,
    context: FragmentStructureContext,
) -> Result<Option<AtomHeader>, FragmentInspectionError> {
    check_cancelled(request)?;
    let header = iterator
        .next_header()
        .map_err(|error| map_atom_error(error, context))?
        .copied();
    if let Some(header) = &header {
        budget.accept_header(header, depth)?;
    }
    Ok(header)
}

/// Возвращает известный payload size.
pub(super) fn known_payload_size(header: &AtomHeader) -> Result<usize, FragmentInspectionError> {
    let size = header
        .data_size()
        .ok_or(FragmentInspectionError::UnsupportedLayout {
            reason: FragmentUnsupportedLayout::UnknownBoxSize,
        })?;
    usize::try_from(size).map_err(|_| FragmentInspectionError::OffsetOverflow)
}

/// Вычисляет payload range относительно переданного container slice.
pub(super) fn payload_range(
    container: &[u8],
    header: &AtomHeader,
) -> Result<Range<usize>, FragmentInspectionError> {
    let start =
        usize::try_from(header.data_pos()).map_err(|_| FragmentInspectionError::OffsetOverflow)?;
    let end = usize::try_from(
        header
            .end()
            .ok_or(FragmentInspectionError::UnsupportedLayout {
                reason: FragmentUnsupportedLayout::UnknownBoxSize,
            })?,
    )
    .map_err(|_| FragmentInspectionError::OffsetOverflow)?;
    if start > end || end > container.len() {
        return Err(FragmentInspectionError::StructuralTruncation {
            context: FragmentStructureContext::TopLevel,
        });
    }
    Ok(start..end)
}

/// Возвращает конец box-а относительно container slice.
pub(super) fn box_end(
    container: &[u8],
    header: &AtomHeader,
) -> Result<usize, FragmentInspectionError> {
    let end = usize::try_from(
        header
            .end()
            .ok_or(FragmentInspectionError::UnsupportedLayout {
                reason: FragmentUnsupportedLayout::UnknownBoxSize,
            })?,
    )
    .map_err(|_| FragmentInspectionError::OffsetOverflow)?;
    if end > container.len() {
        return Err(FragmentInspectionError::StructuralTruncation {
            context: FragmentStructureContext::TopLevel,
        });
    }
    Ok(end)
}

/// Проверяет признанный segment type.
pub(super) fn validate_segment_type(
    input: &[u8],
    header: &AtomHeader,
) -> Result<(), FragmentInspectionError> {
    let bytes = &input[payload_range(input, header)?];
    if bytes.len() < 8 || (bytes.len() - 8) % 4 != 0 {
        return Err(FragmentInspectionError::StructuralTruncation {
            context: FragmentStructureContext::TopLevel,
        });
    }
    let major_brand = [bytes[0], bytes[1], bytes[2], bytes[3]];
    const RECOGNIZED_BRANDS: [[u8; 4]; 6] =
        [*b"msdh", *b"msix", *b"iso6", *b"cmfs", *b"cmfv", *b"cmfa"];
    if !RECOGNIZED_BRANDS.contains(&major_brand) {
        return Err(FragmentInspectionError::UnsupportedLayout {
            reason: FragmentUnsupportedLayout::UnrecognizedSegmentType,
        });
    }
    Ok(())
}

/// Различает DRM/live/private и обычный unsupported layout.
pub(super) fn classify_or_layout<T>(
    container: &[u8],
    header: &AtomHeader,
) -> Result<T, FragmentInspectionError> {
    match header.atom_type() {
        AtomType::Uuid => classify_uuid(container, header),
        AtomType::Other(code) if is_drm_fourcc(code) => {
            Err(FragmentInspectionError::DrmProtected {
                evidence: FragmentDrmEvidence::Box(code),
            })
        }
        AtomType::Other(code) if code == *b"tfrf" => Err(FragmentInspectionError::LiveMetadata),
        AtomType::Other(code) => Err(FragmentInspectionError::UnsupportedLayout {
            reason: FragmentUnsupportedLayout::UnsupportedBox(code),
        }),
        _ => Err(FragmentInspectionError::UnsupportedLayout {
            reason: FragmentUnsupportedLayout::UnsupportedKnownAtom,
        }),
    }
}

/// Узко распознаёт exact PIFF `tfxd` envelope, не интерпретируя его timeline values.
pub(super) fn is_ignorable_piff_absolute_time(
    container: &[u8],
    header: &AtomHeader,
) -> Result<bool, FragmentInspectionError> {
    if header.atom_type() != AtomType::Uuid {
        return Ok(false);
    }
    let bytes = container.get(payload_range(container, header)?).ok_or(
        FragmentInspectionError::StructuralTruncation {
            context: FragmentStructureContext::TrackFragment,
        },
    )?;
    if bytes.get(..16) != Some(PIFF_TFXD_UUID.as_slice()) {
        return Ok(false);
    }
    // Captured dialect использует version 1: UUID + FullBox + два u64.
    if bytes.len() != 36 || bytes[16] != 1 || bytes[17..20] != [0, 0, 0] {
        return Err(FragmentInspectionError::StructuralTruncation {
            context: FragmentStructureContext::TrackFragment,
        });
    }
    Ok(true)
}

/// Классифицирует UUID по первым 16 bytes payload-а.
fn classify_uuid<T>(container: &[u8], header: &AtomHeader) -> Result<T, FragmentInspectionError> {
    let bytes = container.get(payload_range(container, header)?).ok_or(
        FragmentInspectionError::StructuralTruncation {
            context: FragmentStructureContext::TrackFragment,
        },
    )?;
    let uuid: [u8; 16] = bytes
        .get(..16)
        .ok_or(FragmentInspectionError::StructuralTruncation {
            context: FragmentStructureContext::TrackFragment,
        })?
        .try_into()
        .map_err(|_| FragmentInspectionError::StructuralTruncation {
            context: FragmentStructureContext::TrackFragment,
        })?;
    match uuid {
        PIFF_TFRF_UUID => Err(FragmentInspectionError::LiveMetadata),
        PIFF_TFXD_UUID => Err(FragmentInspectionError::PrivateExtension {
            extension: FragmentPrivateExtension::PiffAbsoluteTime,
        }),
        PIFF_SAMPLE_ENCRYPTION_UUID => Err(FragmentInspectionError::DrmProtected {
            evidence: FragmentDrmEvidence::PiffSampleEncryptionUuid,
        }),
        _ => Err(FragmentInspectionError::PrivateExtension {
            extension: FragmentPrivateExtension::UnknownUuid,
        }),
    }
}

/// Определяет encryption-related fourcc.
fn is_drm_fourcc(code: [u8; 4]) -> bool {
    matches!(
        &code,
        b"pssh" | b"senc" | b"saiz" | b"saio" | b"tenc" | b"sinf" | b"schm" | b"frma"
    )
}

/// Сравнивает Other fourcc без раскрытия raw payload.
pub(super) fn is_fourcc(atom_type: AtomType, expected: [u8; 4]) -> bool {
    matches!(atom_type, AtomType::Other(actual) if actual == expected)
}

/// Мапит существующий structural atom boundary в узкие F1A errors.
fn map_atom_error(error: AtomError, context: FragmentStructureContext) -> FragmentInspectionError {
    match error {
        AtomError::InvalidAtomSize
        | AtomError::Overrun
        | AtomError::SeekOutOfRange
        | AtomError::UnexpectedEndOfAtom
        | AtomError::UnexpectedPosition
        | AtomError::UnexpectedUnknownSizeAtom
        | AtomError::UnknownAtomSize => FragmentInspectionError::StructuralTruncation { context },
        AtomError::MaximumDepthReached => FragmentInspectionError::LimitExceeded {
            kind: FragmentInspectionLimitKind::BoxDepth,
            limit: 32,
            observed: 33,
        },
        AtomError::InvalidUtf8
        | AtomError::NoParentAtom
        | AtomError::NoPendingAtom
        | AtomError::UnexpectedReadOperation
        | AtomError::Other(_) => FragmentInspectionError::UnsupportedLayout {
            reason: FragmentUnsupportedLayout::AtomParserRejected,
        },
    }
}
