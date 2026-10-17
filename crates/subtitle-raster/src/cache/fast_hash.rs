// Rust-часть растеризатора, © Fastiplayer contributors.

//! Быстрый нестойкий хэш для кэшей с внутренними ключами.
//!
//! Стандартный `HashMap` считает SipHash-1-3: он защищён от подбора коллизий, но на ключах из
//! десятка целых чисел стоит заметной доли времени запроса к кэшу (на кадре с сотней
//! закэшированных глифов это был главный расход). Ключи кэшей растеризатора - квантованные
//! параметры глифов и идентификаторы, а не данные, которыми злоумышленник мог бы подбирать
//! коллизии к чужому процессу: худший исход подбора - более медленный поиск в пределах лимита
//! памяти кэша. Поэтому здесь используется простой хэш «умножение с поворотом» (семейство FxHash
//! из компилятора Rust): один `u64`-множитель на слово, без случайного начального значения.

use std::hash::{BuildHasherDefault, Hasher};

/// Множитель из `rustc-hash` 2.x (дробная часть золотого сечения, нечётное).
const MULTIPLIER: u64 = 0xf135_7aea_2e62_a9c5;

/// Хэшер «умножение с поворотом».
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct FastHasher {
    state: u64,
}

impl FastHasher {
    /// Подмешивает одно слово в состояние.
    #[inline]
    fn mix(&mut self, word: u64) {
        self.state = self.state.wrapping_add(word).wrapping_mul(MULTIPLIER);
    }
}

impl Hasher for FastHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        // Основная часть - 8-байтными словами, хвост (до 7 байт) - одним словом.
        let (words, tail) = bytes.as_chunks::<8>();
        for word in words {
            self.mix(u64::from_le_bytes(*word));
        }
        if !tail.is_empty() {
            let mut buffer = [0_u8; 8];
            buffer[..tail.len()].copy_from_slice(tail);
            // Длина хвоста в старшем байте отличает «ab» от «ab\0».
            buffer[7] = tail.len() as u8;
            self.mix(u64::from_le_bytes(buffer));
        }
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.mix(u64::from(value));
    }

    #[inline]
    fn write_u16(&mut self, value: u16) {
        self.mix(u64::from(value));
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.mix(u64::from(value));
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.mix(value);
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.mix(value as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        // Хэш-таблица берёт индекс корзины из младших бит, а умножение хорошо перемешивает
        // старшие: поворот переносит их вниз.
        self.state.rotate_left(26)
    }
}

/// Построитель [`FastHasher`] для `HashMap`.
pub(crate) type FastBuildHasher = BuildHasherDefault<FastHasher>;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::hash::{BuildHasher, Hash};

    use super::FastBuildHasher;

    fn hash_of<T: Hash>(value: &T) -> u64 {
        FastBuildHasher::default().hash_one(value)
    }

    #[test]
    fn equal_keys_hash_equally_and_different_keys_differ() {
        assert_eq!(hash_of(&(1_u32, 2_i16, 'x')), hash_of(&(1_u32, 2_i16, 'x')));
        assert_ne!(hash_of(&(1_u32, 2_i16, 'x')), hash_of(&(1_u32, 3_i16, 'x')));
        assert_ne!(hash_of(&(1_u32, 2_i16, 'x')), hash_of(&(2_u32, 2_i16, 'x')));
    }

    #[test]
    fn byte_strings_of_different_length_differ() {
        assert_ne!(hash_of(&b"ab".as_slice()), hash_of(&b"ab\0".as_slice()));
        assert_ne!(hash_of(&"".to_owned()), hash_of(&"\0".to_owned()));
    }

    #[test]
    fn sequential_integer_keys_spread_over_low_bits() {
        // Индекс корзины берётся из младших бит: последовательные ключи не должны
        // собираться в несколько корзин.
        let mut buckets = [0_u32; 64];
        for key in 0_u64..4096 {
            buckets[(hash_of(&key) & 63) as usize] += 1;
        }
        let fullest = buckets.iter().copied().max().unwrap_or(0);
        assert!(fullest < 4096 / 64 * 2, "fullest bucket holds {fullest}");
    }

    #[test]
    fn map_with_fast_hasher_behaves_like_a_map() {
        let mut map: HashMap<(u32, u32), u32, FastBuildHasher> = HashMap::default();
        for key in 0..1000_u32 {
            map.insert((key, key ^ 5), key);
        }
        assert_eq!(map.len(), 1000);
        assert_eq!(map.get(&(77, 77 ^ 5)), Some(&77));
        assert_eq!(map.get(&(77, 77)), None);
    }
}
