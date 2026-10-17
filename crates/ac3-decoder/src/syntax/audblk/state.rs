//! Состояние, которое аудиоблоки кадра передают друг другу (A/52 §5.4.3).
//!
//! Почти каждый элемент `audblk` может сказать «как в прошлом блоке»
//! (`cplstre = 0`, `chexpstr = reuse`, `baie = 0`, …). Это состояние живёт
//! только внутри одного syncframe-а: §7.1.2 — «Information is never shared
//! across syncframes», поэтому [`FrameBlockState::new`] вызывается на каждый
//! кадр. Каждое значение хранится как `Option`: `None` означает «переиспользовать
//! нечего», и попытка reuse превращается в типизированную ошибку.
//!
//! Отдельное правило §5.5: когда coupling включается (`cplinu` 0 → 1), ничего
//! из прежних coupling-данных переиспользовать нельзя — для этого есть
//! [`FrameBlockState::forget_coupling_channel_state`].

use crate::spectral::bit_allocation::{BitAllocationParameters, DeltaSegments, LeakInitialization};
use crate::spectral::coupling::{MAX_COUPLING_SUBBANDS, subband_first_bin};
use crate::spectral::{COEFFICIENTS_PER_BLOCK, MAX_FULL_BANDWIDTH_CHANNELS};

/// Стратегия coupling текущего блока (`cplinu = 1`, §5.4.3.8–5.4.3.13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CouplingStrategy {
    /// `chincpl[ch]`.
    pub(crate) in_coupling: [bool; MAX_FULL_BANDWIDTH_CHANNELS],
    /// `phsflginu` (только 2/0).
    pub(crate) phase_flags_in_use: bool,
    /// Разметка полос: первая/последняя sub-band и `cplbndstrc`.
    pub(crate) layout: CouplingLayout,
}

/// Диапазон coupling и объединение sub-band-ов в полосы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CouplingLayout {
    /// `cplbegf` — первая sub-band.
    pub(crate) begin_subband: usize,
    /// `cplendf + 3` — sub-band за последней.
    pub(crate) end_subband: usize,
    /// `cplbndstrc[sbnd]` по абсолютному номеру sub-band-а: `true` — sub-band
    /// присоединена к предыдущей полосе. Для `begin_subband` всегда `false`.
    pub(crate) merged_with_previous: [bool; MAX_COUPLING_SUBBANDS],
}

impl CouplingLayout {
    /// `cplstrtmant = 37 + 12·cplbegf` (§7.1.3).
    pub(crate) const fn start_mantissa(&self) -> usize {
        subband_first_bin(self.begin_subband)
    }

    /// `cplendmant = 37 + 12·(cplendf + 3)` (§7.1.3).
    pub(crate) const fn end_mantissa(&self) -> usize {
        subband_first_bin(self.end_subband)
    }

    /// `ncplbnd` (§5.4.3.13).
    pub(crate) fn band_count(&self) -> usize {
        (self.begin_subband..self.end_subband)
            .filter(|&subband| !self.merged_with_previous[subband])
            .count()
    }

    /// Номер coupling полосы для каждой sub-band диапазона (для раскладки
    /// координат и фазовых флагов по sub-band-ам, §7.4.3).
    pub(crate) fn band_of_each_subband(&self) -> [usize; MAX_COUPLING_SUBBANDS] {
        let mut bands = [0usize; MAX_COUPLING_SUBBANDS];
        let mut band = 0usize;
        let range = bands
            .iter_mut()
            .enumerate()
            .take(self.end_subband)
            .skip(self.begin_subband);
        for (subband, slot) in range {
            if subband != self.begin_subband && !self.merged_with_previous[subband] {
                band += 1;
            }
            *slot = band;
        }
        bands
    }
}

/// Координаты coupling одного канала, привязанные к разметке, под которую
/// они переданы (§7.10.2 условия 23–24: при смене разметки reuse запрещён).
#[derive(Debug, Clone, Copy)]
pub(crate) struct StoredCouplingCoordinates {
    pub(crate) layout: CouplingLayout,
    pub(crate) per_subband: [f32; MAX_COUPLING_SUBBANDS],
}

/// Флаги rematrixing (только 2/0, §5.4.3.19–5.4.3.20, Таблица 5.15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RematrixFlags {
    /// Сколько полос rematrixing (2, 3 или 4).
    pub(crate) band_count: usize,
    /// `rematflg[rbnd]`.
    pub(crate) flags: [bool; 4],
}

/// Абсолютные экспоненты одного канала и бин за последней переданной.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ExponentSet {
    pub(crate) exponents: [u8; COEFFICIENTS_PER_BLOCK],
    /// `strtmant`: 0 у fbw/LFE, `cplstrtmant` у coupling канала.
    pub(crate) start: usize,
    /// `endmant` / `cplendmant` / 7 у LFE.
    pub(crate) end: usize,
}

/// SNR offset и fast gain одного набора экспонент (§5.4.3.37–5.4.3.43).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChannelSnr {
    /// `fsnroffst` / `cplfsnroffst` / `lfefsnroffst`.
    pub(crate) fine_offset: u8,
    /// `fgaincod` / `cplfgaincod` / `lfefgaincod`.
    pub(crate) fast_gain_code: u8,
}

