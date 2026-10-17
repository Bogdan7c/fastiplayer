//! Улучшенное coupling ECPL: разметка полос и восстановление каналов
//! (A/52 Annex E §3.5; ETSI TS 102 366 v1.4.1 §E.2.5).
//!
//! Две редакции спецификации расходятся. ATSC A/52:2012 §3.5.5 описывает
//! фазовую обработку (углы, «хаос», IMDCT/FFT coupling канала по трём блокам).
//! Более новая ETSI TS 102 366 v1.4.1 (2017) превратила углы и «хаос» в
//! зарезервированные поля, а восстановление канала свела к амплитуде полосы:
//! `chmant[ch][bin] = ecplmant[bin] · ampbnd[ch][sbnd]` (§E.2.5.5.2). Реализована
//! редакция ETSI: она новее, задаёт и синтаксис (длины зарезервированных
//! полей), и обработку, а реальных потоков с ECPL для сверки нет ни у одной.

use super::dither::{ChannelDither, DitherGenerator};
use super::{COEFFICIENTS_PER_BLOCK, exponent_scale};
use crate::tables::eac3::coupling::{
    ECPL_LAST_UNMERGEABLE_SUBBAND, ECPL_SUBBAND_FIRST_BIN, ECPL_SUBBANDS,
};

/// Разметка ECPL одного блока: диапазон sub-band-ов и их объединение в полосы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EcplLayout {
    /// `ecpl_begin_subbnd`.
    pub(crate) begin_subband: usize,
    /// `ecpl_end_subbnd` (sub-band за последним).
    pub(crate) end_subband: usize,
    /// `ecplbndstrc[]` по абсолютному номеру sub-band-а. Для sub-band-ов до
    /// `max(8, begin)` всегда `false` (§2.3.3.19).
    pub(crate) merged_with_previous: [bool; ECPL_SUBBANDS],
}

impl EcplLayout {
    /// Разметка с принудительно нулевыми флагами там, где §2.3.3.19 их запрещает.
    pub(crate) fn new(
        begin_subband: usize,
        end_subband: usize,
        mut merged_with_previous: [bool; ECPL_SUBBANDS],
    ) -> Self {
        let always_separate = ECPL_LAST_UNMERGEABLE_SUBBAND.max(begin_subband);
        for flag in merged_with_previous.iter_mut().take(always_separate + 1) {
            *flag = false;
        }
        Self {
            begin_subband,
            end_subband: end_subband.min(ECPL_SUBBANDS),
            merged_with_previous,
        }
    }

    /// `ecplstartmant = ecplsubbndtab[ecpl_begin_subbnd]` (§3.3.3).
    pub(crate) fn start_bin(&self) -> usize {
        ECPL_SUBBAND_FIRST_BIN[self.begin_subband.min(ECPL_SUBBANDS)]
    }

    /// `ecplendmant = ecplsubbndtab[ecpl_end_subbnd]` (§3.3.5).
    pub(crate) fn end_bin(&self) -> usize {
        ECPL_SUBBAND_FIRST_BIN[self.end_subband.min(ECPL_SUBBANDS)]
    }

    /// `necplbnd = (end − begin) − Σ ecplbndstrc` (§2.3.3.19).
    pub(crate) fn band_count(&self) -> usize {
        (self.begin_subband..self.end_subband)
            .filter(|&subband| !self.merged_with_previous[subband])
            .count()
    }

    /// Номер полосы каждого sub-band-а диапазона (псевдокод §3.5.5.2).
    pub(crate) fn band_of_each_subband(&self) -> [usize; ECPL_SUBBANDS] {
        let mut bands = [0usize; ECPL_SUBBANDS];
        let mut band = 0usize;
        let range = bands
            .iter_mut()
            .zip(self.merged_with_previous)
            .enumerate()
            .take(self.end_subband)
            .skip(self.begin_subband);
        for (subband, (slot, merged)) in range {
            if subband != self.begin_subband && !merged {
                band += 1;
            }
            *slot = band;
        }
        bands
    }
}

/// Coupling канал ECPL в текущем блоке: мантиссы без экспоненты, bap, экспоненты.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EcplChannelView<'a> {
    pub(crate) mantissas: &'a [f32; COEFFICIENTS_PER_BLOCK],
    /// bap = 0 → мантисса заменяется dither-ом канала-получателя (как в AC-3 §7.3.4).
    pub(crate) bap: &'a [u8; COEFFICIENTS_PER_BLOCK],
    pub(crate) exponents: &'a [u8; COEFFICIENTS_PER_BLOCK],
    pub(crate) layout: &'a EcplLayout,
}

