//! Таблицы coupling в синтаксисе E-AC-3: полосы по умолчанию и улучшенное
//! coupling ECPL (A/52 Annex E §2.3.3.15–2.3.3.19, §3.5.2–3.5.3; ETSI TS 102 366
//! v1.4.1 §E.1.3.3.15–E.1.3.3.19, §E.2.5).

use crate::spectral::coupling::MAX_COUPLING_SUBBANDS;

/// Таблица E2.12 (вторая, «Default Coupling Banding Structure»),
/// `defcplbndstrc[]` по абсолютному номеру coupling sub-band-а 0–17:
/// `true` — sub-band присоединяется к предыдущей полосе. Для sub-band-а 0
/// значение в таблице не указано (он никогда не присоединяется).
pub(crate) const DEFAULT_COUPLING_BAND_STRUCTURE: [bool; MAX_COUPLING_SUBBANDS] = [
    false, false, false, false, false, false, false, false, // 0–7
    true, false, true, true, false, true, true, true, true, true, // 8–17
];

/// Число sub-band-ов ECPL (Таблица E3.7: 0–21).
pub(crate) const ECPL_SUBBANDS: usize = 22;

/// Таблица E3.9 «Sub-band Transform Start Coefficients», `ecplsubbndtab[sbnd]`,
/// sbnd 0–22 (запись 22 — бин за последним sub-band-ом). Sub-band-ы 0–3 по
/// 6 бинов, дальше по 12 (Таблица E3.7).
pub(crate) const ECPL_SUBBAND_FIRST_BIN: [usize; ECPL_SUBBANDS + 1] = [
    13, 19, 25, 31, 37, 49, 61, 73, 85, 97, 109, 121, 133, 145, 157, 169, 181, 193, 205, 217, 229,
    241, 253,
];

/// Таблица E2.13 «Default Enhanced Coupling Banding Structure», `defecplbndstrc[]`.
pub(crate) const DEFAULT_ECPL_BAND_STRUCTURE: [bool; ECPL_SUBBANDS] = [
    false, false, false, false, false, false, false, false, false, // 0–8
    true, false, true, false, true, false, true, true, true, false, true, true,
    true, // 9–21
];

/// Sub-band-ы до этого номера включительно никогда не объединяются в полосы
/// (§2.3.3.19: «sub-bands 0 through 7 … and up to … 8 are always zero»).
pub(crate) const ECPL_LAST_UNMERGEABLE_SUBBAND: usize = 8;

/// Таблица E3.10, `ecplampexptab[]` для `ecplamp` 0–30 (31 — минус бесконечность).
const ECPL_AMPLITUDE_EXPONENTS: [u8; 31] = [
    0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 6, 6, 7, 7,
];

/// Таблица E3.10, `ecplampmanttab[]` для `ecplamp` 0–30.
const ECPL_AMPLITUDE_MANTISSAS: [u8; 31] = [
    0x20, 0x1b, 0x17, 0x13, 0x10, 0x1b, 0x17, 0x13, 0x10, 0x1b, 0x17, 0x13, 0x10, 0x1b, 0x17, 0x13,
    0x10, 0x1b, 0x17, 0x13, 0x10, 0x1b, 0x17, 0x13, 0x10, 0x1b, 0x17, 0x13, 0x10, 0x1b, 0x17,
];

/// Знаменатель мантиссы амплитуды: `ecplampmanttab / 32` (§3.5.5.2).
const ECPL_AMPLITUDE_MANTISSA_SCALE: f32 = 32.0;

/// Амплитуда полосы ECPL по 5-битному `ecplamp` (§3.5.5.2 / ETSI §E.2.5.5.1):
/// `(ecplampmanttab / 32) >> ecplampexptab`, а код 31 — ноль (−∞ дБ).
pub(crate) fn ecpl_amplitude(code: u8) -> f32 {
    let index = usize::from(code);
    match (
        ECPL_AMPLITUDE_MANTISSAS.get(index),
        ECPL_AMPLITUDE_EXPONENTS.get(index),
    ) {
        (Some(&mantissa), Some(&exponent)) => {
            f32::from(mantissa) / ECPL_AMPLITUDE_MANTISSA_SCALE / f32::from(1u8 << exponent)
        }
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_COUPLING_BAND_STRUCTURE, DEFAULT_ECPL_BAND_STRUCTURE, ECPL_SUBBAND_FIRST_BIN,
        ecpl_amplitude,
    };
    use crate::spectral::coupling::subband_first_bin;

    #[test]
    fn default_band_structures_follow_tables_e2_12_and_e2_13() {
        let merged =
            |table: &[bool]| -> Vec<usize> { (0..table.len()).filter(|&s| table[s]).collect() };
        assert_eq!(
            merged(&DEFAULT_COUPLING_BAND_STRUCTURE),
            [8, 10, 11, 13, 14, 15, 16, 17]
        );
        assert_eq!(
            merged(&DEFAULT_ECPL_BAND_STRUCTURE),
            [9, 11, 13, 15, 16, 17, 19, 20, 21]
        );
    }

    #[test]
    fn ecpl_subbands_are_six_then_twelve_bins_and_match_coupling_grid() {
        let widths: Vec<usize> = ECPL_SUBBAND_FIRST_BIN
            .windows(2)
            .map(|p| p[1] - p[0])
            .collect();
        assert_eq!(&widths[..4], &[6, 6, 6, 6]);
        assert!(widths[4..].iter().all(|&width| width == 12));
        // Sub-band-ы ECPL 4–21 совпадают с coupling sub-band-ами 0–17 (Таблицы E3.7 и 7.24).
        for coupling_subband in 0..=18 {
            assert_eq!(
                ECPL_SUBBAND_FIRST_BIN[coupling_subband + 4],
                subband_first_bin(coupling_subband)
            );
        }
    }

    /// §3.5.4: коды 0–30 — от 0 до −45,01 дБ шагом ≈1,5 дБ, 31 — −∞.
    #[test]
    fn amplitudes_span_zero_to_minus_45_db_in_1_5_db_steps() {
        assert_eq!(ecpl_amplitude(0), 1.0);
        assert_eq!(ecpl_amplitude(31), 0.0);
        assert_eq!(ecpl_amplitude(32), 0.0);
        let decibels: Vec<f32> = (0..31)
            .map(|code| 20.0 * ecpl_amplitude(code).log10())
            .collect();
        assert!((decibels[30] + 45.01).abs() < 0.01, "{}", decibels[30]);
        for pair in decibels.windows(2) {
            let step = pair[0] - pair[1];
            assert!((1.2..1.8).contains(&step), "{step}");
        }
    }
}
