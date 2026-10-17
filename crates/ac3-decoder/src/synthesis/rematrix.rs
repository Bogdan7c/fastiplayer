//! Rematrixing режима 2/0 (A/52:2012 §7.5).
//!
//! Кодер в отмеченных полосах передаёт `L' = (L+R)/2`, `R' = (L−R)/2`;
//! декодер восстанавливает `L = L' + R'`, `R = L' − R'` (§7.5.4).
//!
//! Границы полос (Таблицы 7.25–7.28) одинаковы для 48/44,1/32 кГц, потому что
//! заданы номерами бинов: нижние края 13, 25, 37, 61, верхний край каждой
//! полосы — нижний край следующей минус 1. Последняя полоса кончается на бине
//! 252 без coupling (Таблица 7.25) и на `A = 36 + 12·cplbegf` (бин перед
//! началом coupling, Таблицы 7.26–7.28) с coupling. Число полос (4, 3 или 2)
//! уже выбрано разбором по Таблице 5.15 и лежит в `RematrixFlags`.
//!
//! §7.5.4: без coupling каналы могут иметь разную полосу, и rematrixing
//! применяется только до меньшей из них — иначе нули «короткого» канала
//! скопировали бы содержимое длинного в оба канала.

use crate::spectral::COEFFICIENTS_PER_BLOCK;
use crate::syntax::audblk::state::RematrixFlags;

/// Нижние края полос rematrixing (бины, Таблица 7.25).
const BAND_LOWER_BINS: [usize; 4] = [13, 25, 37, 61];
/// Последний бин последней полосы без coupling (Таблица 7.25).
const LAST_BIN_WITHOUT_COUPLING: usize = 252;
/// `A = 36 + 12·cplbegf` (Таблицы 7.26–7.28).
const COUPLING_EDGE_BASE_BIN: usize = 36;
const BINS_PER_COUPLING_SUBBAND: usize = 12;

/// Полоса частот стерео-пары в блоке, нужная rematrixing-у.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StereoPairBandwidth {
    /// `cplbegf`, если coupling в блоке включён.
    pub(crate) coupling_begin_subband: Option<usize>,
    /// Бин за последним ненулевым у L и R (`ChannelSpectrum::end_bin`).
    pub(crate) left_end_bin: usize,
    pub(crate) right_end_bin: usize,
}

/// Бин за последним бином последней полосы (исключающая граница).
fn last_band_end(coupling_begin_subband: Option<usize>) -> usize {
    match coupling_begin_subband {
        None => LAST_BIN_WITHOUT_COUPLING + 1,
        Some(begin) => COUPLING_EDGE_BASE_BIN + BINS_PER_COUPLING_SUBBAND * begin + 1,
    }
}

/// Диапазоны бинов, где в блоке действует rematrixing (`rematflg = 1`),
/// уже обрезанные по полосе пары (§7.5.4).
pub(crate) fn rematrixed_ranges(
    flags: &RematrixFlags,
    bandwidth: StereoPairBandwidth,
) -> impl Iterator<Item = std::ops::Range<usize>> {
    let band_count = flags.band_count.min(BAND_LOWER_BINS.len());
    let common_end = bandwidth
        .left_end_bin
        .min(bandwidth.right_end_bin)
        .min(COEFFICIENTS_PER_BLOCK);
    let band_flags = flags.flags;
    (0..band_count)
        .filter(move |&band| band_flags[band])
        .map(move |band| {
            let start = BAND_LOWER_BINS[band];
            let band_end = if band + 1 == band_count {
                last_band_end(bandwidth.coupling_begin_subband)
            } else {
                BAND_LOWER_BINS[band + 1]
            };
            start..band_end.min(common_end).max(start)
        })
}

