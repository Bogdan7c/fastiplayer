//! Спектральное расширение SPX: синтез высоких частот в частотной области
//! (A/52 Annex E §3.6; ETSI TS 102 366 v1.4.1 §E.2.6).
//!
//! Выше `spx_begin_subbnd` каналы с `chinspx = 1` не передают мантисс:
//! декодер копирует низкочастотные коэффициенты вверх (§3.6.4.1), смешивает
//! их с шумом (§3.6.4.2), при `chinspxatten` гасит стыки режекторным
//! фильтром (§3.6.4.2.3) и масштабирует полосы координатами SPX (§3.6.4.3).
//! Работает над уже восстановленными (в т. ч. из coupling) коэффициентами
//! `transform_coefficient[k]` — копируемая область может лежать в coupling.

use super::COEFFICIENTS_PER_BLOCK;
use super::exponent_scale;
use crate::tables::eac3::spx::{
    SPX_ATTENUATION, SPX_SUBBAND_FIRST_BIN, SPX_SUBBAND_WIDTH, SPX_SUBBANDS,
};

/// Множитель итогового масштабирования из псевдокода §3.6.4.3: `tc · spxco · 32`.
const SPX_COORDINATE_GAIN: f32 = 32.0;
/// Экспонента координаты, при которой мантисса передаётся без скрытой единицы.
const DENORMAL_COORDINATE_EXPONENT: u8 = 15;
/// Знаменатель `spxblnd / 32` (§3.6.4.2.1).
const BLEND_CODE_SCALE: f32 = 32.0;
/// Сколько бинов перед первым бином расширения захватывает режекторный фильтр.
const NOTCH_BINS_BEFORE_BORDER: usize = 2;

/// Разметка полос SPX одного блока: диапазон sub-band-ов и их объединение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SpxBandLayout {
    /// `spx_begin_subbnd`.
    begin_subband: usize,
    /// `spx_end_subbnd` (sub-band за последним).
    end_subband: usize,
    /// `spxbndsztab[]`: ширина каждой полосы в бинах.
    band_widths: [usize; SPX_SUBBANDS],
    /// `nspxbnds`.
    band_count: usize,
}

impl SpxBandLayout {
    /// Строит полосы по псевдокоду §3.6.2 (`nspxbnds`, `spxbndsztab`).
    ///
    /// `merged_with_previous` — массив `spxbndstrc[]` по абсолютному номеру
    /// sub-band-а. Вызывающий код гарантирует `begin < end ≤ 17`.
    pub(crate) fn new(
        begin_subband: usize,
        end_subband: usize,
        merged_with_previous: &[bool; SPX_SUBBANDS],
    ) -> Self {
        let end_subband = end_subband.min(SPX_SUBBANDS);
        let mut band_widths = [0usize; SPX_SUBBANDS];
        band_widths[0] = SPX_SUBBAND_WIDTH;
        let mut band_count = 1;
        for &merged in merged_with_previous
            .iter()
            .take(end_subband)
            .skip(begin_subband + 1)
        {
            if merged {
                band_widths[band_count - 1] += SPX_SUBBAND_WIDTH;
            } else {
                band_widths[band_count] = SPX_SUBBAND_WIDTH;
                band_count += 1;
            }
        }
        Self {
            begin_subband,
            end_subband,
            band_widths,
            band_count,
        }
    }

    /// `nspxbnds`.
    pub(crate) const fn band_count(&self) -> usize {
        self.band_count
    }

    /// Первый синтезируемый бин: `spxbandtable[spx_begin_subbnd]`.
    pub(crate) fn begin_bin(&self) -> usize {
        subband_bin(self.begin_subband)
    }

    /// Бин за последним синтезируемым: `spxbandtable[spx_end_subbnd]`.
    pub(crate) fn end_bin(&self) -> usize {
        subband_bin(self.end_subband)
    }

    /// Полосы: (первый бин, ширина) по порядку.
    fn bands(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        let mut first_bin = self.begin_bin();
        self.band_widths
            .iter()
            .take(self.band_count)
            .map(move |&width| {
                let band = (first_bin, width);
                first_bin += width;
                band
            })
    }
}

