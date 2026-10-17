//! Ограниченный побитовый читатель (MSB-first, A/52 §5.2).
//!
//! Читатель знает две границы: конец буфера и «предел» — позицию, дальше
//! которой текущая часть кадра читать не имеет права (например, аудиоблоки
//! не должны залезать в `auxdata`/`errorcheck`). Выход за предел — не паника,
//! а [`ReadPastLimit`], который вызывающий код превращает в типизированную
//! ошибку с указанием секции кадра.

/// Самое широкое поле, которое читается одним вызовом: 16 бит
/// (мантисса bap = 15, `crc1`/`crc2`, синхрослово).
const MAX_FIELD_BITS: u32 = 16;

/// Сколько байт окна нужно, чтобы вытащить до 16 бит с любого битового смещения.
const WINDOW_BYTES: usize = 3;

/// Попытка прочитать дальше разрешённой границы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadPastLimit;

/// Читатель бит поверх неизменяемого среза байт.
#[derive(Debug, Clone)]
pub(crate) struct BitReader<'a> {
    /// Байты кадра (или его начала).
    bytes: &'a [u8],
    /// Текущая позиция в битах от начала `bytes`.
    position_bits: usize,
    /// Первая позиция, которую читать уже нельзя.
    limit_bits: usize,
}

impl<'a> BitReader<'a> {
    /// Читатель всего среза: предел совпадает с концом буфера.
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            position_bits: 0,
            limit_bits: bytes.len().saturating_mul(8),
        }
    }

    /// Текущая позиция в битах от начала буфера.
    pub(crate) const fn position_bits(&self) -> usize {
        self.position_bits
    }

    /// Сужает предел чтения (никогда не расширяет за конец буфера).
    pub(crate) fn restrict_limit(&mut self, limit_bits: usize) {
        self.limit_bits = limit_bits.min(self.bytes.len().saturating_mul(8));
    }

    /// Читает `bit_count` (0–16) бит как беззнаковое число, старший бит первым.
    pub(crate) fn read_bits(&mut self, bit_count: u32) -> Result<u16, ReadPastLimit> {
        if bit_count > MAX_FIELD_BITS {
            // Внутренний инвариант: все поля AC-3 не шире 16 бит. Если кто-то
            // ошибся в вызове, это не повод для паники — просто ошибка чтения.
            return Err(ReadPastLimit);
        }
        if bit_count == 0 {
            return Ok(0);
        }
        let width = bit_count as usize;
        let end_bits = self.position_bits.checked_add(width).ok_or(ReadPastLimit)?;
        if end_bits > self.limit_bits {
            return Err(ReadPastLimit);
        }
        let first_byte = self.position_bits / 8;
        let bit_offset = (self.position_bits % 8) as u32;
        // Собираем 24-битное окно; байты за концом буфера считаются нулями,
        // но до них мы не дойдём — предел уже проверен выше.
        let mut window = 0u32;
        for offset in 0..WINDOW_BYTES {
            let byte = self
                .bytes
                .get(first_byte + offset)
                .copied()
                .unwrap_or_default();
            window = (window << 8) | u32::from(byte);
        }
        let window_bits = (WINDOW_BYTES as u32) * 8;
        let shift = window_bits - bit_offset - bit_count;
        let mask = (1u32 << bit_count) - 1;
        self.position_bits = end_bits;
        Ok(((window >> shift) & mask) as u16)
    }

    /// Читает поле шириной до 8 бит.
    pub(crate) fn read_u8(&mut self, bit_count: u32) -> Result<u8, ReadPastLimit> {
        if bit_count > 8 {
            return Err(ReadPastLimit);
        }
        Ok(self.read_bits(bit_count)? as u8)
    }

    /// Читает один бит как флаг.
    pub(crate) fn read_flag(&mut self) -> Result<bool, ReadPastLimit> {
        Ok(self.read_bits(1)? == 1)
    }

    /// Пропускает `bit_count` бит (например, `addbsi` или `skipfld`).
    pub(crate) fn skip_bits(&mut self, bit_count: usize) -> Result<(), ReadPastLimit> {
        let end_bits = self
            .position_bits
            .checked_add(bit_count)
            .ok_or(ReadPastLimit)?;
        if end_bits > self.limit_bits {
            return Err(ReadPastLimit);
        }
        self.position_bits = end_bits;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{BitReader, ReadPastLimit};

    #[test]
    fn reads_fields_msb_first_across_byte_boundaries() {
        // 0b1011_0111 0b0111_0000 0b1111_1111
        let bytes = [0xB7, 0x70, 0xFF];
        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.read_bits(3), Ok(0b101));
        assert_eq!(reader.read_bits(7), Ok(0b101_1101));
        assert_eq!(reader.read_bits(14), Ok(0b11_0000_1111_1111));
        assert_eq!(reader.position_bits(), 24);
        assert_eq!(reader.read_bits(1), Err(ReadPastLimit));
    }

    #[test]
    fn sixteen_bit_field_at_odd_offset_is_exact() {
        let bytes = [0x0B, 0x77, 0xAB, 0xCD];
        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.read_bits(4), Ok(0x0));
        assert_eq!(reader.read_bits(16), Ok(0xB77A));
        assert_eq!(reader.read_bits(12), Ok(0xBCD));
    }

    #[test]
    fn limit_stops_reading_without_moving_position() {
        let bytes = [0xFF, 0xFF];
        let mut reader = BitReader::new(&bytes);
        reader.restrict_limit(5);
        assert_eq!(reader.read_bits(4), Ok(0xF));
        assert_eq!(reader.read_bits(2), Err(ReadPastLimit));
        assert_eq!(reader.position_bits(), 4);
        assert_eq!(reader.skip_bits(2), Err(ReadPastLimit));
        assert_eq!(reader.skip_bits(1), Ok(()));
        assert_eq!(reader.read_bits(1), Err(ReadPastLimit));
    }

    #[test]
    fn oversized_field_request_is_an_error_not_a_panic() {
        let bytes = [0u8; 8];
        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.read_bits(17), Err(ReadPastLimit));
        assert_eq!(reader.read_u8(9), Err(ReadPastLimit));
        assert_eq!(reader.position_bits(), 0);
    }

    #[test]
    fn limit_cannot_exceed_buffer() {
        let bytes = [0u8; 1];
        let mut reader = BitReader::new(&bytes);
        reader.restrict_limit(1000);
        assert_eq!(reader.skip_bits(9), Err(ReadPastLimit));
        assert_eq!(reader.skip_bits(8), Ok(()));
    }
}
