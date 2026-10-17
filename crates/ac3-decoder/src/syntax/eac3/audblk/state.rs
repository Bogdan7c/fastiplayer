//! Состояние, которое аудиоблоки кадра E-AC-3 передают друг другу
//! (A/52 Annex E §2.2.3 «syntax state initialization», §2.3.2.28–2.3.2.30,
//! §2.3.3).
//!
//! Как и в AC-3, состояние живёт только внутри кадра и создаётся заново в
//! начале каждого кадра ([`EnhancedBlockState::new`]): `firstspxcos`,
//! `firstcplcos`, `firstcplleak` = 1, полосы SPX/coupling/ECPL — по
//! умолчанию (Таблицы E2.11–E2.13). Значение `None` в «переиспользуемых»
//! полях означает «переиспользовать нечего» — попытка reuse становится
//! типизированной ошибкой.

use crate::spectral::MAX_FULL_BANDWIDTH_CHANNELS;
use crate::spectral::bit_allocation::{BitAllocationParameters, LeakInitialization};
use crate::spectral::coupling::MAX_COUPLING_SUBBANDS;
use crate::spectral::ecpl::EcplLayout;
use crate::spectral::spx::{SpxBandLayout, SpxChannelBands};
use crate::syntax::audblk::state::{
    CouplingLayout, DeltaAllocation, ExponentSet, StoredCouplingCoordinates,
};
use crate::tables::eac3::coupling::{
    DEFAULT_COUPLING_BAND_STRUCTURE, DEFAULT_ECPL_BAND_STRUCTURE, ECPL_SUBBANDS,
};
use crate::tables::eac3::spx::{DEFAULT_SPX_BAND_STRUCTURE, SPX_SUBBANDS};

/// Стратегия спектрального расширения блока (`spxinu = 1`, §2.3.3.2–2.3.3.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SpxStrategy {
    /// `chinspx[ch]`.
    pub(crate) in_spx: [bool; MAX_FULL_BANDWIDTH_CHANNELS],
    /// `spxstrtf` — первый копируемый sub-band.
    pub(crate) copy_start_subband: usize,
    /// `spxbegf` (нужен для `cplendf`, `ecpl_end_subbnd` и `nrematbd`).
    pub(crate) begin_code: u8,
    /// Полосы SPX.
    pub(crate) layout: SpxBandLayout,
}

/// Координаты SPX канала с разметкой, под которую они переданы.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StoredSpxBands {
    pub(crate) layout: SpxBandLayout,
    pub(crate) bands: SpxChannelBands,
}

/// Стратегия улучшенного coupling (`ecplinu = 1`, §2.3.3.16–2.3.3.19).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EnhancedCouplingStrategy {
    /// `ecplbegf` (для `nrematbd`).
    pub(crate) begin_code: u8,
    pub(crate) layout: EcplLayout,
}

/// Режим coupling блока.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CouplingMode {
    /// Стандартное coupling (§7.4 в синтаксисе E-AC-3).
    Standard {
        /// `phsflginu` (только 2/0).
        phase_flags_in_use: bool,
        layout: CouplingLayout,
    },
    /// Улучшенное coupling.
    Enhanced(EnhancedCouplingStrategy),
}

/// Стратегия coupling блока с `cplinu = 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EnhancedCouplingConfig {
    /// `chincpl[ch]`.
    pub(crate) in_coupling: [bool; MAX_FULL_BANDWIDTH_CHANNELS],
    pub(crate) mode: CouplingMode,
}

impl EnhancedCouplingConfig {
    /// `cplstrtmant` / `ecplstartmant` — первый бин coupling канала.
    pub(crate) fn start_bin(&self) -> usize {
        match &self.mode {
            CouplingMode::Standard { layout, .. } => layout.start_mantissa(),
            CouplingMode::Enhanced(strategy) => strategy.layout.start_bin(),
        }
    }

    /// `cplendmant` / `ecplendmant` — бин за последним.
    pub(crate) fn end_bin(&self) -> usize {
        match &self.mode {
            CouplingMode::Standard { layout, .. } => layout.end_mantissa(),
            CouplingMode::Enhanced(strategy) => strategy.layout.end_bin(),
        }
    }
}

/// Амплитуды ECPL канала с разметкой, под которую они переданы.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StoredEcplAmplitudes {
    pub(crate) layout: EcplLayout,
    pub(crate) per_band: [f32; ECPL_SUBBANDS],
}

/// SNR offset-ы, действующие в блоке (Таблица E2.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SnrState {
    /// `csnroffst` (или `frmcsnroffst`).
    pub(crate) coarse: Option<u8>,
    /// `fsnroffst[ch]`.
    pub(crate) channels: [Option<u8>; MAX_FULL_BANDWIDTH_CHANNELS],
    /// `cplfsnroffst`.
    pub(crate) coupling: Option<u8>,
    /// `lfefsnroffst`.
    pub(crate) lfe: Option<u8>,
}

/// Fast gain-коды блока (`fgaincod`, по умолчанию 4 — §2.3.3.28).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FastGainCodes {
    pub(crate) channels: [u8; MAX_FULL_BANDWIDTH_CHANNELS],
    pub(crate) coupling: u8,
    pub(crate) lfe: u8,
}

impl FastGainCodes {
    /// Код по умолчанию при `fgaincode = 0`.
    pub(crate) const DEFAULT_CODE: u8 = 4;

    pub(crate) const fn defaults() -> Self {
        Self {
            channels: [Self::DEFAULT_CODE; MAX_FULL_BANDWIDTH_CHANNELS],
            coupling: Self::DEFAULT_CODE,
            lfe: Self::DEFAULT_CODE,
        }
    }
}

