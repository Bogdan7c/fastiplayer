//! Распознавание сырого AC-3 / E-AC-3 по содержимому.
//!
//! Сила доказательства важна, потому что registry выбирает самый уверенный
//! factory, а равные — это ошибка неоднозначности:
//!
//! - кадр ровно в начале (после ID3v2) и цепочка корректных кадров подряд —
//!   **точное** доказательство: ни один другой контейнер так не начинается.
//!   ID3v2 впереди Symphonia считает признаком MP3 (`Signature`), поэтому
//!   AC-3 с ID3-тегом выигрывает только благодаря `Exact`;
//! - кадры после мусора (обрезанное начало, нули) — слабое доказательство:
//!   кадры AC-3 лежат подряд и внутри MKV/MP4 (lacing), поэтому такой файл
//!   принимается, только если расширение тоже говорит «ac3» (уровень
//!   `HintOnly`): любой настоящий контейнер с сигнатурой его перебивает.

use ac3_syncframe::{SyncframeSearch, locate_syncframe, parse_syncframe_header};

use crate::leading_tags::{LeadingTag, MAX_LEADING_TAGS, classify_leading_tag};

/// Сколько кадров подряд доказывают поток (если буфер не кончился раньше).
const REQUIRED_CHAINED_SYNCFRAMES: usize = 3;

/// Сколько мусора перед первым кадром допускается: один максимальный кадр
/// E-AC-3 (2048 слов). Так выглядит файл, обрезанный посреди кадра.
pub(crate) const MAX_GARBAGE_PREFIX_BYTES: usize = 4096;

/// Что содержимое говорит о сыром AC-3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrefixEvidence {
    /// Цепочка кадров с самого начала данных (после ID3v2).
    StreamAtStart,
    /// Цепочка кадров после мусора; без подтверждающего расширения не принимается.
    StreamAfterGarbage,
    /// Начало похоже на кадр, но заголовок оборван концом буфера.
    Truncated {
        /// Сколько байт есть.
        available_bytes: usize,
        /// Сколько нужно для решения.
        required_bytes: usize,
    },
    /// Не сырой AC-3.
    NotAc3,
}

/// Классифицирует bounded prefix источника.
pub(crate) fn classify_prefix(bytes: &[u8]) -> PrefixEvidence {
    let Some(data_start) = skip_leading_tags(bytes) else {
        return PrefixEvidence::NotAc3;
    };
    let data = &bytes[data_start..];
    match chain_at(data) {
        ChainEvidence::Proven => return PrefixEvidence::StreamAtStart,
        ChainEvidence::HeaderTruncated { required_bytes } => {
            return PrefixEvidence::Truncated {
                available_bytes: bytes.len(),
                required_bytes: data_start + required_bytes,
            };
        }
        ChainEvidence::Broken => {}
    }
    if has_chain_after_garbage(data) {
        PrefixEvidence::StreamAfterGarbage
    } else {
        PrefixEvidence::NotAc3
    }
}

/// Смещение после ID3v2-тегов; `None`, если тег не помещается в буфер
/// (доказать поток за ним нечем).
fn skip_leading_tags(bytes: &[u8]) -> Option<usize> {
    let mut offset = 0_usize;
    for _ in 0..MAX_LEADING_TAGS {
        match classify_leading_tag(&bytes[offset..]) {
            LeadingTag::Id3v2 { total_bytes } => {
                let end = usize::try_from(total_bytes).ok()?.checked_add(offset)?;
                if end > bytes.len() {
                    return None;
                }
                offset = end;
            }
            LeadingTag::NeedMoreData if offset == bytes.len() => return None,
            LeadingTag::NeedMoreData | LeadingTag::Absent => return Some(offset),
        }
    }
    Some(offset)
}

/// Итог проверки цепочки кадров с начала буфера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChainEvidence {
    /// Нужное число кадров подряд или цепочка дошла до конца буфера.
    Proven,
    /// Первый же заголовок оборван.
    HeaderTruncated { required_bytes: usize },
    /// На месте очередного кадра нет заголовка.
    Broken,
}

