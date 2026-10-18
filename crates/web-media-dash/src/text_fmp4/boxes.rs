//! Ограниченный обход боксов ISOBMFF и чтение примитивов big-endian.

use super::TextFmp4Error;

/// Бокс: тип и полезная нагрузка (без заголовка), плюс положение в буфере верхнего уровня.
#[derive(Debug, Clone, Copy)]
pub(super) struct Mp4Box<'data> {
    /// Четырёхсимвольный тип.
    pub(super) kind: [u8; 4],
    /// Содержимое без 8/16 байт заголовка.
    pub(super) payload: &'data [u8],
    /// Смещение начала бокса (заголовка) от начала исходного буфера.
    pub(super) start: usize,
    /// Смещение начала полезной нагрузки от начала исходного буфера.
    pub(super) payload_start: usize,
}

/// Итератор по соседним боксам одного уровня.
pub(super) struct BoxIter<'data> {
    data: &'data [u8],
    /// Позиция следующего бокса внутри `data`.
    position: usize,
    /// Смещение `data[0]` от начала исходного буфера (для абсолютных `start`).
    base_offset: usize,
    /// После ошибки итерация прекращается.
    failed: bool,
}

impl<'data> BoxIter<'data> {
    /// Обход боксов буфера, начинающегося в `base_offset` исходного буфера.
    pub(super) fn new(data: &'data [u8], base_offset: usize) -> Self {
        Self {
            data,
            position: 0,
            base_offset,
            failed: false,
        }
    }
}

impl<'data> Iterator for BoxIter<'data> {
    type Item = Result<Mp4Box<'data>, TextFmp4Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.position >= self.data.len() {
            return None;
        }
        let parsed = parse_box(self.data, self.position, self.base_offset);
        match parsed {
            Ok((found, next_position)) => {
                self.position = next_position;
                Some(Ok(found))
            }
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

/// Размер заголовка обычного бокса.
const COMPACT_HEADER_BYTES: usize = 8;
/// Размер заголовка бокса с 64-битным размером (`size == 1`).
const LARGE_HEADER_BYTES: usize = 16;

/// Разбирает бокс в `position`; возвращает бокс и позицию следующего.
fn parse_box(
    data: &[u8],
    position: usize,
    base_offset: usize,
) -> Result<(Mp4Box<'_>, usize), TextFmp4Error> {
    let rest = data.get(position..).ok_or(TextFmp4Error::Truncated)?;
    let mut reader = Reader::new(rest);
    let compact_size = reader.u32()?;
    let kind = reader.fourcc()?;
    let (header_bytes, total_size) = match compact_size {
        // `size == 0`: бокс тянется до конца буфера.
        0 => (COMPACT_HEADER_BYTES, rest.len()),
        1 => {
            let large = usize::try_from(reader.u64()?).map_err(|_| TextFmp4Error::Overflow)?;
            (LARGE_HEADER_BYTES, large)
        }
        compact => (
            COMPACT_HEADER_BYTES,
            usize::try_from(compact).map_err(|_| TextFmp4Error::Overflow)?,
        ),
    };
    if total_size < header_bytes {
        return Err(TextFmp4Error::InvalidBox);
    }
    let payload = rest
        .get(header_bytes..total_size)
        .ok_or(TextFmp4Error::Truncated)?;
    let start = base_offset
        .checked_add(position)
        .ok_or(TextFmp4Error::Overflow)?;
    let next_position = position
        .checked_add(total_size)
        .ok_or(TextFmp4Error::Overflow)?;
    let payload_start = start
        .checked_add(header_bytes)
        .ok_or(TextFmp4Error::Overflow)?;
    Ok((
        Mp4Box {
            kind,
            payload,
            start,
            payload_start,
        },
        next_position,
    ))
}

/// Первый дочерний бокс заданного типа.
pub(super) fn find_child<'data>(
    parent_payload: &'data [u8],
    kind: [u8; 4],
) -> Result<Option<Mp4Box<'data>>, TextFmp4Error> {
    for child in BoxIter::new(parent_payload, 0) {
        let child = child?;
        if child.kind == kind {
            return Ok(Some(child));
        }
    }
    Ok(None)
}

/// Курсор чтения big-endian полей с проверкой границ.
pub(super) struct Reader<'data> {
    data: &'data [u8],
}

impl<'data> Reader<'data> {
    pub(super) fn new(data: &'data [u8]) -> Self {
        Self { data }
    }

    /// Сколько байт осталось.
    pub(super) fn remaining(&self) -> usize {
        self.data.len()
    }

    /// Забирает `count` байт.
    pub(super) fn take(&mut self, count: usize) -> Result<&'data [u8], TextFmp4Error> {
        if self.data.len() < count {
            return Err(TextFmp4Error::Truncated);
        }
        let (head, tail) = self.data.split_at(count);
        self.data = tail;
        Ok(head)
    }

    pub(super) fn skip(&mut self, count: usize) -> Result<(), TextFmp4Error> {
        self.take(count).map(|_| ())
    }

    pub(super) fn u8(&mut self) -> Result<u8, TextFmp4Error> {
        Ok(self.take(1)?[0])
    }

    pub(super) fn u32(&mut self) -> Result<u32, TextFmp4Error> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(super) fn i32(&mut self) -> Result<i32, TextFmp4Error> {
        let bytes = self.take(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(super) fn u64(&mut self) -> Result<u64, TextFmp4Error> {
        let bytes = self.take(8)?;
        let mut array = [0_u8; 8];
        array.copy_from_slice(bytes);
        Ok(u64::from_be_bytes(array))
    }

    pub(super) fn fourcc(&mut self) -> Result<[u8; 4], TextFmp4Error> {
        let bytes = self.take(4)?;
        Ok([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    /// Заголовок FullBox: версия и 24-битные флаги.
    pub(super) fn full_box_header(&mut self) -> Result<(u8, u32), TextFmp4Error> {
        let version = self.u8()?;
        let flag_bytes = self.take(3)?;
        let flags = u32::from(flag_bytes[0]) << 16
            | u32::from(flag_bytes[1]) << 8
            | u32::from(flag_bytes[2]);
        Ok((version, flags))
    }
}
