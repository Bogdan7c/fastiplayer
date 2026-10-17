//! Минимальный MSB-first bit reader для полей заголовка.

use crate::SyncframeHeaderError;

/// Читает биты от старшего к младшему, как записаны поля A/52.
pub(crate) struct BitReader<'bytes> {
    bytes: &'bytes [u8],
    /// Позиция следующего непрочитанного бита от начала `bytes`.
    bit_position: usize,
}

impl<'bytes> BitReader<'bytes> {
    pub(crate) const fn new(bytes: &'bytes [u8]) -> Self {
        Self {
            bytes,
            bit_position: 0,
        }
    }

    /// Читает `bit_count` (1..=16) бит как беззнаковое число.
    ///
    /// При нехватке данных возвращает `Truncated` с точным числом байт,
    /// которое понадобится, — потоковый потребитель дочитает ровно столько.
    pub(crate) fn read_bits(&mut self, bit_count: usize) -> Result<u16, SyncframeHeaderError> {
        debug_assert!((1..=16).contains(&bit_count));
        let end_bit = self.bit_position + bit_count;
        let required_bytes = end_bit.div_ceil(8);
        if required_bytes > self.bytes.len() {
            return Err(SyncframeHeaderError::Truncated {
                available_bytes: self.bytes.len(),
                required_bytes,
            });
        }
        let mut value = 0_u16;
        for bit_index in self.bit_position..end_bit {
            let byte = self.bytes[bit_index / 8];
            let bit = (byte >> (7 - bit_index % 8)) & 1;
            value = (value << 1) | u16::from(bit);
        }
        self.bit_position = end_bit;
        Ok(value)
    }

    /// Читает один бит как флаг наличия поля.
    pub(crate) fn read_flag(&mut self) -> Result<bool, SyncframeHeaderError> {
        Ok(self.read_bits(1)? == 1)
    }

    /// Читает поле шириной до 8 бит.
    pub(crate) fn read_u8(&mut self, bit_count: usize) -> Result<u8, SyncframeHeaderError> {
        debug_assert!(bit_count <= 8);
        // Поле не шире 8 бит, поэтому значение всегда помещается в u8.
        Ok(u8::try_from(self.read_bits(bit_count)?).unwrap_or(u8::MAX))
    }

    /// Пропускает поле, значение которого не нужно demuxer-у.
    pub(crate) fn skip_bits(&mut self, bit_count: usize) -> Result<(), SyncframeHeaderError> {
        self.read_bits(bit_count).map(|_| ())
    }

    /// Сколько целых байт уже затронуто разбором.
    pub(crate) const fn consumed_bytes(&self) -> usize {
        self.bit_position.div_ceil(8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fields_across_byte_boundaries_msb_first() {
        let mut reader = BitReader::new(&[0b1010_1100, 0b0101_0011]);
        assert_eq!(reader.read_bits(3), Ok(0b101));
        assert_eq!(reader.read_bits(7), Ok(0b011_0001));
        assert_eq!(reader.read_flag(), Ok(false));
        assert_eq!(reader.read_bits(5), Ok(0b1_0011));
        assert_eq!(reader.consumed_bytes(), 2);
    }

    #[test]
    fn truncation_reports_exact_required_bytes_and_keeps_position() {
        let mut reader = BitReader::new(&[0xff]);
        assert_eq!(reader.read_bits(6), Ok(0b11_1111));
        assert_eq!(
            reader.read_bits(5),
            Err(SyncframeHeaderError::Truncated {
                available_bytes: 1,
                required_bytes: 2
            })
        );
        // Неудачное чтение не сдвигает позицию: оставшиеся 2 бита всё ещё доступны.
        assert_eq!(reader.read_bits(2), Ok(0b11));
    }
}