/// Восстанавливает ECPL-область канала: `ecplmant · ampbnd` (ETSI §E.2.5.5.2).
///
/// `amplitudes` — амплитуды по полосам (`ampbnd[ch][bnd]`, уже по
/// Таблице E3.10).
pub(crate) fn decouple_enhanced_channel(
    coupling: &EcplChannelView<'_>,
    amplitudes: &[f32; ECPL_SUBBANDS],
    channel_dither: ChannelDither,
    dither: &mut DitherGenerator,
    coefficients: &mut [f32; COEFFICIENTS_PER_BLOCK],
) {
    let layout = coupling.layout;
    let band_of_subband = layout.band_of_each_subband();
    for subband in layout.begin_subband..layout.end_subband {
        let amplitude = amplitudes[band_of_subband[subband]];
        let bins = ECPL_SUBBAND_FIRST_BIN[subband]..ECPL_SUBBAND_FIRST_BIN[subband + 1];
        for bin in bins {
            let (Some(&bap), Some(&mantissa), Some(&exponent), Some(slot)) = (
                coupling.bap.get(bin),
                coupling.mantissas.get(bin),
                coupling.exponents.get(bin),
                coefficients.get_mut(bin),
            ) else {
                continue;
            };
            let mantissa = if bap == 0 {
                dither.zero_bit_mantissa(channel_dither)
            } else {
                mantissa
            };
            *slot = mantissa * exponent_scale(exponent) * amplitude;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EcplChannelView, EcplLayout, decouple_enhanced_channel};
    use crate::spectral::COEFFICIENTS_PER_BLOCK;
    use crate::spectral::dither::{ChannelDither, DitherGenerator, DitherMode};
    use crate::tables::eac3::coupling::{DEFAULT_ECPL_BAND_STRUCTURE, ECPL_SUBBANDS};

    #[test]
    fn layout_counts_bands_and_never_merges_low_sub_bands() {
        // Таблица E2.13 по умолчанию, диапазон 2..22: sub-band-ы 2–8 отдельные,
        // дальше 9 → к 8, 11 → к 10, 13 → к 12, 15–17 → к 14, 19–21 → к 18.
        let layout = EcplLayout::new(2, 22, DEFAULT_ECPL_BAND_STRUCTURE);
        assert_eq!(layout.band_count(), 7 + 4);
        assert_eq!(layout.start_bin(), 25);
        assert_eq!(layout.end_bin(), 253);
        let bands = layout.band_of_each_subband();
        assert_eq!(&bands[2..12], &[0, 1, 2, 3, 4, 5, 6, 6, 7, 7]);
        // Флаг у самого begin (и до 8 включительно) обнуляется.
        let mut merged = [true; ECPL_SUBBANDS];
        merged[12] = false;
        let layout = EcplLayout::new(10, 14, merged);
        assert_eq!(layout.band_count(), 2);
        assert_eq!(&layout.band_of_each_subband()[10..14], &[0, 0, 1, 1]);
    }

    #[test]
    fn channel_is_coupling_coefficient_times_band_amplitude() {
        let layout = EcplLayout::new(0, 6, [false; ECPL_SUBBANDS]);
        let mut mantissas = [0.0f32; COEFFICIENTS_PER_BLOCK];
        let mut bap = [0u8; COEFFICIENTS_PER_BLOCK];
        let mut exponents = [0u8; COEFFICIENTS_PER_BLOCK];
        for bin in 13..61 {
            mantissas[bin] = 0.5;
            bap[bin] = 5;
            exponents[bin] = 1;
        }
        let view = EcplChannelView {
            mantissas: &mantissas,
            bap: &bap,
            exponents: &exponents,
            layout: &layout,
        };
        let mut amplitudes = [0.0f32; ECPL_SUBBANDS];
        amplitudes[0] = 1.0;
        amplitudes[4] = 0.5;
        let mut coefficients = [7.0f32; COEFFICIENTS_PER_BLOCK];
        let mut dither = DitherGenerator::new(DitherMode::Disabled);
        decouple_enhanced_channel(
            &view,
            &amplitudes,
            ChannelDither::Off,
            &mut dither,
            &mut coefficients,
        );
        // 0,5 · 2^−1 · амплитуда; sub-band 0 — бины 13–18, sub-band 4 — 37–48.
        assert!(coefficients[13..19].iter().all(|&value| value == 0.25));
        assert!(coefficients[19..37].iter().all(|&value| value == 0.0));
        assert!(coefficients[37..49].iter().all(|&value| value == 0.125));
        assert_eq!(coefficients[12], 7.0);
        assert_eq!(coefficients[61], 7.0);
    }
}
