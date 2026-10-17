//! Битовое распределение и мантиссы аудиоблока: от экспонент и параметров
//! к коэффициентам каналов (A/52 §5.3.3 «quantized mantissa values», §7.2–§7.4).

use super::BlockContext;
use super::exponent_fields::LFE_END_MANTISSA;
use super::output::{ChannelSpectrum, DecodedAudioBlock, TransformLength};
use super::state::{
    ChannelSnr, CouplingStrategy, DeltaAllocation, ExponentSet, FrameBlockState, SnrOffsets,
};
use crate::bitstream::BitReader;
use crate::error::{Ac3DecodeError, BitstreamViolation, ReusedElement};
use crate::spectral::bit_allocation::{
    AllocationChannelKind, BitAllocationParameters, ChannelAllocationInput, compute_bit_allocation,
    fast_gain, snr_offset,
};
use crate::spectral::coupling::{
    ChannelCouplingGains, CouplingChannelView, MAX_COUPLING_SUBBANDS, decouple_channel,
};
use crate::spectral::dither::{ChannelDither, DitherGenerator};
use crate::spectral::mantissa::{Mantissa, MantissaReadError, MantissaUnpacker};
use crate::spectral::{COEFFICIENTS_PER_BLOCK, MAX_FULL_BANDWIDTH_CHANNELS, exponent_scale};

/// Флаги блока, нужные при чтении мантисс.
pub(crate) struct ChannelFlags {
    /// `blksw[ch]`.
    pub(crate) transform: [TransformLength; MAX_FULL_BANDWIDTH_CHANNELS],
    /// `dithflag[ch]`.
    pub(crate) dither: [ChannelDither; MAX_FULL_BANDWIDTH_CHANNELS],
}

/// Указатели bap всех наборов экспонент блока.
struct BlockAllocation {
    channels: [[u8; COEFFICIENTS_PER_BLOCK]; MAX_FULL_BANDWIDTH_CHANNELS],
    coupling: [u8; COEFFICIENTS_PER_BLOCK],
    lfe: [u8; COEFFICIENTS_PER_BLOCK],
}

/// Считает bap и читает все мантиссы блока в `output`.
pub(crate) fn decode_mantissas(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    coupling: Option<&CouplingStrategy>,
    flags: &ChannelFlags,
    state: &FrameBlockState,
    dither: &mut DitherGenerator,
    output: &mut DecodedAudioBlock,
) -> Result<(), Ac3DecodeError> {
    let allocation = compute_block_allocation(context, coupling, state)?;
    let mut unpacker = MantissaUnpacker::default();
    let mut coupling_mantissas = [0.0f32; COEFFICIENTS_PER_BLOCK];
    let mut coupling_read = false;
    for channel in 0..context.channel_count {
        let exponents = channel_exponents(context, state, channel)?;
        let spectrum = &mut output.full_bandwidth[channel];
        *spectrum = ChannelSpectrum::silent();
        spectrum.transform = flags.transform[channel];
        spectrum.end_bin = exponents.end;
        let mut channel_values = ChannelValues {
            exponents,
            bap: &allocation.channels[channel],
            dither: flags.dither[channel],
        };
        read_channel_mantissas(
            reader,
            context,
            &mut unpacker,
            &mut channel_values,
            dither,
            &mut spectrum.coefficients,
        )?;
        let coupled = coupling.is_some_and(|strategy| strategy.in_coupling[channel]);
        if coupled && !coupling_read {
            // Мантиссы coupling канала идут сразу после первого coupled канала (Таблица 5.3).
            let set = coupling_exponents(context, state)?;
            read_coupling_mantissas(
                reader,
                context,
                &mut unpacker,
                set,
                &allocation.coupling,
                &mut coupling_mantissas,
            )?;
            coupling_read = true;
        }
    }
    if context.lfe_on {
        let exponents = state.lfe_exponents.as_ref().ok_or_else(|| {
            context.violation(BitstreamViolation::InvalidReuse(
                ReusedElement::LfeExponents,
            ))
        })?;
        output.lfe = ChannelSpectrum::silent();
        output.lfe.end_bin = LFE_END_MANTISSA;
        // У LFE нет dithflag (§5.4.3.2 — флаг только у fbw каналов): нули — настоящие нули.
        let mut lfe_values = ChannelValues {
            exponents,
            bap: &allocation.lfe,
            dither: ChannelDither::Off,
        };
        read_channel_mantissas(
            reader,
            context,
            &mut unpacker,
            &mut lfe_values,
            dither,
            &mut output.lfe.coefficients,
        )?;
    } else {
        // Без LFE канал блока остаётся тишиной, а не данными прошлого кадра.
        output.lfe = ChannelSpectrum::silent();
    }
    if let Some(strategy) = coupling {
        let coupling_channel = CouplingChannelMantissas {
            bap: &allocation.coupling,
            mantissas: &coupling_mantissas,
        };
        decouple_coupled_channels(
            context,
            strategy,
            state,
            &coupling_channel,
            flags,
            dither,
            output,
        )?;
    }
    Ok(())
}

