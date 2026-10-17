//! Стратегии экспонент, `chbwcod` и сами экспоненты аудиоблока
//! (A/52 §5.3.3, §5.4.3.21–5.4.3.29; декодирование — §7.1.3).

use super::BlockContext;
use super::state::{CouplingStrategy, ExponentSet, FrameBlockState};
use crate::bitstream::BitReader;
use crate::error::{Ac3DecodeError, BitstreamViolation, ReusedElement};
use crate::spectral::exponents::{
    ExponentDecodeError, ExponentStrategy, GROUPED_EXPONENT_BITS, MAX_EXPONENT_GROUPS,
    coupling_group_count, expand_exponent_groups, full_bandwidth_group_count,
};
use crate::spectral::{COEFFICIENTS_PER_BLOCK, MAX_FULL_BANDWIDTH_CHANNELS};

/// Наибольший допустимый `chbwcod` (§5.4.3.24).
pub(crate) const MAX_CHANNEL_BANDWIDTH_CODE: u8 = 60;
/// Последний бин LFE: `lfeendmant = 7` (§7.1.3).
pub(crate) const LFE_END_MANTISSA: usize = 7;
/// `nlfegrps = 2` (§7.1.3).
const LFE_GROUP_COUNT: usize = 2;

/// `endmant = ((chbwcod + 12) · 3) + 37` для канала вне coupling (§7.1.3).
pub(crate) const fn uncoupled_end_mantissa(bandwidth_code: u8) -> usize {
    (bandwidth_code as usize + 12) * 3 + 37
}

/// Стратегии экспонент всех наборов блока.
struct ExponentStrategies {
    coupling: ExponentStrategy,
    channels: [ExponentStrategy; MAX_FULL_BANDWIDTH_CHANNELS],
    lfe: ExponentStrategy,
}

/// Читает стратегии, `chbwcod` и все новые экспоненты блока, обновляя
/// [`FrameBlockState`]; проверяет правила reuse (§7.10.2 условия 6–11).
pub(crate) fn read_exponents(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    coupling: Option<&CouplingStrategy>,
    state: &mut FrameBlockState,
) -> Result<(), Ac3DecodeError> {
    let strategies = read_strategies(reader, context, coupling)?;
    let channel_ends = resolve_channel_ends(reader, context, coupling, &strategies, state)?;
    if let Some(strategy) = coupling {
        read_coupling_exponents(reader, context, strategy, strategies.coupling, state)?;
    }
    let per_channel = strategies.channels.iter().zip(channel_ends.iter());
    for (channel, (&strategy, &end)) in per_channel.enumerate().take(context.channel_count) {
        read_channel_exponents(reader, context, channel, strategy, end, state)?;
    }
    if context.lfe_on {
        read_lfe_exponents(reader, context, strategies.lfe, state)?;
    }
    Ok(())
}

fn read_strategies(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    coupling: Option<&CouplingStrategy>,
) -> Result<ExponentStrategies, Ac3DecodeError> {
    let overrun = context.overrun();
    let coupling_strategy = if coupling.is_some() {
        ExponentStrategy::from_code(reader.read_u8(2).map_err(&overrun)?)
    } else {
        ExponentStrategy::Reuse
    };
    let mut channels = [ExponentStrategy::Reuse; MAX_FULL_BANDWIDTH_CHANNELS];
    for strategy in channels.iter_mut().take(context.channel_count) {
        *strategy = ExponentStrategy::from_code(reader.read_u8(2).map_err(&overrun)?);
    }
    let lfe = if context.lfe_on {
        ExponentStrategy::from_lfe_flag(reader.read_flag().map_err(&overrun)?)
    } else {
        ExponentStrategy::Reuse
    };
    Ok(ExponentStrategies {
        coupling: coupling_strategy,
        channels,
        lfe,
    })
}

/// Определяет `endmant` каждого fbw канала, читая `chbwcod` там, где он есть.
///
/// Для канала с reuse `chbwcod` не передаётся: `endmant` берётся из прошлых
/// экспонент; для coupled канала он обязан совпасть с `cplstrtmant`
/// (условие 9 — экспоненты нельзя переиспользовать при смене числа мантисс).
fn resolve_channel_ends(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    coupling: Option<&CouplingStrategy>,
    strategies: &ExponentStrategies,
    state: &FrameBlockState,
) -> Result<[usize; MAX_FULL_BANDWIDTH_CHANNELS], Ac3DecodeError> {
    let mut ends = [0usize; MAX_FULL_BANDWIDTH_CHANNELS];
    for (channel, end) in ends.iter_mut().enumerate().take(context.channel_count) {
        let coupled_start = coupling
            .filter(|strategy| strategy.in_coupling[channel])
            .map(|strategy| strategy.layout.start_mantissa());
        *end = if strategies.channels[channel] == ExponentStrategy::Reuse {
            let previous = state.channel_exponents[channel]
                .as_ref()
                .map(|set| set.end)
                .ok_or_else(|| {
                    context.violation(BitstreamViolation::InvalidReuse(
                        ReusedElement::ChannelExponents,
                    ))
                })?;
            if coupled_start.is_some_and(|start| start != previous) {
                return Err(context.violation(BitstreamViolation::InvalidReuse(
                    ReusedElement::ChannelExponents,
                )));
            }
            previous
        } else if let Some(start) = coupled_start {
            start
        } else {
            let code = reader.read_u8(6).map_err(context.overrun())?;
            if code > MAX_CHANNEL_BANDWIDTH_CODE {
                return Err(
                    context.violation(BitstreamViolation::ChannelBandwidthCodeOutOfRange { code })
                );
            }
            uncoupled_end_mantissa(code)
        };
    }
    Ok(ends)
}

