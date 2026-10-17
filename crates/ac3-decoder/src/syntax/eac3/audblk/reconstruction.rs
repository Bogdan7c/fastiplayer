//! Восстановление спектра каналов блока E-AC-3 после чтения мантисс:
//! coupling (стандартное §7.4 или ECPL Annex E §3.5) и затем спектральное
//! расширение (Annex E §3.6). Порядок важен: область копирования SPX может
//! лежать внутри coupling, поэтому SPX работает с уже восстановленными
//! коэффициентами (§3.6.1: coupling «mid-range», SPX — выше).

use super::EnhancedBlockContext;
use super::mantissas::{ChannelFlags, CouplingChannelMantissas};
use super::state::{CouplingMode, EnhancedBlockState, EnhancedCouplingConfig, SpxStrategy};
use crate::error::{Ac3DecodeError, BitstreamViolation, ReusedElement};
use crate::spectral::coupling::{
    ChannelCouplingGains, CouplingChannelView, MAX_COUPLING_SUBBANDS, decouple_channel,
};
use crate::spectral::dither::DitherGenerator;
use crate::spectral::ecpl::{EcplChannelView, decouple_enhanced_channel};
use crate::spectral::spx::{SpxNoiseGenerator, SpxSynthesis, synthesize_extension};
use crate::syntax::audblk::output::DecodedAudioBlock;

/// Восстанавливает coupled области каналов блока.
pub(super) fn decouple_channels(
    context: &EnhancedBlockContext<'_>,
    config: &EnhancedCouplingConfig,
    coupling_channel: &CouplingChannelMantissas,
    flags: &ChannelFlags,
    state: &EnhancedBlockState,
    dither: &mut DitherGenerator,
    output: &mut DecodedAudioBlock,
) -> Result<(), Ac3DecodeError> {
    let exponents = state.coupling_exponents.as_ref().ok_or_else(|| {
        context.base.violation(BitstreamViolation::InvalidReuse(
            ReusedElement::CouplingExponents,
        ))
    })?;
    let missing = |element| {
        context
            .base
            .violation(BitstreamViolation::InvalidReuse(element))
    };
    let no_phase_inversion = [false; MAX_COUPLING_SUBBANDS];
    for channel in 0..context.base.channel_count {
        if !config.in_coupling[channel] {
            continue;
        }
        let spectrum = &mut output.full_bandwidth[channel];
        match &config.mode {
            CouplingMode::Standard { layout, .. } => {
                let coordinates = state.coupling_coordinates[channel]
                    .as_ref()
                    .ok_or_else(|| missing(ReusedElement::CouplingCoordinates))?;
                let view = CouplingChannelView {
                    mantissas: &coupling_channel.mantissas,
                    bap: &coupling_channel.pointers,
                    exponents: &exponents.exponents,
                    begin_subband: layout.begin_subband,
                    end_subband: layout.end_subband,
                };
                // Фазовые флаги — только у правого канала режима 2/0 (§7.4.1).
                let phase_inverted = if channel == 1 {
                    &state.phase_inverted
                } else {
                    &no_phase_inversion
                };
                let gains = ChannelCouplingGains {
                    coordinates: &coordinates.per_subband,
                    phase_inverted,
                    dither: flags.dither[channel],
                };
                decouple_channel(&view, &gains, dither, &mut spectrum.coefficients);
            }
            CouplingMode::Enhanced(strategy) => {
                let amplitudes = state.ecpl_amplitudes[channel]
                    .as_ref()
                    .ok_or_else(|| missing(ReusedElement::EnhancedCouplingAmplitudes))?;
                let view = EcplChannelView {
                    mantissas: &coupling_channel.mantissas,
                    bap: &coupling_channel.pointers,
                    exponents: &exponents.exponents,
                    layout: &strategy.layout,
                };
                decouple_enhanced_channel(
                    &view,
                    &amplitudes.per_band,
                    flags.dither[channel],
                    dither,
                    &mut spectrum.coefficients,
                );
            }
        }
        spectrum.end_bin = config.end_bin();
    }
    Ok(())
}

/// Синтезирует области SPX каналов блока.
pub(super) fn extend_channels(
    context: &EnhancedBlockContext<'_>,
    strategy: &SpxStrategy,
    state: &EnhancedBlockState,
    noise: &mut SpxNoiseGenerator,
    output: &mut DecodedAudioBlock,
) -> Result<(), Ac3DecodeError> {
    for channel in 0..context.base.channel_count {
        if !strategy.in_spx[channel] {
            continue;
        }
        let stored = state.spx_bands[channel].as_ref().ok_or_else(|| {
            context.base.violation(BitstreamViolation::InvalidReuse(
                ReusedElement::SpectralExtensionCoordinates,
            ))
        })?;
        let synthesis = SpxSynthesis {
            layout: &strategy.layout,
            copy_start_subband: strategy.copy_start_subband,
            bands: &stored.bands,
            attenuation_code: context.frame.summary.spectral_extension_attenuation[channel],
        };
        let spectrum = &mut output.full_bandwidth[channel];
        synthesize_extension(&synthesis, noise, &mut spectrum.coefficients);
        spectrum.end_bin = strategy.layout.end_bin();
    }
    Ok(())
}
