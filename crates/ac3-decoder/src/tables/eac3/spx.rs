//! Таблицы спектрального расширения SPX (A/52 Annex E §2.3.3.7, §3.6;
//! ETSI TS 102 366 v1.4.1 §E.1.3.3.7, §E.2.6).

/// Число sub-band-ов SPX (Таблица E3.13: 0–16; запись 17 — только граница).
pub(crate) const SPX_SUBBANDS: usize = 17;

/// Таблица E3.13 «Spectral Extension Band Table», колонка «low tc #»:
/// первый бин sub-band-а 0–17 (`spxbandtable[]`). Sub-band по 12 бинов;
/// запись 17 (бин 229) нужна для `spxendf = 7`.
pub(crate) const SPX_SUBBAND_FIRST_BIN: [usize; SPX_SUBBANDS + 1] = [
    25, 37, 49, 61, 73, 85, 97, 109, 121, 133, 145, 157, 169, 181, 193, 205, 217, 229,
];

/// Ширина sub-band-а SPX в бинах (§3.6.2: «17 sub-bands of 12 coefficients»).
pub(crate) const SPX_SUBBAND_WIDTH: usize = 12;

/// Таблица E2.11 «Default Spectral Extension Banding Structure»,
/// `defspxbndstrc[]` по абсолютному номеру sub-band-а: `true` — sub-band
/// присоединяется к предыдущей полосе.
pub(crate) const DEFAULT_SPX_BAND_STRUCTURE: [bool; SPX_SUBBANDS] = [
    false, false, false, false, false, false, false, false, // 0–7
    true, false, true, false, true, false, true, false, true, // 8–16
];

/// Таблица E3.14 «Spectral Extension Attenuation Table», `spxattentab[code][binindex]`:
/// три коэффициента режекторного фильтра на границах SPX (§3.6.4.2.3).
/// Хранится в f64 ровно как в тексте спецификации (9 знаков).
pub(crate) const SPX_ATTENUATION: [[f64; 3]; 32] = [
    [0.954_841_604, 0.911_722_489, 0.870_550_563],
    [0.911_722_489, 0.831_237_896, 0.757_858_283],
    [0.870_550_563, 0.757_858_283, 0.659_753_955],
    [0.831_237_896, 0.690_956_440, 0.574_349_177],
    [0.793_700_526, 0.629_960_525, 0.500_000_000],
    [0.757_858_283, 0.574_349_177, 0.435_275_282],
    [0.723_634_619, 0.523_647_061, 0.378_929_142],
    [0.690_956_440, 0.477_420_802, 0.329_876_978],
    [0.659_753_955, 0.435_275_282, 0.287_174_589],
    [0.629_960_525, 0.396_850_263, 0.250_000_000],
    [0.601_512_518, 0.361_817_309, 0.217_637_641],
    [0.574_349_177, 0.329_876_978, 0.189_464_571],
    [0.548_412_490, 0.300_756_259, 0.164_938_489],
    [0.523_647_061, 0.274_206_245, 0.143_587_294],
    [0.500_000_000, 0.250_000_000, 0.125_000_000],
    [0.477_420_802, 0.227_930_622, 0.108_818_820],
    [0.455_861_244, 0.207_809_474, 0.094_732_285],
    [0.435_275_282, 0.189_464_571, 0.082_469_244],
    [0.415_618_948, 0.172_739_110, 0.071_793_647],
    [0.396_850_263, 0.157_490_131, 0.062_500_000],
    [0.378_929_142, 0.143_587_294, 0.054_409_410],
    [0.361_817_309, 0.130_911_765, 0.047_366_143],
    [0.345_478_220, 0.119_355_200, 0.041_234_622],
    [0.329_876_978, 0.108_818_820, 0.035_896_824],
    [0.314_980_262, 0.099_212_566, 0.031_250_000],
    [0.300_756_259, 0.090_454_327, 0.027_204_705],
    [0.287_174_589, 0.082_469_244, 0.023_683_071],
    [0.274_206_245, 0.075_189_065, 0.020_617_311],
    [0.261_823_531, 0.068_551_561, 0.017_948_412],
    [0.250_000_000, 0.062_500_000, 0.015_625_000],
    [0.238_710_401, 0.056_982_656, 0.013_602_353],
    [0.227_930_622, 0.051_952_369, 0.011_841_536],
];

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_SPX_BAND_STRUCTURE, SPX_ATTENUATION, SPX_SUBBAND_FIRST_BIN, SPX_SUBBAND_WIDTH,
    };
    use crate::spectral::coupling::subband_first_bin;

    #[test]
    fn band_table_is_twelve_bin_grid_coinciding_with_coupling_subbands() {
        assert!(
            SPX_SUBBAND_FIRST_BIN
                .windows(2)
                .all(|pair| pair[1] - pair[0] == SPX_SUBBAND_WIDTH)
        );
        assert_eq!(SPX_SUBBAND_FIRST_BIN[0], 25);
        assert_eq!(SPX_SUBBAND_FIRST_BIN[17], 229);
        // §3.6.2: «sub-bands containing … #37 through #228 coincide with coupling sub-bands».
        for (coupling_subband, &first_bin) in SPX_SUBBAND_FIRST_BIN[1..].iter().enumerate() {
            assert_eq!(first_bin, subband_first_bin(coupling_subband));
        }
        let merged: Vec<usize> = (0..17).filter(|&s| DEFAULT_SPX_BAND_STRUCTURE[s]).collect();
        assert_eq!(merged, [8, 10, 12, 14, 16]);
    }

    /// Сумма значений по тексту (печать `E3.14 sum` скрипта) и структура строк:
    /// `[a, a², a³]` с `a = 2^(−(code+1)/15)` — шаг 0,4 дБ на код.
    #[test]
    fn attenuation_table_matches_spec_sum_and_power_structure() {
        let sum: f64 = SPX_ATTENUATION.iter().flatten().sum();
        assert!((sum - 32.761_591_374).abs() < 1e-8, "{sum}");
        for (code, row) in SPX_ATTENUATION.iter().enumerate() {
            let base = 2f64.powf(-((code + 1) as f64) / 15.0);
            assert!((row[0] - base).abs() < 1e-8, "code {code}");
            assert!((row[1] - base * base).abs() < 1e-8, "code {code}");
            assert!((row[2] - base * base * base).abs() < 1e-8, "code {code}");
        }
    }
}
