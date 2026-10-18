//! Нарезка потока на сегменты PGS: блок Matroska и файл `.sup`.
//!
//! Сегмент: `[тип u8][размер u16 BE][payload]`. В Matroska блок содержит один или несколько
//! голых сегментов; в файле `.sup` перед каждым сегментом стоит префикс из 10 байт
//! (`'P' 'G'`, PTS u32, DTS u32, оба в тиках 90 кГц) — вместе с 3-байтным заголовком самого
//! сегмента это и есть известные «13 байт заголовка записи».

use subtitle_core::SubtitleTime;

/// Тип сегмента.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SegmentKind {
    /// 0x16 — Presentation Composition Segment: что показать.
    PresentationComposition,
    /// 0x17 — Window Definition Segment: окна экрана.
    WindowDefinition,
    /// 0x14 — Palette Definition Segment: палитры.
    PaletteDefinition,
    /// 0x15 — Object Definition Segment: картинки (возможно, по фрагментам).
    ObjectDefinition,
    /// 0x80 — конец набора отображения.
    EndOfDisplaySet,
    /// Любой другой байт типа (пропускается с подсчётом).
    Unknown(u8),
}

impl SegmentKind {
    pub const fn from_byte(byte: u8) -> Self {
        match byte {
            0x16 => Self::PresentationComposition,
            0x17 => Self::WindowDefinition,
            0x14 => Self::PaletteDefinition,
            0x15 => Self::ObjectDefinition,
            0x80 => Self::EndOfDisplaySet,
            other => Self::Unknown(other),
        }
    }

    pub const fn to_byte(self) -> u8 {
        match self {
            Self::PresentationComposition => 0x16,
            Self::WindowDefinition => 0x17,
            Self::PaletteDefinition => 0x14,
            Self::ObjectDefinition => 0x15,
            Self::EndOfDisplaySet => 0x80,
            Self::Unknown(byte) => byte,
        }
    }
}

/// Один сегмент: тип и ссылка на payload (без 3-байтного заголовка сегмента).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment<'a> {
    pub kind: SegmentKind,
    pub payload: &'a [u8],
}

/// Ошибка нарезки: данные кончились посреди сегмента.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SegmentFramingError {
    /// Остался хвост короче 3-байтного заголовка сегмента.
    #[error("trailing {remaining} bytes are shorter than a segment header")]
    TruncatedHeader { remaining: usize },
    /// Заголовок объявил payload больше, чем осталось данных.
    #[error("segment {kind:?} declares {declared} payload bytes but only {available} remain")]
    TruncatedPayload {
        kind: SegmentKind,
        declared: usize,
        available: usize,
    },
}

/// Результат нарезки блока: целые сегменты и (если блок оборван) причина остановки.
/// Сегменты до обрыва остаются пригодными к использованию.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitBlock<'a> {
    pub segments: Vec<Segment<'a>>,
    pub framing_error: Option<SegmentFramingError>,
}

/// Размер заголовка сегмента: тип + размер payload.
const SEGMENT_HEADER_LEN: usize = 3;
/// Размер префикса записи `.sup` перед заголовком сегмента: `PG` + PTS + DTS.
const SUP_HEADER_LEN: usize = 10;
/// Сигнатура записи `.sup`.
const SUP_MAGIC: [u8; 2] = *b"PG";
/// Тактовая частота PTS/DTS в `.sup`.
const SUP_CLOCK_HZ: u64 = 90_000;

/// Читает один сегмент с начала `data`; возвращает его и остаток данных.
fn read_segment(data: &[u8]) -> Result<(Segment<'_>, &[u8]), SegmentFramingError> {
    if data.len() < SEGMENT_HEADER_LEN {
        return Err(SegmentFramingError::TruncatedHeader {
            remaining: data.len(),
        });
    }
    let kind = SegmentKind::from_byte(data[0]);
    let declared = usize::from(u16::from_be_bytes([data[1], data[2]]));
    let body = &data[SEGMENT_HEADER_LEN..];
    if body.len() < declared {
        return Err(SegmentFramingError::TruncatedPayload {
            kind,
            declared,
            available: body.len(),
        });
    }
    let (payload, rest) = body.split_at(declared);
    Ok((Segment { kind, payload }, rest))
}