/// `spxbandtable[sub-band]` с защитой от выхода за таблицу.
fn subband_bin(subband: usize) -> usize {
    SPX_SUBBAND_FIRST_BIN
        .get(subband)
        .copied()
        .unwrap_or(COEFFICIENTS_PER_BLOCK)
}

/// Координата SPX одной полосы (§3.6.3): `spxcomant/4` при экспоненте 15,
/// иначе `(spxcomant + 4)/8`; затем сдвиг на `spxcoexp + 3·mstrspxco`.
pub(crate) fn spx_coordinate(exponent: u8, mantissa: u8, master: u8) -> f32 {
    let base = if exponent == DENORMAL_COORDINATE_EXPONENT {
        f32::from(mantissa) / 4.0
    } else {
        f32::from(mantissa + 4) / 8.0
    };
    base * exponent_scale(exponent.saturating_add(master.saturating_mul(3)))
}

/// Всё, что SPX-синтезу канала нужно знать о его полосах в текущем блоке.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SpxChannelBands {
    /// `spxco[ch][bnd]`.
    pub(crate) coordinates: [f32; SPX_SUBBANDS],
    /// `nblendfact[ch][bnd]` — доля шума.
    pub(crate) noise_blend: [f32; SPX_SUBBANDS],
    /// `sblendfact[ch][bnd]` — доля перенесённого сигнала.
    pub(crate) signal_blend: [f32; SPX_SUBBANDS],
}

/// Коэффициенты смешения шума для всех полос (§3.6.4.2.1); считаются только
/// когда пришли новые координаты (`spxcoe = 1`), иначе переиспользуются.
pub(crate) fn blending_factors(
    layout: &SpxBandLayout,
    blend_code: u8,
) -> ([f32; SPX_SUBBANDS], [f32; SPX_SUBBANDS]) {
    let noise_offset = f32::from(blend_code) / BLEND_CODE_SCALE;
    let end_bin = layout.end_bin() as f32;
    let mut noise = [0.0f32; SPX_SUBBANDS];
    let mut signal = [0.0f32; SPX_SUBBANDS];
    for (band, (first_bin, width)) in layout.bands().enumerate() {
        // Отношение центра полосы к верхней границе расширения минус смещение.
        let ratio =
            ((first_bin as f32 + 0.5 * width as f32) / end_bin - noise_offset).clamp(0.0, 1.0);
        noise[band] = ratio.sqrt();
        signal[band] = (1.0 - ratio).sqrt();
    }
    (noise, signal)
}

/// Источник шума `noise()` §3.6.4.2.4: нулевое среднее, единичная дисперсия.
///
/// Спецификация конкретный генератор не задаёт; берём детерминированный
/// 32-битный LCG и равномерное распределение на [−√3, √3) (дисперсия 1),
/// чтобы декодирование было воспроизводимым. Состояние живёт между кадрами
/// в декодере и сбрасывается вместе с ним.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpxNoiseGenerator {
    state: u32,
}

impl SpxNoiseGenerator {
    /// Зерно по умолчанию (произвольная фиксированная константа).
    const SEED: u32 = 0x5EC7_E3A1;
    /// Константы LCG «Numerical Recipes» (как у генератора dither-а).
    const MULTIPLIER: u32 = 1_664_525;
    const INCREMENT: u32 = 1_013_904_223;
    /// Старшие 24 бита состояния — точность мантиссы f32.
    const SAMPLE_BITS: u32 = 24;

    pub(crate) fn new() -> Self {
        Self { state: Self::SEED }
    }

    #[cfg(test)]
    pub(crate) const fn with_seed(state: u32) -> Self {
        Self { state }
    }

    /// Следующее значение шума.
    fn next(&mut self) -> f32 {
        self.state = self
            .state
            .wrapping_mul(Self::MULTIPLIER)
            .wrapping_add(Self::INCREMENT);
        let half_range = 1i32 << (Self::SAMPLE_BITS - 1);
        let sample = (self.state >> (32 - Self::SAMPLE_BITS)) as i32 - half_range;
        sample as f32 / half_range as f32 * 3f32.sqrt()
    }
}

