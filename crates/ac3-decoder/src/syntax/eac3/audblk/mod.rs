//! Разбор аудиоблока E-AC-3 `audblk` (A/52 Annex E §2.2.4, Таблица E1.4;
//! ETSI TS 102 366 v1.4.1 §E.1.2.4).
//!
//! | поля Таблицы E1.4                              | модуль               |
//! |------------------------------------------------|----------------------|
//! | `blksw`, `dithflag`, `dynrng*`, `rematstr`      | этот файл            |
//! | `spxstre` … `spxcomant`                        | `spx_fields`         |
//! | `ecplinu`, `cplbegf` … `phsflg`, `ecplamp`      | `coupling_fields`    |
//! | `chbwcod`, `cplexps`, `exps`, `lfeexps`         | `exponent_fields`    |
//! | `baie` … `skipfld`                             | `allocation_fields`  |
//! | `chmant`, `cplmant`, `lfemant`, AHT            | `mantissas`          |
//! | восстановление coupling/ECPL, синтез SPX        | `reconstruction`     |
//!
//! Результат — тот же [`DecodedAudioBlock`], что даёт AC-3: коэффициенты
//! уже восстановлены из coupling/ECPL, AHT и SPX, поэтому синтез PCM (D2)
//! применяется без изменений. Единственное отличие: rematrixing (§7.5)
//! E-AC-3 уже применён (`rematrix = None`), потому что SPX обязан копировать
//! коэффициенты L/R, а не (L±R)/2 — так декодируют оба эталона (ffmpeg и
//! Nero: в стерео-потоке со SPX иначе R в области SPX на 15,7 дБ тише).
//! Annex E порядок этих шагов явно не задаёт; это решение D3.

mod allocation_fields;
mod bit_allocation;
mod coupling_fields;
mod exponent_fields;
pub(crate) mod mantissas;
mod reconstruction;
mod spx_fields;
pub(crate) mod state;

use self::mantissas::{
    AhtFrameStore, ChannelFlags, CouplingChannelMantissas, MantissaTargets, read_block_mantissas,
};
use self::state::{
    CouplingMode, EnhancedBlockState, EnhancedCouplingConfig, EnhancedRematrix, SpxStrategy,
};
use super::audfrm::AudioFrame;
use super::tool_usage::EnhancedToolUsage;
use crate::bitstream::BitReader;
use crate::error::{Ac3DecodeError, BitstreamViolation, ReusedElement};
use crate::spectral::coupling::{COUPLING_FIRST_BIN, COUPLING_SUBBAND_WIDTH};
use crate::spectral::dither::{ChannelDither, DitherGenerator};
use crate::spectral::spx::SpxNoiseGenerator;
use crate::spectral::{COEFFICIENTS_PER_BLOCK, MAX_FULL_BANDWIDTH_CHANNELS};
use crate::syntax::audblk::BlockContext;
use crate::syntax::audblk::output::{DecodedAudioBlock, TransformLength};
use crate::syntax::audblk::state::RematrixFlags;
use crate::syntax::bsi_values::AudioCodingMode;
use crate::synthesis::rematrix::{StereoPairBandwidth, restore_left_right};

/// Полос rematrixing без coupling/SPX (Таблица 7.25 / §3.3.2: `nrematbd = 4`).
const MAX_REMATRIX_BANDS: usize = 4;

/// Неизменные в пределах кадра параметры, нужные разбору блока E-AC-3.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EnhancedBlockContext<'a> {
    /// Общие с AC-3 параметры: режим, каналы, LFE, строка таблиц, номер блока.
    pub(crate) base: BlockContext,
    /// Разобранный `audfrm`.
    pub(crate) frame: &'a AudioFrame,
    /// `strmtyp = 0`: есть поля конвертера (`convsnroffste`).
    pub(crate) independent: bool,
}

/// Изменяемые ресурсы разбора блока, принадлежащие кадру или декодеру.
pub(crate) struct BlockResources<'a> {
    pub(crate) state: &'a mut EnhancedBlockState,
    pub(crate) aht: &'a mut AhtFrameStore,
    pub(crate) dither: &'a mut DitherGenerator,
    pub(crate) spx_noise: &'a mut SpxNoiseGenerator,
    pub(crate) usage: &'a mut EnhancedToolUsage,
}