/// Все SNR-параметры блока, кроме coupling канала (он хранится отдельно,
/// потому что сбрасывается при включении coupling).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SnrOffsets {
    /// `csnroffst`.
    pub(crate) coarse_offset: u8,
    /// По fbw каналам.
    pub(crate) full_bandwidth: [ChannelSnr; MAX_FULL_BANDWIDTH_CHANNELS],
    /// LFE (если `lfeon`).
    pub(crate) lfe: Option<ChannelSnr>,
}

/// Действующее состояние delta bit allocation одного канала.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DeltaAllocation {
    /// Поправок нет (начальное состояние кадра, §7.2.2.6, и код `'10'`).
    #[default]
    Inactive,
    /// Действуют переданные сегменты.
    Active(DeltaSegments),
}

impl DeltaAllocation {
    /// Сегменты для шага 6 битового распределения, если поправка активна.
    pub(crate) fn segments(&self) -> Option<&DeltaSegments> {
        match self {
            Self::Inactive => None,
            Self::Active(segments) => Some(segments),
        }
    }
}

/// Что известно о coupling в предыдущем блоке кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CouplingStatus {
    /// Ни одного `cplstre = 1` в кадре ещё не было (начало кадра).
    NotSignalled,
    /// `cplinu = 0`.
    Off,
    /// `cplinu = 1` с этой стратегией.
    On(CouplingStrategy),
}

/// Состояние, переходящее от блока к блоку внутри одного кадра.
#[derive(Debug, Clone)]
pub(crate) struct FrameBlockState {
    /// Стратегия coupling предыдущего блока.
    pub(crate) coupling: CouplingStatus,
    pub(crate) coupling_coordinates:
        [Option<StoredCouplingCoordinates>; MAX_FULL_BANDWIDTH_CHANNELS],
    /// Фазовые флаги правого канала 2/0 по sub-band-ам.
    pub(crate) phase_inverted: [bool; MAX_COUPLING_SUBBANDS],
    pub(crate) rematrix: Option<RematrixFlags>,
    pub(crate) channel_exponents: [Option<ExponentSet>; MAX_FULL_BANDWIDTH_CHANNELS],
    pub(crate) coupling_exponents: Option<ExponentSet>,
    pub(crate) lfe_exponents: Option<ExponentSet>,
    pub(crate) bit_allocation: Option<BitAllocationParameters>,
    pub(crate) snr_offsets: Option<SnrOffsets>,
    pub(crate) coupling_snr: Option<ChannelSnr>,
    pub(crate) coupling_leak: Option<LeakInitialization>,
    pub(crate) channel_deltas: [DeltaAllocation; MAX_FULL_BANDWIDTH_CHANNELS],
    pub(crate) coupling_delta: DeltaAllocation,
    /// Действующие `dynrng`/`dynrng2` (§5.4.3.3: в блоке 0 без слова — 0).
    pub(crate) dynamic_range_words: [u8; 2],
}

impl FrameBlockState {
    /// Состояние в начале кадра: ничего не передано, dba выключена, dynrng = 0.
    pub(crate) fn new() -> Self {
        Self {
            coupling: CouplingStatus::NotSignalled,
            coupling_coordinates: [None; MAX_FULL_BANDWIDTH_CHANNELS],
            phase_inverted: [false; MAX_COUPLING_SUBBANDS],
            rematrix: None,
            channel_exponents: [None; MAX_FULL_BANDWIDTH_CHANNELS],
            coupling_exponents: None,
            lfe_exponents: None,
            bit_allocation: None,
            snr_offsets: None,
            coupling_snr: None,
            coupling_leak: None,
            channel_deltas: [DeltaAllocation::Inactive; MAX_FULL_BANDWIDTH_CHANNELS],
            coupling_delta: DeltaAllocation::Inactive,
            dynamic_range_words: [0; 2],
        }
    }

    /// Был ли coupling включён в предыдущем блоке этого кадра.
    pub(crate) fn coupling_was_in_use(&self) -> bool {
        matches!(self.coupling, CouplingStatus::On(_))
    }

    /// §5.5: «Whenever the state of cplinu changes from off to on, all coupling
    /// information shall be included … No coupling related information shall be
    /// reused». Забываем всё, что относится к coupling каналу.
    pub(crate) fn forget_coupling_channel_state(&mut self) {
        self.coupling_coordinates = [None; MAX_FULL_BANDWIDTH_CHANNELS];
        self.phase_inverted = [false; MAX_COUPLING_SUBBANDS];
        self.coupling_exponents = None;
        self.coupling_snr = None;
        self.coupling_leak = None;
        self.coupling_delta = DeltaAllocation::Inactive;
    }
}

#[cfg(test)]
mod tests {
    use super::{CouplingLayout, MAX_COUPLING_SUBBANDS};

    #[test]
    fn band_structure_maps_subbands_to_bands() {
        let mut merged = [false; MAX_COUPLING_SUBBANDS];
        // sub-band-ы 3..9: 4 присоединён к 3, 6 и 7 — к 5.
        merged[4] = true;
        merged[6] = true;
        merged[7] = true;
        let layout = CouplingLayout {
            begin_subband: 3,
            end_subband: 9,
            merged_with_previous: merged,
        };
        assert_eq!(layout.band_count(), 3);
        assert_eq!(&layout.band_of_each_subband()[3..9], &[0, 0, 1, 1, 1, 2]);
        assert_eq!(layout.start_mantissa(), 73);
        assert_eq!(layout.end_mantissa(), 145);
    }
}
