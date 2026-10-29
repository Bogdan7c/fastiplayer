//! Последняя линия против DRM: зашифрованное видео не играем, а честно отказываем.
//!
//! Экстеншен не трогает элементы с ключами (`setMediaKeys`), но признак может не
//! прийти вовсе: Vimeo шифрует CENC без события `encrypted` (разведка 00c). Без
//! этой проверки на экране был бы мусор. Признаки:
//! - fMP4: описание дорожки `encv`/`enca` в `stsd` (так CENC переименовывает
//!   исходный `avc1`/`mp4a`; `tenc` лежит внутри них);
//! - WebM: элемент `ContentEncryption` в `ContentEncodings` дорожки.

use crate::container::PushedContainer;
use crate::framing::{EbmlElementSize, read_box_header, read_element_header};

/// Итог проверки init-сегмента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtectionVerdict {
    /// Шифрования нет.
    Clear,
    /// Хотя бы одна дорожка зашифрована.
    Protected,
}

/// Структура init-сегмента не сходится: проверить шифрование нельзя.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the initialization segment structure is inconsistent")]
pub(crate) struct InconsistentInitialization;

/// Проверяет init-сегмент контейнера на шифрование.
///
/// Нечитаемая структура — ошибка, а не «чисто»: иначе страница могла бы спрятать
/// `encv` за кривыми размерами.
pub(crate) fn scan_initialization(
    container: PushedContainer,
    initialization: &[u8],
) -> Result<ProtectionVerdict, InconsistentInitialization> {
    let protected = match container {
        PushedContainer::FragmentedMp4 => iso_bmff_has_protected_sample_entry(initialization)?,
        PushedContainer::WebM => matroska_has_content_encryption(initialization)?,
        PushedContainer::MpegTs => false,
    };
    Ok(if protected {
        ProtectionVerdict::Protected
    } else {
        ProtectionVerdict::Clear
    })
}

/// Путь от `moov` до таблицы описаний дорожки.
const ISO_PATH_TO_SAMPLE_DESCRIPTIONS: [&[u8; 4]; 5] =
    [b"trak", b"mdia", b"minf", b"stbl", b"stsd"];
/// `stsd` — «полный» бокс: 4 байта версии и флагов, затем 4 байта числа записей.
const STSD_PREAMBLE_BYTES: usize = 8;
/// Типы описаний, которые CENC ставит вместо исходных.
const PROTECTED_SAMPLE_ENTRIES: [&[u8; 4]; 2] = [b"encv", b"enca"];

/// Есть ли зашифрованное описание дорожки в `moov`.
fn iso_bmff_has_protected_sample_entry(
    initialization: &[u8],
) -> Result<bool, InconsistentInitialization> {
    for (box_type, movie) in iso_children(initialization)? {
        if &box_type != b"moov" {
            continue;
        }
        let mut regions = vec![movie];
        for step in ISO_PATH_TO_SAMPLE_DESCRIPTIONS {
            let mut next_regions = Vec::new();
            for region in regions {
                for (child_type, body) in iso_children(region)? {
                    if &child_type == step {
                        next_regions.push(body);
                    }
                }
            }
            regions = next_regions;
        }
        for sample_descriptions in regions {
            let entries = sample_descriptions
                .get(STSD_PREAMBLE_BYTES..)
                .ok_or(InconsistentInitialization)?;
            if iso_children(entries)?
                .iter()
                .any(|(entry_type, _)| PROTECTED_SAMPLE_ENTRIES.contains(&entry_type))
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Бокс внутри региона: тип и тело.
type IsoChild<'region> = ([u8; 4], &'region [u8]);

/// Дети региона боксов: тип и тело каждого. Регион обязан делиться ровно на боксы.
fn iso_children(region: &[u8]) -> Result<Vec<IsoChild<'_>>, InconsistentInitialization> {
    let mut children = Vec::new();
    let mut position = 0;
    while position < region.len() {
        let rest = &region[position..];
        let header = read_box_header(rest)
            .map_err(|_| InconsistentInitialization)?
            .ok_or(InconsistentInitialization)?;
        let total_len =
            usize::try_from(header.total_len).map_err(|_| InconsistentInitialization)?;
        let body = rest
            .get(header.header_len..total_len)
            .ok_or(InconsistentInitialization)?;
        children.push((header.box_type, body));
        position += total_len;
    }
    Ok(children)
}

/// EBML-заголовок документа.
const EBML_HEADER_ID: u32 = 0x1A45_DFA3;
/// `Segment`.
const SEGMENT_ID: u32 = 0x1853_8067;
/// Путь от детей `Segment` до признака шифрования.
const MATROSKA_PATH_TO_ENCRYPTION: [u32; 5] = [
    0x1654_AE6B, // Tracks
    0xAE,        // TrackEntry
    0x6D80,      // ContentEncodings
    0x6240,      // ContentEncoding
    0x5035,      // ContentEncryption
];

/// Есть ли `ContentEncryption` у какой-нибудь дорожки.
fn matroska_has_content_encryption(
    initialization: &[u8],
) -> Result<bool, InconsistentInitialization> {
    let document = ebml_children(initialization, AllowUnknownSize::OnlyLast)?;
    let mut regions = Vec::new();
    for (id, body) in document {
        match id {
            EBML_HEADER_ID => {}
            SEGMENT_ID => regions.push(body),
            _ => return Err(InconsistentInitialization),
        }
    }
    for step in MATROSKA_PATH_TO_ENCRYPTION {
        let mut next_regions = Vec::new();
        for region in regions {
            for (id, body) in ebml_children(region, AllowUnknownSize::Never)? {
                if id == step {
                    next_regions.push(body);
                }
            }
        }
        regions = next_regions;
    }
    Ok(!regions.is_empty())
}

/// Может ли элемент региона иметь «неизвестный» размер.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AllowUnknownSize {
    /// Только последний (`Segment` init-сегмента тянется до конца байтов).
    OnlyLast,
    /// Нигде.
    Never,
}

/// Дети региона EBML: ID и тело каждого элемента.
fn ebml_children(
    region: &[u8],
    unknown_size: AllowUnknownSize,
) -> Result<Vec<(u32, &[u8])>, InconsistentInitialization> {
    let mut children = Vec::new();
    let mut position = 0;
    while position < region.len() {
        let rest = &region[position..];
        let header = read_element_header(rest)
            .map_err(|_| InconsistentInitialization)?
            .ok_or(InconsistentInitialization)?;
        let end = match header.size {
            EbmlElementSize::Known(body_len) => usize::try_from(body_len)
                .ok()
                .and_then(|body_len| body_len.checked_add(header.header_len))
                .ok_or(InconsistentInitialization)?,
            EbmlElementSize::Unknown if unknown_size == AllowUnknownSize::OnlyLast => rest.len(),
            EbmlElementSize::Unknown => return Err(InconsistentInitialization),
        };
        let body = rest
            .get(header.header_len..end)
            .ok_or(InconsistentInitialization)?;
        children.push((header.id, body));
        position += end;
    }
    Ok(children)
}

#[cfg(test)]
#[path = "protection_tests.rs"]
mod tests;
