//! Декодирование экспонент (A/52 §7.1).
//!
//! Экспоненты передаются дифференциально: абсолютное значение, затем группы
//! по три «отображённых» разности (7 бит на группу, §7.1.2). Стратегия
//! D15/D25/D45 задаёт, на сколько соседних бинов распространяется каждая
//! разность. Модуль общий для AC-3 и E-AC-3 (Annex E использует тот же
//! механизм).

use super::COEFFICIENTS_PER_BLOCK;

/// Наибольшая допустимая экспонента (§7.1.1: «allowed to range from 0 … to 24»).
pub(crate) const MAX_EXPONENT: u8 = 24;

/// Наибольший корректный 7-битный групповой код: 25·4 + 5·4 + 4 = 124
/// (§7.1.2, условие 17 из §7.10.2).
pub(crate) const MAX_GROUPED_EXPONENT_CODE: u8 = 124;

/// Наибольшее число групп у одного набора экспонент: D15 до бина 253
/// даёт (253 − 1) / 3 = 84 группы (§7.1.3).
pub(crate) const MAX_EXPONENT_GROUPS: usize = 84;

/// Ширина одной групповой записи в потоке.
pub(crate) const GROUPED_EXPONENT_BITS: u32 = 7;

/// Стратегия экспонент (Таблица 7.4, для LFE — Таблица 7.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExponentStrategy {
    /// `'00'` — экспоненты прошлого блока.
    Reuse,
    /// `'01'` — по одной разности на бин.
    D15,
    /// `'10'` — разность на пару бинов.
    D25,
    /// `'11'` — разность на четвёрку бинов.
    D45,
}

impl ExponentStrategy {
    /// 2-битный `chexpstr`/`cplexpstr`.
    pub(crate) const fn from_code(code: u8) -> Self {
        match code & 0x3 {
            0 => Self::Reuse,
            1 => Self::D15,
            2 => Self::D25,
            _ => Self::D45,
        }
    }

    /// 1-битный `lfeexpstr`: `'0'` — reuse, `'1'` — D15 (Таблица 7.5).
    pub(crate) const fn from_lfe_flag(new_exponents: bool) -> Self {
        if new_exponents {
            Self::D15
        } else {
            Self::Reuse
        }
    }

    /// Сколько бинов разделяют одну разность (`grpsize`), `None` для reuse.
    pub(crate) const fn bins_per_difference(self) -> Option<usize> {
        match self {
            Self::Reuse => None,
            Self::D15 => Some(1),
            Self::D25 => Some(2),
            Self::D45 => Some(4),
        }
    }

    /// Сколько бинов покрывает одна 7-битная группа (3, 6 или 12; Таблица 7.4).
    const fn bins_per_group(self) -> Option<usize> {
        match self.bins_per_difference() {
            Some(bins) => Some(bins * 3),
            None => None,
        }
    }
}

/// `nchgrps` для полнополосного (или coupled) канала (§7.1.3):
/// `truncate((endmant − 1 + (grpsize·3 − 3)) / (grpsize·3))`.
pub(crate) const fn full_bandwidth_group_count(
    strategy: ExponentStrategy,
    end_mantissa: usize,
) -> usize {
    match strategy.bins_per_group() {
        Some(bins) => (end_mantissa.saturating_sub(1) + bins - 3) / bins,
        None => 0,
    }
}

/// `ncplgrps` для coupling канала (§7.1.3): `(cplendmant − cplstrtmant) / (grpsize·3)`.
pub(crate) const fn coupling_group_count(
    strategy: ExponentStrategy,
    start_mantissa: usize,
    end_mantissa: usize,
) -> usize {
    match strategy.bins_per_group() {
        Some(bins) => end_mantissa.saturating_sub(start_mantissa) / bins,
        None => 0,
    }
}

/// Ошибки декодирования экспонент.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExponentDecodeError {
    /// Групповой код > 124 (§7.10.2, условие 17).
    GroupedCodeOutOfRange { code: u8 },
    /// Накопленная экспонента вышла за 0..=24.
    ExponentOutOfRange,
}