/// Параметры синтеза одного канала в блоке.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SpxSynthesis<'a> {
    pub(crate) layout: &'a SpxBandLayout,
    /// `spxstrtf` — первый копируемый sub-band (вызывающий код гарантирует,
    /// что он ниже `spx_begin_subbnd`).
    pub(crate) copy_start_subband: usize,
    pub(crate) bands: &'a SpxChannelBands,
    /// `spxattencod[ch]`, если для канала включено `chinspxatten`.
    pub(crate) attenuation_code: Option<u8>,
}

/// Синтезирует бины `[begin_bin, end_bin)` канала (§3.6.4.1–3.6.4.3).
pub(crate) fn synthesize_extension(
    synthesis: &SpxSynthesis<'_>,
    noise: &mut SpxNoiseGenerator,
    coefficients: &mut [f32; COEFFICIENTS_PER_BLOCK],
) {
    let layout = synthesis.layout;
    let wrapped = translate_coefficients(layout, synthesis.copy_start_subband, coefficients);
    let rms = banded_rms(layout, coefficients);
    if let Some(code) = synthesis.attenuation_code {
        apply_border_notches(layout, &wrapped, code, coefficients);
    }
    for (band, (first_bin, width)) in layout.bands().enumerate() {
        let noise_scale = rms[band] * synthesis.bands.noise_blend[band];
        let signal_scale = synthesis.bands.signal_blend[band];
        let gain = synthesis.bands.coordinates[band] * SPX_COORDINATE_GAIN;
        for coefficient in coefficients.iter_mut().skip(first_bin).take(width) {
            // §3.6.4.2.4 (смешение) и §3.6.4.3 (масштаб координатой) подряд.
            let blended = *coefficient * signal_scale + noise.next() * noise_scale;
            *coefficient = blended * gain;
        }
    }
}

/// §3.6.4.1: копирование `[copystartmant, copyendmant)` вверх полосами;
/// возвращает `wrapflag[bnd]` — где копирование начиналось заново.
fn translate_coefficients(
    layout: &SpxBandLayout,
    copy_start_subband: usize,
    coefficients: &mut [f32; COEFFICIENTS_PER_BLOCK],
) -> [bool; SPX_SUBBANDS] {
    let copy_start = subband_bin(copy_start_subband);
    let copy_end = layout.begin_bin();
    let mut wrapped = [false; SPX_SUBBANDS];
    if copy_start >= copy_end {
        // Синтаксис отбрасывает такие кадры раньше; защита от зацикливания.
        return wrapped;
    }
    let mut copy_index = copy_start;
    for (band, (first_bin, width)) in layout.bands().enumerate() {
        if copy_index + width > copy_end {
            copy_index = copy_start;
            wrapped[band] = true;
        }
        for insert_index in first_bin..first_bin + width {
            if copy_index == copy_end {
                copy_index = copy_start;
            }
            if let (Some(&source), true) = (
                coefficients.get(copy_index),
                insert_index < COEFFICIENTS_PER_BLOCK,
            ) {
                coefficients[insert_index] = source;
            }
            copy_index += 1;
        }
    }
    wrapped
}

/// §3.6.4.2.2: RMS перенесённых коэффициентов каждой полосы.
fn banded_rms(
    layout: &SpxBandLayout,
    coefficients: &[f32; COEFFICIENTS_PER_BLOCK],
) -> [f32; SPX_SUBBANDS] {
    let mut rms = [0.0f32; SPX_SUBBANDS];
    for (band, (first_bin, width)) in layout.bands().enumerate() {
        let energy: f32 = coefficients
            .iter()
            .skip(first_bin)
            .take(width)
            .map(|value| value * value)
            .sum();
        rms[band] = (energy / width as f32).sqrt();
    }
    rms
}

