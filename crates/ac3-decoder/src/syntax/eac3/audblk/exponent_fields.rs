//! `chbwcod` и экспоненты аудиоблока E-AC-3 (A/52 Annex E Таблица E1.4;
//! `endmant` §3.3.3, `ncplgrps` §3.3.5; декодирование — общий §7.1.3).
//!
//! Стратегии экспонент E-AC-3 передаются в `audfrm`; здесь читаются только
//! сами экспоненты. Отличие от AC-3 — `endmant`: у канала в coupling это
//! начало coupling (стандартного или ECPL), у канала в SPX без coupling —
//! начало SPX, иначе — по `chbwcod`.

use super::EnhancedBlockContext;
use super::state::{EnhancedBlockState, EnhancedCouplingConfig, SpxStrategy};
use crate::bitstream::BitReader;
use crate::error::{Ac3DecodeError, BitstreamViolation, ReusedElement};
use crate::spectral::exponents::{
    ExponentStrategy, coupling_group_count, full_bandwidth_group_count,
};
use crate::spectral::{COEFFICIENTS_PER_BLOCK, MAX_FULL_BANDWIDTH_CHANNELS};
use crate::syntax::audblk::exponent_fields::{
    LFE_END_MANTISSA, MAX_CHANNEL_BANDWIDTH_CODE, decode_groups, uncoupled_end_mantissa,
};
use crate::syntax::audblk::state::ExponentSet;

/// `nlfegrps = 2` (§7.1.3).
const LFE_GROUP_COUNT: usize = 2;

/// Читает `chbwcod` и все новые экспоненты блока.
pub(super) fn read_exponents(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    coupling: Option<&EnhancedCouplingConfig>,
    spx: Option<&SpxStrategy>,
    state: &mut EnhancedBlockState,
) -> Result<(), Ac3DecodeError> {
    let block = usize::from(context.base.block.get());
    let channel_strategies = context.frame.channel_exponent_strategy[block];
    let ends = resolve_channel_ends(reader, context, coupling, spx, &channel_strategies, state)?;
    if let Some(config) = coupling {
        read_coupling_exponents(
            reader,
            context,
            config,
            context.frame.coupling_exponent_strategy[block],
            state,
        )?;
    }
    for channel in 0..context.base.channel_count {
        let strategy = channel_strategies[channel];
        let Some(bins_per_difference) = strategy.bins_per_difference() else {
            continue;
        };
        let overrun = context.base.overrun();
        let reference = reader.read_u8(4).map_err(&overrun)?;
        let mut set = ExponentSet {
            exponents: [0; COEFFICIENTS_PER_BLOCK],
            start: 0,
            end: ends[channel],
        };
        set.exponents[0] = reference;
        let groups = full_bandwidth_group_count(strategy, set.end);
        decode_groups(
            reader,
            &context.base,
            reference,
            groups,
            bins_per_difference,
            1,
            &mut set,
        )?;
        // gainrng — только для целочисленного IMDCT (§7.9.5): пропускаем.
        reader.skip_bits(2).map_err(&overrun)?;
        state.channel_exponents[channel] = Some(set);
    }
    if context.base.lfe_on {
        read_lfe_exponents(
            reader,
            context,
            context.frame.lfe_exponent_strategy[block],
            state,
        )?;
    }
    Ok(())
}

