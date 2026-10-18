//! ODS (0x15): картинка объекта, возможно разбитая на несколько сегментов-фрагментов.

use crate::error::SegmentParseError;
use crate::reader::ByteReader;
use crate::segment::SegmentKind;

/// Положение фрагмента в последовательности.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FragmentPosition {
    /// 0xC0 — единственный фрагмент (и первый, и последний).
    Only,
    /// 0x80 — первый из нескольких.
    First,
    /// 0x00 — промежуточный.
    Middle,
    /// 0x40 — последний из нескольких.
    Last,
}

impl FragmentPosition {
    fn from_flags(flags: u8) -> Self {
        match (flags & 0x80 != 0, flags & 0x40 != 0) {
            (true, true) => Self::Only,
            (true, false) => Self::First,
            (false, true) => Self::Last,
            (false, false) => Self::Middle,
        }
    }

    /// Начинает ли фрагмент новый объект (несёт размеры и длину данных).
    pub const fn starts_object(self) -> bool {
        matches!(self, Self::Only | Self::First)
    }

    /// Завершает ли фрагмент объект.
    pub const fn ends_object(self) -> bool {
        matches!(self, Self::Only | Self::Last)
    }
}

/// Заголовок объекта, присутствующий только в первом фрагменте.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectHeader {
    /// Объявленная длина данных объекта (24 бита); включает 4 байта ширины и высоты.
    pub declared_data_length: u32,
    pub width: u16,
    pub height: u16,
}

impl ObjectHeader {
    /// Сколько байт RLE ожидается: объявленная длина без 4 байт `width`/`height`.
    pub fn expected_rle_len(&self) -> usize {
        usize::try_from(self.declared_data_length.saturating_sub(4)).unwrap_or(usize::MAX)
    }
}

/// Один фрагмент ODS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectFragment<'a> {
    pub object_id: u16,
    pub version: u8,
    pub position: FragmentPosition,
    /// `Some` только у фрагментов, начинающих объект.
    pub header: Option<ObjectHeader>,
    /// RLE-байты этого фрагмента.
    pub rle_bytes: &'a [u8],
}

/// Разбирает payload ODS. Нулевой размер картинки в первом фрагменте — ошибка.
pub fn parse_ods(payload: &[u8]) -> Result<ObjectFragment<'_>, SegmentParseError> {
    let mut reader = ByteReader::new(SegmentKind::ObjectDefinition, payload);
    let object_id = reader.u16()?;
    let version = reader.u8()?;
    let position = FragmentPosition::from_flags(reader.u8()?);
    let header = if position.starts_object() {
        let declared_data_length = reader.u24()?;
        let width = reader.u16()?;
        let height = reader.u16()?;
        if width == 0 || height == 0 {
            return Err(SegmentParseError::ZeroObjectSize);
        }
        Some(ObjectHeader {
            declared_data_length,
            width,
            height,
        })
    } else {
        None
    };
    Ok(ObjectFragment {
        object_id,
        version,
        position,
        header,
        rle_bytes: reader.rest(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_fragment_carries_header_and_rle() {
        let payload = [0, 9, 3, 0xC0, 0, 0, 7, 0, 4, 0, 2, 0xAA, 0xBB, 0xCC];
        let fragment = parse_ods(&payload).expect("valid");
        assert_eq!(fragment.object_id, 9);
        assert_eq!(fragment.version, 3);
        assert_eq!(fragment.position, FragmentPosition::Only);
        assert_eq!(
            fragment.header,
            Some(ObjectHeader {
                declared_data_length: 7,
                width: 4,
                height: 2
            })
        );
        assert_eq!(fragment.header.map(|h| h.expected_rle_len()), Some(3));
        assert_eq!(fragment.rle_bytes, [0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn continuation_fragments_have_no_header() {
        let middle = parse_ods(&[0, 9, 0, 0x00, 1, 2]).expect("valid");
        assert_eq!(middle.position, FragmentPosition::Middle);
        assert_eq!(middle.header, None);
        assert_eq!(middle.rle_bytes, [1, 2]);
        let last = parse_ods(&[0, 9, 0, 0x40, 3]).expect("valid");
        assert_eq!(last.position, FragmentPosition::Last);
        assert!(last.position.ends_object() && !last.position.starts_object());
        let first = parse_ods(&[0, 9, 0, 0x80, 0, 0, 5, 0, 1, 0, 1, 7]).expect("valid");
        assert!(first.position.starts_object() && !first.position.ends_object());
    }

    #[test]
    fn broken_first_fragments_are_errors() {
        assert!(matches!(
            parse_ods(&[0, 9, 0, 0xC0, 0, 0]),
            Err(SegmentParseError::Truncated { .. })
        ));
        assert_eq!(
            parse_ods(&[0, 9, 0, 0xC0, 0, 0, 4, 0, 0, 0, 1]),
            Err(SegmentParseError::ZeroObjectSize)
        );
        assert!(parse_ods(&[0, 9]).is_err());
    }
}
