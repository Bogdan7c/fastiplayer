//! «Вес» видео потока: битрейт как отношение байтов сегментов к покрытому ими
//! медиавремени (решение владельца 277). Это размер того, что показывается, а не
//! скорость скачивания страницей.

use std::num::NonZeroU64;

/// Микросекунд в секунде: перевод медиавремени в секунды при расчёте.
const MICROS_PER_SECOND: u128 = 1_000_000;
/// Бит в байте.
const BITS_PER_BYTE: u128 = 8;

/// Битрейт видео, бит в секунду (всегда больше нуля: «нет числа» — это `None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MediaBitrate {
    /// Бит в секунду.
    bits_per_second: NonZeroU64,
}

impl MediaBitrate {
    /// Битрейт из готового числа бит в секунду: для потребителей, которым нужно значение
    /// без потока (тесты окна, будущие источники). Источник сам строит его из веса полки.
    #[must_use]
    pub const fn from_bits_per_second(bits_per_second: NonZeroU64) -> Self {
        Self { bits_per_second }
    }

    /// Бит в секунду.
    #[must_use]
    pub const fn bits_per_second(self) -> u64 {
        self.bits_per_second.get()
    }
}

/// Сумма по полкам видео: сколько байтов и сколько медиавремени они покрывают.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct VideoWeight {
    /// Байты сегментов целиком (без обрезки по краям окна).
    pub(crate) bytes: u64,
    /// Объединение отрезков времени сегментов, микросекунды.
    pub(crate) covered_micros: u64,
}

impl VideoWeight {
    /// Прибавляет вес ещё одной полосы с видео: байты складываются, а время — берётся
    /// наибольшее (полосы идут параллельно по одному и тому же времени показа).
    pub(crate) fn with_lane(self, lane: Self) -> Self {
        Self {
            bytes: self.bytes.saturating_add(lane.bytes),
            covered_micros: self.covered_micros.max(lane.covered_micros),
        }
    }

    /// Битрейт: `байты × 8 ÷ секунды`, округлённый до ближайшего бита.
    /// Нет времени или нет байтов — `None`.
    pub(crate) fn bitrate(self) -> Option<MediaBitrate> {
        if self.covered_micros == 0 {
            return None;
        }
        let covered = u128::from(self.covered_micros);
        let scaled_bits = u128::from(self.bytes) * BITS_PER_BYTE * MICROS_PER_SECOND;
        // Прибавка половины делителя превращает целочисленное деление в округление.
        let rounded = (scaled_bits + covered / 2) / covered;
        let bits = NonZeroU64::new(u64::try_from(rounded).unwrap_or(u64::MAX))?;
        Some(MediaBitrate {
            bits_per_second: bits,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_is_bits_over_covered_seconds_rounded() {
        let weight = VideoWeight {
            bytes: 500_000,
            covered_micros: 4_000_000,
        };
        assert_eq!(
            weight.bitrate().map(MediaBitrate::bits_per_second),
            Some(1_000_000)
        );
        // 1 байт за 3 с = 2,67 бита/с -> 3.
        let tiny = VideoWeight {
            bytes: 1,
            covered_micros: 3_000_000,
        };
        assert_eq!(tiny.bitrate().map(MediaBitrate::bits_per_second), Some(3));
    }

    #[test]
    fn no_time_or_no_bytes_is_unknown() {
        let no_time = VideoWeight {
            bytes: 10,
            covered_micros: 0,
        };
        let no_bytes = VideoWeight {
            bytes: 0,
            covered_micros: 1_000_000,
        };
        assert_eq!(no_time.bitrate(), None);
        assert_eq!(no_bytes.bitrate(), None);
    }

    #[test]
    fn parallel_lanes_add_bytes_and_keep_the_longest_time() {
        let first = VideoWeight {
            bytes: 100,
            covered_micros: 2_000_000,
        };
        let second = VideoWeight {
            bytes: 50,
            covered_micros: 3_000_000,
        };
        assert_eq!(
            first.with_lane(second),
            VideoWeight {
                bytes: 150,
                covered_micros: 3_000_000
            }
        );
    }
}