/// Экспоненты fbw канала из состояния (после `read_exponents` они обязаны быть).
fn channel_exponents<'a>(
    context: &BlockContext,
    state: &'a FrameBlockState,
    channel: usize,
) -> Result<&'a ExponentSet, Ac3DecodeError> {
    state
        .channel_exponents
        .get(channel)
        .and_then(Option::as_ref)
        .ok_or_else(|| {
            context.violation(BitstreamViolation::InvalidReuse(
                ReusedElement::ChannelExponents,
            ))
        })
}

fn coupling_exponents<'a>(
    context: &BlockContext,
    state: &'a FrameBlockState,
) -> Result<&'a ExponentSet, Ac3DecodeError> {
    state.coupling_exponents.as_ref().ok_or_else(|| {
        context.violation(BitstreamViolation::InvalidReuse(
            ReusedElement::CouplingExponents,
        ))
    })
}

/// Шаг §7.2.2.1.1: если все SNR offset-ы нулевые, все bap = 0; иначе
/// параметрическое распределение каждого набора экспонент.
fn compute_block_allocation(
    context: &BlockContext,
    coupling: Option<&CouplingStrategy>,
    state: &FrameBlockState,
) -> Result<BlockAllocation, Ac3DecodeError> {
    let mut allocation = BlockAllocation {
        channels: [[0; COEFFICIENTS_PER_BLOCK]; MAX_FULL_BANDWIDTH_CHANNELS],
        coupling: [0; COEFFICIENTS_PER_BLOCK],
        lfe: [0; COEFFICIENTS_PER_BLOCK],
    };
    let parameters = state.bit_allocation.as_ref().ok_or_else(|| {
        context.violation(BitstreamViolation::InvalidReuse(
            ReusedElement::BitAllocationParameters,
        ))
    })?;
    let snr = state.snr_offsets.as_ref().ok_or_else(|| {
        context.violation(BitstreamViolation::InvalidReuse(ReusedElement::SnrOffsets))
    })?;
    let coupling_snr = match coupling {
        Some(_) => Some(state.coupling_snr.ok_or_else(|| {
            context.violation(BitstreamViolation::InvalidReuse(ReusedElement::SnrOffsets))
        })?),
        None => None,
    };
    if all_snr_offsets_are_zero(context, snr, coupling_snr) {
        return Ok(allocation);
    }
    let allocator = Allocator {
        context,
        parameters,
        coarse_offset: snr.coarse_offset,
    };
    for channel in 0..context.channel_count {
        let set = channel_exponents(context, state, channel)?;
        let input = allocator.input(
            set,
            snr.full_bandwidth[channel],
            AllocationChannelKind::StartsAtDc,
            &state.channel_deltas[channel],
        );
        allocator.run(&input, &mut allocation.channels[channel])?;
    }
    if let (Some(_), Some(channel_snr)) = (coupling, coupling_snr) {
        let set = coupling_exponents(context, state)?;
        let leak = state.coupling_leak.ok_or_else(|| {
            context.violation(BitstreamViolation::InvalidReuse(
                ReusedElement::CouplingLeak,
            ))
        })?;
        let input = allocator.input(
            set,
            channel_snr,
            AllocationChannelKind::Coupling(leak),
            &state.coupling_delta,
        );
        allocator.run(&input, &mut allocation.coupling)?;
    }
    if let (true, Some(lfe_snr), Some(set)) =
        (context.lfe_on, snr.lfe, state.lfe_exponents.as_ref())
    {
        // LFE не имеет delta bit allocation (§7.2.2.6: только fbw и coupling).
        let input = allocator.input(
            set,
            lfe_snr,
            AllocationChannelKind::StartsAtDc,
            &DeltaAllocation::Inactive,
        );
        allocator.run(&input, &mut allocation.lfe)?;
    }
    Ok(allocation)
}

fn all_snr_offsets_are_zero(
    context: &BlockContext,
    snr: &SnrOffsets,
    coupling: Option<ChannelSnr>,
) -> bool {
    let fine_offsets_zero = snr
        .full_bandwidth
        .iter()
        .take(context.channel_count)
        .all(|channel| channel.fine_offset == 0);
    let coupling_zero = coupling.is_none_or(|channel| channel.fine_offset == 0);
    let lfe_zero = !context.lfe_on || snr.lfe.is_none_or(|channel| channel.fine_offset == 0);
    snr.coarse_offset == 0 && fine_offsets_zero && coupling_zero && lfe_zero
}

/// Общие параметры распределения одного блока.
struct Allocator<'a> {
    context: &'a BlockContext,
    parameters: &'a BitAllocationParameters,
    coarse_offset: u8,
}