/// Восстанавливает L/R в полосах с `rematflg = 1`.
pub(crate) fn restore_left_right(
    left: &mut [f32; COEFFICIENTS_PER_BLOCK],
    right: &mut [f32; COEFFICIENTS_PER_BLOCK],
    flags: &RematrixFlags,
    bandwidth: StereoPairBandwidth,
) {
    for range in rematrixed_ranges(flags, bandwidth) {
        for bin in range {
            let (sum, difference) = (left[bin], right[bin]);
            left[bin] = sum + difference;
            right[bin] = sum - difference;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{StereoPairBandwidth, last_band_end, restore_left_right};
    use crate::spectral::COEFFICIENTS_PER_BLOCK;
    use crate::syntax::audblk::state::RematrixFlags;

    type Spectrum = [f32; COEFFICIENTS_PER_BLOCK];

    /// L' = 1 во всех бинах, R' = 0,25: в rematrix-полосах L = 1,25, R = 0,75.
    fn restore(flags: RematrixFlags, bandwidth: StereoPairBandwidth) -> (Spectrum, Spectrum) {
        let mut left = [1.0; COEFFICIENTS_PER_BLOCK];
        let mut right = [0.25; COEFFICIENTS_PER_BLOCK];
        restore_left_right(&mut left, &mut right, &flags, bandwidth);
        (left, right)
    }

    fn rematrixed_bins(left: &Spectrum) -> Vec<usize> {
        (0..COEFFICIENTS_PER_BLOCK)
            .filter(|&bin| left[bin] == 1.25)
            .collect()
    }

    const FULL_BANDWIDTH: StereoPairBandwidth = StereoPairBandwidth {
        coupling_begin_subband: None,
        left_end_bin: 253,
        right_end_bin: 253,
    };

    #[test]
    fn table_7_25_bands_without_coupling() {
        let all = RematrixFlags {
            band_count: 4,
            flags: [true; 4],
        };
        let (left, right) = restore(all, FULL_BANDWIDTH);
        assert_eq!(rematrixed_bins(&left), (13..=252).collect::<Vec<_>>());
        assert!((13..=252).all(|bin| right[bin] == 0.75));
        assert_eq!((left[12], right[12], left[253]), (1.0, 0.25, 1.0));
        // Только полоса 2 (бины 37–60, Таблица 7.25).
        let band_two = RematrixFlags {
            band_count: 4,
            flags: [false, false, true, false],
        };
        let (left, _) = restore(band_two, FULL_BANDWIDTH);
        assert_eq!(rematrixed_bins(&left), (37..=60).collect::<Vec<_>>());
    }

    #[test]
    fn coupling_moves_the_last_band_edge_tables_7_26_to_7_28() {
        let all4 = RematrixFlags {
            band_count: 4,
            flags: [true; 4],
        };
        let all3 = RematrixFlags {
            band_count: 3,
            flags: [true, true, true, false],
        };
        let all2 = RematrixFlags {
            band_count: 2,
            flags: [true, true, false, false],
        };
        // (флаги, cplbegf, последний rematrix-бин A = 36 + 12·cplbegf)
        for (flags, begin, last_bin) in [
            (all4, 3, 72),
            (all4, 15, 216),
            (all3, 1, 48),
            (all3, 2, 60),
            (all2, 0, 36),
        ] {
            let bandwidth = StereoPairBandwidth {
                coupling_begin_subband: Some(begin),
                ..FULL_BANDWIDTH
            };
            let (left, _) = restore(flags, bandwidth);
            assert_eq!(
                rematrixed_bins(&left),
                (13..=last_bin).collect::<Vec<_>>(),
                "cplbegf {begin}"
            );
        }
        assert_eq!(last_band_end(None), 253);
    }

    #[test]
    fn rematrixing_stops_at_the_narrower_channel() {
        let all = RematrixFlags {
            band_count: 4,
            flags: [true; 4],
        };
        let narrow_right = StereoPairBandwidth {
            left_end_bin: 200,
            right_end_bin: 150,
            ..FULL_BANDWIDTH
        };
        let (left, right) = restore(all, narrow_right);
        assert_eq!(rematrixed_bins(&left), (13..150).collect::<Vec<_>>());
        // Выше 150 каналы не смешиваются: R остаётся своим.
        assert_eq!((left[160], right[160]), (1.0, 0.25));
    }

    #[test]
    fn cleared_flags_leave_both_channels_untouched() {
        let none = RematrixFlags {
            band_count: 4,
            flags: [false; 4],
        };
        let (left, right) = restore(none, FULL_BANDWIDTH);
        assert!(left.iter().all(|&value| value == 1.0));
        assert!(right.iter().all(|&value| value == 0.25));
    }
}
