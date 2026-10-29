//! Заголовки элементов EBML (основа Matroska/WebM, RFC 8794).
//!
//! Элемент — `[ID][размер][тело]`. ID и размер — числа переменной длины: число
//! ведущих нулевых битов первого байта задаёт длину. ID хранится вместе с битом-
//! маркером (так их пишут в спецификации: `Cluster` = `0x1F43B675`). Размер из
//! одних единиц означает «неизвестен» — так пишут потоки без конца.

use super::FramingError;

/// Максимальная длина ID элемента в байтах (RFC 8794, `EBMLMaxIDLength` WebM = 4).
const MAX_ID_BYTES: usize = 4;
/// Максимальная длина поля размера в байтах.
const MAX_SIZE_BYTES: usize = 8;
/// Поле размера «неизвестен» длиной 8 байтов: маркер `0x01` и семь байтов единиц.
pub(crate) const UNKNOWN_SIZE_FIELD: [u8; MAX_SIZE_BYTES] =
    [0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
/// Наибольший размер, который помещается в 8-байтовое поле и не совпадает с «неизвестен».
const MAX_KNOWN_SIZE_IN_FIELD: u64 = (1 << 56) - 2;

/// Размер элемента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EbmlElementSize {
    /// Размер тела в байтах.
    Known(u64),
    /// Размер не указан: тело кончается там, где начинается элемент верхнего уровня.
    Unknown,
}

/// Заголовок элемента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EbmlElementHeader {
    /// ID вместе с битом-маркером.
    pub(crate) id: u32,
    /// Длина ID + поля размера.
    pub(crate) header_len: usize,
    /// Размер тела.
    pub(crate) size: EbmlElementSize,
}

impl EbmlElementHeader {
    /// Полная длина элемента, если размер известен.
    pub(crate) fn total_len(&self) -> Option<u64> {
        match self.size {
            EbmlElementSize::Known(body_len) => body_len.checked_add(self.header_len as u64),
            EbmlElementSize::Unknown => None,
        }
    }
}

/// Читает заголовок элемента в начале `bytes`; `None` — байтов ещё мало.
pub(crate) fn read_element_header(bytes: &[u8]) -> Result<Option<EbmlElementHeader>, FramingError> {
    let Some(&first_id_byte) = bytes.first() else {
        return Ok(None);
    };
    let id_len = vint_length(first_id_byte, MAX_ID_BYTES)?;
    let Some(id_bytes) = bytes.get(..id_len) else {
        return Ok(None);
    };
    let id = id_bytes
        .iter()
        .fold(0_u32, |value, byte| (value << 8) | u32::from(*byte));

    let Some(&first_size_byte) = bytes.get(id_len) else {
        return Ok(None);
    };
    let size_len = vint_length(first_size_byte, MAX_SIZE_BYTES)?;
    let Some(size_bytes) = bytes.get(id_len..id_len + size_len) else {
        return Ok(None);
    };
    Ok(Some(EbmlElementHeader {
        id,
        header_len: id_len + size_len,
        size: decode_size(size_bytes),
    }))
}

/// Длина числа переменной длины по первому байту.
fn vint_length(first_byte: u8, max_len: usize) -> Result<usize, FramingError> {
    // Длина = число ведущих нулей + 1; нулевой первый байт длину не задаёт.
    let len = first_byte.leading_zeros() as usize + 1;
    if first_byte == 0 || len > max_len {
        return Err(FramingError::InvalidEbmlNumber);
    }
    Ok(len)
}

/// Значение поля размера без бита-маркера; все единицы — «неизвестен».
fn decode_size(size_bytes: &[u8]) -> EbmlElementSize {
    let len = size_bytes.len();
    // Бит-маркер стоит в первом байте на позиции `8 - len`; данные — биты ниже него.
    let first_data_bits = u64::from(size_bytes[0]) & ((1_u64 << (8 - len)) - 1);
    let value = size_bytes[1..].iter().fold(first_data_bits, |value, byte| {
        (value << 8) | u64::from(*byte)
    });
    let all_ones = (1_u64 << (7 * len)) - 1;
    if value == all_ones {
        EbmlElementSize::Unknown
    } else {
        EbmlElementSize::Known(value)
    }
}

/// Поле известного размера фиксированной длины 8 байтов (маркер `0x01`).
///
/// Одна длина для всех упрощает переписывание заголовков; `None` — размер
/// не помещается (такой сегмент всё равно отвергнут пределами раньше).
pub(crate) fn encode_known_size_field(body_len: u64) -> Option<[u8; MAX_SIZE_BYTES]> {
    if body_len > MAX_KNOWN_SIZE_IN_FIELD {
        return None;
    }
    let mut field = body_len.to_be_bytes();
    field[0] = 0x01;
    Some(field)
}

/// ID элемента в байтах (без ведущих нулевых байтов).
pub(crate) fn encode_id(id: u32) -> Vec<u8> {
    let bytes = id.to_be_bytes();
    let first_significant = bytes.iter().position(|byte| *byte != 0).unwrap_or(3);
    bytes[first_significant..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::{
        EbmlElementSize, UNKNOWN_SIZE_FIELD, encode_id, encode_known_size_field,
        read_element_header,
    };
    use crate::framing::FramingError;

    #[test]
    fn reads_four_byte_id_and_one_byte_size() {
        let header = read_element_header(&[0x1A, 0x45, 0xDF, 0xA3, 0x9F, 0x00])
            .expect("valid")
            .expect("complete");
        assert_eq!(header.id, 0x1A45_DFA3);
        assert_eq!(header.header_len, 5);
        assert_eq!(header.size, EbmlElementSize::Known(0x1F));
        assert_eq!(header.total_len(), Some(0x1F + 5));
    }

    #[test]
    fn eight_byte_all_ones_size_is_unknown() {
        let mut bytes = vec![0x18, 0x53, 0x80, 0x67];
        bytes.extend_from_slice(&UNKNOWN_SIZE_FIELD);
        let header = read_element_header(&bytes)
            .expect("valid")
            .expect("complete");
        assert_eq!(header.size, EbmlElementSize::Unknown);
        assert_eq!(header.total_len(), None);
        let one_byte_unknown = read_element_header(&[0xA3, 0xFF])
            .expect("valid")
            .expect("complete");
        assert_eq!(one_byte_unknown.size, EbmlElementSize::Unknown);
    }

    #[test]
    fn short_input_waits_and_zero_byte_is_invalid() {
        assert_eq!(read_element_header(&[0x1A, 0x45]), Ok(None));
        assert_eq!(read_element_header(&[0x1F, 0x43, 0xB6, 0x75]), Ok(None));
        assert_eq!(
            read_element_header(&[0x00, 0x81]),
            Err(FramingError::InvalidEbmlNumber)
        );
        // ID длиннее 4 байтов запрещён.
        assert_eq!(
            read_element_header(&[0x08, 0, 0, 0, 0, 0x81]),
            Err(FramingError::InvalidEbmlNumber)
        );
    }

    #[test]
    fn known_size_field_round_trips_and_rejects_reserved_value() {
        let field = encode_known_size_field(7141).expect("fits");
        let mut bytes = encode_id(0x1F43_B675);
        bytes.extend_from_slice(&field);
        let header = read_element_header(&bytes)
            .expect("valid")
            .expect("complete");
        assert_eq!(header.id, 0x1F43_B675);
        assert_eq!(header.size, EbmlElementSize::Known(7141));
        assert_eq!(encode_known_size_field(1 << 56), None);
        assert_eq!(encode_id(0xA3), vec![0xA3]);
    }
}
