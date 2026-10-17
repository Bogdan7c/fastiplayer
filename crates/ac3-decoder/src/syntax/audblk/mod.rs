//! Разбор аудиоблока `audblk` (A/52 §5.3.3, Таблица 5.3).
//!
//! Порядок полей строго как в Таблице 5.3; каждая группа полей вынесена в
//! свой модуль:
//!
//! | поля Таблицы 5.3                                   | модуль               |
//! |----------------------------------------------------|----------------------|
//! | `blksw`, `dithflag`, `dynrng*`, `rematstr`, `skip*` | этот файл            |
//! | `cplstre` … `phsflg`                               | `coupling_fields`    |
//! | `cplexpstr` … `lfeexps`                            | `exponent_fields`    |
//! | `baie` … `deltba`                                  | `allocation_fields`  |
//! | `chmant`, `cplmant`, `lfemant`                     | `mantissas`          |
//!
//! Состояние, переходящее между блоками кадра, — `state::FrameBlockState`.

pub(crate) mod allocation_fields;
mod coupling_fields;
pub(crate) mod exponent_fields;
mod mantissas;
pub(crate) mod output;
pub(crate) mod state;

use self::mantissas::ChannelFlags;
use self::output::{DecodedAudioBlock, TransformLength};
use self::state::{CouplingStrategy, FrameBlockState, RematrixFlags};
use crate::bitstream::{BitReader, ReadPastLimit};
use crate::error::{
    Ac3DecodeError, AudioBlockIndex, BitstreamViolation, FrameSection, ReusedElement,
};
use crate::spectral::MAX_FULL_BANDWIDTH_CHANNELS;
use crate::spectral::dither::{ChannelDither, DitherGenerator};
use crate::syntax::bsi_values::AudioCodingMode;
use crate::tables::frame_size::SampleRateCode;

/// Неизменные в пределах кадра параметры, нужные разбору блока.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BlockContext {
    pub(crate) mode: AudioCodingMode,
    /// `nfchans`.
    pub(crate) channel_count: usize,
    pub(crate) lfe_on: bool,
    /// `fscod` — строка порога слышимости (Таблица 7.15).
    pub(crate) sample_rate: SampleRateCode,
    pub(crate) block: AudioBlockIndex,
}

impl BlockContext {
    pub(crate) const fn section(&self) -> FrameSection {
        FrameSection::AudioBlock(self.block)
    }

    /// Преобразователь «кончились биты» → типизированная ошибка этого блока.
    pub(crate) fn overrun(&self) -> impl Fn(ReadPastLimit) -> Ac3DecodeError + use<> {
        let section = self.section();
        move |_| Ac3DecodeError::BitstreamOverrun { section }
    }

    pub(crate) const fn violation(&self, violation: BitstreamViolation) -> Ac3DecodeError {
        Ac3DecodeError::in_block(self.block, violation)
    }
}

/// Разбирает один аудиоблок и пишет его коэффициенты в `output`.
pub(crate) fn decode_audio_block(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    state: &mut FrameBlockState,
    dither: &mut DitherGenerator,
    output: &mut DecodedAudioBlock,
) -> Result<(), Ac3DecodeError> {
    let flags = read_block_switch_and_dither(reader, context)?;
    read_dynamic_range(reader, context, state)?;
    let coupling = coupling_fields::read_coupling_strategy(reader, context, state)?;
    if let Some(strategy) = coupling.as_ref() {
        coupling_fields::read_coupling_coordinates(reader, context, strategy, state)?;
    }
    let rematrix = if context.mode == AudioCodingMode::Stereo {
        Some(read_rematrix(reader, context, coupling.as_ref(), state)?)
    } else {
        None
    };
    exponent_fields::read_exponents(reader, context, coupling.as_ref(), state)?;
    allocation_fields::read_allocation_parameters(reader, context, coupling.as_ref(), state)?;
    skip_dummy_data(reader, context)?;
    mantissas::decode_mantissas(
        reader,
        context,
        coupling.as_ref(),
        &flags,
        state,
        dither,
        output,
    )?;
    output.dynamic_range_word = state.dynamic_range_words[0];
    output.dual_mono_dynamic_range_word = state.dynamic_range_words[1];
    output.rematrix = rematrix;
    output.coupling_begin_subband = coupling.map(|strategy| strategy.layout.begin_subband);
    Ok(())
}