/// Флаги rematrixing с «истинным» числом полос E-AC-3 (`nrematbd` 0–4, §3.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EnhancedRematrix {
    pub(crate) band_count: usize,
    pub(crate) flags: [bool; 4],
}

/// Состояние между блоками одного кадра E-AC-3.
#[derive(Debug, Clone)]
pub(crate) struct EnhancedBlockState {
    /// Стратегия SPX прошлого блока (`None` — `spxinu = 0`).
    pub(crate) spx: Option<SpxStrategy>,
    /// `spxbndstrc[]` по абсолютному номеру sub-band-а.
    pub(crate) spx_band_structure: [bool; SPX_SUBBANDS],
    /// `firstspxcos[ch]`.
    pub(crate) first_spx_coordinates: [bool; MAX_FULL_BANDWIDTH_CHANNELS],
    pub(crate) spx_bands: [Option<StoredSpxBands>; MAX_FULL_BANDWIDTH_CHANNELS],
    /// Стратегия coupling прошлого блока (`None` — `cplinu = 0`).
    pub(crate) coupling: Option<EnhancedCouplingConfig>,
    /// `cplbndstrc[]` по абсолютному номеру sub-band-а.
    pub(crate) coupling_band_structure: [bool; MAX_COUPLING_SUBBANDS],
    /// `ecplbndstrc[]` по абсолютному номеру sub-band-а.
    pub(crate) ecpl_band_structure: [bool; ECPL_SUBBANDS],
    /// `firstcplcos[ch]`.
    pub(crate) first_coupling_coordinates: [bool; MAX_FULL_BANDWIDTH_CHANNELS],
    /// `firstcplleak`.
    pub(crate) first_coupling_leak: bool,
    pub(crate) coupling_coordinates:
        [Option<StoredCouplingCoordinates>; MAX_FULL_BANDWIDTH_CHANNELS],
    pub(crate) ecpl_amplitudes: [Option<StoredEcplAmplitudes>; MAX_FULL_BANDWIDTH_CHANNELS],
    /// Фазовые флаги правого канала 2/0 по coupling sub-band-ам.
    pub(crate) phase_inverted: [bool; MAX_COUPLING_SUBBANDS],
    pub(crate) rematrix: Option<EnhancedRematrix>,
    pub(crate) channel_exponents: [Option<ExponentSet>; MAX_FULL_BANDWIDTH_CHANNELS],
    pub(crate) coupling_exponents: Option<ExponentSet>,
    pub(crate) lfe_exponents: Option<ExponentSet>,
    pub(crate) bit_allocation: Option<BitAllocationParameters>,
    pub(crate) snr: SnrState,
    pub(crate) fast_gain: FastGainCodes,
    pub(crate) coupling_leak: Option<LeakInitialization>,
    pub(crate) channel_deltas: [DeltaAllocation; MAX_FULL_BANDWIDTH_CHANNELS],
    pub(crate) coupling_delta: DeltaAllocation,
    /// Действующие `dynrng`/`dynrng2` (в блоке 0 без слова — 0 дБ).
    pub(crate) dynamic_range_words: [u8; 2],
}

impl EnhancedBlockState {
    /// Состояние в начале кадра (§2.2.3, конец Таблицы E1.3).
    pub(crate) fn new() -> Self {
        Self {
            spx: None,
            spx_band_structure: DEFAULT_SPX_BAND_STRUCTURE,
            first_spx_coordinates: [true; MAX_FULL_BANDWIDTH_CHANNELS],
            spx_bands: [None; MAX_FULL_BANDWIDTH_CHANNELS],
            coupling: None,
            coupling_band_structure: DEFAULT_COUPLING_BAND_STRUCTURE,
            ecpl_band_structure: DEFAULT_ECPL_BAND_STRUCTURE,
            first_coupling_coordinates: [true; MAX_FULL_BANDWIDTH_CHANNELS],
            first_coupling_leak: true,
            coupling_coordinates: [None; MAX_FULL_BANDWIDTH_CHANNELS],
            ecpl_amplitudes: [None; MAX_FULL_BANDWIDTH_CHANNELS],
            phase_inverted: [false; MAX_COUPLING_SUBBANDS],
            rematrix: None,
            channel_exponents: [None; MAX_FULL_BANDWIDTH_CHANNELS],
            coupling_exponents: None,
            lfe_exponents: None,
            bit_allocation: None,
            snr: SnrState::default(),
            fast_gain: FastGainCodes::defaults(),
            coupling_leak: None,
            channel_deltas: [DeltaAllocation::Inactive; MAX_FULL_BANDWIDTH_CHANNELS],
            coupling_delta: DeltaAllocation::Inactive,
            dynamic_range_words: [0; 2],
        }
    }

    /// `cplinu = 0`: каналы выходят из coupling, `firstcplcos`/`firstcplleak` = 1,
    /// прошлые координаты/амплитуды забываются (Таблица E1.4, ветка `!cplinu`).
    pub(crate) fn leave_coupling(&mut self) {
        self.coupling = None;
        self.first_coupling_coordinates = [true; MAX_FULL_BANDWIDTH_CHANNELS];
        self.first_coupling_leak = true;
        self.coupling_coordinates = [None; MAX_FULL_BANDWIDTH_CHANNELS];
        self.ecpl_amplitudes = [None; MAX_FULL_BANDWIDTH_CHANNELS];
        self.phase_inverted = [false; MAX_COUPLING_SUBBANDS];
    }

    /// `spxinu = 0`: `chinspx = 0`, `firstspxcos = 1`.
    pub(crate) fn leave_spectral_extension(&mut self) {
        self.spx = None;
        self.first_spx_coordinates = [true; MAX_FULL_BANDWIDTH_CHANNELS];
        self.spx_bands = [None; MAX_FULL_BANDWIDTH_CHANNELS];
    }
}