/// Режет payload одного блока Matroska на сегменты.
///
/// Терпимость: если блок начинается с префикса `.sup` (`PG` + PTS + DTS, 10 байт) — он
/// пропускается (так выглядят потоки, кое-как перепакованные из `.sup`). Байт `'P'` (0x50)
/// не является допустимым типом сегмента, поэтому путаницы нет.
pub fn split_block(data: &[u8]) -> SplitBlock<'_> {
    let mut rest = if data.len() >= SUP_HEADER_LEN && data.starts_with(&SUP_MAGIC) {
        &data[SUP_HEADER_LEN..]
    } else {
        data
    };
    let mut segments = Vec::new();
    while !rest.is_empty() {
        match read_segment(rest) {
            Ok((segment, tail)) => {
                segments.push(segment);
                rest = tail;
            }
            Err(error) => {
                return SplitBlock {
                    segments,
                    framing_error: Some(error),
                };
            }
        }
    }
    SplitBlock {
        segments,
        framing_error: None,
    }
}

/// Сегмент из файла `.sup` вместе с его временем показа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupSegment<'a> {
    /// PTS записи, переведённый из тиков 90 кГц.
    pub presentation_time: SubtitleTime,
    pub segment: Segment<'a>,
}

/// Ошибка чтения файла `.sup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SupError {
    /// Запись не начинается с сигнатуры `PG`.
    #[error("bad .sup magic at byte offset {offset}")]
    BadMagic { offset: usize },
    /// Хвост файла короче префикса записи.
    #[error("truncated .sup record header at byte offset {offset}")]
    TruncatedHeader { offset: usize },
    /// Внутри записи оборван сегмент.
    #[error("truncated .sup segment at byte offset {offset}: {error}")]
    TruncatedSegment {
        offset: usize,
        error: SegmentFramingError,
    },
}

/// Переводит тики 90 кГц в микросекунды (округление вниз; 45000 тиков = ровно 0.5 с).
pub fn ticks_90khz_to_time(ticks: u32) -> SubtitleTime {
    let micros = u64::from(ticks) * 1_000_000 / SUP_CLOCK_HZ;
    // Максимум u32 тиков ≈ 47 721 858 844 мкс — заведомо помещается в i64.
    SubtitleTime::from_micros(i64::try_from(micros).unwrap_or(i64::MAX))
}