/// `blksw[ch]`, затем `dithflag[ch]`.
fn read_block_switch_and_dither(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
) -> Result<ChannelFlags, Ac3DecodeError> {
    let overrun = context.overrun();
    let mut flags = ChannelFlags {
        transform: [TransformLength::Long; MAX_FULL_BANDWIDTH_CHANNELS],
        dither: [ChannelDither::Off; MAX_FULL_BANDWIDTH_CHANNELS],
    };
    for transform in flags.transform.iter_mut().take(context.channel_count) {
        *transform = if reader.read_flag().map_err(&overrun)? {
            TransformLength::ShortPair
        } else {
            TransformLength::Long
        };
    }
    for dither in flags.dither.iter_mut().take(context.channel_count) {
        *dither = ChannelDither::from_flag(reader.read_flag().map_err(&overrun)?);
    }
    Ok(flags)
}

/// `dynrnge/dynrng` и, в режиме 1+1, `dynrng2e/dynrng2` (§5.4.3.3–5.4.3.6):
/// без слова действует прежнее значение, а в блоке 0 — ноль (оно уже ноль
/// в начальном состоянии кадра).
fn read_dynamic_range(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    state: &mut FrameBlockState,
) -> Result<(), Ac3DecodeError> {
    let overrun = context.overrun();
    let programs = if context.mode == AudioCodingMode::DualMono {
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

/// `rematstr` и `rematflg[rbnd]` (только 2/0; число полос — Таблица 5.15).
fn read_rematrix(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    coupling: Option<&CouplingStrategy>,
    state: &mut FrameBlockState,
) -> Result<RematrixFlags, Ac3DecodeError> {
    let band_count = rematrix_band_count(coupling);
    let overrun = context.overrun();
    if reader.read_flag().map_err(&overrun)? {
        let mut flags = [false; 4];
        for flag in flags.iter_mut().take(band_count) {
            *flag = reader.read_flag().map_err(&overrun)?;
        }
        let rematrix = RematrixFlags { band_count, flags };
        state.rematrix = Some(rematrix);
        return Ok(rematrix);
    }
    // Reuse запрещён в блоке 0 и при смене числа полос (условия 5, 25 §7.10.2).
    state
        .rematrix
        .filter(|previous| previous.band_count == band_count)
        .ok_or_else(|| {
            context.violation(BitstreamViolation::InvalidReuse(
                ReusedElement::RematrixFlags,
            ))
        })
}

/// Таблица 5.15: 4 полосы без coupling или при `cplbegf > 2`, 3 при
/// `2 ≥ cplbegf > 0`, 2 при `cplbegf = 0`.
fn rematrix_band_count(coupling: Option<&CouplingStrategy>) -> usize {
    match coupling.map(|strategy| strategy.layout.begin_subband) {
        None => 4,
        Some(begin) if begin > 2 => 4,
        Some(0) => 2,
        Some(_) => 3,
    }
}

/// `skiple`, `skipl` (9 бит), `skipfld` (`skipl × 8` бит) — пустые байты
/// перед мантиссами (§5.4.3.58–5.4.3.60). Тот же синтаксис у E-AC-3
/// (Annex E Таблица E1.4, при `skipflde = 1`).
pub(crate) fn skip_dummy_data(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
) -> Result<(), Ac3DecodeError> {
    let overrun = context.overrun();
    if reader.read_flag().map_err(&overrun)? {
        let skip_bytes = usize::from(reader.read_bits(9).map_err(&overrun)?);
        reader.skip_bits(skip_bytes * 8).map_err(&overrun)?;
    }
    Ok(())
}
