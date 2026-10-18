//! Минимальный безопасный читатель big-endian полей из среза байт.

use crate::error::SegmentParseError;
use crate::segment::SegmentKind;

/// Курсор по байтам сегмента. Любое чтение за концом — типизированная ошибка, не паника.
pub(crate) struct ByteReader<'a> {
    bytes: &'a [u8],
    position: usize,
    segment: SegmentKind,
}

impl<'a> ByteReader<'a> {
    pub(crate) fn new(segment: SegmentKind, bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            position: 0,
            segment,
        }
    }

    /// Сколько байт ещё не прочитано.
    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    /// Читает ровно `count` байт или сообщает, сколько не хватило.
    pub(crate) fn take(&mut self, count: usize) -> Result<&'a [u8], SegmentParseError> {
        let available = self.remaining();
        if available < count {
            return Err(SegmentParseError::Truncated {
                segment: self.segment,
                needed: count,
                available,
            });
        }
        let slice = &self.bytes[self.position..self.position + count];
        self.position += count;
        Ok(slice)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, SegmentParseError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, SegmentParseError> {
        let raw = self.take(2)?;
        Ok(u16::from_be_bytes([raw[0], raw[1]]))
    }

    pub(crate) fn u24(&mut self) -> Result<u32, SegmentParseError> {
        let raw = self.take(3)?;
        Ok(u32::from_be_bytes([0, raw[0], raw[1], raw[2]]))
    }

    /// Все непрочитанные байты (курсор уходит в конец).
    pub(crate) fn rest(&mut self) -> &'a [u8] {
        let slice = &self.bytes[self.position..];
        self.position = self.bytes.len();
        slice
    }
}