/// Разбирает один аудиоблок E-AC-3 и пишет его коэффициенты в `output`.
pub(crate) fn decode_enhanced_audio_block(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    resources: BlockResources<'_>,
    output: &mut DecodedAudioBlock,
) -> Result<(), Ac3DecodeError> {
    let BlockResources {
        state,
        aht,
        dither,
        spx_noise,
        usage,
    } = resources;
    let flags = read_block_switch_and_dither(reader, context)?;
    read_dynamic_range(reader, context, state)?;
    let spx = spx_fields::read_spx_strategy(reader, context, state)?;
    if let Some(strategy) = spx.as_ref() {
        spx_fields::read_spx_coordinates(reader, context, strategy, state)?;
    }
    let coupling = coupling_fields::read_coupling_strategy(reader, context, spx.as_ref(), state)?;
    if let Some(config) = coupling.as_ref() {
        coupling_fields::read_coupling_parameters(reader, context, config, state)?;
    }
    let rematrix = if context.base.mode == AudioCodingMode::Stereo {
        Some(read_rematrix(
            reader,
            context,
            coupling.as_ref(),
            spx.as_ref(),
            state,
        )?)
    } else {
        None
    };
    exponent_fields::read_exponents(reader, context, coupling.as_ref(), spx.as_ref(), state)?;
    allocation_fields::read_allocation_parameters(reader, context, coupling.is_some(), state)?;
    let mut coupling_channel = CouplingChannelMantissas {
        mantissas: [0.0; COEFFICIENTS_PER_BLOCK],
        pointers: [0; COEFFICIENTS_PER_BLOCK],
    };
    read_block_mantissas(
        reader,
        context,
        coupling.as_ref(),
        &flags,
        MantissaTargets {
            state,
            aht,
            dither,
            output,
            coupling_channel: &mut coupling_channel,
        },
    )?;
    if let Some(config) = coupling.as_ref() {
        reconstruction::decouple_channels(
            context,
            config,
            &coupling_channel,
            &flags,
            state,
            dither,
            output,
        )?;
    }
    let region_start = joint_region_start_bin(coupling.as_ref(), spx.as_ref());
    restore_left_right_before_extension(rematrix, region_start, output);
    if let Some(strategy) = spx.as_ref() {
        reconstruction::extend_channels(context, strategy, state, spx_noise, output)?;
    }
    output.dynamic_range_word = state.dynamic_range_words[0];
    output.dual_mono_dynamic_range_word = state.dynamic_range_words[1];
    // Rematrixing уже применён; синтезу больше нечего делать с полосами.
    output.rematrix = None;
    output.coupling_begin_subband = None;
    usage.record_block(coupling.as_ref(), spx.is_some());
    Ok(())
}

/// `blksw[ch]` (при `blkswe`, иначе 0) и `dithflag[ch]` (при `dithflage`, иначе 1).
fn read_block_switch_and_dither(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
) -> Result<ChannelFlags, Ac3DecodeError> {
    let overrun = context.base.overrun();
    let channels = context.base.channel_count;
    let mut flags = ChannelFlags {
        transform: [TransformLength::Long; MAX_FULL_BANDWIDTH_CHANNELS],
        dither: [ChannelDither::Requested; MAX_FULL_BANDWIDTH_CHANNELS],
    };
    if context.frame.syntax.block_switch {
        for transform in flags.transform.iter_mut().take(channels) {
            if reader.read_flag().map_err(&overrun)? {
                *transform = TransformLength::ShortPair;
            }
        }
    }
    if context.frame.syntax.dither_flags {
        for dither in flags.dither.iter_mut().take(channels) {
            *dither = ChannelDither::from_flag(reader.read_flag().map_err(&overrun)?);
        }
    }
    Ok(flags)
}

/// `dynrnge/dynrng` и (1+1) `dynrng2e/dynrng2`; без слова — прежнее значение.
fn read_dynamic_range(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    state: &mut EnhancedBlockState,
) -> Result<(), Ac3DecodeError> {
    let overrun = context.base.overrun();
    let programs = if context.base.mode == AudioCodingMode::DualMono {
        2
    } else {
        1
    };
    for word in state.dynamic_range_words.iter_mut().take(programs) {
        if reader.read_flag().map_err(&overrun)? {
            *word = reader.read_u8(8).map_err(&overrun)?;
        }
    }
    Ok(())
}

/// `nrematbd` по §3.3.2 (зависит от coupling, ECPL и SPX).
fn rematrix_band_count(
    coupling: Option<&EnhancedCouplingConfig>,
    spx: Option<&SpxStrategy>,
) -> usize {
    match (coupling.map(|config| &config.mode), spx) {
        (Some(CouplingMode::Enhanced(strategy)), _) => match strategy.begin_code {
            0 => 0,
            1 => 1,
            2 => 2,
            3 | 4 => 3,
            _ => 4,
        },
        (Some(CouplingMode::Standard { layout, .. }), _) => match layout.begin_subband {
            0 => 2,
            1 | 2 => 3,
            _ => 4,
        },
        (None, Some(strategy)) if strategy.begin_code < 2 => 3,
        _ => MAX_REMATRIX_BANDS,
    }
}

/// `rematstr` (в блоке 0 — неявно 1) и `rematflg[bnd]` для `nrematbd` полос.
fn read_rematrix(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    coupling: Option<&EnhancedCouplingConfig>,
    spx: Option<&SpxStrategy>,
    state: &mut EnhancedBlockState,
) -> Result<EnhancedRematrix, Ac3DecodeError> {
    let band_count = rematrix_band_count(coupling, spx);
    let overrun = context.base.overrun();
    let present = context.base.block.get() == 0 || reader.read_flag().map_err(&overrun)?;
    if present {
        let mut flags = [false; MAX_REMATRIX_BANDS];
        for flag in flags.iter_mut().take(band_count) {
            *flag = reader.read_flag().map_err(&overrun)?;
        }
        let rematrix = EnhancedRematrix { band_count, flags };
        state.rematrix = Some(rematrix);
        return Ok(rematrix);
    }
    state
        .rematrix
        .filter(|previous| previous.band_count == band_count)
        .ok_or_else(|| {
            context.base.violation(BitstreamViolation::InvalidReuse(
                ReusedElement::RematrixFlags,
            ))
        })
}