/// Разворачивает группы разностей в абсолютные экспоненты (псевдокод §7.1.3).
///
/// `reference` — абсолютная экспонента, от которой считаются разности
/// (`exps[ch][0]`, `lfeexps[0]` или `cplabsexp << 1`). Первая разность даёт
/// бин `first_bin`, каждая следующая — на `grpsize` бинов дальше. Записи за
/// пределами 256 бинов отбрасываются (их не бывает при корректных размерах:
/// округление D25/D45 вверх не выходит за бин 252, §7.1.3).
pub(crate) fn expand_exponent_groups(
    reference: u8,
    grouped_codes: &[u8],
    bins_per_difference: usize,
    first_bin: usize,
    exponents: &mut [u8; COEFFICIENTS_PER_BLOCK],
) -> Result<(), ExponentDecodeError> {
    let mut previous = i16::from(reference);
    let mut bin = first_bin;
    for &code in grouped_codes {
        if code > MAX_GROUPED_EXPONENT_CODE {
            return Err(ExponentDecodeError::GroupedCodeOutOfRange { code });
        }
        // M1 = gexp / 25, M2 = (gexp % 25) / 5, M3 = gexp % 5; разность = M − 2.
        let mapped = [code / 25, (code % 25) / 5, code % 5];
        for value in mapped {
            let absolute = previous + i16::from(value) - 2;
            if !(0..=i16::from(MAX_EXPONENT)).contains(&absolute) {
                return Err(ExponentDecodeError::ExponentOutOfRange);
            }
            previous = absolute;
            for _ in 0..bins_per_difference {
                if let Some(slot) = exponents.get_mut(bin) {
                    *slot = absolute as u8;
                }
                bin += 1;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ExponentDecodeError, ExponentStrategy, coupling_group_count, expand_exponent_groups,
        full_bandwidth_group_count,
    };

    #[test]
    fn group_counts_cover_exactly_the_coded_bins() {
        // Для любого chbwcod (0–60) число переданных экспонент 1 + nchgrps·3·grpsize
        // не меньше endmant и не выходит за бин 253.
        for chbwcod in 0..=60usize {
            let end = (chbwcod + 12) * 3 + 37;
            for strategy in [
                ExponentStrategy::D15,
                ExponentStrategy::D25,
                ExponentStrategy::D45,
            ] {
                let per_difference = strategy.bins_per_difference().unwrap();
                let coded = 1 + full_bandwidth_group_count(strategy, end) * 3 * per_difference;
                assert!(coded >= end, "chbwcod {chbwcod} {strategy:?}");
                assert!(
                    coded < end + 3 * per_difference && coded <= 253,
                    "chbwcod {chbwcod}"
                );
            }
        }
        assert_eq!(full_bandwidth_group_count(ExponentStrategy::D15, 253), 84);
        assert_eq!(full_bandwidth_group_count(ExponentStrategy::Reuse, 253), 0);
        // Coupling: 12 бинов на sub-band, деление всегда точное.
        assert_eq!(coupling_group_count(ExponentStrategy::D15, 37, 253), 72);
        assert_eq!(coupling_group_count(ExponentStrategy::D45, 37, 253), 18);
    }

    #[test]
    fn expansion_follows_pseudocode_for_each_strategy() {
        // Код 25·3 + 5·1 + 2 = 82: разности +1, −1, 0.
        let mut exponents = [0u8; 256];
        expand_exponent_groups(10, &[82], 2, 1, &mut exponents).unwrap();
        assert_eq!(&exponents[..8], &[0, 11, 11, 10, 10, 10, 10, 0]);
        let mut d45 = [99u8; 256];
        expand_exponent_groups(4, &[124], 4, 37, &mut d45).unwrap();
        // 124 = 4,4,4 → +2 трижды: 6, 8, 10 по 4 бина начиная с 37.
        assert_eq!(&d45[37..49], &[6, 6, 6, 6, 8, 8, 8, 8, 10, 10, 10, 10]);
        assert_eq!(d45[36], 99);
    }

    #[test]
    fn invalid_codes_and_ranges_are_rejected() {
        let mut exponents = [0u8; 256];
        assert_eq!(
            expand_exponent_groups(0, &[125], 1, 1, &mut exponents),
            Err(ExponentDecodeError::GroupedCodeOutOfRange { code: 125 })
        );
        // Ноль минус 2 — отрицательная экспонента.
        assert_eq!(
            expand_exponent_groups(0, &[0], 1, 1, &mut exponents),
            Err(ExponentDecodeError::ExponentOutOfRange)
        );
        // 24 + 2 > 24.
        assert_eq!(
            expand_exponent_groups(24, &[124], 1, 1, &mut exponents),
            Err(ExponentDecodeError::ExponentOutOfRange)
        );
    }
}