/// `cplabsexp` + `cplexps[ncplgrps]`.
fn read_coupling_exponents(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    coupling: &CouplingStrategy,
    strategy: ExponentStrategy,
    state: &mut FrameBlockState,
) -> Result<(), Ac3DecodeError> {
    let start = coupling.layout.start_mantissa();
    let end = coupling.layout.end_mantissa();
    let Some(bins_per_difference) = strategy.bins_per_difference() else {
        // Reuse: экспоненты должны быть и относиться к тому же диапазону
        // (условия 6, 7; при включении coupling они уже забыты).
        let reusable = state
            .coupling_exponents
            .is_some_and(|set| set.start == start && set.end == end);
        if !reusable {
            return Err(context.violation(BitstreamViolation::InvalidReuse(
                ReusedElement::CouplingExponents,
            )));
        }
        return Ok(());
    };
    // cplabsexp — 4 бита, соответствует чётной 5-битной экспоненте (§7.1.3).
    let reference = reader.read_u8(4).map_err(context.overrun())? << 1;
    let mut set = ExponentSet {
        exponents: [0; COEFFICIENTS_PER_BLOCK],
        start,
        end,
    };
    let group_count = coupling_group_count(strategy, start, end);
    decode_groups(
        reader,
        context,
        reference,
        group_count,
        bins_per_difference,
        start,
        &mut set,
    )?;
    state.coupling_exponents = Some(set);
    Ok(())
}

/// `exps[ch][0]` + `exps[ch][1..=nchgrps]` + `gainrng[ch]`.
fn read_channel_exponents(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    channel: usize,
    strategy: ExponentStrategy,
    end: usize,
    state: &mut FrameBlockState,
) -> Result<(), Ac3DecodeError> {
    let Some(bins_per_difference) = strategy.bins_per_difference() else {
        // Reuse уже проверен в resolve_channel_ends.
        return Ok(());
    };
    let overrun = context.overrun();
    let reference = reader.read_u8(4).map_err(&overrun)?;
    let mut set = ExponentSet {
        exponents: [0; COEFFICIENTS_PER_BLOCK],
        start: 0,
        end,
    };
    set.exponents[0] = reference;
    let group_count = full_bandwidth_group_count(strategy, end);
    decode_groups(
        reader,
        context,
        reference,
        group_count,
        bins_per_difference,
        1,
        &mut set,
    )?;
    // gainrng (§5.4.3.28) нужен только фиксированной арифметике обратного
    // преобразования (§7.9.5); f32-синтез его не использует — пропускаем.
    reader.skip_bits(2).map_err(&overrun)?;
    if let Some(slot) = state.channel_exponents.get_mut(channel) {
        *slot = Some(set);
    }
    Ok(())
}

/// `lfeexps[0]` + `lfeexps[1..=2]` (D15, 7 экспонент).
fn read_lfe_exponents(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    strategy: ExponentStrategy,
    state: &mut FrameBlockState,
) -> Result<(), Ac3DecodeError> {
    let Some(bins_per_difference) = strategy.bins_per_difference() else {
        if state.lfe_exponents.is_none() {
            return Err(context.violation(BitstreamViolation::InvalidReuse(
                ReusedElement::LfeExponents,
            )));
        }
        return Ok(());
    };
    let reference = reader.read_u8(4).map_err(context.overrun())?;
    let mut set = ExponentSet {
        exponents: [0; COEFFICIENTS_PER_BLOCK],
        start: 0,
        end: LFE_END_MANTISSA,
    };
    set.exponents[0] = reference;
    decode_groups(
        reader,
        context,
        reference,
        LFE_GROUP_COUNT,
        bins_per_difference,
        1,
        &mut set,
    )?;
    state.lfe_exponents = Some(set);
    Ok(())
}

/// Читает `group_count` 7-битных групп и разворачивает их в `set`.
pub(crate) fn decode_groups(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    reference: u8,
    group_count: usize,
    bins_per_difference: usize,
    first_bin: usize,
    set: &mut ExponentSet,
) -> Result<(), Ac3DecodeError> {
    let mut codes = [0u8; MAX_EXPONENT_GROUPS];
    let group_count = group_count.min(MAX_EXPONENT_GROUPS);
    for code in codes.iter_mut().take(group_count) {
        *code = reader
            .read_u8(GROUPED_EXPONENT_BITS)
            .map_err(context.overrun())?;
    }
    let grouped = codes.get(..group_count).unwrap_or_default();
    expand_exponent_groups(
        reference,
        grouped,
        bins_per_difference,
        first_bin,
        &mut set.exponents,
    )
    .map_err(|error| {
        context.violation(match error {
            ExponentDecodeError::GroupedCodeOutOfRange { code } => {
                BitstreamViolation::GroupedExponentOutOfRange { code }
            }
            ExponentDecodeError::ExponentOutOfRange => BitstreamViolation::ExponentOutOfRange,
        })
    })
}