/// Первый бин области, где каналы кодированы совместно (coupling, ECPL или
/// SPX без coupling): там rematrixing заканчивается (Таблицы 7.26–7.28, §3.3.2).
fn joint_region_start_bin(
    coupling: Option<&EnhancedCouplingConfig>,
    spx: Option<&SpxStrategy>,
) -> Option<usize> {
    coupling
        .map(EnhancedCouplingConfig::start_bin)
        .or_else(|| spx.map(|strategy| strategy.layout.begin_bin()))
}

/// Восстанавливает L/R (§7.5.4) до синтеза SPX общим шагом rematrixing
/// (`synthesis::rematrix`); полосы E-AC-3 переводятся в его форму.
fn restore_left_right_before_extension(
    rematrix: Option<EnhancedRematrix>,
    region_start: Option<usize>,
    output: &mut DecodedAudioBlock,
) {
    let (Some(flags), equivalent_begin) = rematrix_for_synthesis(rematrix, region_start) else {
        return;
    };
    let bandwidth = StereoPairBandwidth {
        coupling_begin_subband: equivalent_begin,
        left_end_bin: output.full_bandwidth[0].end_bin,
        right_end_bin: output.full_bandwidth[1].end_bin,
    };
    let (left, right) = output.full_bandwidth.split_at_mut(1);
    if let (Some(left), Some(right)) = (left.first_mut(), right.first_mut()) {
        restore_left_right(
            &mut left.coefficients,
            &mut right.coefficients,
            &flags,
            bandwidth,
        );
    }
}

/// Переводит rematrixing E-AC-3 в форму общего шага rematrixing:
/// флаги с числом полос и «эквивалентный `cplbegf`», задающий край последней
/// полосы как `37 + 12·cplbegf` (Таблицы 7.26–7.28).
///
/// Для любой области, начинающейся на бине ≥ 37, это точное представление:
/// и границы ECPL ≥ 37, и границы SPX лежат на сетке coupling sub-band-ов
/// (Таблицы E3.7, E3.13). Единственный случай вне сетки — ECPL с
/// `ecplbegf = 1` (`nrematbd = 1`, область с бина 25): полоса 0 (13–24) там
/// не последняя по форме — добавляем выключенную полосу 1, и полоса 0
/// кончается на бине 24, как в Таблице 7.25.
fn rematrix_for_synthesis(
    rematrix: Option<EnhancedRematrix>,
    region_start: Option<usize>,
) -> (Option<RematrixFlags>, Option<usize>) {
    let Some(rematrix) = rematrix else {
        return (None, region_start.map(equivalent_coupling_begin));
    };
    let mut flags = RematrixFlags {
        band_count: rematrix.band_count,
        flags: rematrix.flags,
    };
    let equivalent = region_start.map(equivalent_coupling_begin);
    if region_start.is_some_and(|bin| bin < COUPLING_FIRST_BIN) && rematrix.band_count == 1 {
        flags.band_count = 2;
        flags.flags[1] = false;
    }
    (Some(flags), equivalent)
}

/// `(bin − 37) / 12` для границы на сетке coupling sub-band-ов (0 ниже бина 37).
fn equivalent_coupling_begin(bin: usize) -> usize {
    bin.saturating_sub(COUPLING_FIRST_BIN) / COUPLING_SUBBAND_WIDTH
}

#[cfg(test)]
mod tests {
    use super::{EnhancedRematrix, rematrix_for_synthesis};

    #[test]
    fn rematrix_mapping_reproduces_region_edges_for_synthesis() {
        let all = |band_count| EnhancedRematrix {
            band_count,
            flags: [true; 4],
        };
        // SPX с бина 49 (spxbegf 0): 3 полосы, край последней — 37 + 12·1 = 49.
        let (flags, begin) = rematrix_for_synthesis(Some(all(3)), Some(49));
        assert_eq!((flags.unwrap().band_count, begin), (3, Some(1)));
        // ECPL с бина 25: одна полоса 13–24, дополненная выключенной полосой 1.
        let (flags, begin) = rematrix_for_synthesis(Some(all(1)), Some(25));
        let flags = flags.unwrap();
        assert_eq!(
            (flags.band_count, flags.flags[0], flags.flags[1]),
            (2, true, false)
        );
        assert_eq!(begin, Some(0));
        // ECPL с бина 13: полос нет.
        let (flags, _) = rematrix_for_synthesis(Some(all(0)), Some(13));
        assert_eq!(flags.unwrap().band_count, 0);
        // Без совместного кодирования — 4 полосы до конца спектра.
        assert_eq!(rematrix_for_synthesis(Some(all(4)), None).1, None);
    }
}
