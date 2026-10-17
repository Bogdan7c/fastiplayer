//! Результат разбора аудиоблока: частотные коэффициенты и то, что нужно
//! синтезу (пакет D2) — длины преобразования, rematrixing, DRC.
//!
//! Коэффициенты уже восстановлены из coupling (§7.4) и сдвинуты на свои
//! экспоненты, то есть это значения `transform_coefficient[k]` из §7.3. Ни
//! rematrixing (§7.5), ни dynrng (§7.7) к ним ещё не применены: это шаги D2.
//!
//! Блоки E-AC-3 (`syntax::eac3`) используют то же представление: AHT, ECPL и
//! SPX уже развёрнуты в коэффициенты, а rematrixing уже применён
//! (`rematrix = None`, `coupling_begin_subband = None`), потому что SPX
//! обязан копировать коэффициенты L/R (см. `syntax::eac3::audblk`).

use super::state::RematrixFlags;
use crate::spectral::{COEFFICIENTS_PER_BLOCK, MAX_FULL_BANDWIDTH_CHANNELS};

/// Длина обратного преобразования канала в блоке (`blksw[ch]`, §5.4.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum TransformLength {
    /// `blksw = 0`: одно 512-точечное преобразование.
    #[default]
    Long,
    /// `blksw = 1`: два 256-точечных преобразования.
    ShortPair,
}

/// Спектр одного канала в одном блоке.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ChannelSpectrum {
    /// Коэффициенты по бинам; за `end_bin` — нули.
    pub(crate) coefficients: [f32; COEFFICIENTS_PER_BLOCK],
    /// Бин за последним ненулевым по построению (`endmant`, `cplendmant`
    /// для coupled каналов, 7 у LFE).
    pub(crate) end_bin: usize,
    /// Длина преобразования.
    pub(crate) transform: TransformLength,
}

impl ChannelSpectrum {
    pub(crate) const fn silent() -> Self {
        Self {
            coefficients: [0.0; COEFFICIENTS_PER_BLOCK],
            end_bin: 0,
            transform: TransformLength::Long,
        }
    }
}

/// Один декодированный аудиоблок AC-3.
#[derive(Debug, Clone)]
pub(crate) struct DecodedAudioBlock {
    /// Полнополосные каналы в порядке Таблицы 5.8 (действительны первые `nfchans`).
    pub(crate) full_bandwidth: [ChannelSpectrum; MAX_FULL_BANDWIDTH_CHANNELS],
    /// LFE (действителен при `lfeon`).
    pub(crate) lfe: ChannelSpectrum,
    /// Действующее 8-битное слово `dynrng` (§7.7.1); 0 = 0 дБ.
    pub(crate) dynamic_range_word: u8,
    /// Действующее `dynrng2` для Ch2 в режиме 1+1.
    pub(crate) dual_mono_dynamic_range_word: u8,
    /// Флаги rematrixing (только 2/0).
    pub(crate) rematrix: Option<RematrixFlags>,
    /// `cplbegf`, если coupling включён: от него зависят полосы rematrixing (§7.5.2).
    pub(crate) coupling_begin_subband: Option<usize>,
}

impl DecodedAudioBlock {
    pub(crate) fn silent() -> Self {
        Self {
            full_bandwidth: [ChannelSpectrum::silent(); MAX_FULL_BANDWIDTH_CHANNELS],
            lfe: ChannelSpectrum::silent(),
            dynamic_range_word: 0,
            dual_mono_dynamic_range_word: 0,
            rematrix: None,
            coupling_begin_subband: None,
        }
    }
}