/// `endmant` каждого fbw канала; `chbwcod` читается только у канала вне
/// coupling и SPX с новыми экспонентами (Таблица E1.4).
fn resolve_channel_ends(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    coupling: Option<&EnhancedCouplingConfig>,
    spx: Option<&SpxStrategy>,
    strategies: &[ExponentStrategy; MAX_FULL_BANDWIDTH_CHANNELS],
    state: &EnhancedBlockState,
) -> Result<[usize; MAX_FULL_BANDWIDTH_CHANNELS], Ac3DecodeError> {
    let mut ends = [0usize; MAX_FULL_BANDWIDTH_CHANNELS];
    for (channel, end) in ends.iter_mut().enumerate().take(context.base.channel_count) {
        // §3.3.3: в coupling — начало coupling (ECPL или стандартного), в SPX — начало SPX.
        let derived_end = coupling
            .filter(|config| config.in_coupling[channel])
            .map(EnhancedCouplingConfig::start_bin)
            .or_else(|| {
                spx.filter(|strategy| strategy.in_spx[channel])
                    .map(|strategy| strategy.layout.begin_bin())
            });
        *end = if strategies[channel] == ExponentStrategy::Reuse {
            let previous = state.channel_exponents[channel]
                .as_ref()
                .map(|set| set.end)
                .ok_or_else(|| reuse_error(context, ReusedElement::ChannelExponents))?;
            if derived_end.is_some_and(|expected| expected != previous) {
                return Err(reuse_error(context, ReusedElement::ChannelExponents));
            }
            previous
        } else if let Some(expected) = derived_end {
            expected
        } else {
            let code = reader.read_u8(6).map_err(context.base.overrun())?;
            if code > MAX_CHANNEL_BANDWIDTH_CODE {
                return Err(context
                    .base
                    .violation(BitstreamViolation::ChannelBandwidthCodeOutOfRange { code }));
            }
            uncoupled_end_mantissa(code)
        };
    }
    Ok(ends)
}

/// `cplabsexp` + `cplexps[ncplgrps]` для диапазона coupling/ECPL.
fn read_coupling_exponents(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    config: &EnhancedCouplingConfig,
    strategy: ExponentStrategy,
    state: &mut EnhancedBlockState,
) -> Result<(), Ac3DecodeError> {
    let start = config.start_bin();
    let end = config.end_bin();
    let Some(bins_per_difference) = strategy.bins_per_difference() else {
        let reusable = state
            .coupling_exponents
            .is_some_and(|set| set.start == start && set.end == end);
        if !reusable {
            return Err(reuse_error(context, ReusedElement::CouplingExponents));
        }
        return Ok(());
    };
    // cplabsexp — 4 бита чётной 5-битной экспоненты (§7.1.3).
    let reference = reader.read_u8(4).map_err(context.base.overrun())? << 1;
    let mut set = ExponentSet {
        exponents: [0; COEFFICIENTS_PER_BLOCK],
        start,
        end,
    };
    let groups = coupling_group_count(strategy, start, end);
    decode_groups(
        reader,
        &context.base,
        reference,
        groups,
        bins_per_difference,
        start,
        &mut set,
    )?;
    state.coupling_exponents = Some(set);
    Ok(())
}

/// `lfeexps[0..=2]` (D15, 7 экспонент).
fn read_lfe_exponents(
    reader: &mut BitReader<'_>,
    context: &EnhancedBlockContext<'_>,
    strategy: ExponentStrategy,
    state: &mut EnhancedBlockState,
) -> Result<(), Ac3DecodeError> {
    let Some(bins_per_difference) = strategy.bins_per_difference() else {
        if state.lfe_exponents.is_none() {
            return Err(reuse_error(context, ReusedElement::LfeExponents));
        }
        return Ok(());
    };
    let reference = reader.read_u8(4).map_err(context.base.overrun())?;
    let mut set = ExponentSet {
        exponents: [0; COEFFICIENTS_PER_BLOCK],
        start: 0,
        end: LFE_END_MANTISSA,
    };
    set.exponents[0] = reference;
    decode_groups(
        reader,
        &context.base,
        reference,
        LFE_GROUP_COUNT,
        bins_per_difference,
        1,
        &mut set,
    )?;
    state.lfe_exponents = Some(set);
    Ok(())
}

fn reuse_error(context: &EnhancedBlockContext<'_>, element: ReusedElement) -> Ac3DecodeError {
    context
        .base
        .violation(BitstreamViolation::InvalidReuse(element))
}