/// Читает файл `.sup` целиком: записи `[заголовок 13 байт][сегмент]` до конца данных.
pub fn read_sup(data: &[u8]) -> Result<Vec<SupSegment<'_>>, SupError> {
    let mut offset = 0usize;
    let mut records = Vec::new();
    while offset < data.len() {
        let rest = &data[offset..];
        if rest.len() < SUP_HEADER_LEN {
            return Err(SupError::TruncatedHeader { offset });
        }
        if !rest.starts_with(&SUP_MAGIC) {
            return Err(SupError::BadMagic { offset });
        }
        let ticks = u32::from_be_bytes([rest[2], rest[3], rest[4], rest[5]]);
        // DTS (байты 6..10) для показа не нужен.
        let body = &rest[SUP_HEADER_LEN..];
        let (segment, _) = read_segment(body).map_err(|error| SupError::TruncatedSegment {
            offset: offset + SUP_HEADER_LEN,
            error,
        })?;
        records.push(SupSegment {
            presentation_time: ticks_90khz_to_time(ticks),
            segment,
        });
        offset += SUP_HEADER_LEN + SEGMENT_HEADER_LEN + segment.payload.len();
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_segment(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![kind];
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn splits_several_segments_in_order() {
        let mut block = raw_segment(0x16, &[1, 2, 3]);
        block.extend(raw_segment(0x80, &[]));
        let split = split_block(&block);
        assert_eq!(split.framing_error, None);
        assert_eq!(
            split.segments,
            [
                Segment {
                    kind: SegmentKind::PresentationComposition,
                    payload: &[1, 2, 3]
                },
                Segment {
                    kind: SegmentKind::EndOfDisplaySet,
                    payload: &[]
                },
            ]
        );
    }

    #[test]
    fn skips_leading_sup_header() {
        let mut block = b"PG".to_vec();
        block.extend_from_slice(&[0; 8]);
        block.extend(raw_segment(0x14, &[9]));
        let split = split_block(&block);
        assert_eq!(split.framing_error, None);
        assert_eq!(split.segments.len(), 1);
        assert_eq!(split.segments[0].kind, SegmentKind::PaletteDefinition);
        assert_eq!(split.segments[0].payload, [9]);
    }

    #[test]
    fn truncated_payload_keeps_earlier_segments_and_reports_error() {
        let mut block = raw_segment(0x80, &[]);
        block.extend_from_slice(&[0x15, 0x00, 0x10, 1, 2]);
        let split = split_block(&block);
        assert_eq!(split.segments.len(), 1);
        assert_eq!(
            split.framing_error,
            Some(SegmentFramingError::TruncatedPayload {
                kind: SegmentKind::ObjectDefinition,
                declared: 16,
                available: 2
            })
        );
    }

    #[test]
    fn short_tail_is_a_truncated_header_and_unknown_kind_is_preserved() {
        let mut block = raw_segment(0x7F, &[5]);
        block.extend_from_slice(&[0x16, 0x00]);
        let split = split_block(&block);
        assert_eq!(split.segments[0].kind, SegmentKind::Unknown(0x7F));
        assert_eq!(
            split.framing_error,
            Some(SegmentFramingError::TruncatedHeader { remaining: 2 })
        );
        assert_eq!(split_block(&[]).segments.len(), 0);
    }

    fn sup_record(ticks: u32, kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = b"PG".to_vec();
        out.extend_from_slice(&ticks.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend(raw_segment(kind, payload));
        out
    }

    #[test]
    fn reads_sup_records_with_90khz_time() {
        let mut file = sup_record(45_000, 0x16, &[1, 2]);
        file.extend(sup_record(90_000, 0x80, &[]));
        let records = read_sup(&file).expect("valid sup");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].presentation_time, SubtitleTime::from_millis(500));
        assert_eq!(records[0].segment.payload, [1, 2]);
        assert_eq!(
            records[1].presentation_time,
            SubtitleTime::from_millis(1_000)
        );
        assert_eq!(records[1].segment.kind, SegmentKind::EndOfDisplaySet);
    }

    #[test]
    fn sup_errors_report_offsets() {
        let mut file = sup_record(0, 0x80, &[]);
        let second = file.len();
        file.extend_from_slice(b"XX0123456789012");
        assert_eq!(read_sup(&file), Err(SupError::BadMagic { offset: second }));
        let mut short = sup_record(0, 0x80, &[]);
        short.extend_from_slice(b"PG12");
        assert_eq!(
            read_sup(&short),
            Err(SupError::TruncatedHeader { offset: 13 })
        );
        let mut cut = sup_record(0, 0x15, &[1, 2, 3, 4]);
        cut.truncate(cut.len() - 2);
        assert!(matches!(
            read_sup(&cut),
            Err(SupError::TruncatedSegment { offset: 10, .. })
        ));
    }

    #[test]
    fn tick_conversion_rounds_down() {
        assert_eq!(ticks_90khz_to_time(1).as_micros(), 11);
        assert_eq!(ticks_90khz_to_time(u32::MAX).as_micros(), 47_721_858_833);
    }
}