impl<'a> Allocator<'a> {
    fn input(
        &self,
        set: &'a ExponentSet,
        channel_snr: ChannelSnr,
        kind: AllocationChannelKind,
        delta: &'a DeltaAllocation,
    ) -> ChannelAllocationInput<'a> {
        ChannelAllocationInput {
            exponents: &set.exponents,
            start: set.start,
            end: set.end,
            fast_gain: fast_gain(channel_snr.fast_gain_code),
            snr_offset: snr_offset(self.coarse_offset, channel_snr.fine_offset),
            kind,
            deltas: delta.segments(),
        }
    }

    fn run(
        &self,
        input: &ChannelAllocationInput<'_>,
        bap: &mut [u8; COEFFICIENTS_PER_BLOCK],
    ) -> Result<(), Ac3DecodeError> {
        compute_bit_allocation(self.parameters, self.context.sample_rate, input, bap).map_err(
            |_| {
                self.context
                    .violation(BitstreamViolation::DeltaBitAllocationBeyondBands)
            },
        )
    }
}

/// Всё, что нужно для чтения мантисс одного набора экспонент.
struct ChannelValues<'a> {
    exponents: &'a ExponentSet,
    bap: &'a [u8; COEFFICIENTS_PER_BLOCK],
    dither: ChannelDither,
}

/// Мантиссы бинов `start..end` набора; результат — коэффициенты с учётом
/// экспоненты (bap = 0 → dither канала или ноль, §7.3.4).
fn read_channel_mantissas(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    unpacker: &mut MantissaUnpacker,
    values: &mut ChannelValues<'_>,
    dither: &mut DitherGenerator,
    coefficients: &mut [f32; COEFFICIENTS_PER_BLOCK],
) -> Result<(), Ac3DecodeError> {
    let end = values.exponents.end.min(COEFFICIENTS_PER_BLOCK);
    let bins = coefficients
        .iter_mut()
        .enumerate()
        .take(end)
        .skip(values.exponents.start);
    for (bin, coefficient) in bins {
        let mantissa = match read_mantissa(reader, context, unpacker, values.bap[bin])? {
            Mantissa::ZeroBits => dither.zero_bit_mantissa(values.dither),
            Mantissa::Value(value) => value,
        };
        *coefficient = mantissa * exponent_scale(values.exponents.exponents[bin]);
    }
    Ok(())
}

/// Мантиссы coupling канала без экспоненты: экспонента и dither применяются
/// при восстановлении каждого канала (§7.4.3, §7.3.4).
fn read_coupling_mantissas(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    unpacker: &mut MantissaUnpacker,
    set: &ExponentSet,
    bap: &[u8; COEFFICIENTS_PER_BLOCK],
    mantissas: &mut [f32; COEFFICIENTS_PER_BLOCK],
) -> Result<(), Ac3DecodeError> {
    let end = set.end.min(COEFFICIENTS_PER_BLOCK);
    for bin in set.start..end {
        mantissas[bin] = match read_mantissa(reader, context, unpacker, bap[bin])? {
            Mantissa::ZeroBits => 0.0,
            Mantissa::Value(value) => value,
        };
    }
    Ok(())
}

fn read_mantissa(
    reader: &mut BitReader<'_>,
    context: &BlockContext,
    unpacker: &mut MantissaUnpacker,
    bap: u8,
) -> Result<Mantissa, Ac3DecodeError> {
    unpacker.read(reader, bap).map_err(|error| match error {
        MantissaReadError::Overrun => Ac3DecodeError::BitstreamOverrun {
            section: context.section(),
        },
        MantissaReadError::CodeOutOfRange { bap, code } => {
            context.violation(BitstreamViolation::MantissaCodeOutOfRange { bap, code })
        }
    })
}

/// Прочитанный coupling канал блока: указатели bap и мантиссы без экспоненты.
struct CouplingChannelMantissas<'a> {
    bap: &'a [u8; COEFFICIENTS_PER_BLOCK],
    mantissas: &'a [f32; COEFFICIENTS_PER_BLOCK],
}

/// Восстанавливает coupled полосу каждого канала, участвующего в coupling.
fn decouple_coupled_channels(
    context: &BlockContext,
    strategy: &CouplingStrategy,
    state: &FrameBlockState,
    coupling_channel: &CouplingChannelMantissas<'_>,
    flags: &ChannelFlags,
    dither: &mut DitherGenerator,
    output: &mut DecodedAudioBlock,
) -> Result<(), Ac3DecodeError> {
    let set = coupling_exponents(context, state)?;
    let view = CouplingChannelView {
        mantissas: coupling_channel.mantissas,
        bap: coupling_channel.bap,
        exponents: &set.exponents,
        begin_subband: strategy.layout.begin_subband,
        end_subband: strategy.layout.end_subband,
    };
    let no_phase_inversion = [false; MAX_COUPLING_SUBBANDS];
    for channel in 0..context.channel_count {
        if !strategy.in_coupling[channel] {
            continue;
        }
        let coordinates = state.coupling_coordinates[channel]
            .as_ref()
            .ok_or_else(|| {
                context.violation(BitstreamViolation::InvalidReuse(
                    ReusedElement::CouplingCoordinates,
                ))
            })?;
        // Фазовые флаги относятся только к правому каналу режима 2/0 (§7.4.1).
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
        let spectrum = &mut output.full_bandwidth[channel];
        decouple_channel(&view, &gains, dither, &mut spectrum.coefficients);
        spectrum.end_bin = strategy.layout.end_mantissa();
    }
    Ok(())
}