fn chain_at(bytes: &[u8]) -> ChainEvidence {
    let mut cursor = 0_usize;
    for chained in 0..REQUIRED_CHAINED_SYNCFRAMES {
        match parse_syncframe_header(&bytes[cursor..]) {
            Ok(header) => cursor += header.frame_length_bytes(),
            Err(error) if error.is_truncation() => {
                return match (chained, error) {
                    (0, ac3_syncframe::SyncframeHeaderError::Truncated { required_bytes, .. }) => {
                        ChainEvidence::HeaderTruncated { required_bytes }
                    }
                    // Оборван заголовок после целых кадров — то же «буфер кончился».
                    _ if chained > 0 => ChainEvidence::Proven,
                    _ => ChainEvidence::Broken,
                };
            }
            Err(_) => return ChainEvidence::Broken,
        }
        // Буфер (маленький файл целиком или окно sniff-а) кончился посреди
        // цепочки. Доказательством считается хотя бы один *целый* кадр.
        if cursor == bytes.len() || (cursor > bytes.len() && chained > 0) {
            return ChainEvidence::Proven;
        }
        if cursor > bytes.len() {
            return ChainEvidence::Broken;
        }
    }
    ChainEvidence::Proven
}

fn has_chain_after_garbage(bytes: &[u8]) -> bool {
    let search_end = bytes.len().min(MAX_GARBAGE_PREFIX_BYTES);
    let mut search_from = 0_usize;
    while search_from < search_end {
        match locate_syncframe(&bytes[search_from..]) {
            SyncframeSearch::Found { offset, .. } => {
                let candidate = search_from + offset;
                if candidate >= search_end {
                    return false;
                }
                if chain_at(&bytes[candidate..]) == ChainEvidence::Proven {
                    return true;
                }
                search_from = candidate + 1;
            }
            SyncframeSearch::NeedMoreData { .. } => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{PrefixEvidence, classify_prefix};

    /// Минимальный корректный заголовок AC-3: 48 кГц, 32 кбит/с (128 байт), bsid 8, 2/0.
    fn ac3_frame() -> Vec<u8> {
        let mut frame = vec![0_u8; 128];
        frame[..7].copy_from_slice(&[0x0B, 0x77, 0x00, 0x00, 0x00, 0x40, 0x40]);
        frame
    }

    fn stream(frames: usize) -> Vec<u8> {
        (0..frames).flat_map(|_| ac3_frame()).collect()
    }

    #[test]
    fn chained_frames_at_start_are_proven() {
        assert_eq!(classify_prefix(&stream(5)), PrefixEvidence::StreamAtStart);
    }

    #[test]
    fn single_whole_frame_file_is_proven() {
        assert_eq!(classify_prefix(&stream(1)), PrefixEvidence::StreamAtStart);
    }

    #[test]
    fn id3v2_prefix_is_skipped_by_declared_size() {
        let mut bytes = b"ID3\x04\x00\x00\x00\x00\x00\x14".to_vec();
        // Тело тега содержит ложное синхрослово — оно не должно сбить разбор.
        bytes.extend_from_slice(&[0x0B, 0x77, 0xFF, 0xFF]);
        bytes.extend_from_slice(&[0_u8; 16]);
        bytes.extend(stream(4));
        assert_eq!(classify_prefix(&bytes), PrefixEvidence::StreamAtStart);
    }

    #[test]
    fn id3v2_larger_than_buffer_proves_nothing() {
        let mut bytes = b"ID3\x04\x00\x00\x00\x00\x7F\x7F".to_vec();
        bytes.extend(stream(4));
        assert_eq!(classify_prefix(&bytes), PrefixEvidence::NotAc3);
    }

    #[test]
    fn garbage_before_chain_is_weak_evidence() {
        let mut bytes = vec![0x55_u8; 300];
        bytes.extend(stream(4));
        assert_eq!(classify_prefix(&bytes), PrefixEvidence::StreamAfterGarbage);
    }

    #[test]
    fn garbage_longer_than_one_frame_is_not_ac3() {
        let mut bytes = vec![0x55_u8; super::MAX_GARBAGE_PREFIX_BYTES + 1];
        bytes.extend(stream(4));
        assert_eq!(classify_prefix(&bytes), PrefixEvidence::NotAc3);
    }

    /// Одиночное синхрослово без продолжения цепочки — не поток.
    #[test]
    fn broken_chain_is_not_ac3() {
        let mut bytes = ac3_frame();
        bytes.extend_from_slice(&[0x11_u8; 400]);
        assert_eq!(classify_prefix(&bytes), PrefixEvidence::NotAc3);
    }

    #[test]
    fn truncated_first_header_asks_for_more_bytes() {
        assert!(matches!(
            classify_prefix(&[0x0B, 0x77, 0x00]),
            PrefixEvidence::Truncated {
                available_bytes: 3,
                ..
            }
        ));
    }

    #[test]
    fn other_containers_and_empty_input_are_not_ac3() {
        assert_eq!(classify_prefix(b"OggS\x00\x02"), PrefixEvidence::NotAc3);
        assert_eq!(classify_prefix(&[0x47; 376]), PrefixEvidence::NotAc3);
        assert_eq!(classify_prefix(&[]), PrefixEvidence::NotAc3);
    }
}
