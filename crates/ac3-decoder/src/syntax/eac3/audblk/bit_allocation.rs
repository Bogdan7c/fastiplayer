//! Указатели квантователей блока E-AC-3: `bap` обычных наборов экспонент и
//! `hebap` наборов с AHT (A/52 §7.2.2; Annex E §3.4.3.1).
//!
//! Алгоритм распределения — общий шаг `spectral::bit_allocation`; здесь
//! только сбор входов из состояния кадра E-AC-3 (SNR offset-ы по стратегии
//! кадра, fast gain по `fgaincode`, delta bit allocation, leak coupling).

use super::EnhancedBlockContext;
use super::state::{EnhancedBlockState, EnhancedCouplingConfig};
use crate::error::{Ac3DecodeError, BitstreamViolation, ReusedElement};
use crate::spectral::COEFFICIENTS_PER_BLOCK;
use crate::spectral::bit_allocation::{
    AllocationChannelKind, AllocationPointerTable, BitAllocationParameters, ChannelAllocationInput,
    compute_bit_allocation_with_table, fast_gain, snr_offset,
};
use crate::syntax::audblk::state::{DeltaAllocation, ExponentSet};

/// Какой набор экспонент распределяется.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExponentSetId {
    Channel(usize),
    Coupling,
    Lfe,
}

/// Указатели (`bap` или `hebap`) одного набора экспонент.
pub(super) type AllocationPointers = [u8; COEFFICIENTS_PER_BLOCK];

/// Экспоненты набора из состояния (после `read_exponents` они обязаны быть).
pub(super) fn exponent_set<'a>(
    context: &EnhancedBlockContext<'_>,
    state: &'a EnhancedBlockState,
    set: ExponentSetId,
) -> Result<&'a ExponentSet, Ac3DecodeError> {
    let (stored, element) = match set {
        ExponentSetId::Channel(channel) => (
            state
                .channel_exponents
                .get(channel)
                .and_then(Option::as_ref),
            ReusedElement::ChannelExponents,
        ),
        ExponentSetId::Coupling => (
            state.coupling_exponents.as_ref(),
            ReusedElement::CouplingExponents,
        ),
        ExponentSetId::Lfe => (state.lfe_exponents.as_ref(), ReusedElement::LfeExponents),
    };
    stored.ok_or_else(|| {
        context
            .base
            .violation(BitstreamViolation::InvalidReuse(element))
    })
}

/// §7.2.2.1.1: при нулевом `csnroffst` и нулевых `fsnroffst` всех наборов
/// блока все указатели равны нулю.
pub(super) fn all_snr_offsets_are_zero(
    context: &EnhancedBlockContext<'_>,
    coupling: Option<&EnhancedCouplingConfig>,
    state: &EnhancedBlockState,
) -> bool {
    let snr = &state.snr;
    let channels_zero = snr
        .channels
        .iter()
        .take(context.base.channel_count)
        .all(|fine| *fine == Some(0));
    let coupling_zero = coupling.is_none() || snr.coupling == Some(0);
    let lfe_zero = !context.base.lfe_on || snr.lfe == Some(0);
    snr.coarse == Some(0) && channels_zero && coupling_zero && lfe_zero
}

/// Считает указатели набора `set` по таблице `table` в `pointers`.
pub(super) fn allocate_set(
    context: &EnhancedBlockContext<'_>,
    state: &EnhancedBlockState,
    set: ExponentSetId,
    table: AllocationPointerTable,
    pointers: &mut AllocationPointers,
) -> Result<(), Ac3DecodeError> {
    *pointers = [0; COEFFICIENTS_PER_BLOCK];
    let parameters: &BitAllocationParameters = state.bit_allocation.as_ref().ok_or_else(|| {
        context.base.violation(BitstreamViolation::InvalidReuse(
            ReusedElement::BitAllocationParameters,
        ))
    })?;
    let snr_error = || {
        context
            .base
            .violation(BitstreamViolation::InvalidReuse(ReusedElement::SnrOffsets))
    };
    let coarse = state.snr.coarse.ok_or_else(snr_error)?;
    let (fine, gain_code, kind, delta): (Option<u8>, u8, AllocationChannelKind, &DeltaAllocation) =
        match set {
            ExponentSetId::Channel(channel) => (
                state.snr.channels.get(channel).copied().flatten(),
                state
                    .fast_gain
                    .channels
                    .get(channel)
                    .copied()
                    .unwrap_or_default(),
                AllocationChannelKind::StartsAtDc,
                state
                    .channel_deltas
                    .get(channel)
                    .unwrap_or(&DeltaAllocation::Inactive),
            ),
            ExponentSetId::Coupling => {
                let leak = state.coupling_leak.ok_or_else(|| {
                    context.base.violation(BitstreamViolation::InvalidReuse(
                        ReusedElement::CouplingLeak,
                    ))
                })?;
                (
                    state.snr.coupling,
                    state.fast_gain.coupling,
                    AllocationChannelKind::Coupling(leak),
                    &state.coupling_delta,
                )
            }
            // LFE не имеет delta bit allocation (§7.2.2.6).
            ExponentSetId::Lfe => (
                state.snr.lfe,
                state.fast_gain.lfe,
                AllocationChannelKind::StartsAtDc,
                &DeltaAllocation::Inactive,
            ),
        };
    let fine = fine.ok_or_else(snr_error)?;
    let exponents = exponent_set(context, state, set)?;
    let input = ChannelAllocationInput {
        exponents: &exponents.exponents,
        start: exponents.start,
        end: exponents.end,
        fast_gain: fast_gain(gain_code),
        snr_offset: snr_offset(coarse, fine),
        kind,
        deltas: delta.segments(),
    };
    compute_bit_allocation_with_table(
        parameters,
        context.base.sample_rate,
        &input,
        table,
        pointers,
    )
    .map_err(|_| {
        context
            .base
            .violation(BitstreamViolation::DeltaBitAllocationBeyondBands)
    })
}