/// §3.6.4.2.3: 5-точечный режекторный фильтр `[a, a², a³, a², a]`
/// (Таблица E3.14) вокруг начала расширения и каждой точки «заворота».
///
/// Псевдокод спецификации содержит опечатки в индексах (`binindex` вместо
/// `bin` в первых циклах); принят единственно осмысленный вариант из текста:
/// «первые 3 значения из таблицы, последние 2 — по симметрии».
fn apply_border_notches(
    layout: &SpxBandLayout,
    wrapped: &[bool; SPX_SUBBANDS],
    attenuation_code: u8,
    coefficients: &mut [f32; COEFFICIENTS_PER_BLOCK],
) {
    let Some(row) = SPX_ATTENUATION.get(usize::from(attenuation_code)) else {
        return;
    };
    let filter = [row[0], row[1], row[2], row[1], row[0]].map(|value| value as f32);
    let mut notch = |border_bin: usize| {
        let Some(first) = border_bin.checked_sub(NOTCH_BINS_BEFORE_BORDER) else {
            return;
        };
        for (offset, &gain) in filter.iter().enumerate() {
            if let Some(coefficient) = coefficients.get_mut(first + offset) {
                *coefficient *= gain;
            }
        }
    };
    for (band, (first_bin, _)) in layout.bands().enumerate() {
        // Полоса 0 всегда граничит с базовой полосой; остальные — при заворачивании.
        if band == 0 || wrapped[band] {
            notch(first_bin);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SpxBandLayout, SpxChannelBands, SpxNoiseGenerator, SpxSynthesis, blending_factors,
        spx_coordinate, synthesize_extension, translate_coefficients,
    };
    use crate::spectral::COEFFICIENTS_PER_BLOCK;
    use crate::tables::eac3::spx::{DEFAULT_SPX_BAND_STRUCTURE, SPX_SUBBANDS};

    #[test]
    fn default_band_structure_merges_sub_bands_into_wider_bands() {
        // sub-band-ы 2..14 при Таблице E2.11: 8, 10, 12 присоединяются к соседям.
        let layout = SpxBandLayout::new(2, 14, &DEFAULT_SPX_BAND_STRUCTURE);
        let widths: Vec<_> = layout.bands().map(|(_, width)| width).collect();
        assert_eq!(widths, [12, 12, 12, 12, 12, 24, 24, 24, 12]);
        assert_eq!(layout.band_count(), 9);
        assert_eq!(layout.begin_bin(), 49);
        assert_eq!(layout.end_bin(), 193);
        assert_eq!(widths.iter().sum::<usize>(), 193 - 49);
    }

    #[test]
    fn coordinates_follow_section_3_6_3() {
        assert_eq!(spx_coordinate(0, 0, 0), 0.5);
        assert_eq!(spx_coordinate(0, 3, 0), 7.0 / 8.0);
        assert_eq!(spx_coordinate(15, 2, 0), 0.5 / 32_768.0);
        // mstrspxco 1 добавляет 3 разряда (Таблица E2.12: ×2^−3).
        assert_eq!(spx_coordinate(1, 0, 1), 0.5 / 16.0);
    }

    #[test]
    fn translation_wraps_copy_region_and_reports_wrap_points() {
        // Копируем [25, 49) (sub-band 0–1) в расширение 49..121 тремя полосами по 24.
        let mut merged = [false; SPX_SUBBANDS];
        merged[3] = true;
        merged[5] = true;
        merged[7] = true;
        let layout = SpxBandLayout::new(2, 8, &merged);
        let mut coefficients = [0.0f32; COEFFICIENTS_PER_BLOCK];
        for (bin, value) in coefficients.iter_mut().enumerate().take(49) {
            *value = bin as f32;
        }
        let wrapped = translate_coefficients(&layout, 0, &mut coefficients);
        // Полоса 0 помещается целиком; полосы 1 и 2 начинают копирование заново.
        assert_eq!(&wrapped[..3], &[false, true, true]);
        assert_eq!(coefficients[49], 25.0);
        assert_eq!(coefficients[72], 48.0);
        assert_eq!(coefficients[73], 25.0);
        assert_eq!(coefficients[120], 48.0);
        assert_eq!(coefficients[121], 0.0);
    }

    #[test]
    fn blending_moves_from_signal_to_noise_towards_high_bands() {
        let layout = SpxBandLayout::new(2, 14, &DEFAULT_SPX_BAND_STRUCTURE);
        let (noise, signal) = blending_factors(&layout, 0);
        for band in 0..layout.band_count() {
            assert!((noise[band] * noise[band] + signal[band] * signal[band] - 1.0).abs() < 1e-6);
        }
        assert!(noise[0] < noise[layout.band_count() - 1]);
        // Код 31 почти полностью убирает шум у нижних полос.
        let (noise, _) = blending_factors(&layout, 31);
        assert_eq!(noise[0], 0.0);
    }

    #[test]
    fn noise_has_zero_mean_and_unit_variance() {
        let mut generator = SpxNoiseGenerator::new();
        let values: Vec<f32> = (0..50_000).map(|_| generator.next()).collect();
        let mean = values.iter().sum::<f32>() / values.len() as f32;
        let variance =
            values.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / values.len() as f32;
        assert!(mean.abs() < 0.02, "{mean}");
        assert!((variance - 1.0).abs() < 0.03, "{variance}");
        // Детерминированность: новый генератор повторяет ту же последовательность.
        assert_eq!(SpxNoiseGenerator::new().next(), values[0]);
    }

    /// Без шума (blend → весь сигнал) синтез — копия, умноженная на 32·spxco:
    /// энергия полосы предсказуема, нули за `end_bin` не трогаются.
    #[test]
    fn signal_only_synthesis_scales_translated_band_by_coordinate() {
        let layout = SpxBandLayout::new(3, 5, &[false; SPX_SUBBANDS]);
        let mut coefficients = [0.0f32; COEFFICIENTS_PER_BLOCK];
        coefficients[30] = 0.25;
        coefficients[42] = 0.5;
        let mut bands = SpxChannelBands {
            coordinates: [0.0; SPX_SUBBANDS],
            noise_blend: [0.0; SPX_SUBBANDS],
            signal_blend: [1.0; SPX_SUBBANDS],
        };
        bands.coordinates[0] = 1.0 / 64.0;
        bands.coordinates[1] = 1.0 / 32.0;
        let synthesis = SpxSynthesis {
            layout: &layout,
            copy_start_subband: 0,
            bands: &bands,
            attenuation_code: None,
        };
        synthesize_extension(&synthesis, &mut SpxNoiseGenerator::new(), &mut coefficients);
        // Копия [25, 49) подряд: бин 30 → 66 (полоса 0, ×0,5), бин 42 → 78 (полоса 1, ×1).
        assert_eq!(coefficients[66], 0.125);
        assert_eq!(coefficients[78], 0.5);
        assert_eq!(coefficients[30], 0.25);
        assert!(coefficients[85..].iter().all(|&value| value == 0.0));
    }

    #[test]
    fn border_notch_attenuates_five_bins_around_extension_start() {
        let layout = SpxBandLayout::new(3, 4, &[false; SPX_SUBBANDS]);
        let mut coefficients = [1.0f32; COEFFICIENTS_PER_BLOCK];
        let bands = SpxChannelBands {
            coordinates: [1.0 / 32.0; SPX_SUBBANDS],
            noise_blend: [0.0; SPX_SUBBANDS],
            signal_blend: [1.0; SPX_SUBBANDS],
        };
        let synthesis = SpxSynthesis {
            layout: &layout,
            copy_start_subband: 1,
            bands: &bands,
            attenuation_code: Some(14),
        };
        synthesize_extension(&synthesis, &mut SpxNoiseGenerator::new(), &mut coefficients);
        // Код 14: 0,5; 0,25; 0,125 (Таблица E3.14) симметрично вокруг бина 61.
        assert_eq!(
            &coefficients[58..65],
            &[1.0, 0.5, 0.25, 0.125, 0.25, 0.5, 1.0]
        );
    }
}
