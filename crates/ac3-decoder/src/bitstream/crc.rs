//! CRC-16 syncframe-а AC-3 (A/52 §7.10.1).
//!
//! Порождающий полином `x^16 + x^15 + x^2 + 1` (0x8005), регистр сдвига
//! обнуляется перед проверкой, биты подаются в порядке передачи (MSB-first),
//! без отражения и без финального XOR. Проверка устроена так, как описывает
//! спецификация: после прогона защищённой области через регистр в нём должен
//! остаться ноль (кодер подбирает `crc1`/`crc2` именно под это условие, поэтому
//! сравнивать «посчитанную CRC» с полем `crc1` нельзя — §7.10.1, последний абзац).

use crate::error::CrcRegion;
use crate::tables::frame_size::five_eighths_frame_bytes;

/// Полином без старшего члена `x^16`: `x^15 + x^2 + 1`.
const CRC16_POLYNOMIAL: u16 = 0x8005;

/// Таблица побайтового сдвига регистра, построенная на этапе компиляции.
const CRC16_TABLE: [u16; 256] = build_crc16_table();

/// Длина синхрослова, которое ни одна CRC не покрывает.
const SYNC_WORD_BYTES: usize = 2;

const fn build_crc16_table() -> [u16; 256] {
    let mut table = [0u16; 256];
    let mut byte = 0usize;
    while byte < 256 {
        // Сдвигаем байт через регистр бит за битом (эквивалент LFSR из §7.10.1).
        let mut register = (byte as u16) << 8;
        let mut bit = 0;
        while bit < 8 {
            register = if register & 0x8000 != 0 {
                (register << 1) ^ CRC16_POLYNOMIAL
            } else {
                register << 1
            };
            bit += 1;
        }
        table[byte] = register;
        byte += 1;
    }
    table
}

/// Прогоняет байты через регистр, начиная с нулевого состояния.
pub(crate) fn crc16_register(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0u16, |register, &byte| {
        let index = usize::from((register >> 8) as u8 ^ byte);
        (register << 8) ^ CRC16_TABLE[index]
    })
}

/// Проверяет обе CRC целого syncframe-а AC-3.
///
/// `frame` — ровно один кадр (длина из Таблицы 5.18). `crc1` покрывает байты
/// от конца синхрослова до границы 5/8 кадра; `crc2` проверяется по
/// оставшимся 3/8 с обнулённого регистра. Так обе ошибки локализуются
/// независимо (§7.10.1 разрешает такой «сброс» регистра для crc2), а при
/// исправной первой части это эквивалентно прогону всего кадра.
pub(crate) fn verify_syncframe_crcs(frame: &[u8], frame_words: usize) -> Result<(), CrcRegion> {
    let boundary = five_eighths_frame_bytes(frame_words).min(frame.len());
    let first_region = frame.get(SYNC_WORD_BYTES..boundary).unwrap_or_default();
    if crc16_register(first_region) != 0 {
        return Err(CrcRegion::FirstFiveEighths);
    }
    let last_region = frame.get(boundary..).unwrap_or_default();
    if crc16_register(last_region) != 0 {
        return Err(CrcRegion::LastThreeEighths);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CRC16_POLYNOMIAL, crc16_register};

    /// Побитовая эталонная реализация LFSR прямо по описанию §7.10.1.
    fn bitwise_register(bytes: &[u8]) -> u16 {
        let mut register = 0u16;
        for &byte in bytes {
            for bit in (0..8).rev() {
                let input = u16::from((byte >> bit) & 1);
                let feedback = (register >> 15) ^ input;
                register <<= 1;
                if feedback == 1 {
                    register ^= CRC16_POLYNOMIAL;
                }
            }
        }
        register
    }

    #[test]
    fn table_driven_register_matches_bitwise_lfsr() {
        let message: Vec<u8> = (0u16..300).map(|value| (value * 37 % 251) as u8).collect();
        assert_eq!(crc16_register(&message), bitwise_register(&message));
        assert_ne!(crc16_register(&message), 0);
    }

    #[test]
    fn appending_register_value_makes_remainder_zero() {
        // Свойство, на котором стоит проверка «регистр должен обнулиться»:
        // сообщение + его остаток (MSB-first) делится на полином.
        let message = b"fastiplayer ac3 crc";
        let remainder = crc16_register(message);
        let mut protected = message.to_vec();
        protected.extend_from_slice(&remainder.to_be_bytes());
        assert_eq!(crc16_register(&protected), 0);
        protected[3] ^= 0x10;
        assert_ne!(crc16_register(&protected), 0);
    }
}
